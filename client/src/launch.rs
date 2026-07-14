//! Rocket launch sequence — the client half (UI + thrust), single-player *and*
//! multiplayer.
//!
//! When the character is touching its room's **main assembly** (the largest group of
//! parts jointed together — the thrust arrow / COM-orb set), a "Launch" button appears at
//! the top-centre. Pressing it starts a `3 → 2 → 1 → Blastoff!` countdown; at blastoff
//! every joint pinning the assembly to the ground is cut and the assembly's rockets fire
//! with balanced, anti-spin thrust (see [`bad_spaceship_shared::launch`]). The COM orb and
//! combined thrust arrow hide once the launch is armed (see [`launch_armed`]).
//!
//! **Two modes, one feel:**
//! - *Single-player* is client-authoritative: this file owns the countdown, cuts the
//!   ground joints, and applies thrust to the local sim.
//! - *Multiplayer* is server-authoritative: the button sends a [`RequestLaunch`], the
//!   server runs the countdown + cuts ground joints, and replicates the state on the
//!   room's orb ([`NetLaunch`]). The countdown banner is drawn from that replicated
//!   state, and the same balanced thrust is applied here to the **predicted** rockets so
//!   the liftoff is smooth rather than rollback-jittered (the server applies the identical
//!   force, so prediction converges).

use avian3d::prelude::{
    AngularVelocity, Collider, ComputedMass, Forces, Gravity, LinearVelocity, Position, Rotation,
    SphericalJoint, WriteRigidBodyForces,
};
use bad_spaceship_shared::guidance::{program_guidance, Guidance, PitchProgram};
use bad_spaceship_shared::launch::{
    assembly_burn, burn_impulse, measure_assembly_spin, AssemblySpin, LAUNCH_COUNTDOWN_SECS,
};
use bad_spaceship_shared::map::apply_gravity_correction;
use bad_spaceship_shared::net::{
    ControlChannel, InLargestAssembly, NetLaunch, NetLockJoint, NetPart, NetPlayer, RequestLaunch,
    SetLocked,
};
use bad_spaceship_shared::part::{
    avatar_lock_contacts, capsule_bottom_center, cleanup_lock_joints, despawn_player_lock_welds,
    Gimbal, Holdable, LockJoint, RocketEngine, SuppressLocalParts, TargetPosition,
};
use bad_spaceship_shared::character::{apparent_up, drive_felt_up, FeltUp};
use bad_spaceship_shared::Character;
use bevy::prelude::*;
use bevy_egui::{
    egui::{self, Align2, Color32, Frame},
    EguiContexts,
};
use lightyear::prelude::{Connected, LocalId, MessageSender, Predicted};
use std::collections::HashSet;

use crate::net::ClientRoomFrame;
use crate::render_main_pass::flame_material::FlameThrottle;
use crate::render_secondary_pass::{assembly_members, main_assembly};
use crate::ui::EguiDrawSystems;

pub struct LaunchPlugin;

impl Plugin for LaunchPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LaunchLocal>()
            .init_resource::<LaunchCameraZoom>()
            .init_resource::<FuelUsed>()
            .init_resource::<ApparentUp>()
            .add_message::<SpSetLock>()
            .add_systems(Update, (tick_launch, ease_launch_zoom))
            // Single-player half of the Lock button: weld/unweld the local character
            // to the parts it touches, plus the shared dangling-weld sweep. Gated off
            // in multiplayer, where the lock welds are server-owned replicated
            // entities the client must never despawn locally
            // (`bind_replicated_lock_joints` rebuilds them as predicted physics
            // instead; the server registers the same sweep for its own welds).
            .add_systems(
                Update,
                (sp_apply_lock, cleanup_lock_joints)
                    .run_if(not(resource_exists::<SuppressLocalParts>)),
            )
            .add_systems(
                bevy_egui::EguiPrimaryContextPass,
                show_launch_ui.in_set(EguiDrawSystems),
            )
            // Planet gravity + thrust are continuous forces → apply once per physics tick
            // (an Update-rate force would make them frame-rate-dependent). One path per
            // mode: the single-player sim vs. the predicted multiplayer bodies. Gravity
            // precedes thrust so the two `Forces` writers on the rockets are ordered.
            .add_systems(
                FixedUpdate,
                (
                    // Zero every rocket's flame target first: only rockets the burn
                    // below actually fires this tick read back non-zero (a rocket
                    // that breaks off the assembly goes dark).
                    reset_flame_targets,
                    apply_sp_gravity.run_if(not(resource_exists::<SuppressLocalParts>)),
                    apply_mp_gravity.run_if(resource_exists::<SuppressLocalParts>),
                    apply_sp_thrust.run_if(not(resource_exists::<SuppressLocalParts>)),
                    apply_mp_thrust.run_if(resource_exists::<SuppressLocalParts>),
                    // Feed each rider's felt-up window (camera + movement basis) from
                    // the apparent-up the burn above just published.
                    sample_felt_up,
                    // Structural damping across welded pairs — drains contact/joint
                    // pump energy before it can run away (see `damp_weld_motion`).
                    // Single-player ONLY: on the predicted multiplayer twin the damper
                    // would smear half-applied rollback corrections across the cluster
                    // and feed a rollback storm; the server's authoritative damping
                    // protects multiplayer flights.
                    bad_spaceship_shared::part::damp_weld_motion
                        .run_if(not(resource_exists::<SuppressLocalParts>)),
                )
                    .chain(),
            );
    }
}

