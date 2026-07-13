//! Fuel-optimal launch guidance: which way the autopilot points the assembly's thrust,
//! and when to cut it, to leave the planet on the least fuel.
//!
//! **The physics that makes this simple.** Burning fuel does *not* reduce an assembly's
//! mass in this sim (parts carry a fixed density-mass), so there is no rocket-equation
//! trade — the propellant cost of a burn is just its **impulse**, `∫|thrust| dt`.
//!
//! **What's actually optimal here (measured, not assumed).** The textbook move is a
//! gravity turn: pitch over so thrust follows velocity and gravity does less work against
//! it. That's a huge win on an Earth-sized world where you escape hundreds of km up. On
//! *this* 15 km planet it is **not** worth it — flight-recorder A/B tests on the real
//! vehicle show a pitched ascent burns *slightly more* fuel than straight up, for two
//! reasons: (1) you reach escape energy at only ~6–10 km, so there's almost no gravity
//! loss to recover; (2) the real stack has to physically rotate to pitch over (attitude
//! lag, gimbal-rate-limited), wasting thrust off-prograde during the slew — a cost the
//! idealized point-mass model can't see. So the autopilot flies **straight up** (thrust
//! radially outward, away from the planet centre — stable, and it never amplifies a
//! sideways disturbance the way chasing velocity would).
//!
//! **The real win is the cutoff.** The one big, guaranteed saving is cutting the throttle
//! to zero the instant the assembly reaches escape energy (`E = ½v² − μ/r ≥ 0`): past that
//! it will coast away on its own, so every further second of burn is wasted fuel. That's
//! the adaptive part — the autopilot watches its own orbital energy each tick and stops at
//! the optimal moment.
//!
//! A **pitchover** knob remains (default `0` = straight up, the measured optimum) so a
//! gravity turn can still be flown for the trajectory preview or a dramatic demo; it just
//! isn't the default because it costs fuel here. Everything is a pure function of the
//! vehicle's live true position + velocity, so the same law drives the live autopilot, the
//! dotted-line trajectory preview, and any offline analysis — they can never disagree.

use crate::map::{gravity_at, GRAVITY_MU, PLANET_CENTER_Y};
use bevy::math::Vec3;

/// The planet centre in true world coordinates (the fixed frame `gravity_at` uses).
pub const PLANET_CENTER: Vec3 = Vec3::new(0.0, PLANET_CENTER_Y, 0.0);

/// The default ascent pitchover (rad): `0` = straight up, which A/B flight tests show is
/// the fuel optimum on this planet (see the module docs). Both the client and server read
/// this same constant, so their predicted flights match with nothing to replicate.
pub const DEFAULT_PITCHOVER: f32 = 0.0;

/// Speed (m/s) below which the pitchover is blended in (the vehicle is still clearing the
/// pad and its velocity has no reliable direction yet). Above it a nonzero pitchover holds
/// a prograde gravity turn. Irrelevant at the default pitchover of 0.
pub const TURN_SPEED: f32 = 40.0;

/// The fixed horizontal direction a (nonzero) pitchover leans toward. The planet is
/// spherically symmetric so the choice is arbitrary; shared so every site turns the same
/// way.
pub const DOWNRANGE_AZIMUTH: Vec3 = Vec3::X;

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

