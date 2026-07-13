//! Fuel-optimal launch guidance: which way the autopilot points the assembly's thrust,
//! and when to cut it, to leave the planet on the least fuel.
//!
//! **The physics that makes this simple.** Burning fuel does *not* reduce an assembly's
//! mass in this sim (parts carry a fixed density-mass), so there is no rocket-equation
//! trade — the propellant cost of a burn is just its **impulse**, `∫|thrust| dt`. With
//! constant mass the orbital-energy rate is `dE/dt = v·(T/m)`: to buy escape energy with
//! the least impulse you point thrust **along the velocity** (prograde), which maximises
//! `v·T` at every instant. That single fact is the whole guidance law:
//!
//! 1. **Vertical hold** at very low speed — velocity has no reliable direction yet and we
//!    must build vertical speed and clear the pad, so thrust points radially *up* (away
//!    from the planet centre). A small **pitchover** is blended in as speed builds, tipping
//!    thrust toward a fixed downrange azimuth to seed the turn — the one thing a
//!    perfectly-vertical prograde law can never do on its own (straight up is an unstable
//!    equilibrium).
//! 2. **Gravity turn** once moving — thrust points prograde. Gravity bends the velocity
//!    over; prograde thrust follows the arc, spending impulse where it does the most good.
//! 3. **Cutoff** the instant escape energy is reached — any burn past `E ≥ 0` is wasted.
//!
//! The lone free parameter is the **pitchover angle**: `0` gives a pure vertical ascent
//! (an unstable optimum with the lowest sideways energy but the highest gravity loss),
//! larger angles trade sideways energy for lower gravity loss. On a 15 km planet whose
//! gravity falls off fast, the best angle is small — so we let the optimizer *measure* it
//! (see [`optimize_pitchover`]) rather than guess. Everything else here is a pure function
//! of the vehicle's live true position + velocity, so the same law drives the live
//! autopilot, the dotted-line trajectory preview, and the optimizer's forward sim — they
//! can never disagree.

use crate::map::{gravity_at, GRAVITY_MU, GRAVITY_REF_RADIUS, PLANET_CENTER_Y};
use bevy::math::Vec3;

/// The planet centre in true world coordinates (the fixed frame `gravity_at` uses).
pub const PLANET_CENTER: Vec3 = Vec3::new(0.0, PLANET_CENTER_Y, 0.0);

/// Radius (m from centre) the forward sim treats as the ground: a trajectory that sinks
/// below the launch pad has crashed, not escaped. Without this floor the optimizer would
/// happily pick a near-horizontal launch that builds orbital speed while gravity pulls it
/// *through* the planet (gravity loss is lowest thrusting sideways — but you have to not
/// hit the ground while you do it). The floor is what forces a real gravity turn: climb
/// enough to keep clear, then lean over.
pub const GROUND_RADIUS: f32 = GRAVITY_REF_RADIUS - 2.0;

/// Speed (m/s) below which the autopilot holds the vertical/pitchover attitude rather
/// than steering prograde: under this the velocity direction is too noisy to chase and
/// the vehicle is still clearing the pad. By the time it is reached (a second or two of
/// climb) the stack is tens of metres up and moving cleanly.
pub const TURN_SPEED: f32 = 40.0;

/// The fixed horizontal direction the pitchover leans toward. The planet is spherically
/// symmetric so the choice is arbitrary; every thrust site shares this one so the server
/// and its predicted clients turn the same way.
pub const DOWNRANGE_AZIMUTH: Vec3 = Vec3::X;

/// How an ascent is shaped. The pitchover angle is the only knob; the azimuth just fixes
/// which way the (spherically-symmetric) turn leans so a flight is repeatable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AscentPolicy {
    /// Pitchover angle (rad) blended in by [`TURN_SPEED`]: how far off straight-up the
    /// thrust tips to seed the gravity turn. `0` = straight up. The optimizer searches it.
    pub pitchover_rad: f32,
    /// Downrange azimuth the pitchover leans toward — any fixed horizontal direction.
    pub azimuth: Vec3,
}