/// Seconds the "Blastoff!" banner lingers after the count reaches zero.
const BLASTOFF_BANNER_SECS: f32 = 1.5;

/// Single-player countdown/launch phase. In multiplayer the server owns this (replicated
/// via [`NetLaunch`]), so `sp` stays `Idle` there and is unused.
#[derive(Default, PartialEq, Clone, Copy)]
enum SpPhase {
    #[default]
    Idle,
    Countdown {
        remaining: f32,
    },
    Launched,
}

#[derive(Resource, Default)]
pub(crate) struct LaunchLocal {
    /// "Blastoff!" banner timer (both modes).
    banner: f32,
    /// Single-player countdown/launch phase.
    sp: SpPhase,
    /// Multiplayer: whether we've already fired the banner for the current launch (so the
    /// replicated `launched` edge triggers the banner exactly once).
    mp_banner_fired: bool,
}

impl LaunchLocal {
    /// Single-player: a launch is armed (counting down or lifted off) — the assembly
    /// visuals (COM orb + thrust arrow) hide once it is. Multiplayer launch state lives
    /// on the replicated [`NetLaunch`], not here, so [`launch_armed`] combines both.
    fn launching(&self) -> bool {
        self.sp != SpPhase::Idle
    }

    /// Single-player: whether blastoff has happened (lifted off, not merely counting
    /// down) — the trigger for the launch camera zoom-out and the planet's green ring.
    pub(crate) fn sp_launched(&self) -> bool {
        self.sp == SpPhase::Launched
    }
}

/// The launched assembly's **apparent up**: the direction a plumb line would hang for a
/// passenger aboard — `normalize(thrust_accel − gravity_at(true_com))`. `None` whenever
/// nothing is launched (the riders' target is then plain world-up).
///
/// Why this quantity and not the raw thrust/body axis: pure felt acceleration IS the
/// thrust axis, but following it blindly turned the camera fully sideways-then-inverted
/// as the gravity turn arced toward (and slightly below) horizontal. A passenger near a
/// planet weighs the burn against the visible/gravitational down: subtracting gravity
/// bounds the tilt (≈35° at a horizontal burn with TWR ~1.7 — the planet stays below),
/// makes the pad/coast cases exactly radial-up (no slow roll with a drifting hull after
/// cutoff — free fall feels nothing), and asymptotes to the pure thrust axis as gravity
/// fades with distance, which is the true weightless-passenger feel. Written by the
/// thrust systems (which have the burn forces, masses, and true position), read by
/// [`sample_felt_up`].
#[derive(Resource, Default)]
struct ApparentUp(Option<Vec3>);

/// Cumulative launch fuel spent by the local player's assembly, as thrust **impulse**
/// (N·s = ∫ Σ|engine force| dt). Burning fuel doesn't reduce mass in this sim, so total
/// impulse — not a rocket-equation Δv — is the honest propellant cost, and it is exactly
/// the quantity the fuel-optimal autopilot minimizes. Shown on the flight HUD in kN·s.
/// The thrust systems accumulate it while the local assembly is under power and zero it
/// while idle, so each launch starts from zero. In multiplayer this counts the *predicted*
/// burn, so an occasional rollback replay can nudge it a hair above the server's exact
/// figure — fine for a glanceable readout; the flight recorder carries the authoritative
/// number for analysis.
#[derive(Resource, Default)]
pub struct FuelUsed(pub f32);

/// How far the camera zooms out once a launch lifts off — 2× the player's current
/// distance, eased in/out. Applied on top of the scroll-zoom distance by
/// `client::input::zoom_camera`.
pub(crate) const LAUNCH_CAMERA_ZOOM: f32 = 2.0;

/// Eased camera zoom-out factor driven by the room's launch state: `1.0` on the pad,
/// easing toward [`LAUNCH_CAMERA_ZOOM`] after blastoff (and back on reset). Read by
/// `zoom_camera` so the liftoff pulls the view out to show the climbing rocket
/// without fighting the player's scroll zoom.
#[derive(Resource)]
pub(crate) struct LaunchCameraZoom(pub f32);

impl Default for LaunchCameraZoom {
    fn default() -> Self {
        Self(1.0)
    }
}

