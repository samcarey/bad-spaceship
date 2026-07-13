//! Fuel-optimal launch guidance: which way the autopilot points the assembly's thrust,
//! and when to cut it, to leave the planet on the least fuel.
//!
//! **The physics.** Burning fuel does *not* reduce an assembly's mass in this sim (parts
//! carry a fixed density-mass), so there is no rocket-equation trade — the propellant cost
//! of a burn is just its **impulse**, `∫|thrust| dt`. What a burn *wastes* is **gravity
//! loss**: while thrust fights gravity head-on, `1/TWR` of the propellant does nothing but
//! hold the ship up. A **gravity turn** — a small pitchover kick at liftoff, then thrust
//! held prograde while gravity bends the arc over — banks horizontal speed that gravity
//! can't tax, so it beats a vertical climb by a margin that explodes as TWR → 1 (a
//! hovering-slow vertical ascent wastes nearly everything) and vanishes as TWR grows (a
//! strong stack is out of the well before gravity loss matters). With engines at real
//! first-stage strength (see `ROCKET_THRUST_PART_WEIGHTS`), a typical cargo-laden build
//! sits at TWR ~1.2–1.5 where the turn saves 15–35% — the turn is how you fly efficiently
//! — while an engine-dense, low-payload stack reaches TWR ≳2.5 where straight up is fine.
//!
//! **The law** (a pure function of the vehicle's live true state, so the live autopilot,
//! the trajectory preview, and the optimizer's forward sim can never disagree):
//!
//! 1. **Pitchover kick** below [`TURN_SPEED`]: thrust starts radial-out and tips toward
//!    [`DOWNRANGE_AZIMUTH`] by up to the policy angle as speed builds — the one thing a
//!    prograde law can't do on its own (straight up is an unstable equilibrium).
//! 2. **Gravity turn** above it: thrust prograde; the arc shape then adapts continuously
//!    to the vehicle's actual thrust, mass, and the weakening gravity field.
//! 3. **Escape cutoff**: throttle to zero the instant `E = ½v² − μ/r ≥ 0` — past that the
//!    ship coasts away on its own, so every further second of burn is wasted.
//!
//! **The one free parameter** — the pitchover angle — is chosen per assembly by
//! [`optimize_pitchover`]: a point-mass forward-sim sweep from the vehicle's launch state
//! (its real thrust-to-weight), constrained by a ground floor (a too-aggressive turn arcs
//! back into the terrain before escaping) and backed off from the crash boundary for
//! attitude-lag margin. Weak stacks get a gentle lean (~5–15°), strong stacks a hard one
//! (or fly nearly straight); the server replicates the chosen angle so predicted clients
//! fly the identical turn.

use crate::map::{gravity_at, GRAVITY_MU, GRAVITY_REF_RADIUS, PLANET_CENTER_Y};
use bevy::math::Vec3;

/// The planet centre in true world coordinates (the fixed frame `gravity_at` uses).
pub const PLANET_CENTER: Vec3 = Vec3::new(0.0, PLANET_CENTER_Y, 0.0);

/// Radius (m from centre) the forward sim treats as the ground: a trajectory that sinks
/// back below the launch pad has crashed, not escaped. Without this floor the optimizer
/// would pick a near-horizontal launch that builds orbital speed while gravity pulls it
/// *through* the planet — the floor is what forces a real gravity turn: climb enough to
/// keep clear, then lean.
pub const GROUND_RADIUS: f32 = GRAVITY_REF_RADIUS - 2.0;

/// Speed (m/s) below which the autopilot holds the vertical/pitchover attitude rather
/// than steering prograde: under this the velocity direction is too noisy to chase and
/// the vehicle is still clearing the pad.
pub const TURN_SPEED: f32 = 40.0;

/// The fixed horizontal direction the pitchover leans toward. The planet is spherically
/// symmetric so the choice is arbitrary; shared so the server and its predicted clients
/// turn the same way.
pub const DOWNRANGE_AZIMUTH: Vec3 = Vec3::X;

/// Fallback pitchover (rad) when no optimized angle is available yet (e.g. the replicated
/// value hasn't arrived, or an assembly with no measurable mass): straight up — always
/// safe, never optimal for a heavy build.
pub const DEFAULT_PITCHOVER: f32 = 0.0;