impl Default for AscentPolicy {
    fn default() -> Self {
        // Straight up until the optimizer measures a better angle — the safe, no-surprise
        // baseline (and the behaviour a pure prograde law converges to with no kick).
        Self { pitchover_rad: 0.0, azimuth: Vec3::X }
    }
}

/// The autopilot's command for a tick: which way to point the net thrust (unit vector)
/// and how hard to burn (`throttle` ∈ [0, 1] — the *overall* scale on top of the
/// per-engine balance, `0` once escape is reached).
#[derive(Clone, Copy, Debug)]
pub struct Guidance {
    pub thrust_dir: Vec3,
    pub throttle: f32,
}

/// Specific orbital energy `½v² − μ/r` (J/kg) at a true world position + velocity.
/// `≥ 0` means the assembly is on an escape trajectory — it will leave the planet even
/// with the engines off.
pub fn specific_energy(true_pos: Vec3, true_vel: Vec3) -> f32 {
    let r = (true_pos - PLANET_CENTER).length().max(1.0);
    0.5 * true_vel.length_squared() - GRAVITY_MU / r
}

/// Whether the assembly has reached escape energy (`E ≥ 0`).
pub fn escaped(true_pos: Vec3, true_vel: Vec3) -> bool {
    specific_energy(true_pos, true_vel) >= 0.0
}

/// The commanded thrust direction (unit) for the ascent law at a true position +
/// velocity. Radial-up with a blended pitchover below [`TURN_SPEED`], prograde above it.
pub fn ascent_thrust_dir(true_pos: Vec3, true_vel: Vec3, policy: AscentPolicy) -> Vec3 {
    let up = (true_pos - PLANET_CENTER).normalize_or(Vec3::Y);
    let speed = true_vel.length();
    if speed >= TURN_SPEED {
        // Gravity turn: follow velocity. `normalize_or(up)` guards the (unreachable at
        // this speed) zero-velocity case.
        return true_vel.normalize_or(up);
    }
    // Vertical/pitchover hold: tip from radial toward the downrange azimuth, the tilt
    // growing linearly to `pitchover_rad` as speed approaches TURN_SPEED so the kick is a
    // ramp, not a step.
    let horiz = (policy.azimuth - up * up.dot(policy.azimuth)).normalize_or_zero();
    let angle = policy.pitchover_rad * (speed / TURN_SPEED);
    (up * angle.cos() + horiz * angle.sin()).normalize_or(up)
}

/// The full guidance command for a tick: [`ascent_thrust_dir`] plus a throttle that cuts
/// to zero once escape energy is reached (burning past `E ≥ 0` only wastes fuel).
pub fn ascent_guidance(true_pos: Vec3, true_vel: Vec3, policy: AscentPolicy) -> Guidance {
    let throttle = if escaped(true_pos, true_vel) { 0.0 } else { 1.0 };
    Guidance { thrust_dir: ascent_thrust_dir(true_pos, true_vel, policy), throttle }
}

/// The outcome of forward-simulating the ascent law as a point mass (see [`propagate`]).
pub struct Prediction {
    /// Sampled true-world positions along the predicted path (for the trajectory line).
    pub path: Vec<Vec3>,
    /// Fuel spent to escape, as impulse per unit mass (m/s) — multiply by the assembly
    /// mass for N·s. `None` if escape wasn't reached within the step budget.
    pub burn_dv: Option<f32>,
    /// Whether escape energy was reached before the budget ran out.
    pub escaped: bool,
}