/// Ease [`LaunchCameraZoom`] toward its target each frame: out to [`LAUNCH_CAMERA_ZOOM`]
/// while the room is lifted off, back to `1.0` otherwise (single-player from
/// [`LaunchLocal`], multiplayer from the replicated [`NetLaunch`]). Frame-rate-
/// independent exponential approach, so the same feel at any refresh rate.
fn ease_launch_zoom(
    time: Res<Time>,
    mut zoom: ResMut<LaunchCameraZoom>,
    local: Res<LaunchLocal>,
    multiplayer: Option<Res<SuppressLocalParts>>,
    orb: Query<&NetLaunch>,
) {
    let launched = room_launched(&local, multiplayer.is_some(), &orb);
    let target = if launched { LAUNCH_CAMERA_ZOOM } else { 1.0 };
    // ~1.2 s to close most of the gap (rate 2.0), matching the liftoff's own pace.
    let alpha = 1.0 - (-2.0 * time.delta_secs()).exp();
    zoom.0 += (target - zoom.0) * alpha;
}

/// Whether the room has blasted off (lifted off, not merely counting down), across both
/// modes: single-player [`LaunchLocal::sp_launched`], multiplayer the replicated
/// [`NetLaunch::launched`]. Drives the launch camera zoom-out and the planet's green ring.
pub(crate) fn room_launched(
    local: &LaunchLocal,
    multiplayer: bool,
    orb: &Query<&NetLaunch>,
) -> bool {
    if multiplayer {
        orb.iter().next().is_some_and(|l| l.launched)
    } else {
        local.sp_launched()
    }
}

/// Whether the room's assembly is mid-launch (counting down or lifted off), across both
/// modes: single-player state on [`LaunchLocal`], multiplayer on the replicated
/// [`NetLaunch`]. The COM orb and combined thrust arrow hide once a launch is armed.
pub(crate) fn launch_armed(local: &LaunchLocal, net_launch: &Query<&NetLaunch>) -> bool {
    local.launching()
        || net_launch
            .iter()
            .next()
            .is_some_and(|l| l.launched || l.remaining > 0.0)
}

/// Advance the single-player countdown + the blastoff banner, and detect the multiplayer
/// blastoff edge. In single-player, the tick that crosses zero transitions to `Launched`
/// and cuts the local assembly's ground joints.
fn tick_launch(
    time: Res<Time>,
    mut local: ResMut<LaunchLocal>,
    mut commands: Commands,
    multiplayer: Option<Res<SuppressLocalParts>>,
    // Single-player ground-joint cut at blastoff. `Without<LockJoint>`: a player-lock
    // weld's avatar endpoint isn't `Holdable` either, so the cut would otherwise
    // sever a locked rider at the exact moment of blastoff.
    joints: Query<(Entity, &SphericalJoint), Without<LockJoint>>,
    holdables: Query<Entity, With<Holdable>>,
    // Multiplayer launch state (replicated on the room's orb).
    orb: Query<&NetLaunch>,
) {
    let dt = time.delta_secs();
    if local.banner > 0.0 {
        local.banner = (local.banner - dt).max(0.0);
    }

    if multiplayer.is_some() {
        // Fire the banner once on the replicated `launched` rising edge.
        let launched = orb.iter().next().is_some_and(|l| l.launched);
        if launched && !local.mp_banner_fired {
            local.banner = BLASTOFF_BANNER_SECS;
            local.mp_banner_fired = true;
        } else if !launched {
            local.mp_banner_fired = false;
        }
        return;
    }

    if let SpPhase::Countdown { remaining } = local.sp {
        let remaining = remaining - dt;
        if remaining <= 0.0 {
            local.sp = SpPhase::Launched;
            local.banner = BLASTOFF_BANNER_SECS;
            cut_ground_joints(&mut commands, &joints, &holdables);
        } else {
            local.sp = SpPhase::Countdown { remaining };
        }
    }
}

/// Despawn every joint with an endpoint that isn't a `Holdable` part — a joint pinning a
/// part to the ground (the only other jointable body is the static ground, which isn't
/// `Holdable`). Part-to-part joints stay intact so the assembly holds together as it lifts.
fn cut_ground_joints(
    commands: &mut Commands,
    joints: &Query<(Entity, &SphericalJoint), Without<LockJoint>>,
    holdables: &Query<Entity, With<Holdable>>,
) {
    let parts: HashSet<Entity> = holdables.iter().collect();
    for (entity, joint) in joints.iter() {
        if !parts.contains(&joint.body1) || !parts.contains(&joint.body2) {
            commands.entity(entity).despawn();
        }
    }
}

/// Planet gravity for the single-player sim: a per-tick radial correction on every
/// dynamic body (avatar + parts) so gravity points at the planet centre and weakens
/// with altitude. The correction rides on top of Avian's unchanged uniform `Gravity`
/// (~zero at the pad, so building is untouched — see [`gravity_at`](bad_spaceship_shared::map::gravity_at)). Single-player has
/// no floating-origin frame, so a body's local `Position` *is* its true world position.
fn apply_sp_gravity(
    gravity: Res<Gravity>,
    mut bodies: Query<(&Position, Forces), Or<(With<Holdable>, With<Character>)>>,
) {
    for (position, mut forces) in &mut bodies {
        apply_gravity_correction(&mut forces, position.0, gravity.0);
    }
}

