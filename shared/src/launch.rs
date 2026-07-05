//! Rocket-launch thrust math, shared by the thrust-vector visualisation, the
//! single-player launch, and the multiplayer server + predicted clients so every
//! side agrees on how much force each rocket makes and where.
//!
//! Pure functions — no ECS, no lightyear — over world-space rocket poses. The launch
//! *sequence* (slider, countdown, who-owns-what) lives in the client and server; this
//! is only the physics: one rocket's world thrust ([`rocket_world_thrust`]) and the
//! per-rocket throttles that let an assembly rise without spinning
//! ([`balanced_assembly_thrust`]).

use crate::part::{
    NOMINAL_PART_MASS, ROCKET_THRUST_DIR_LOCAL, ROCKET_THRUST_ORIGIN_LOCAL,
    ROCKET_THRUST_PART_WEIGHTS,
};
use bevy::math::{Mat3, Quat, Vec3};
use bevy::prelude::Entity;

/// Launch countdown duration in seconds — long enough to show `3 → 2 → 1`. Shared so
/// the single-player client and the multiplayer server run the same count.
pub const LAUNCH_COUNTDOWN_SECS: f32 = 3.0;

/// The full thrust of one rocket engine in newtons: enough to lift
/// `ROCKET_THRUST_PART_WEIGHTS` average parts against gravity. The single source of this
/// magnitude for both the thrust-arrow length and the launch force.
pub fn full_rocket_thrust(gravity: Vec3) -> f32 {
    ROCKET_THRUST_PART_WEIGHTS * NOMINAL_PART_MASS * gravity.length()
}

/// A rocket's world-space thrust application point (the flare base, inside the rocket
/// where the flare begins) and its full-thrust force vector (up the cylinder axis, away
/// from the flare), for a rocket at world pose `(translation, rotation)`. Shared by the
/// thrust-arrow viz and the launch force so the two can never disagree on thrust
/// direction, point, or magnitude.
pub fn rocket_world_thrust(translation: Vec3, rotation: Quat, full_thrust: f32) -> (Vec3, Vec3) {
    let point = translation + rotation * ROCKET_THRUST_ORIGIN_LOCAL;
    let force = (rotation * ROCKET_THRUST_DIR_LOCAL).normalize_or_zero() * full_thrust;
    (point, force)
}

/// One rocket's balanced launch thrust: the (throttled) force to apply and the world
/// point to apply it at.
pub struct RocketThrust {
    pub entity: Entity,
    pub force: Vec3,
    pub point: Vec3,
}

/// Balanced launch thrust for the rockets of one assembly. Each rocket at full throttle
/// would exert a torque `τᵢ = (application point − COM) × Fᵢ` about the assembly's centre
/// of mass; left unbalanced the stack tumbles. We scale each rocket by `aᵢ ∈ [0, 1]`
/// (see [`balanced_thrust_scales`]) to cancel the *net* torque with the least loss of
/// thrust, and return each rocket's scaled force + application point.
///
/// `rockets` is `(entity, world translation, world rotation)` for the assembly's rockets;
/// `com` is the assembly's centre of mass (mass-weighted over all its parts).
pub fn balanced_assembly_thrust(
    com: Vec3,
    gravity: Vec3,
    rockets: &[(Entity, Vec3, Quat)],
) -> Vec<RocketThrust> {
    let full = full_rocket_thrust(gravity);
    let mut points = Vec::with_capacity(rockets.len());
    let mut forces = Vec::with_capacity(rockets.len());
    let mut torques = Vec::with_capacity(rockets.len());
    for &(_, translation, rotation) in rockets {
        let (point, force) = rocket_world_thrust(translation, rotation, full);
        torques.push((point - com).cross(force));
        points.push(point);
        forces.push(force);
    }
    let scales = balanced_thrust_scales(&torques);
    rockets
        .iter()
        .enumerate()
        .map(|(i, &(entity, _, _))| RocketThrust {
            entity,
            force: forces[i] * scales[i],
            point: points[i],
        })
        .collect()
}

/// Per-rocket throttle factors `aᵢ ∈ [0, 1]` that cancel an assembly's net thrust torque
/// with the minimum departure from full throttle.
///
/// We want `Σ aᵢ τᵢ = 0`. Writing `aᵢ = 1 + cᵢ`, that is `Σ cᵢ τᵢ = −Στᵢ`; the
/// minimum-norm `c` solving it is `cᵢ = −τᵢ · x` with `x = (Σ τᵢτᵢᵀ + λI)⁻¹ Στᵢ` (a
/// single 3×3 solve — `Σ τᵢτᵢᵀ` is the `TTᵀ` normal matrix, `λI` regularises the
/// degenerate cases where the torques don't span 3-D, e.g. collinear rockets). Clamping
/// `1 + cᵢ` to `[0, 1]` keeps every throttle physical; where the clamp bites the torque
/// isn't perfectly cancelled — the "as good as possible" the balance can do.
pub fn balanced_thrust_scales(torques: &[Vec3]) -> Vec<f32> {
    let torque_sum: Vec3 = torques.iter().copied().sum();
    // Normal matrix Σ τᵢτᵢᵀ (each term is the outer product, whose columns are τ·τ.{x,y,z}).
    let mut normal = Mat3::ZERO;
    for &t in torques {
        normal += Mat3::from_cols(t * t.x, t * t.y, t * t.z);
    }
    // Tikhonov regularisation, scaled to the matrix so it's meaningful at any torque
    // magnitude while staying small enough not to blunt the cancellation.
    let trace = normal.x_axis.x + normal.y_axis.y + normal.z_axis.z;
    let lambda = (trace * 1e-4).max(1e-9);
    normal += Mat3::from_diagonal(Vec3::splat(lambda));
    let x = normal.inverse() * torque_sum;
    torques.iter().map(|t| (1.0 - t.dot(x)).clamp(0.0, 1.0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A symmetric pair of upright rockets straddling the COM needs no throttling —
    /// their torques cancel, so both fire at full.
    #[test]
    fn symmetric_pair_fires_full() {
        let scales = balanced_thrust_scales(&[Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, -1.0)]);
        assert!(scales.iter().all(|&a| a > 0.99), "scales = {scales:?}");
    }

    /// A lone off-centre rocket spins the assembly; it can't be balanced (nothing to
    /// oppose it), so it throttles down toward zero.
    #[test]
    fn lone_offset_rocket_throttles_down() {
        let scales = balanced_thrust_scales(&[Vec3::new(0.0, 0.0, 1.0)]);
        assert!(scales[0] < 0.5, "scales = {scales:?}");
    }

    /// Rockets whose thrust passes through the COM make no torque, so full throttle.
    #[test]
    fn zero_torque_fires_full() {
        let scales = balanced_thrust_scales(&[Vec3::ZERO, Vec3::ZERO, Vec3::ZERO]);
        assert!(scales.iter().all(|&a| (a - 1.0).abs() < 1e-6), "scales = {scales:?}");
    }
}
