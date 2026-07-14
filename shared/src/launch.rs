//! Rocket-launch thrust math, shared by the thrust-vector visualisation, the
//! single-player launch, and the multiplayer server + predicted clients so every
//! side agrees on how much force each rocket makes and where.
//!
//! Pure functions — no ECS, no lightyear — over world-space rocket poses. The launch
//! *sequence* (slider, countdown, who-owns-what) lives in the client and server; this
//! is only the physics: one rocket's world thrust ([`rocket_world_thrust`]) and the
//! per-rocket throttles that let an assembly rise without spinning
//! ([`balanced_assembly_thrust`]).

use crate::guidance::Guidance;
use crate::part::{
    NOMINAL_PART_MASS, ROCKET_THRUST_DIR_LOCAL, ROCKET_THRUST_ORIGIN_LOCAL,
    ROCKET_THRUST_PART_WEIGHTS,
};
use bevy::math::{Mat3, Quat, Vec2, Vec3};
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

/// Thrust vectoring: how far a rocket's nozzle can deflect off the body axis
/// (±18°; was ±15°, +20% by feel).
pub const GIMBAL_MAX_RAD: f32 = 18.0 * std::f32::consts::PI / 180.0;
/// Thrust vectoring: how fast the nozzle can slew toward its commanded deflection.
/// Tested up the ladder (flight-recorder-verified at each step): 20°/s (real-TVC
/// plausible) puts the full ±15° traverse at 0.75 s — the nozzle always torques about
/// half a correction cycle late, and a rider's off-centre weight at blastoff flipped
/// the stack outright; 60°/s survives blastoff; 120°/s is what a rider-disturbed
/// 1–2-rocket stack needs to hold attitude through boarding shoves and weight
/// shifts, while still being a visibly rate-limited actuator.
pub const GIMBAL_RATE_RAD: f32 = 120.0 * std::f32::consts::PI / 180.0;

/// One rocket's balanced launch command: how hard to fire (`throttle` ∈ [0, 1] of
/// [`full_rocket_thrust`]) and where the autopilot wants the nozzle pointed
/// (`desired_gimbal`, a local-frame tilt vector — see [`Gimbal`](crate::part::Gimbal)).
/// The *applied* force comes from [`gimbaled_rocket_thrust`] after the caller slews the
/// rocket's actual gimbal toward the target with [`gimbal_step`] — the nozzle is a real
/// rate-limited actuator, not an instant one.
pub struct RocketThrust {
    pub entity: Entity,
    pub throttle: f32,
    pub desired_gimbal: Vec2,
}

/// Slew a nozzle's current deflection toward `desired` at the gimbal's rate limit
/// ([`GIMBAL_RATE_RAD`]), keeping it inside the ±[`GIMBAL_MAX_RAD`] cone. Pure and
/// shared so the server and the client's predicted twin integrate the same actuator.
pub fn gimbal_step(current: Vec2, desired: Vec2, dt: f32) -> Vec2 {
    let desired = desired.clamp_length_max(GIMBAL_MAX_RAD);
    let step = desired - current;
    let max_step = GIMBAL_RATE_RAD * dt;
    let next = if step.length() <= max_step {
        desired
    } else {
        current + step.normalize() * max_step
    };
    next.clamp_length_max(GIMBAL_MAX_RAD)
}

/// A rocket's world-space thrust with its nozzle deflected by `gimbal` (a local-frame
/// tilt vector: direction = which way the thrust tips off the body axis, length = the
/// tilt angle in radians) at `throttle` of full thrust. The application point is the
/// flare base, same as [`rocket_world_thrust`] — the nozzle pivots there.
pub fn gimbaled_rocket_thrust(
    translation: Vec3,
    rotation: Quat,
    full_thrust: f32,
    throttle: f32,
    gimbal: Vec2,
) -> (Vec3, Vec3) {
    let point = translation + rotation * ROCKET_THRUST_ORIGIN_LOCAL;
    let force =
        (rotation * gimbal_thrust_dir_local(gimbal)).normalize_or_zero() * full_thrust * throttle;
    (point, force)
}