/// Planet gravity for the predicted multiplayer bodies — the same radial correction as
/// [`apply_sp_gravity`], but true position folds in the room's floating-origin offset
/// ([`ClientRoomFrame`]) so `r` is the real distance from the centre while the co-moving
/// frame keeps the body near the local origin. Only `Predicted` bodies are locally
/// simulated (interpolated remotes are replication-driven), so only they get the force;
/// the server applies the identical field, so prediction converges.
fn apply_mp_gravity(
    gravity: Res<Gravity>,
    frame: Res<ClientRoomFrame>,
    mut bodies: Query<
        (&Position, Forces),
        (With<Predicted>, Or<(With<NetPart>, With<Character>)>),
    >,
) {
    let offset = frame.offset.as_vec3();
    for (position, mut forces) in &mut bodies {
        apply_gravity_correction(&mut forces, position.0 + offset, gravity.0);
    }
}

/// Apply balanced thrust to the single-player main assembly's rockets each physics tick.
#[allow(clippy::too_many_arguments)]
fn apply_sp_thrust(
    time: Res<Time>,
    // The launch autopilot's per-assembly PID integral state (see `assembly_burn`).
    mut integral: Local<Vec3>,
    // The fuel-optimal ascent plan (pitchover + the pitch program the autopilot flies),
    // optimized once at launch from this stack's thrust-to-weight, then frozen; cleared
    // when the launch ends so a rebuilt stack gets re-planned. (Single player owns its
    // own optimizer — there's no server.)
    mut sp_plan: Local<Option<PitchProgram>>,
    local: Res<LaunchLocal>,
    parts: Query<(Entity, &GlobalTransform, &ComputedMass), With<Holdable>>,
    joints: Query<&SphericalJoint>,
    // `Forces` takes `AngularVelocity` mutably inside (and writes each rocket's
    // `Gimbal` the geometry pass reads), so the spin/geometry reads and the force
    // write cannot coexist as sibling queries (B0001) — sequence them.
    mut set: ParamSet<(
        Query<(&LinearVelocity, &AngularVelocity)>,
        Query<(Entity, &GlobalTransform, &Gimbal), With<RocketEngine>>,
        Query<(Entity, Forces, &mut Gimbal, Option<&mut FlameThrottle>), With<RocketEngine>>,
    )>,
    gravity: Res<Gravity>,
    mut fuel: ResMut<FuelUsed>,
    mut apparent: ResMut<ApparentUp>,
    // The rider's mass, for the ascent plan: locked aboard at launch, so their weight
    // flies with the stack and belongs in the planned thrust-to-weight.
    rider: Query<&ComputedMass, With<Character>>,
) {
    if local.sp != SpPhase::Launched {
        fuel.0 = 0.0; // idle: keep the readout at zero until the next launch fires
        *sp_plan = None; // re-plan the ascent for the next launch
        apparent.0 = None;
        return;
    }
    let Some((members, _)) = main_assembly(&parts, &joints) else {
        apparent.0 = None;
        return;
    };
    // The assembly's COM + motion state, via the shared measurement (see
    // `measure_assembly_spin`) so the trim matches the server/MP paths exactly.
    let Some((com, spin)) = ({
        let velocities = set.p0();
        let samples = || {
            parts
                .iter()
                .filter(|(entity, ..)| members.contains(entity))
                .map(|(entity, transform, part_mass)| {
                    let (linear, angular) = velocities
                        .get(entity)
                        .map(|(l, a)| (l.0, a.0))
                        .unwrap_or_default();
                    (transform.translation(), linear, angular, part_mass.value())
                })
        };
        measure_assembly_spin(samples)
    }) else {
        return;
    };
    let geometry: Vec<(Entity, Vec3, Quat, bevy::math::Vec2)> = set
        .p1()
        .iter()
        .filter(|(entity, ..)| members.contains(entity))
        .map(|(entity, transform, gimbal)| {
            let (_, rotation, translation) = transform.to_scale_rotation_translation();
            (entity, translation, rotation, gimbal.0)
        })
        .collect();
    // Fuel-optimal ascent guidance: a per-assembly pitchover (optimized once at launch
    // from this stack's thrust-to-weight — heavy haulers lean, engine-dense stacks go
    // vertical), flown as a pitch program with an escape-energy throttle cutoff. No
    // floating origin in single player, so the true planet-frame state is just the local
    // COM + velocity.
    let program = sp_plan.get_or_insert_with(|| {
        let total_mass: f32 = parts
            .iter()
            .filter(|(entity, ..)| members.contains(entity))
            .map(|(_, _, m)| m.value())
            .sum::<f32>()
            + rider.iter().map(|m| m.value()).sum::<f32>();
        PitchProgram::plan(com, spin.linear_velocity, geometry.len(), gravity.0, total_mass, None)
    });
    let guidance = program_guidance(com, spin.linear_velocity, program);
    let net_force = apply_thrust(
        com,
        gravity.0,
        &geometry,
        &spin,
        time.delta_secs(),
        &mut integral,
        guidance,
        &mut fuel.0,
        &mut set.p2(),
    );
    // Apparent up for the riders (see `ApparentUp`); single-player true == local.
    let total_mass: f32 = parts
        .iter()
        .filter(|(entity, ..)| members.contains(entity))
        .map(|(_, _, m)| m.value())
        .sum::<f32>()
        + rider.iter().map(|m| m.value()).sum::<f32>();
    apparent.0 = (total_mass > 0.0).then(|| apparent_up(net_force, total_mass, com));
}