/// Forward-integrate the ascent law as a **point mass** from a true position + velocity:
/// full-throttle prograde thrust (magnitude `thrust_accel` m/s²) plus the planet's radial
/// gravity, until escape or `max_steps`. Semi-implicit Euler at `dt`. Reused by the
/// trajectory preview (needs `path`) and the optimizer (needs `burn_dv`).
///
/// It treats the stack as a point that thrusts exactly along the guidance direction —
/// i.e. it assumes attitude tracks instantly. That's the right fidelity for "where is the
/// autopilot taking me" and for ranking pitchover angles; the live controller handles the
/// real attitude lag.
pub fn propagate(
    mut pos: Vec3,
    mut vel: Vec3,
    thrust_accel: f32,
    policy: AscentPolicy,
    dt: f32,
    max_steps: usize,
    sample_every: usize,
    floor_radius: f32,
) -> Prediction {
    let mut path = Vec::with_capacity(max_steps / sample_every.max(1) + 1);
    let mut burn_dv = 0.0f32;
    let mut escaped_at = None;
    for step in 0..max_steps {
        if step % sample_every.max(1) == 0 {
            path.push(pos);
        }
        let g = ascent_guidance(pos, vel, policy);
        let accel = g.thrust_dir * (thrust_accel * g.throttle) + gravity_at(pos);
        burn_dv += thrust_accel * g.throttle * dt;
        vel += accel * dt;
        pos += vel * dt;
        // Dropped below the floor before escaping → this trajectory is not a valid
        // ascent (fuel-to-escape is undefined). The optimizer reads `burn_dv: None` as
        // failure. The live/preview path passes the true ground; the optimizer passes a
        // floor raised by a clearance margin so it never picks a pitchover that flies at
        // the crash edge (where the real, attitude-lagging vehicle could stray under).
        if (pos - PLANET_CENTER).length() < floor_radius {
            path.push(pos);
            return Prediction { path, burn_dv: None, escaped: false };
        }
        if escaped_at.is_none() && escaped(pos, vel) {
            escaped_at = Some(burn_dv);
            path.push(pos);
            break;
        }
    }
    Prediction { path, burn_dv: escaped_at, escaped: escaped_at.is_some() }
}

