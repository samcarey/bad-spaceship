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

/// The assembly's rotational state, for the launch stability assist: differential
/// throttle can only *react* to rotation it can measure. Both are computed over the
/// assembly's **members** (not just its rockets), by every thrust site the same way,
/// so the server and the client's predicted twin command identical trims.
pub struct AssemblySpin {
    /// Mass-weighted mean angular velocity of the members (rad/s).
    pub angular_velocity: Vec3,
    /// Point-mass moment-of-inertia proxy about the COM: `Σ mᵢ·|rᵢ − com|²` (kg·m²).
    /// It ignores each part's own inertia, which only makes the assist slightly
    /// softer than critical on compact assemblies — safe in the stable direction.
    pub inertia: f32,
}

/// Attitude-hold stiffness: restoring angular acceleration (rad/s²) per radian of
/// net-thrust tilt away from world-up. With [`STABILITY_KD`] = 2·√KP the loop is
/// critically damped — a disturbed assembly rights itself without overshoot.
const STABILITY_KP: f32 = 4.0;
/// Rate damping: angular deceleration (rad/s²) per rad/s of assembly spin.
const STABILITY_KD: f32 = 4.0;

/// Balanced launch thrust for the rockets of one assembly. Each rocket at full throttle
/// would exert a torque `τᵢ = (application point − COM) × Fᵢ` about the assembly's centre
/// of mass; left unbalanced the stack tumbles. We scale each rocket by `aᵢ ∈ [0, 1]`
/// (see [`balanced_thrust_scales_toward`]) so the net thrust torque hits a **stability
/// target** with the least loss of thrust, and return each rocket's scaled force +
/// application point.
///
/// The target is a critically-damped PD attitude hold — `I·(KP·(t̂ × ŷ) − KD·ω)`, where
/// `t̂` is the net full-thrust direction — rather than plain zero. Zeroing thrust torque
/// alone cannot see *external* torques (a player standing off-centre on the platform, a
/// nudge from another body, constraint-solve noise at altitude): any of those slowly
/// spun the stack until it flipped and flew into the ground (verified frame-by-frame
/// with the flight recorder). The PD trim leans the thrust against measured rotation,
/// so a rideable stack stays upright — within throttle authority: scales stay clamped
/// to `[0, 1]` and the lift guard still keeps the average at [`LIFT_FLOOR`].
///
/// `rockets` is `(entity, world translation, world rotation)` for the assembly's rockets;
/// `com` is the assembly's centre of mass (mass-weighted over all its parts); `spin` is
/// the assembly's measured rotational state (see [`AssemblySpin`]).
pub fn balanced_assembly_thrust(
    com: Vec3,
    gravity: Vec3,
    rockets: &[(Entity, Vec3, Quat)],
    spin: &AssemblySpin,
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
    // Attitude error: the axis (with sin-of-angle magnitude) that rotates the net
    // full-thrust direction onto world-up. Zero when pointing straight up — the PD
    // then only damps spin.
    let thrust_dir = forces.iter().copied().sum::<Vec3>().normalize_or_zero();
    let target = spin.inertia
        * (STABILITY_KP * thrust_dir.cross(Vec3::Y) - STABILITY_KD * spin.angular_velocity);
    let scales = balanced_thrust_scales_toward(&torques, target);
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

/// The launch will not trade away more than `1 - LIFT_FLOOR` of its average thrust for
/// spin balance. Throttling rockets can only ever *reduce* total thrust, so a stack that
/// can't be balanced (e.g. a lone off-centre rocket, whose torque nothing opposes) would
/// otherwise throttle to nothing and never leave the ground. Lift wins over spin: below
/// this average throttle we boost the rockets back toward full. `0.85` = at most a 15%
/// average-thrust sacrifice for balance before lift takes priority.
const LIFT_FLOOR: f32 = 0.85;

/// [`balanced_thrust_scales_toward`] a zero target: cancel the net thrust torque.
pub fn balanced_thrust_scales(torques: &[Vec3]) -> Vec<f32> {
    balanced_thrust_scales_toward(torques, Vec3::ZERO)
}

/// Per-rocket throttle factors `aᵢ ∈ [0, 1]` that steer an assembly's net thrust torque
/// toward `target` (zero = just stop it spinning) without sacrificing so much thrust
/// that it won't lift.
///
/// First, the torque-steering step: we want `Σ aᵢ τᵢ = target`. Writing `aᵢ = 1 + cᵢ`,
/// that is `Σ cᵢ τᵢ = target − Στᵢ`; the minimum-norm `c` solving it is `cᵢ = −τᵢ · x`
/// with `x = (Σ τᵢτᵢᵀ + λI)⁻¹ (Στᵢ − target)` (a single 3×3 solve — `Σ τᵢτᵢᵀ` is the
/// `TTᵀ` normal matrix, `λI` regularises the degenerate cases where the torques don't
/// span 3-D, e.g. collinear rockets). Clamping `1 + cᵢ` to `[0, 1]` keeps every throttle
/// physical.
///
/// That step minimises spin, but because throttling only ever *reduces* thrust it can
/// drive an unbalanceable stack's thrust to zero. So second, the lift guard
/// ([`LIFT_FLOOR`]): if the average throttle dropped below the floor, boost every rocket
/// back toward full — preserving their *relative* trim — until the average reaches the
/// floor. A balanceable stack rises without spinning; an unbalanceable one still rises
/// (and tumbles, unavoidably) instead of sitting dead on the pad.
pub fn balanced_thrust_scales_toward(torques: &[Vec3], target: Vec3) -> Vec<f32> {
    if torques.is_empty() {
        return Vec::new();
    }
    // Min-norm solve for Σ cᵢτᵢ = rhs over the rockets picked by `free`, via the
    // regularised normal matrix Σ τᵢτᵢᵀ (each term is the outer product, whose
    // columns are τ·τ.{x,y,z}). The Tikhonov λ is scaled to the matrix so it's
    // meaningful at any torque magnitude while staying small enough not to blunt
    // the cancellation.
    let solve = |rhs: Vec3, free: &dyn Fn(usize) -> bool| -> Vec3 {
        let mut normal = Mat3::ZERO;
        for (i, &t) in torques.iter().enumerate() {
            if free(i) {
                normal += Mat3::from_cols(t * t.x, t * t.y, t * t.z);
            }
        }
        let trace = normal.x_axis.x + normal.y_axis.y + normal.z_axis.z;
        let lambda = (trace * 1e-4).max(1e-9);
        normal += Mat3::from_diagonal(Vec3::splat(lambda));
        normal.inverse() * rhs
    };

    let x = solve(torques.iter().copied().sum::<Vec3>() - target, &|_| true);
    let mut scales: Vec<f32> =
        torques.iter().map(|t| (1.0 - t.dot(x)).clamp(0.0, 1.0)).collect();

    // The clamp discards whatever correction the saturated rockets couldn't take
    // (throttles only go *down* from full), leaving the net torque short of the
    // target. One refinement pass re-solves the residual over the unsaturated
    // rockets, recovering the reachable part of that lost authority.
    let net: Vec3 = torques.iter().zip(&scales).map(|(t, &a)| *t * a).sum();
    let residual = target - net;
    if residual.length_squared() > 1e-12 {
        let free: Vec<bool> = scales.iter().map(|&a| a > 1e-6 && a < 1.0 - 1e-6).collect();
        let y = solve(residual, &|i| free[i]);
        for (i, t) in torques.iter().enumerate() {
            if free[i] {
                scales[i] = (scales[i] + t.dot(y)).clamp(0.0, 1.0);
            }
        }
    }

    // Lift guard: boost back toward full if balancing sacrificed too much average thrust.
    let mean = scales.iter().sum::<f32>() / scales.len() as f32;
    if mean < LIFT_FLOOR {
        let headroom = 1.0 - mean; // = mean(1 − aᵢ)
        let t = if headroom > 1e-6 {
            (LIFT_FLOOR - mean) / headroom
        } else {
            0.0
        };
        for a in &mut scales {
            *a += (1.0 - *a) * t;
        }
    }
    scales
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

    /// A lone off-centre rocket spins the assembly and can't be balanced (nothing opposes
    /// it) — but lift must win over spin, so the lift guard keeps it firing at least
    /// `LIFT_FLOOR` rather than throttling it dead. (It will tumble; that's unavoidable
    /// with one off-centre rocket.)
    #[test]
    fn lone_offset_rocket_still_lifts() {
        let scales = balanced_thrust_scales(&[Vec3::new(0.0, 0.0, 1.0)]);
        assert!(scales[0] >= LIFT_FLOOR - 1e-6, "scales = {scales:?}");
    }

    /// The lift guard never drops the average throttle below `LIFT_FLOOR`, whatever the
    /// torques — an assembly always gets enough thrust to have a chance at lifting.
    #[test]
    fn average_thrust_never_below_floor() {
        for torques in [
            vec![Vec3::new(1.0, 0.0, 0.0)],
            vec![Vec3::new(2.0, 0.0, 0.0), Vec3::new(2.0, 1.0, 0.0)],
            vec![Vec3::new(0.0, 3.0, 0.0), Vec3::new(0.0, 3.0, 0.0), Vec3::new(0.0, -1.0, 0.0)],
        ] {
            let scales = balanced_thrust_scales(&torques);
            let mean = scales.iter().sum::<f32>() / scales.len() as f32;
            assert!(mean >= LIFT_FLOOR - 1e-6, "mean {mean} for {torques:?} -> {scales:?}");
        }
    }

    /// Rockets whose thrust passes through the COM make no torque, so full throttle.
    #[test]
    fn zero_torque_fires_full() {
        let scales = balanced_thrust_scales(&[Vec3::ZERO, Vec3::ZERO, Vec3::ZERO]);
        assert!(scales.iter().all(|&a| (a - 1.0).abs() < 1e-6), "scales = {scales:?}");
    }

    /// Steering toward a non-zero target trims the pair differentially: the net
    /// torque `Σ aᵢτᵢ` lands on the target (within the regularised solve's slack).
    #[test]
    fn target_torque_is_steered_to() {
        let torques = [Vec3::new(0.0, 0.0, 10.0), Vec3::new(0.0, 0.0, -10.0)];
        let target = Vec3::new(0.0, 0.0, 3.0);
        let scales = balanced_thrust_scales_toward(&torques, target);
        let net: Vec3 = torques.iter().zip(&scales).map(|(t, &a)| *t * a).sum();
        assert!((net - target).length() < 0.1, "net {net:?} scales {scales:?}");
    }

    /// The PD stability assist counters measured spin: a spinning-but-symmetric
    /// assembly gets a *differential* trim (the spin-side rocket throttles down),
    /// where the old zero-target trim left both at full and the spin uncorrected.
    #[test]
    fn stability_assist_counters_spin() {
        // Two upright rockets straddling the COM on the x axis.
        let rockets = [
            (Entity::from_raw_u32(1).unwrap(), Vec3::new(-1.0, 0.0, 0.0), Quat::IDENTITY),
            (Entity::from_raw_u32(2).unwrap(), Vec3::new(1.0, 0.0, 0.0), Quat::IDENTITY),
        ];
        let gravity = Vec3::new(0.0, -9.81, 0.0);
        let com = Vec3::new(0.0, -(crate::part::ROCKET_BODY_HEIGHT / 2.0), 0.0);
        // Spinning about +Z: the assist must torque about -Z, i.e. throttle the
        // rocket whose full-thrust torque is +Z (the -x one) relative to the other.
        let spin = AssemblySpin { angular_velocity: Vec3::new(0.0, 0.0, 1.0), inertia: 10.0 };
        let thrusts = balanced_assembly_thrust(com, gravity, &rockets, &spin);
        let net_torque: Vec3 = thrusts
            .iter()
            .map(|t| (t.point - com).cross(t.force))
            .sum();
        assert!(net_torque.z < -1.0, "expected counter-spin torque, got {net_torque:?}");
        // And a still, upright assembly keeps the symmetric full-throttle solution.
        let still = AssemblySpin { angular_velocity: Vec3::ZERO, inertia: 10.0 };
        let thrusts = balanced_assembly_thrust(com, gravity, &rockets, &still);
        assert!(
            thrusts.iter().all(|t| t.force.length() > 0.99 * full_rocket_thrust(gravity)),
            "still assembly should fire (near) full"
        );
    }
}