/// The body-local thrust direction for a nozzle deflection: [`ROCKET_THRUST_DIR_LOCAL`]
/// tipped by `gimbal` (direction = tip direction, length = tilt angle in radians).
/// The single source of the tilt law — the exhaust-flame visual aims by it too, so
/// the drawn plume can never disagree with the applied force.
pub fn gimbal_thrust_dir_local(gimbal: Vec2) -> Vec3 {
    let angle = gimbal.length();
    if angle < 1e-6 {
        ROCKET_THRUST_DIR_LOCAL
    } else {
        // libm: identical across wasm/native (prediction determinism).
        ROCKET_THRUST_DIR_LOCAL * libm::cosf(angle)
            + Vec3::new(gimbal.x, 0.0, gimbal.y) / angle * libm::sinf(angle)
    }
}

/// One rocket's resolved burn for a physics tick: the nozzle deflection to store back
/// on its [`Gimbal`](crate::part::Gimbal) and the deflected force to apply at `point`.
pub struct RocketBurn {
    pub entity: Entity,
    pub gimbal: Vec2,
    pub point: Vec3,
    pub force: Vec3,
}

/// The fuel a tick's burn costs, as thrust impulse `Σ|force|·dt` (N·s). |force| =
/// full·throttle (the gimbal only rotates it), so this is exactly the propellant burned.
/// The single definition both the authoritative server tally (`RoomFuel`) and the
/// client's predicted HUD tally (`FuelUsed`) accumulate, so they can't disagree on what
/// "fuel" means.
pub fn burn_impulse(burns: &[RocketBurn], dt: f32) -> f32 {
    burns.iter().map(|b| b.force.length()).sum::<f32>() * dt
}

/// One physics tick of an assembly's launch burn, start to finish: balance the
/// throttles + gimbal commands ([`balanced_assembly_thrust`]), slew each nozzle toward
/// its command at the actuator's rate limit ([`gimbal_step`]), and resolve the deflected
/// world force ([`gimbaled_rocket_thrust`]). The single entry point all three thrust
/// sites (server rooms, single-player, predicted multiplayer) drive their `Forces` +
/// `Gimbal` writes from, so the sites stay thin and identical.
///
/// `geometry` is `(entity, world translation, world rotation, current gimbal)` per
/// rocket; `com`/`spin` as for [`balanced_assembly_thrust`].
pub fn assembly_burn(
    com: Vec3,
    gravity: Vec3,
    dt: f32,
    geometry: &[(Entity, Vec3, Quat, Vec2)],
    spin: &AssemblySpin,
    integral: &mut Vec3,
    guidance: Guidance,
) -> Vec<RocketBurn> {
    let full = full_rocket_thrust(gravity);
    balanced_assembly_thrust(com, gravity, dt, geometry, spin, integral, guidance.thrust_dir)
        .into_iter()
        .zip(geometry)
        .map(|(thrust, &(entity, translation, rotation, current))| {
            debug_assert_eq!(entity, thrust.entity);
            let gimbal = gimbal_step(current, thrust.desired_gimbal, dt);
            let (point, mut force) =
                gimbaled_rocket_thrust(translation, rotation, full, thrust.throttle, gimbal);
            // Guidance throttle is the *overall* burn level on top of the per-engine
            // balance: 1 during ascent, 0 once escape energy is reached (coast). Scaling
            // the resolved force keeps the per-engine attitude balance intact while cutting
            // total thrust — and drops the fuel tally + flame to zero on cutoff.
            force *= guidance.throttle;
            RocketBurn { entity, gimbal, point, force }
        })
        .collect()
}