/// The autopilot's command for a tick: which way to point the net thrust (unit vector)
/// and how hard to burn (`throttle` ∈ {0, 1} — `0` once escape energy is reached).
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
/// velocity: radial-out tipped by the blended `pitchover` below [`TURN_SPEED`], prograde
/// (a gravity turn) above it. `pitchover` of 0 is a pure vertical ascent.
pub fn ascent_thrust_dir(true_pos: Vec3, true_vel: Vec3, pitchover: f32) -> Vec3 {
    let up = (true_pos - PLANET_CENTER).normalize_or(Vec3::Y);
    let speed = true_vel.length();
    if speed >= TURN_SPEED && pitchover.abs() >= 1e-4 {
        // Gravity turn: follow velocity. `normalize_or(up)` guards the (unreachable at
        // this speed) zero-velocity case.
        return true_vel.normalize_or(up);
    }
    if pitchover.abs() < 1e-4 {
        return up; // vertical ascent: never chases a sideways disturbance
    }
    // Pitchover kick: tip from radial toward the downrange azimuth, the tilt growing
    // linearly to `pitchover` as speed approaches TURN_SPEED, so the kick is a ramp the
    // attitude loop can track rather than a step.
    let horiz = (DOWNRANGE_AZIMUTH - up * up.dot(DOWNRANGE_AZIMUTH)).normalize_or_zero();
    let angle = pitchover * (speed / TURN_SPEED);
    (up * angle.cos() + horiz * angle.sin()).normalize_or(up)
}

/// The full guidance command for a tick: [`ascent_thrust_dir`] plus a throttle that cuts
/// to zero once escape energy is reached (burning past `E ≥ 0` only wastes fuel).
pub fn ascent_guidance(true_pos: Vec3, true_vel: Vec3, pitchover: f32) -> Guidance {
    let throttle = if escaped(true_pos, true_vel) { 0.0 } else { 1.0 };
    Guidance { thrust_dir: ascent_thrust_dir(true_pos, true_vel, pitchover), throttle }
}

/// A **pitch program**: the ascent command precomputed as "flight-path angle vs speed",
/// sampled off the ideal point-mass trajectory at launch. The live autopilot flies *this*
/// instead of chasing the raw prograde direction.
///
/// Why: closed-loop prograde steering tracks a target that moves every tick (the live
/// velocity vector, noise included), so the throttle/gimbal allocator trims continuously
/// for the whole climb — recorded A/B flights measured ~5–7% of the burn lost to that
/// chatter, *more* than a gentle turn saves, regardless of how small the kick angle was.
/// Real launch vehicles solve this exactly this way: guidance is an open-loop pitch
/// program, and only *attitude* is closed-loop. The program's command varies smoothly and
/// slowly (it's indexed by the vehicle's own speed, monotonic during a burn), so holding
/// it costs the attitude loop no more than holding straight-up — while the trajectory
/// still bends into the fuel-saving arc.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PitchProgram {
    /// `(speed m/s, command angle from radial-up rad)`, speeds strictly ascending —
    /// the ideal law's own command schedule along its trajectory.
    samples: Vec<(f32, f32)>,
}

impl PitchProgram {
    /// Build the program by flying the ideal ascent law ([`ascent_thrust_dir`]) as a
    /// point mass from the launch state and recording its command angle at each speed.
    /// A zero `pitchover` yields an all-zero program (straight up), so high-TWR stacks
    /// are byte-identical to the fixed vertical command.
    pub fn build(true_pos: Vec3, true_vel: Vec3, thrust_accel: f32, pitchover: f32) -> Self {
        let mut samples = Vec::new();
        let mut pos = true_pos;
        let mut vel = true_vel;
        for _ in 0..OPTIMIZER_STEPS {
            let dir = ascent_thrust_dir(pos, vel, pitchover);
            let up = (pos - PLANET_CENTER).normalize_or(Vec3::Y);
            let speed = vel.length();
            let angle = dir.dot(up).clamp(-1.0, 1.0).acos();
            if samples.last().is_none_or(|&(s, _)| speed > s + 1.0) {
                samples.push((speed, angle));
            }
            if escaped(pos, vel) {
                break;
            }
            let accel = dir * thrust_accel + gravity_at(pos);
            vel += accel * OPTIMIZER_DT;
            pos += vel * OPTIMIZER_DT;
            // Ideal trajectory dove below the pad (an over-aggressive angle the optimizer
            // shouldn't have picked): stop sampling rather than record a descent.
            if (pos - PLANET_CENTER).length() < GROUND_RADIUS {
                break;
            }
        }
        Self { samples }
    }