/// Apply balanced thrust to the multiplayer assembly's **predicted** rockets each physics
/// tick while the room is launched. Membership + pose come from the replicated
/// `InLargestAssembly` markers and the predicted Avian `Position`/`Rotation` (not
/// `GlobalTransform`, which `lightyear_avian` drives out of the fixed schedule).
fn apply_mp_thrust(
    time: Res<Time>,
    // The launch autopilot's per-assembly PID integral state (see `assembly_burn`).
    mut integral: Local<Vec3>,
    // The ascent plan mirror: the pitch program rebuilt from the server's replicated
    // pitchover angle (keyed by that angle so a re-plan on the server rebuilds here too).
    mut mp_plan: Local<Option<PitchProgram>>,
    orb: Query<&NetLaunch>,
    // `Forces` takes `AngularVelocity` mutably inside, so the member read and the
    // force write cannot coexist as sibling queries (B0001) — sequence them.
    mut set: ParamSet<(
        Query<
            (
                Entity,
                &Position,
                &Rotation,
                &LinearVelocity,
                &AngularVelocity,
                &ComputedMass,
                Option<&Gimbal>,
            ),
            (With<NetPart>, With<Predicted>, With<InLargestAssembly>),
        >,
        Query<
            (Entity, Forces, &mut Gimbal, Option<&mut FlameThrottle>),
            (With<RocketEngine>, With<Predicted>),
        >,
    )>,
    frame: Res<ClientRoomFrame>,
    gravity: Res<Gravity>,
    mut fuel: ResMut<FuelUsed>,
    mut apparent: ResMut<ApparentUp>,
    // Riders' masses for the ascent plan (all avatars in the room are locked aboard at
    // launch, and all are predicted): matches the server's rider-inclusive plan.
    riders: Query<&ComputedMass, (With<Character>, With<Predicted>)>,
) {
    let Some(launch) = orb.iter().next().filter(|l| l.launched) else {
        fuel.0 = 0.0; // idle: keep the readout at zero until the room launches
        *mp_plan = None; // re-plan on the next launch
        apparent.0 = None;
        return;
    };
    // The server's optimized ascent angle, replicated so the predicted turn matches
    // (`DEFAULT_PITCHOVER` = 0 for the tick or two before the first value arrives —
    // that window is inside the vertical kick phase, where 0 and the real angle agree).
    let pitchover = launch.pitchover;
    let need_plan = mp_plan.as_ref().map(|p| p.pitchover) != Some(pitchover);
    // The assembly's COM + motion state, via the shared measurement (see
    // `measure_assembly_spin`) so the trim matches the server exactly; collect
    // the member rockets' poses alongside (`Gimbal` marks the rockets — it rides
    // `insert_rocket_physics`). The mass sum feeds only a plan rebuild, so it's
    // gathered only on the (once-per-launch) tick that needs it.
    let (measured, geometry, part_mass_total) = {
        let members = set.p0();
        let samples = || {
            members
                .iter()
                .map(|(_, position, _, linear, angular, part_mass, _)| {
                    (position.0, linear.0, angular.0, part_mass.value())
                })
        };
        let geometry: Vec<(Entity, Vec3, Quat, bevy::math::Vec2)> = members
            .iter()
            .filter_map(|(entity, position, rotation, _, _, _, gimbal)| {
                gimbal.map(|g| (entity, position.0, rotation.0, g.0))
            })
            .collect();
        // Needed every tick now (apparent-up divides the net thrust by it), not just
        // on plan-rebuild ticks.
        let part_mass_total: f32 =
            members.iter().map(|(_, _, _, _, _, part_mass, _)| part_mass.value()).sum();
        (measure_assembly_spin(samples), geometry, part_mass_total)
    };
    let Some((com, spin)) = measured else {
        apparent.0 = None;
        return;
    };
    // True planet-frame state folds in the room's floating-origin frame (offset +
    // co-moving velocity), so the guidance sees real altitude/velocity under a rebase.
    let true_com = com + frame.offset.as_vec3();
    let true_vel = spin.linear_velocity + frame.velocity;
    // Rebuild the pitch program when the replicated angle (re)arrives, through the same
    // shared constructor the server plans with — locally measured masses are identical
    // (same parts, same densities), so the rebuilt program matches the one it flies.
    if need_plan {
        let total_mass = part_mass_total + riders.iter().map(|m| m.value()).sum::<f32>();
        *mp_plan = Some(PitchProgram::plan(
            true_com,
            true_vel,
            geometry.len(),
            gravity.0,
            total_mass,
            Some(pitchover),
        ));
    }
    let guidance = program_guidance(true_com, true_vel, mp_plan.as_ref().unwrap());
    let net_force = apply_thrust(
        com,
        gravity.0,
        &geometry,
        &spin,
        time.delta_secs(),
        &mut integral,
        guidance,
        &mut fuel.0,
        &mut set.p1(),
    );
    // Apparent up for the riders (see `ApparentUp`), from the true (frame-folded) state —
    // same formula as the server so the predicted movement basis matches.
    let total_mass = part_mass_total + riders.iter().map(|m| m.value()).sum::<f32>();
    apparent.0 = (total_mass > 0.0).then(|| apparent_up(net_force, total_mass, true_com));
}