/// The assembly's rotational state, for the launch stability assist: differential
/// throttle can only *react* to rotation it can measure. Both are computed over the
/// assembly's **members** (not just its rockets), by every thrust site the same way,
/// so the server and the client's predicted twin command identical trims.
pub struct AssemblySpin {
    /// Mass-weighted mean linear velocity of the members (m/s) — the velocity-hold
    /// term leans the attitude target against its horizontal part so the stack
    /// brakes lateral drift instead of carrying it forever.
    pub linear_velocity: Vec3,
    /// Mass-weighted mean angular velocity of the members (rad/s).
    pub angular_velocity: Vec3,
    /// Point-mass moment-of-inertia proxy about the COM: `Σ mᵢ·|rᵢ − com|²` (kg·m²).
    /// It ignores each part's own inertia, which only makes the assist slightly
    /// softer than critical on compact assemblies — safe in the stable direction.
    pub inertia: f32,
}

/// Measure an assembly's mass-weighted COM + motion state from `(position,
/// linear_velocity, angular_velocity, mass)` member samples — the **single**
/// implementation every thrust site (server rooms, client single-player, client
/// predicted multiplayer) feeds its ECS gathering into, so the trims they command can
/// never drift apart. `None` when the samples carry no mass.
///
/// Takes a re-callable sample source and iterates it twice (COM first, then the
/// inertia about it) instead of the one-pass parallel-axis form
/// `Σm·|p|² − M·|com|²` — at high altitude that difference is catastrophic f32
/// cancellation (|p| ~ 5e5 m while the true inertia arm is ~1 m), and rockets
/// live at high altitude.
pub fn measure_assembly_spin<I: Iterator<Item = (Vec3, Vec3, Vec3, f32)>>(
    samples: impl Fn() -> I,
) -> Option<(Vec3, AssemblySpin)> {
    let mut mass = 0.0;
    let mut weighted_pos = Vec3::ZERO;
    let mut weighted_lin = Vec3::ZERO;
    let mut weighted_ang = Vec3::ZERO;
    for (position, linear, angular, m) in samples() {
        mass += m;
        weighted_pos += position * m;
        weighted_lin += linear * m;
        weighted_ang += angular * m;
    }
    if mass <= 0.0 {
        return None;
    }
    let com = weighted_pos / mass;
    let inertia = samples().map(|(position, _, _, m)| m * position.distance_squared(com)).sum();
    Some((
        com,
        AssemblySpin {
            linear_velocity: weighted_lin / mass,
            angular_velocity: weighted_ang / mass,
            inertia,
        },
    ))
}