/// Find the pitchover angle that reaches escape on the least fuel from a given true
/// state, by forward-simulating [`propagate`] over a coarse-then-fine sweep of angles.
/// Returns the best [`AscentPolicy`]. This is the "figure out the efficient path" step:
/// it re-plans from the vehicle's *current* thrust-to-weight, mass, altitude and velocity,
/// so the answer adapts as those change through the flight.
pub fn optimize_pitchover(
    true_pos: Vec3,
    true_vel: Vec3,
    thrust_accel: f32,
    azimuth: Vec3,
    dt: f32,
    max_steps: usize,
) -> AscentPolicy {
    // Min fuel-to-escape sits right at the crash boundary (more pitchover = less fuel,
    // until the arc dips back into the ground). Flying the exact boundary is reckless for
    // the real attitude-lagging vehicle, so we find the min-fuel angle against the true
    // ground and then back it off by this fraction for altitude margin — a less-aggressive
    // turn climbs higher.
    const SAFETY: f32 = 0.85;
    let floor = GROUND_RADIUS;
    // Cost of a candidate angle = fuel-to-escape (infinite if it crashes or fails to escape
    // in budget, so the search always prefers a trajectory that actually leaves).
    let cost = |angle: f32| {
        let policy = AscentPolicy { pitchover_rad: angle, azimuth };
        propagate(true_pos, true_vel, thrust_accel, policy, dt, max_steps, max_steps, floor)
            .burn_dv
            .unwrap_or(f32::INFINITY)
    };
    // Coarse sweep 0..75° then refine around the best with ternary passes — the cost is
    // smooth and unimodal in the angle here (rising gravity loss toward 0°, the crash
    // cliff toward horizontal).
    let mut best_angle = 0.0f32;
    let mut best_cost = f32::INFINITY;
    let coarse = 15;
    for i in 0..=coarse {
        let angle = (i as f32 / coarse as f32) * (75.0_f32.to_radians());
        let c = cost(angle);
        if c < best_cost {
            best_cost = c;
            best_angle = angle;
        }
    }
    // Local refine: shrink a window around best_angle.
    let mut lo = (best_angle - 5.0_f32.to_radians()).max(0.0);
    let mut hi = best_angle + 5.0_f32.to_radians();
    for _ in 0..20 {
        let m1 = lo + (hi - lo) / 3.0;
        let m2 = hi - (hi - lo) / 3.0;
        if cost(m1) < cost(m2) {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    let angle = 0.5 * (lo + hi) * SAFETY;
    AscentPolicy { pitchover_rad: angle, azimuth }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{GRAVITY_REF_RADIUS, SURFACE_GRAVITY};

    /// A point on the pad, at rest, is deep in the well: energy is very negative.
    #[test]
    fn at_rest_on_pad_is_bound() {
        let pos = Vec3::new(0.0, 0.0, 0.0); // world y=0 = the pad, r = GRAVITY_REF_RADIUS
        assert!(!escaped(pos, Vec3::ZERO));
        assert!(specific_energy(pos, Vec3::ZERO) < 0.0);
    }

    /// Escape speed at the surface is √(2μ/R); just over it escapes, just under doesn't —
    /// regardless of direction (energy is a scalar).
    #[test]
    fn escape_speed_threshold() {
        let pos = Vec3::new(0.0, 0.0, 0.0);
        let v_esc = (2.0 * GRAVITY_MU / GRAVITY_REF_RADIUS).sqrt();
        // ~543 m/s for this planet.
        assert!((v_esc - (2.0 * SURFACE_GRAVITY * GRAVITY_REF_RADIUS).sqrt()).abs() < 1.0);
        for dir in [Vec3::Y, Vec3::X, Vec3::new(1.0, 1.0, 0.0).normalize()] {
            assert!(escaped(pos, dir * (v_esc + 1.0)), "should escape along {dir:?}");
            assert!(!escaped(pos, dir * (v_esc - 1.0)), "should not escape along {dir:?}");
        }
    }

    /// Below the turn speed the command is essentially radial-up (small tilt); well above
    /// it the command follows velocity (prograde).
    #[test]
    fn steering_is_vertical_then_prograde() {
        let pos = Vec3::new(0.0, 100.0, 0.0);
        let policy = AscentPolicy { pitchover_rad: 10.0_f32.to_radians(), azimuth: Vec3::X };
        // Near-zero speed: within the pitchover of straight up.
        let slow = ascent_thrust_dir(pos, Vec3::new(0.0, 1.0, 0.0), policy);
        assert!(slow.dot(Vec3::Y) > 0.9, "slow dir {slow:?} should be ~up");
        // Fast, moving down-range at 45°: command tracks that velocity.
        let vel = Vec3::new(200.0, 200.0, 0.0);
        let fast = ascent_thrust_dir(pos, vel, policy);
        assert!(fast.dot(vel.normalize()) > 0.999, "fast dir {fast:?} should be prograde");
    }

    /// Guidance cuts the throttle once escape energy is reached.
    #[test]
    fn throttle_cuts_at_escape() {
        let pos = Vec3::new(0.0, 0.0, 0.0);
        let v_esc = (2.0 * GRAVITY_MU / GRAVITY_REF_RADIUS).sqrt();
        assert_eq!(ascent_guidance(pos, Vec3::Y * (v_esc + 5.0), AscentPolicy::default()).throttle, 0.0);
        assert_eq!(ascent_guidance(pos, Vec3::Y * 10.0, AscentPolicy::default()).throttle, 1.0);
    }

    /// A stack with plenty of thrust-to-weight escapes straight up, and the optimizer's
    /// chosen pitchover is at least as cheap as going straight up.
    #[test]
    fn optimizer_beats_or_matches_straight_up() {
        let pos = Vec3::new(0.0, 0.0, 0.0);
        let thrust_accel = 3.0 * SURFACE_GRAVITY; // TWR 3 at the surface
        let dt = 0.05;
        let max_steps = 4000;
        let floor = GRAVITY_REF_RADIUS - 2.0;
        let straight = propagate(
            pos,
            Vec3::ZERO,
            thrust_accel,
            AscentPolicy::default(),
            dt,
            max_steps,
            max_steps,
            floor,
        );
        assert!(straight.escaped, "TWR-3 stack should escape straight up");
        let best = optimize_pitchover(pos, Vec3::ZERO, thrust_accel, Vec3::X, dt, max_steps);
        // The optimized angle leans over, so it must beat straight up on fuel.
        let best_cost =
            propagate(pos, Vec3::ZERO, thrust_accel, best, dt, max_steps, max_steps, floor)
                .burn_dv
                .unwrap();
        assert!(
            best_cost < straight.burn_dv.unwrap(),
            "optimized {best_cost} should beat straight-up {:?}",
            straight.burn_dv
        );
    }
}