    /// The command angle (rad from radial-up) at a given speed — linear interpolation,
    /// clamped to the table ends. Empty table = straight up.
    pub fn angle_at(&self, speed: f32) -> f32 {
        let Some(&(first_s, first_a)) = self.samples.first() else {
            return 0.0;
        };
        if speed <= first_s {
            return first_a;
        }
        for pair in self.samples.windows(2) {
            let (s0, a0) = pair[0];
            let (s1, a1) = pair[1];
            if speed <= s1 {
                let t = ((speed - s0) / (s1 - s0)).clamp(0.0, 1.0);
                return a0 + (a1 - a0) * t;
            }
        }
        self.samples.last().map(|&(_, a)| a).unwrap_or(0.0)
    }

    /// The commanded thrust direction at a true position + speed: radial-up tipped by
    /// [`Self::angle_at`] toward [`DOWNRANGE_AZIMUTH`].
    pub fn thrust_dir(&self, true_pos: Vec3, speed: f32) -> Vec3 {
        let up = (true_pos - PLANET_CENTER).normalize_or(Vec3::Y);
        let angle = self.angle_at(speed);
        if angle.abs() < 1e-4 {
            return up;
        }
        let horiz = (DOWNRANGE_AZIMUTH - up * up.dot(DOWNRANGE_AZIMUTH)).normalize_or_zero();
        (up * angle.cos() + horiz * angle.sin()).normalize_or(up)
    }
}

/// The live autopilot's guidance command: the pitch-program direction at the vehicle's
/// current speed, plus the escape-energy throttle cutoff. This is what the three thrust
/// sites fly; [`ascent_guidance`] (the raw closed-loop law) remains the *planning* model
/// the program is sampled from.
pub fn program_guidance(true_pos: Vec3, true_vel: Vec3, program: &PitchProgram) -> Guidance {
    let throttle = if escaped(true_pos, true_vel) { 0.0 } else { 1.0 };
    Guidance { thrust_dir: program.thrust_dir(true_pos, true_vel.length()), throttle }
}

/// The outcome of forward-simulating the ascent law as a point mass (see [`propagate`]).
pub struct Prediction {
    /// Sampled true-world positions along the predicted path (for the trajectory line).
    pub path: Vec<Vec3>,
    /// Fuel spent to escape, as impulse per unit mass (m/s) — multiply by the assembly
    /// mass for N·s. `None` if the trajectory crashed or ran out of steps first.
    pub burn_dv: Option<f32>,
    /// Whether escape energy was reached before the budget ran out.
    pub escaped: bool,
}

/// Forward-integrate the ascent law as a **point mass** from a true position + velocity:
/// thrust of magnitude `thrust_accel` (m/s²) along the guidance direction plus the
/// planet's radial gravity, until escape, a crash below `floor_radius`, or `max_steps`.
/// Semi-implicit Euler at `dt`; the path is sampled every `sample_every` steps. Reused by
/// the trajectory preview (needs `path`) and the optimizer (needs `burn_dv`).
///
/// It treats the stack as a point that thrusts exactly along the guidance direction —
/// i.e. it assumes attitude tracks instantly. That's the right fidelity for "where is the
/// autopilot taking me" and for ranking pitchover angles; the live controller handles the
/// real attitude lag (and the optimizer's safety backoff covers the difference).
pub fn propagate(
    mut pos: Vec3,
    mut vel: Vec3,
    thrust_accel: f32,
    pitchover: f32,
    dt: f32,
    max_steps: usize,
    sample_every: usize,
    floor_radius: f32,
) -> Prediction {
    let mut path = Vec::with_capacity(max_steps / sample_every.max(1) + 2);
    let mut burn_dv = 0.0f32;
    for step in 0..max_steps {
        if step % sample_every.max(1) == 0 {
            path.push(pos);
        }
        let g = ascent_guidance(pos, vel, pitchover);
        let accel = g.thrust_dir * (thrust_accel * g.throttle) + gravity_at(pos);
        burn_dv += thrust_accel * g.throttle * dt;
        vel += accel * dt;
        pos += vel * dt;
        // Sank below the floor before escaping → crashed; fuel-to-escape is undefined.
        if (pos - PLANET_CENTER).length() < floor_radius {
            path.push(pos);
            return Prediction { path, burn_dv: None, escaped: false };
        }
        if escaped(pos, vel) {
            path.push(pos);
            return Prediction { path, burn_dv: Some(burn_dv), escaped: true };
        }
    }
    Prediction { path, burn_dv: None, escaped: false }
}