/// Attitude-hold stiffness: restoring angular acceleration (rad/s²) per radian of
/// net-thrust tilt away from the commanded direction. With [`STABILITY_KD`] = 2·√KP
/// the loop is critically damped — a disturbed assembly rights itself without
/// overshoot.
const STABILITY_KP: f32 = 4.0;
/// Rate damping: angular deceleration (rad/s²) per rad/s of assembly spin.
const STABILITY_KD: f32 = 4.0;
// (The old velocity-hold "drift brake" — STABILITY_KV / STABILITY_MAX_LEAN — that leaned
// the commanded up-direction against lateral velocity is gone: the guidance now commands
// a *prograde* direction on purpose (the gravity turn follows lateral velocity rather than
// braking it), so a drift brake would fight the intended flight path. Riders are locked to
// the assembly at launch, so the weight-shift steering it originally defended against can
// no longer happen mid-flight.)
/// Attitude integral (the I in PID): restoring angular acceleration (rad/s²) per
/// rad·s of accumulated attitude error. Without it the loop is P-only against
/// *external* torque, so holding a rider's off-centre weight required a standing
/// attitude error `e = τ/(KP·I)` — the body stalled ~7° short of the velocity-hold
/// lean while the rider-trim nozzle force pushed sideways, and the stack cruised
/// off laterally forever (recorder: nozzles +0.016 rad, body −0.015, velocity
/// pinned at 4.8 m/s — the forces exactly cancelled). The integral winds until the
/// external torque is held with zero standing error.
const STABILITY_KI: f32 = 0.5;
/// Anti-windup bound on the accumulated attitude error (rad·s). Holding an external
/// torque τ takes `∫e = τ/(KI·I)`, so SMALL-inertia assemblies need the most integral
/// headroom: a rider's standing trim on a one-rocket stack (I ≈ 3 kg·m²) needs
/// ~2.7 rad·s — a 2.0 clamp saturated exactly there and the un-held remainder drifted
/// the stack ~2 km sideways (level the whole way). But the clamp also bounds how much
/// integral energy can couple into a riderless single rocket's free roll (TVC can't
/// damp roll): at 8.0 the unmanned single wobbled (|ω| ~2, altitude oscillating)
/// where 2.0 flew clean. 4.0 = rider trim + 50% margin, roll coupling still bounded.
const ATTITUDE_INTEGRAL_MAX: f32 = 4.0;

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
/// `rockets` is `(entity, world translation, world rotation, current gimbal)` for the
/// assembly's rockets; `com` is the assembly's centre of mass (mass-weighted over all
/// its parts); `spin` is the assembly's measured motion state (see [`AssemblySpin`]);
/// `integral` is the assembly's accumulated attitude error (the PID's I state — one
/// `Vec3` the caller persists per assembly across ticks; see [`STABILITY_KI`]).
pub fn balanced_assembly_thrust(
    com: Vec3,
    gravity: Vec3,
    dt: f32,
    rockets: &[(Entity, Vec3, Quat, Vec2)],
    spin: &AssemblySpin,
    integral: &mut Vec3,
    up_command: Vec3,
) -> Vec<RocketThrust> {
    let full = full_rocket_thrust(gravity);
    let mut points = Vec::with_capacity(rockets.len());
    let mut forces = Vec::with_capacity(rockets.len());
    let mut torques = Vec::with_capacity(rockets.len());
    for &(_, translation, rotation, _) in rockets {
        let (point, force) = rocket_world_thrust(translation, rotation, full);
        torques.push((point - com).cross(force));
        points.push(point);
        forces.push(force);
    }
    // `up_command` is the direction the guidance wants the net thrust to point (the
    // fuel-optimal ascent law computes it — prograde gravity turn — in `assembly_burn`;
    // see `crate::guidance`). Attitude error: the axis (with sin-of-angle magnitude) that
    // rotates the net full-thrust direction onto that command. Zero when pointing along
    // it — the PD then only damps spin.
    let up_command = up_command.normalize_or(Vec3::Y);
    let thrust_dir = forces.iter().copied().sum::<Vec3>().normalize_or_zero();
    let error = thrust_dir.cross(up_command);
    *integral = (*integral + error * dt).clamp_length_max(ATTITUDE_INTEGRAL_MAX);
    let target = spin.inertia
        * (STABILITY_KP * error + STABILITY_KI * *integral
            - STABILITY_KD * spin.angular_velocity);
    let scales = balanced_thrust_scales_toward(&torques, target);
    // The nozzles cover whatever torque the throttles can't (they only reduce thrust,
    // the lift guard bounds how much, and 1–2 rockets barely span any torque at all).
    // The gimbal command is **incremental** — current deflection + the correction for
    // the torque still missing from the *actually applied* thrust (gimbals included) —
    // which makes the nozzle an integrator, exactly like real TVC: a steady external
    // torque (a rider standing off-centre) ends up absorbed by a *held* deflection
    // with zero steady-state tilt. The earlier positional form (deflection recomputed
    // from scratch against nominal thrust) needed a standing attitude ERROR to hold a
    // standing disturbance — e = τ/(KP·I), and on a one-rocket stack (I ≈ 2 kg·m²) a
    // rider 0.45 m off-centre made that ~30° of trim heel: the stack leaned until it
    // fell over (recorder-verified). The slight leak (×0.995) bleeds off deflection
    // the loop no longer asks for, so paired nozzles can't wander into an
    // equal-and-opposite null-space set point.
    let net: Vec3 = rockets
        .iter()
        .enumerate()
        .map(|(i, &(_, translation, rotation, gimbal))| {
            let (point, force) =
                gimbaled_rocket_thrust(translation, rotation, full, scales[i], gimbal);
            (point - com).cross(force)
        })
        .sum();
    let share = (target - net) / rockets.len() as f32;
    rockets
        .iter()
        .enumerate()
        .map(|(i, &(entity, _, rotation, gimbal))| RocketThrust {
            entity,
            throttle: scales[i],
            // Leak: 0.05%/tick (~3%/s). The leak must be tiny — the integrator has to
            // refill it every tick, which costs a standing torque error ∝ leak × held
            // deflection. At 0.5%/tick that droop capped the body at ~2° of a
            // commanded 8.5° velocity-hold lean while the rider-trim deflection kept
            // pushing sideways: recorder showed a ridden stack accelerating to
            // −41 m/s lateral (kilometres of drift) *against* its own lean.
            desired_gimbal: gimbal * 0.9995
                + gimbal_correction(points[i] - com, rotation, full * scales[i], share),
        })
        .collect()
}