/// Per-tick flame reset — see the registration comment and
/// [`FlameThrottle`](crate::render_main_pass::flame_material::FlameThrottle).
fn reset_flame_targets(mut throttles: Query<&mut FlameThrottle>) {
    for mut throttle in &mut throttles {
        throttle.target = 0.0;
    }
}

/// Feed every character's [`FeltUp`] window one sample of this tick's apparent-up
/// direction (see [`ApparentUp`]): the launched assembly's plumb-line direction while
/// launched, plain world-up otherwise. One system for both modes — the thrust systems
/// (whichever ran) already published the target; predicted and single-player avatars
/// alike carry `Character`.
fn sample_felt_up(
    mut commands: Commands,
    apparent: Res<ApparentUp>,
    mut characters: Query<
        (Entity, Option<&mut FeltUp>, &mut Rotation, &mut Position, &Collider),
        With<Character>,
    >,
) {
    let target = apparent.0.unwrap_or(Vec3::Y);
    for (entity, felt, mut rotation, mut position, collider) in &mut characters {
        let pivot = capsule_bottom_center(collider);
        drive_felt_up(&mut commands, entity, felt, &mut rotation, &mut position, pivot, target);
    }
}

/// Resolve the assembly's burn for this tick (shared `assembly_burn`) and write each
/// rocket's slewed gimbal + deflected flare-base force (plus its flame's throttle, for
/// the exhaust visual). Shared by the single-player and multiplayer thrust systems
/// (which differ only in how they gather membership + pose). Returns the net thrust
/// force this tick (for the riders' apparent-up).
#[allow(clippy::too_many_arguments)]
fn apply_thrust(
    com: Vec3,
    gravity: Vec3,
    geometry: &[(Entity, Vec3, Quat, bevy::math::Vec2)],
    spin: &AssemblySpin,
    dt: f32,
    integral: &mut Vec3,
    guidance: Guidance,
    fuel: &mut f32,
    rocket_forces: &mut Query<
        (Entity, Forces, &mut Gimbal, Option<&mut FlameThrottle>),
        impl bevy::ecs::query::QueryFilter,
    >,
) -> Vec3 {
    if geometry.is_empty() {
        return Vec3::ZERO;
    }
    let full = bad_spaceship_shared::launch::full_rocket_thrust(gravity);
    let burns = assembly_burn(com, gravity, dt, geometry, spin, integral, guidance);
    // Fuel spent this tick (see `FuelUsed` and the shared `burn_impulse` definition).
    *fuel += burn_impulse(&burns, dt);
    let net_force: Vec3 = burns.iter().map(|b| b.force).sum();
    for burn in burns {
        if let Ok((_, mut forces, mut gimbal, flame)) = rocket_forces.get_mut(burn.entity) {
            gimbal.0 = burn.gimbal;
            forces.apply_force_at_point(burn.force, burn.point);
            // `Option`: the flame rides the render visual, which may lag the
            // physics by a frame — thrust must not depend on it.
            if let Some(mut flame) = flame {
                flame.target = (burn.force.length() / full).clamp(0.0, 1.0);
            }
        }
    }
    net_force
}

/// The lock-state inputs of [`show_launch_ui`], bundled into one `SystemParam`
/// (the function was over Bevy's 16-parameter system limit): the replicated lock
/// welds + every player's id (multiplayer, where "everyone aboard" spans the room),
/// or the local lock welds (single-player), plus the two toggle sinks.
#[derive(bevy::ecs::system::SystemParam)]
struct LockUi<'w, 's> {
    mp_assembly_ids: Query<'w, 's, &'static NetPart, (With<InLargestAssembly>, With<Predicted>)>,
    mp_players: Query<'w, 's, &'static NetPlayer, With<Predicted>>,
    net_lock_welds: Query<'w, 's, &'static NetLockJoint>,
    sp_lock_joints: Query<'w, 's, &'static SphericalJoint, With<LockJoint>>,
    local_id: Query<'w, 's, &'static LocalId, With<Connected>>,
    lock_sender: Query<'w, 's, &'static mut MessageSender<SetLocked>, With<Connected>>,
    sp_lock_toggle: MessageWriter<'w, SpSetLock>,
}