/// Point-mass integration step for [`propagate`] / [`optimize_pitchover`] (s). Coarse is
/// fine: the sweep only *ranks* pitchover angles, it doesn't fly them.
pub const OPTIMIZER_DT: f32 = 0.1;
/// Step budget for one optimizer trajectory — 0.1 s × 4000 = 400 sim-seconds, enough for
/// the slowest barely-lifting stack to reach escape or demonstrably fail.
pub const OPTIMIZER_STEPS: usize = 4000;

/// Find the pitchover angle (rad) that reaches escape on the least fuel from a given true
/// state, by forward-simulating [`propagate`] over a coarse-then-refined sweep of angles.
/// `thrust_accel` is the assembly's full-throttle thrust acceleration (Σ engine thrust /
/// total mass). This is the per-assembly "figure out the efficient path" step — it adapts
/// the flight plan to whatever the player built.
///
/// The min-fuel angle sits right at the crash boundary (more lean = less gravity loss,
/// until the arc clips the terrain), and the real attitude-lagged vehicle shouldn't fly
/// the exact boundary — so the returned angle is backed off by `SAFETY`, trading a little
/// fuel for altitude margin.
pub fn optimize_pitchover(true_pos: Vec3, true_vel: Vec3, thrust_accel: f32) -> f32 {
    const SAFETY: f32 = 0.85;
    /// Attitude-execution cost of a commanded lean, as extra fuel Δv (m/s) per radian of
    /// pitchover. The point-mass sim assumes thrust snaps to the guidance direction; the
    /// real stack must physically rotate, and imperfect tracking points some thrust
    /// off-plan. Charging the sweep for it keeps the optimizer off angles that only pay
    /// on paper, and gives engine-dense stacks (steep ideal turn, small gravity-loss
    /// prize) their measured-cheapest near-vertical ascent.
    ///
    /// Calibration history — this constant is tied to HOW the command is flown:
    /// - Prograde-chasing era (thrust tracked the live velocity vector): execution ate
    ///   5–7% of every burn at ANY angle (allocator trimmed continuously against the
    ///   moving target), and a TWR-2.1 stack commanded 29° trimmed itself to
    ///   hover-thrust and broke up → the penalty had to sit at 300 m/s/rad, which
    ///   capped every build's turn at ~17°.
    /// - Pitch-program era (current): the command is a smooth angle-vs-speed schedule,
    ///   as cheap to hold as vertical — measured turn savings now land ON the
    ///   point-mass prediction (19–22%). 50 covers the residual attitude lag while
    ///   letting mid-TWR builds fly the real optimum (~25–35°); the straight-up
    ///   transition moves to TWR ≈ 2.2–2.4, which engine-count asymptotics make
    ///   genuinely "lots of rockets" territory (a bare engine is TWR ~2.2).
    const EXECUTION_PENALTY_PER_RAD: f32 = 50.0;
    let cost = |angle: f32| {
        let ideal = propagate(
            true_pos,
            true_vel,
            thrust_accel,
            angle,
            OPTIMIZER_DT,
            OPTIMIZER_STEPS,
            OPTIMIZER_STEPS, // no path samples needed — only the fuel figure
            GROUND_RADIUS,
        )
        .burn_dv
        .unwrap_or(f32::INFINITY);
        ideal + EXECUTION_PENALTY_PER_RAD * angle
    };
    // Coarse sweep 0..75°, then ternary refinement around the best — the cost is smooth
    // and unimodal in the angle (rising gravity loss toward 0°, the crash cliff toward
    // horizontal), and INFINITY past the cliff.
    let mut best_angle = 0.0f32;
    let mut best_cost = f32::INFINITY;
    let coarse = 15;
    for i in 0..=coarse {
        let angle = (i as f32 / coarse as f32) * 75.0_f32.to_radians();
        let c = cost(angle);
        if c < best_cost {
            best_cost = c;
            best_angle = angle;
        }
    }
    let mut lo = (best_angle - 5.0_f32.to_radians()).max(0.0);
    let mut hi = best_angle + 5.0_f32.to_radians();
    for _ in 0..12 {
        let m1 = lo + (hi - lo) / 3.0;
        let m2 = hi - (hi - lo) / 3.0;
        if cost(m1) < cost(m2) {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    0.5 * (lo + hi) * SAFETY
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::SURFACE_GRAVITY;

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
        assert!((v_esc - (2.0 * SURFACE_GRAVITY * GRAVITY_REF_RADIUS).sqrt()).abs() < 1.0);
        for dir in [Vec3::Y, Vec3::X, Vec3::new(1.0, 1.0, 0.0).normalize()] {
            assert!(escaped(pos, dir * (v_esc + 1.0)), "should escape along {dir:?}");
            assert!(!escaped(pos, dir * (v_esc - 1.0)), "should not escape along {dir:?}");
        }
    }

    /// Zero pitchover commands radial-out regardless of drift — it never chases a
    /// disturbance; a nonzero pitchover leans at low speed and goes prograde once fast.
    #[test]
    fn steering_vertical_kick_then_prograde() {
        let pos = Vec3::new(0.0, 100.0, 0.0);
        let up = (pos - PLANET_CENTER).normalize();
        for vel in [Vec3::ZERO, Vec3::new(50.0, 5.0, 0.0)] {
            let dir = ascent_thrust_dir(pos, vel, 0.0);
            assert!(dir.dot(up) > 0.999, "zero pitchover should be radial-up, got {dir:?}");
        }
        let slow = ascent_thrust_dir(pos, Vec3::new(0.0, 1.0, 0.0), 10.0_f32.to_radians());
        assert!(slow.dot(Vec3::Y) > 0.9 && slow.x > 0.0, "kick should tip toward +x: {slow:?}");
        let vel = Vec3::new(200.0, 200.0, 0.0);
        let fast = ascent_thrust_dir(pos, vel, 10.0_f32.to_radians());
        assert!(fast.dot(vel.normalize()) > 0.999, "fast should be prograde: {fast:?}");
    }

    /// Guidance cuts the throttle once escape energy is reached.
    #[test]
    fn throttle_cuts_at_escape() {
        let pos = Vec3::new(0.0, 0.0, 0.0);
        let v_esc = (2.0 * GRAVITY_MU / GRAVITY_REF_RADIUS).sqrt();
        assert_eq!(ascent_guidance(pos, Vec3::Y * (v_esc + 5.0), 0.0).throttle, 0.0);
        assert_eq!(ascent_guidance(pos, Vec3::Y * 10.0, 0.0).throttle, 1.0);
    }

    /// The optimizer never picks a crashing angle, and its choice beats or matches
    /// straight up on fuel — with a low-TWR stack it must beat it by a wide margin
    /// (that's the whole point of the turn at real-rocket thrust levels).
    #[test]
    fn optimizer_beats_straight_up_at_low_twr() {
        let pos = Vec3::new(0.0, 0.0, 0.0);
        let thrust_accel = 1.35 * SURFACE_GRAVITY; // a heavy hauler
        let straight = propagate(
            pos,
            Vec3::ZERO,
            thrust_accel,
            0.0,
            OPTIMIZER_DT,
            OPTIMIZER_STEPS,
            OPTIMIZER_STEPS,
            GROUND_RADIUS,
        );
        assert!(straight.escaped, "even TWR 1.35 escapes straight up eventually");
        let angle = optimize_pitchover(pos, Vec3::ZERO, thrust_accel);
        assert!(angle > 1.0_f32.to_radians(), "low TWR should get a real lean: {angle}");
        let turned = propagate(
            pos,
            Vec3::ZERO,
            thrust_accel,
            angle,
            OPTIMIZER_DT,
            OPTIMIZER_STEPS,
            OPTIMIZER_STEPS,
            GROUND_RADIUS,
        );
        let (s, t) = (straight.burn_dv.unwrap(), turned.burn_dv.unwrap());
        assert!(t < s * 0.9, "turn ({t:.0}) should save >10% vs straight ({s:.0})");
    }

    /// The trajectory prediction produces a path that climbs and ends escaped.
    #[test]
    fn predicted_path_climbs_and_escapes() {
        let pos = Vec3::new(0.0, 0.0, 0.0);
        let p = propagate(
            pos,
            Vec3::ZERO,
            2.0 * SURFACE_GRAVITY,
            10.0_f32.to_radians(),
            OPTIMIZER_DT,
            OPTIMIZER_STEPS,
            20,
            GROUND_RADIUS,
        );
        assert!(p.escaped && p.path.len() > 5);
        let climb = (*p.path.last().unwrap() - PLANET_CENTER).length()
            - (pos - PLANET_CENTER).length();
        assert!(climb > 1000.0, "path should climb well clear of the pad: {climb:.0} m");
    }
}