/// The nozzle-deflection *increment* (local-frame tilt vector, ≤ [`GIMBAL_MAX_RAD`])
/// that makes a rocket's thrust produce `torque_share` more torque about the assembly
/// COM. `arm` is flare base − COM, `axial_thrust` the rocket's throttled thrust
/// magnitude.
///
/// The minimum lateral force with `arm × l = torque_share` is `l = (share × arm)/|arm|²`
/// (the arm-parallel component of the share — roll about the lever — is unreachable by a
/// force at its tip and drops out). Only force ⊥ the nozzle axis is reachable by tilting
/// the nozzle, so `l` is projected off the axis; the tilt angle then satisfies
/// `sin θ = |l| / axial_thrust`, clamped to the gimbal cone.
fn gimbal_correction(arm: Vec3, rotation: Quat, axial_thrust: f32, torque_share: Vec3) -> Vec2 {
    if axial_thrust < 1e-6 || arm.length_squared() < 1e-6 {
        return Vec2::ZERO;
    }
    let mut lateral = torque_share.cross(arm) / arm.length_squared();
    let axis = (rotation * ROCKET_THRUST_DIR_LOCAL).normalize_or_zero();
    lateral -= axis * axis.dot(lateral);
    let magnitude = lateral.length();
    if magnitude < 1e-6 {
        return Vec2::ZERO;
    }
    // libm: identical across wasm/native (prediction determinism).
    let angle = libm::asinf((magnitude / axial_thrust).min(1.0)).min(GIMBAL_MAX_RAD);
    let local = rotation.inverse() * (lateral / magnitude);
    Vec2::new(local.x, local.z).normalize_or_zero() * angle
}