/// Draw the launch button, the Lock/Unlock button just below its spot, and the
/// countdown / blastoff banner (top-centre). A launch press starts the launch
/// (single-player: locally; multiplayer: send a `RequestLaunch`); a lock press welds
/// the character to the parts it's touching (single-player: locally via
/// [`SpSetLock`]; multiplayer: send [`SetLocked`], the server welds and the welds
/// replicate back). The launch button only appears once **every player in the room
/// is locked to the assembly** (single-player: the one local player).
fn show_launch_ui(
    mut contexts: EguiContexts,
    mut local: ResMut<LaunchLocal>,
    multiplayer: Option<Res<SuppressLocalParts>>,
    // Membership sources: single-player assembly, or the replicated multiplayer markers.
    sp_parts: Query<(Entity, &GlobalTransform, &ComputedMass), With<Holdable>>,
    // Lock welds land in this query too — harmless: their avatar endpoint isn't an
    // indexed part, so they contribute no assembly edge (see `main_assembly`).
    sp_joints: Query<&SphericalJoint>,
    mp_members: Query<Entity, (With<InLargestAssembly>, With<Predicted>)>,
    orb: Query<&NetLaunch>,
    character: Query<Entity, With<Character>>,
    collisions: avian3d::prelude::Collisions,
    mut launch_sender: Query<&mut MessageSender<RequestLaunch>, With<Connected>>,
    mut lock_ui: LockUi,
) -> Result {
    // Current countdown/launched state and whether we can still start a launch.
    let (counting, launched) = if multiplayer.is_some() {
        match orb.iter().next() {
            Some(l) => (l.remaining, l.launched),
            None => (0.0, false),
        }
    } else {
        match local.sp {
            SpPhase::Countdown { remaining } => (remaining, false),
            SpPhase::Launched => (0.0, true),
            SpPhase::Idle => (0.0, false),
        }
    };
    let idle = counting <= 0.0 && !launched;

    let ctx = contexts.ctx_mut()?;

    // Big centred countdown word, or the lingering "Blastoff!" banner.
    let banner = if local.banner > 0.0 {
        Some("Blastoff!".to_owned())
    } else if counting > 0.0 {
        Some(countdown_word(counting))
    } else {
        None
    };
    if let Some(text) = banner {
        egui::Area::new(egui::Id::new("bs_launch_banner"))
            // Below the Lock button's slot (72), which stays visible mid-countdown.
            .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 132.0))
            .show(ctx, |ui| {
                // Let the big word size to its natural width instead of wrapping "Blastoff!"
                // onto several lines inside the anchored (width-less) area.
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.label(
                    egui::RichText::new(text)
                        .size(64.0)
                        .strong()
                        .color(Color32::from_rgb(255, 220, 80)),
                );
            });
    }

    // My lock state — needed every frame for the button label, and cheap (a linear
    // scan over the handful of welds). Multiplayer derives it from the replicated
    // `NetLockJoint`s (the server's welds are the truth — the button flips when the
    // weld actually exists); single-player from the local welds.
    let my_locked = if multiplayer.is_some() {
        crate::net::my_netcode_id(&lock_ui.local_id)
            .is_some_and(|id| lock_ui.net_lock_welds.iter().any(|weld| weld.player == id))
    } else {
        !lock_ui.sp_lock_joints.is_empty()
    };

    // Assembly membership + contact only *gate the buttons in*: the Lock button
    // while not yet locked, and the launch gate while idle. A locked rider
    // mid-countdown / mid-flight — the steady state of a ride — needs neither, so
    // skip the union-find and contact scan entirely then.
    let (touching, all_aboard) = if !my_locked || idle {
        let members = assembly_members(multiplayer.is_some(), &sp_parts, &sp_joints, &mp_members);
        let touching = character_touches_assembly(&character, &collisions, &members);
        // "Aboard" = welded into the *largest assembly* specifically — what the
        // launch gate counts, for EVERY player in the room. Only meaningful while
        // idle (it feeds nothing but the launch button's availability).
        let all_aboard = idle
            && if multiplayer.is_some() {
                let assembly_ids: HashSet<u64> =
                    lock_ui.mp_assembly_ids.iter().map(|part| part.id).collect();
                let aboard: HashSet<u64> = lock_ui
                    .net_lock_welds
                    .iter()
                    .filter(|weld| assembly_ids.contains(&weld.part))
                    .map(|weld| weld.player)
                    .collect();
                !lock_ui.mp_players.is_empty()
                    && lock_ui.mp_players.iter().all(|p| aboard.contains(&p.client_id))
            } else {
                lock_ui.sp_lock_joints.iter().any(|joint| members.contains(&joint.body2))
            };
        (touching, all_aboard)
    } else {
        (false, false)
    };

    // The Lock/Unlock button, just below the launch button's slot: shown while
    // standing on the assembly, and always while locked (so you can always unlock —
    // a rigid weld can disable the avatar↔deck *contact*, which would hide a
    // touch-gated button).
    if my_locked || touching {
        let mut toggle = false;
        egui::Area::new(egui::Id::new("bs_lock_button"))
            .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 72.0))
            .show(ctx, |ui| {
                // The anchored (width-less) area remembers its previous size, so the
                // label change (Lock ↔ Unlock) would wrap onto two lines without this.
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                Frame::default()
                    .fill(Color32::from_black_alpha(160))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        let label = if my_locked { "Unlock" } else { "Lock" };
                        let button = egui::Button::new(
                            egui::RichText::new(label)
                                .size(22.0)
                                .strong()
                                .color(Color32::from_rgb(160, 220, 255)),
                        );
                        if ui.add(button).clicked() {
                            toggle = true;
                        }
                    });
            });
        if toggle {
            if multiplayer.is_some() {
                if let Ok(mut sender) = lock_ui.lock_sender.single_mut() {
                    sender.send::<ControlChannel>(SetLocked(!my_locked));
                }
            } else {
                lock_ui.sp_lock_toggle.write(SpSetLock(!my_locked));
            }
        }
    }

    // The launch button only shows while idle, the character is on the assembly
    // (touching it, or locked to it — a rigid lock weld can disable the contact),
    // and EVERY player in the room is locked to the assembly.
    let available = idle && all_aboard && (touching || my_locked);
    if !available {
        return Ok(());
    }

    let mut arm = false;
    egui::Area::new(egui::Id::new("bs_launch_button"))
        .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 24.0))
        .show(ctx, |ui| {
            Frame::default()
                .fill(Color32::from_black_alpha(160))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    let button = egui::Button::new(
                        egui::RichText::new("Launch")
                            .size(22.0)
                            .strong()
                            .color(Color32::from_rgb(255, 220, 80)),
                    );
                    if ui.add(button).clicked() {
                        arm = true;
                    }
                });
        });

    if arm {
        if multiplayer.is_some() {
            if let Ok(mut sender) = launch_sender.single_mut() {
                sender.send::<ControlChannel>(RequestLaunch);
            }
        } else {
            local.sp = SpPhase::Countdown {
                remaining: LAUNCH_COUNTDOWN_SECS,
            };
        }
    }
    Ok(())
}