/// The commanded thrust direction (unit) for the ascent law at a true position + velocity.
/// At the default `pitchover` of 0 this is simply **radial-out** (straight up). A nonzero
/// pitchover tips the thrust toward [`DOWNRANGE_AZIMUTH`] as speed builds, then follows
/// velocity (prograde) above [`TURN_SPEED`] — a gravity turn.
pub fn ascent_thrust_dir(true_pos: Vec3, true_vel: Vec3, pitchover: f32) -> Vec3 {
    let up = (true_pos - PLANET_CENTER).normalize_or(Vec3::Y);
    if pitchover.abs() < 1e-4 {
        return up; // straight up — the fuel optimum here
    }
    let speed = true_vel.length();
    if speed >= TURN_SPEED {
        return true_vel.normalize_or(up); // gravity turn: follow velocity
    }
    // Blend the pitchover in as speed approaches TURN_SPEED so the kick is a ramp.
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

/// Forward-integrate the ascent law as a **point mass** from a true position + velocity —
/// full-throttle thrust (magnitude `thrust_accel` m/s²) along the guidance direction plus
/// the planet's radial gravity, until escape (energy ≥ 0) or `max_steps`, sampling the path
/// every `sample_every` steps. Semi-implicit Euler at `dt`. This is what the dotted
/// trajectory preview draws: it treats the stack as a point that thrusts exactly along the
/// guidance direction (assumes attitude tracks instantly), which is the right fidelity for
/// "roughly where is the autopilot taking me". Returns the sampled true-world positions.
pub fn predict_path(
    mut pos: Vec3,
    mut vel: Vec3,
    thrust_accel: f32,
    pitchover: f32,
    dt: f32,
    max_steps: usize,
    sample_every: usize,
) -> Vec<Vec3> {
    let mut path = Vec::with_capacity(max_steps / sample_every.max(1) + 2);
    for step in 0..max_steps {
        if step % sample_every.max(1) == 0 {
            path.push(pos);
        }
        let g = ascent_guidance(pos, vel, pitchover);
        let accel = g.thrust_dir * (thrust_accel * g.throttle) + gravity_at(pos);
        vel += accel * dt;
        pos += vel * dt;
        // Once escaped, sample a few more coasting points to show where it's heading,
        // then stop — the trajectory is settled.
        if g.throttle == 0.0 && escaped(pos, vel) && step % sample_every.max(1) == 0 && path.len() > 3
        {
            path.push(pos);
        }
        if escaped(pos, vel) && path.len() >= 40 {
            break;
        }
    }
    path.push(pos);
    path
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
        assert!((v_esc - (2.0 * SURFACE_GRAVITY * GRAVITY_REF_RADIUS).sqrt()).abs() < 1.0);
        for dir in [Vec3::Y, Vec3::X, Vec3::new(1.0, 1.0, 0.0).normalize()] {
            assert!(escaped(pos, dir * (v_esc + 1.0)), "should escape along {dir:?}");
            assert!(!escaped(pos, dir * (v_esc - 1.0)), "should not escape along {dir:?}");
        }
    }

    /// At the default pitchover the command is radial-out (straight up), independent of
    /// which way the vehicle happens to be drifting — it never chases a disturbance.
    #[test]
    fn default_is_straight_up() {
        let pos = Vec3::new(0.0, 100.0, 0.0);
        let up = (pos - PLANET_CENTER).normalize();
        for vel in [Vec3::ZERO, Vec3::new(50.0, 5.0, 0.0), Vec3::new(-200.0, 100.0, 30.0)] {
            let dir = ascent_thrust_dir(pos, vel, DEFAULT_PITCHOVER);
            assert!(dir.dot(up) > 0.999, "default should be radial-up, got {dir:?}");
        }
    }

    /// A nonzero pitchover leans off vertical at low speed and tracks velocity once fast.
    #[test]
    fn pitchover_turns_then_follows_velocity() {
        let pos = Vec3::new(0.0, 100.0, 0.0);
        let slow = ascent_thrust_dir(pos, Vec3::new(0.0, 1.0, 0.0), 10.0_f32.to_radians());
        assert!(slow.dot(Vec3::Y) > 0.9 && slow.x > 0.0, "slow should tip toward +x: {slow:?}");
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

    /// A stack with thrust-to-weight > 1 escapes straight up, and the predicted path both
    /// climbs and reaches escape (throttle cuts, so the path ends coasting outward).
    #[test]
    fn predicted_path_climbs_and_escapes() {
        let pos = Vec3::new(0.0, 0.0, 0.0);
        let thrust_accel = 2.0 * SURFACE_GRAVITY; // TWR 2 at the surface
        let path = predict_path(pos, Vec3::ZERO, thrust_accel, DEFAULT_PITCHOVER, 0.05, 6000, 20);
        assert!(path.len() > 5, "should sample a path");
        let last = *path.last().unwrap();
        let climb = (last - PLANET_CENTER).length() - (pos - PLANET_CENTER).length();
        assert!(climb > 1000.0, "path should climb well clear of the pad, got {climb:.0} m");
    }
}