/// The launch will not trade away more than `1 - LIFT_FLOOR` of its average thrust for
/// spin balance. Throttling rockets can only ever *reduce* total thrust, so a stack that
/// can't be balanced (e.g. a lone off-centre rocket, whose torque nothing opposes) would
/// otherwise throttle to nothing and never leave the ground. Lift wins over spin: below
/// this average throttle we boost the rockets back toward full. `0.85` = at most a 15%
/// average-thrust sacrifice for balance before lift takes priority.
pub const LIFT_FLOOR: f32 = 0.85;

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

    /// Steady-state world (point, force) per command — the gimbal assumed to have
    /// reached its target — for checking the torque the autopilot converges on.
    fn world_thrusts(
        rockets: &[(Entity, Vec3, Quat, Vec2)],
        gravity: Vec3,
        thrusts: &[RocketThrust],
    ) -> Vec<(Vec3, Vec3)> {
        let full = full_rocket_thrust(gravity);
        thrusts
            .iter()
            .map(|t| {
                let &(_, pos, rot, _) =
                    rockets.iter().find(|(entity, ..)| *entity == t.entity).unwrap();
                gimbaled_rocket_thrust(pos, rot, full, t.throttle, t.desired_gimbal)
            })
            .collect()
    }

    /// A symmetric pair of upright rockets straddling the COM needs no throttling —
    /// their torques cancel, so both fire at full.
    #[test]
    fn symmetric_pair_fires_full() {
        let scales = balanced_thrust_scales_toward(&[Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, 0.0, -1.0)], Vec3::ZERO);
        assert!(scales.iter().all(|&a| a > 0.99), "scales = {scales:?}");
    }

    /// A lone off-centre rocket spins the assembly and can't be balanced (nothing opposes
    /// it) — but lift must win over spin, so the lift guard keeps it firing at least
    /// `LIFT_FLOOR` rather than throttling it dead. (It will tumble; that's unavoidable
    /// with one off-centre rocket.)
    #[test]
    fn lone_offset_rocket_still_lifts() {
        let scales = balanced_thrust_scales_toward(&[Vec3::new(0.0, 0.0, 1.0)], Vec3::ZERO);
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
            let scales = balanced_thrust_scales_toward(&torques, Vec3::ZERO);
            let mean = scales.iter().sum::<f32>() / scales.len() as f32;
            assert!(mean >= LIFT_FLOOR - 1e-6, "mean {mean} for {torques:?} -> {scales:?}");
        }
    }

    /// Rockets whose thrust passes through the COM make no torque, so full throttle.
    #[test]
    fn zero_torque_fires_full() {
        let scales = balanced_thrust_scales_toward(&[Vec3::ZERO, Vec3::ZERO, Vec3::ZERO], Vec3::ZERO);
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
            (Entity::from_raw_u32(1).unwrap(), Vec3::new(-1.0, 0.0, 0.0), Quat::IDENTITY, Vec2::ZERO),
            (Entity::from_raw_u32(2).unwrap(), Vec3::new(1.0, 0.0, 0.0), Quat::IDENTITY, Vec2::ZERO),
        ];
        let gravity = Vec3::new(0.0, -9.81, 0.0);
        let com = Vec3::new(0.0, -(crate::part::ROCKET_BODY_HEIGHT / 2.0), 0.0);
        // Spinning about +Z: the assist must torque about -Z, i.e. throttle the
        // rocket whose full-thrust torque is +Z (the -x one) relative to the other.
        let spin = AssemblySpin { linear_velocity: Vec3::ZERO, angular_velocity: Vec3::new(0.0, 0.0, 1.0), inertia: 10.0 };
        let thrusts = balanced_assembly_thrust(com, gravity, 1.0 / 60.0, &rockets, &spin, &mut Vec3::ZERO, Vec3::Y);
        let net_torque: Vec3 = world_thrusts(&rockets, gravity, &thrusts)
            .iter()
            .map(|(point, force)| (*point - com).cross(*force))
            .sum();
        assert!(net_torque.z < -1.0, "expected counter-spin torque, got {net_torque:?}");
        // And a still, upright assembly keeps the symmetric full-throttle solution.
        let still = AssemblySpin { linear_velocity: Vec3::ZERO, angular_velocity: Vec3::ZERO, inertia: 10.0 };
        let thrusts = balanced_assembly_thrust(com, gravity, 1.0 / 60.0, &rockets, &still, &mut Vec3::ZERO, Vec3::Y);
        assert!(
            thrusts.iter().all(|t| t.throttle > 0.99),
            "still assembly should fire (near) full"
        );
    }

    /// The nozzle actuator honors both its limits: it never moves faster than
    /// `GIMBAL_RATE_RAD` per second and never leaves the `GIMBAL_MAX_RAD` cone.
    #[test]
    fn gimbal_step_respects_rate_and_range() {
        let dt = 1.0 / 60.0;
        // A big commanded deflection: one step moves exactly the rate limit toward it.
        let step = gimbal_step(Vec2::ZERO, Vec2::new(1.0, 0.0), dt);
        assert!((step.length() - GIMBAL_RATE_RAD * dt).abs() < 1e-6, "step = {step:?}");
        // Slewing forever saturates at the cone edge, not the (over-range) command.
        let mut current = Vec2::ZERO;
        for _ in 0..600 {
            current = gimbal_step(current, Vec2::new(1.0, 0.0), dt);
        }
        assert!((current.length() - GIMBAL_MAX_RAD).abs() < 1e-5, "saturated = {current:?}");
        // A reachable command is hit exactly (no overshoot, no dithering).
        let near = Vec2::new(0.001, 0.0);
        assert_eq!(gimbal_step(Vec2::ZERO, near, dt), near);
    }

    /// A *single* rocket has no throttle authority at all (its thrust passes through
    /// the COM), so the whole PD correction must come out of the nozzle: a spinning
    /// lone rocket gets a non-zero gimbal whose steady-state torque counters the spin.
    #[test]
    fn single_rocket_gimbal_counters_spin() {
        let rockets = [(Entity::from_raw_u32(1).unwrap(), Vec3::ZERO, Quat::IDENTITY, Vec2::ZERO)];
        let gravity = Vec3::new(0.0, -9.81, 0.0);
        let com = Vec3::new(0.0, 0.3, 0.0); // payload above: COM sits above the nozzle
        let spin = AssemblySpin { linear_velocity: Vec3::ZERO, angular_velocity: Vec3::new(0.0, 0.0, 1.0), inertia: 2.0 };
        let thrusts = balanced_assembly_thrust(com, gravity, 1.0 / 60.0, &rockets, &spin, &mut Vec3::ZERO, Vec3::Y);
        let desired = thrusts[0].desired_gimbal;
        assert!(desired.length() > 1e-4, "expected a gimbal command, got {desired:?}");
        assert!(desired.length() <= GIMBAL_MAX_RAD + 1e-6, "over-range: {desired:?}");
        let net_torque: Vec3 = world_thrusts(&rockets, gravity, &thrusts)
            .iter()
            .map(|(point, force)| (*point - com).cross(*force))
            .sum();
        assert!(net_torque.z < 0.0, "expected counter-spin torque, got {net_torque:?}");
    }

    /// A rocket *pair* spans only one torque axis with throttle (perpendicular to the
    /// line between them); spin about the pair line itself is throttle-invisible and
    /// must be countered by the gimbals.
    #[test]
    fn pair_roll_axis_handled_by_gimbal() {
        let rockets = [
            (Entity::from_raw_u32(1).unwrap(), Vec3::new(-1.0, 0.0, 0.0), Quat::IDENTITY, Vec2::ZERO),
            (Entity::from_raw_u32(2).unwrap(), Vec3::new(1.0, 0.0, 0.0), Quat::IDENTITY, Vec2::ZERO),
        ];
        let gravity = Vec3::new(0.0, -9.81, 0.0);
        let com = Vec3::new(0.0, 0.0, 0.0);
        let spin = AssemblySpin { linear_velocity: Vec3::ZERO, angular_velocity: Vec3::new(1.0, 0.0, 0.0), inertia: 4.0 };
        let thrusts = balanced_assembly_thrust(com, gravity, 1.0 / 60.0, &rockets, &spin, &mut Vec3::ZERO, Vec3::Y);
        let net_torque: Vec3 = world_thrusts(&rockets, gravity, &thrusts)
            .iter()
            .map(|(point, force)| (*point - com).cross(*force))
            .sum();
        assert!(net_torque.x < -0.5, "expected counter-roll torque, got {net_torque:?}");
    }
}