/// The single-player half of the Lock button: a buffered toggle written by
/// [`show_launch_ui`] (`true` = lock, `false` = unlock) and applied by
/// [`sp_apply_lock`] — a message so the egui pass stays out of the physics queries.
/// The multiplayer path sends [`SetLocked`] to the server instead.
#[derive(Message)]
struct SpSetLock(bool);

/// Apply the single-player Lock toggle: weld the character to every part currently
/// within the weld gap (the same freeze-in-place `part_gap_contacts` manifold the
/// server and the part-attach path weld with — one `SphericalJoint` + [`LockJoint`]
/// per contact, character = `body1`), or dissolve all of its welds. Never welds the
/// held part (`Without<TargetPosition>`) — locking is for what you stand on, not
/// what you carry. The ground is deliberately not a candidate either: an
/// avatar↔ground weld would pin the rider to the pad at blastoff.
fn sp_apply_lock(
    mut commands: Commands,
    mut toggles: MessageReader<SpSetLock>,
    characters: Query<(Entity, &Collider, &Position, &Rotation), With<Character>>,
    parts: Query<
        (Entity, &Collider, &Position, &Rotation),
        (With<Holdable>, Without<TargetPosition>, Without<Character>),
    >,
    lock_joints: Query<(Entity, &SphericalJoint), With<LockJoint>>,
) {
    let Some(&SpSetLock(want)) = toggles.read().last() else {
        return;
    };
    let Ok((character, collider, position, rotation)) = characters.single() else {
        return;
    };
    if !want {
        despawn_player_lock_welds(&mut commands, &lock_joints, character);
        return;
    }
    if lock_joints.iter().any(|(_, joint)| joint.body1 == character) {
        return; // Already locked.
    }
    avatar_lock_contacts(
        (collider, position.0, rotation.0),
        parts.iter().map(|(part, c, p, r)| (part, c, p.0, r.0)),
        |part, character_local, part_local| {
            commands.spawn((
                SphericalJoint::new(character, part)
                    .with_local_anchor1(character_local)
                    .with_local_anchor2(part_local),
                LockJoint,
            ));
        },
    );
}

/// Whether the character's body is in contact with any part of the assembly.
fn character_touches_assembly(
    character: &Query<Entity, With<Character>>,
    collisions: &avian3d::prelude::Collisions,
    members: &HashSet<Entity>,
) -> bool {
    let Ok(character) = character.single() else {
        return false;
    };
    if members.is_empty() {
        return false;
    }
    collisions
        .collisions_with(character)
        .filter(|pair| pair.is_touching())
        .any(|pair| {
            let other = if pair.collider1 == character {
                pair.collider2
            } else {
                pair.collider1
            };
            members.contains(&other)
        })
}

/// The countdown word for a given remaining time: `"3"` while `2 < t ≤ 3`, `"2"` while
/// `1 < t ≤ 2`, `"1"` while `0 < t ≤ 1`.
fn countdown_word(remaining: f32) -> String {
    (remaining.ceil().max(1.0) as i32).to_string()
}
