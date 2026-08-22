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
use bad_spaceship_shared::guidance::{
    program_guidance, steer_guidance, Guidance, LaunchSeed, PitchProgram, Vehicle,
};
use bad_spaceship_shared::launch::{
    assembly_burn, burn_impulse, burn_trace, measure_assembly_spin, AssemblySpin,
    LAUNCH_COUNTDOWN_SECS,
};
use bad_spaceship_shared::map::apply_gravity_correction;
use bad_spaceship_shared::net::{
    part_volume, rebase_room_frame, weighted_anchor, ControlChannel, InLargestAssembly, NetJoint,
    NetInput, NetLaunch, NetLockJoint, NetPart, NetPlayer, NetRoomFrame, RequestLaunch,
    SetLocked, GROUND_JOINT_ID,
};
use bad_spaceship_shared::part::{
    avatar_lock_contacts, capsule_bottom_center, cleanup_lock_joints, despawn_player_lock_welds,
    AttitudeIntegral, EscapeCut, Gimbal, Holdable, LockJoint, RocketEngine, SuppressLocalParts,
    TargetPosition,
};
use bad_spaceship_shared::character::{apparent_up, drive_felt_up, FeltUp};
use bad_spaceship_shared::{Character, InputEvents, Modifying, UpdateJointsLabel};
use bevy::prelude::*;
use bevy_egui::{
    egui::{self, Align2, Color32, Frame},
    EguiContexts,
};
use bevy::math::DVec3;
use lightyear::prelude::input::native::{ActionState, InputMarker};
use lightyear::prelude::{Connected, LocalId, MessageSender, Predicted, Tick};
use std::collections::HashSet;

use crate::render_main_pass::flame_material::FlameThrottle;
use crate::render_secondary_pass::{assembly_members, main_assembly};
use crate::ui::EguiDrawSystems;

pub struct LaunchPlugin;

impl Plugin for LaunchPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LaunchLocal>()
            .init_resource::<PredictedRebase>()
            .init_resource::<LaunchCameraZoom>()
            .init_resource::<FuelUsed>()
            .init_resource::<ApparentUp>()
            .init_resource::<Autopilot>()
            .init_resource::<BuildingLockedOut>()
            .add_message::<SpSetLock>()
            .add_systems(Update, (tick_launch, ease_launch_zoom, autolock_rider, autolaunch))
            // Between the three `Modifying` writers (all in `InputEvents`) and every
            // reader of it — `update_predelete_joints` heads `UpdateJointsLabel`.
            .add_systems(
                Update,
                lock_out_building_in_flight
                    .after(InputEvents)
                    .before(UpdateJointsLabel),
            );
        app
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
                    // The floating-origin rebase leads the chain, exactly like the
                    // server's `rebase_room_frames`: everything below computes
                    // world-space force targets for this tick, and a target computed
                    // pre-shift but applied post-shift is a km-scale lever arm.
                    predict_room_rebase.run_if(resource_exists::<SuppressLocalParts>),
                    // Let the pad go on the scheduled tick, before this tick's thrust —
                    // the predicted twin of the server's `fire_scheduled_launches`.
                    release_predicted_ground_joints
                        .run_if(resource_exists::<SuppressLocalParts>),
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
                    // pump energy before it can run away (see `damp_weld_motion`). Runs in
                    // ALL worlds, in the same chain position the server uses (after the
                    // burn): the server damps its welded pairs every tick, so a predicted
                    // client that skips it is running a *different simulation* by
                    // construction — a systematic client/server asymmetry, and therefore a
                    // permanent divergence source, no matter how well every other term is
                    // matched. (An earlier trial gated it back off after an inconclusive
                    // rollback-rate A/B; that measurement was variance-dominated and the
                    // asymmetry it left behind is incompatible with zero divergence.)
                    bad_spaceship_shared::part::damp_weld_motion,
                )
                    .chain(),
            );
    }
}

/// Test hook: `BS_AUTOLOCK` (native) / `?autolock=1` (web — see
/// [`crate::net::autolock_requested`]) welds this client to the deck it's standing
/// on a few seconds after boot — turning a mid-flight-joining measurement client into a
/// *locked rider* (the user's actual crash scenario: the rider's avatar↔deck lock-weld
/// is the extra welded pair a spectator never has). Sends one `SetLocked(true)`, the
/// same message the Lock button sends. No effect unless the flag is set.
fn autolock_rider(
    time: Res<Time>,
    mut elapsed: Local<f32>,
    mut sent: Local<bool>,
    mut sender: Query<&mut MessageSender<SetLocked>, With<Connected>>,
) {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(crate::net::autolock_requested) || *sent {
        return;
    }
    *elapsed += time.delta_secs();
    if *elapsed < 6.0 {
        return; // let it join, respawn onto the deck, and settle first
    }
    if let Ok(mut s) = sender.single_mut() {
        s.send::<ControlChannel>(SetLocked(true));
        *sent = true;
        println!("[autolock] sent SetLocked(true)");
    }
}

/// Test hook: `BS_AUTOLAUNCH` (native) / `?autolaunch=<secs>` (web) sends one
/// `RequestLaunch` — the same message the swipe control sends — that many seconds after
/// boot, making this client the flight's **pilot**. Without it a screenless run can never
/// reach the chase camera or the steering stick, since both are gated on being the player
/// who pressed the button. No effect unless the flag is set.
fn autolaunch(
    time: Res<Time>,
    mut elapsed: Local<f32>,
    mut sent: Local<bool>,
    mut sender: Query<&mut MessageSender<RequestLaunch>, With<Connected>>,
) {
    use std::sync::OnceLock;
    static AFTER: OnceLock<Option<f32>> = OnceLock::new();
    let Some(after) = *AFTER.get_or_init(crate::net::autolaunch_after) else {
        return;
    };
    if *sent {
        return;
    }
    *elapsed += time.delta_secs();
    if *elapsed < after {
        return;
    }
    if let Ok(mut s) = sender.single_mut() {
        s.send::<ControlChannel>(RequestLaunch);
        *sent = true;
        println!("[autolaunch] sent RequestLaunch at t={:.1}s", *elapsed);
    }
}

/// Seconds the "Blastoff!" banner lingers after the count reaches zero.
const BLASTOFF_BANNER_SECS: f32 = 1.5;

/// Top-centre HUD stack layout (points below the top edge). The Launch button owns the
/// very top; the flight-telemetry HUD (drawn by `ui::show_flight_hud`, anchored at 44)
/// stays hidden on the pad but grows to ~5 lines (~150 pt tall) once flying. So while
/// idle the Lock button and countdown banner sit high, and once **launched** they drop
/// below the now-visible HUD instead of being buried under it. All in egui points, so the
/// spacing scales with the UI zoom the same way the panel's own text does (mobile-safe).
const LAUNCH_BUTTON_Y: f32 = 24.0;
const LOCK_BUTTON_Y_IDLE: f32 = 72.0;
const LOCK_BUTTON_Y_FLIGHT: f32 = 168.0;
const BANNER_Y_IDLE: f32 = 132.0;
const BANNER_Y_FLIGHT: f32 = 232.0;

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
    /// Multiplayer: where this room is in the banner's one-shot lifecycle, so the
    /// "Blastoff!" banner fires exactly once per launch and only on a transition we
    /// witnessed — never when joining or reconnecting into an already-launched room
    /// (which re-fired it every foreground).
    mp_banner: MpBanner,
}

/// Lifecycle of the multiplayer blastoff banner for the current room. The banner is a
/// launch *transition*, but the replicated `NetLaunch::launched` is a *level* whose
/// source can be absent (the world is torn down during a reconnect, or not yet
/// replicated on a fresh reload); this latch turns that level into a witnessed edge.
#[derive(Default, PartialEq)]
enum MpBanner {
    /// (Re)connected; haven't seen this room un-launched yet, so a launched reading is a
    /// join-into-flight, not a transition — stay quiet.
    #[default]
    WaitingForPrelaunch,
    /// Saw the room un-launched; the next launched reading is a real blastoff → fire.
    Armed,
    /// Already fired for the current launch.
    Fired,
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

/// The live autopilot's state this tick, published by whichever thrust system ran (SP or
/// predicted MP) for the flight HUD and the trajectory line — the client computes full
/// guidance locally in both modes, so nothing here needs the wire. `None` whenever no
/// launch is being flown (idle, or the assembly broke up / lost its rockets).
#[derive(Resource, Default)]
pub struct Autopilot(pub Option<AutopilotSnapshot>);

pub struct AutopilotSnapshot {
    /// Assembly COM in the **true planet frame**, f64 (the flown trail must stay
    /// smooth at Mm-scale altitudes where f32 steps by whole metres).
    pub true_pos: bevy::math::DVec3,
    /// The room-frame offset [`Self::true_pos`] was folded with, so a consumer can fold
    /// back to the SAME room-local frame the rocket is rendered in
    /// (`true_pos - frame_offset` is exactly the local COM). Load-bearing for the
    /// trajectory line: it stores its path in true coordinates and must subtract this,
    /// **not** the visual `ClientRoomFrame` — those two frames differ by tens of metres
    /// for a packet or two around a rebase (that gap is what PR #178 measured), and
    /// folding by the wrong one hangs the whole line that far off the rocket.
    pub frame_offset: bevy::math::DVec3,
    /// Assembly velocity in the true planet frame (frame-folded in MP).
    pub true_vel: Vec3,
    /// The derated point-mass vehicle the plan was optimized for — what the trajectory
    /// preview re-propagates.
    pub vehicle: Vehicle,
    /// The whole planning seed of the launch being flown — the pitchover angle plus the
    /// state it was sampled from. The trajectory preview needs the seed rather than just
    /// the angle so it can rebuild the *identical* [`PitchProgram`] the autopilot is
    /// holding and forecast under that (see `guidance::propagate_program`); an angle alone
    /// only identifies the ideal law, which is not what gets flown.
    pub seed: LaunchSeed,
    /// The pitch program's commanded tilt from radial-up at the current speed (rad).
    pub command_angle: f32,
    /// One member body of the flown assembly and its **raw** local position at this
    /// fixed step — whichever body the stable `NetPart::id` order puts first.
    ///
    /// The trajectory line uses the pair to re-anchor itself onto the pose the rocket is
    /// actually *drawn* at: predicted bodies are frame-interpolated between fixed ticks
    /// (`FrameInterpolationPlugin<Position>`, `net.rs`), so by render time the body has
    /// moved off this raw sample. The line's `follow_rendered_pose` reads the body's
    /// resulting `Transform` in `PostUpdate` and shifts by the difference — which is why
    /// the raw value has to be captured *here*, in `FixedUpdate`, before interpolation
    /// touches it. Bundled as one field because they are only meaningful together: the
    /// entity says *which* body, the position says where it was beforehand.
    pub anchor: Option<(Entity, Vec3)>,
    /// Guidance throttle after the escape cutoff: `0.0` = engines cut, coasting.
    pub throttle: f32,
    /// Current aerodynamic drag on the assembly (N).
    pub drag: f32,
    /// Net thrust force actually applied this tick (N) — the drag readout's yardstick.
    pub net_thrust: f32,
    /// The flown assembly's own size: half the diagonal of the box its parts occupy (m).
    ///
    /// Measured here because this is the only place that already walks the members every
    /// tick, and framing the craft is not something the camera can guess: a three-block
    /// hopper and a forty-part tower need the chase camera at very different distances,
    /// and a fixed number is wrong for one of them. Half the bounding diagonal rather than
    /// a COM-relative radius on purpose — the camera is framing the *silhouette*, which
    /// doesn't care where the mass sits.
    pub radius: f32,
}

/// Half the diagonal of the axis-aligned box enclosing `points` — the craft's own size, as
/// the chase camera frames it. Zero for a single point.
fn spread(points: impl Iterator<Item = Vec3>) -> f32 {
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let mut any = false;
    for point in points {
        lo = lo.min(point);
        hi = hi.max(point);
        any = true;
    }
    if any {
        (hi - lo).length() * 0.5
    } else {
        0.0
    }
}

impl AutopilotSnapshot {
    /// Build the snapshot from the flown true (frame-folded) state and this tick's plan +
    /// forces. The one constructor both thrust sites publish through, so the single-player
    /// and predicted-multiplayer readouts can't drift: the derated vehicle, command angle,
    /// and drag are derived here rather than spelled out per site. `net_force` is the raw
    /// vector (its magnitude is stored). Single-player passes local == true.
    fn new(
        true_pos: bevy::math::DVec3,
        frame_offset: bevy::math::DVec3,
        true_vel: Vec3,
        engines: usize,
        gravity: Vec3,
        total_mass: f32,
        program: &PitchProgram,
        anchor: Option<(Entity, Vec3)>,
        throttle: f32,
        net_force: Vec3,
        radius: f32,
    ) -> Self {
        Self {
            radius,
            true_pos,
            frame_offset,
            true_vel,
            vehicle: Vehicle::derated(engines, gravity, total_mass),
            seed: program.seed,
            command_angle: program.angle_at(true_vel.length()),
            anchor,
            throttle,
            drag: bad_spaceship_shared::map::drag_force(true_pos.as_vec3(), true_vel).length(),
            net_thrust: net_force.length(),
        }
    }
}

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
        net_launched(orb)
    } else {
        local.sp_launched()
    }
}

/// Whether the room is in flight, so building is locked out — grab, attach, and delete
/// all stand down until it lands. Written every frame by [`lock_out_building_in_flight`]
/// (the one place that reads the launch state for this purpose) and read by the touch
/// overlay, which stops drawing *and* hit-testing the grab/action buttons rather than
/// leaving two dead controls under the rider's thumb.
#[derive(Resource, Default)]
pub(crate) struct BuildingLockedOut(pub bool);

/// Once the room has blasted off you're riding, not building — so force the build
/// modifier off for the rest of the flight, and publish that fact for the UI.
///
/// [`Modifying`] is the **one** gate every piece of the delete gesture reads: the
/// delete-zone sphere (`delete_zone_visibility`), the red in-zone joint markers
/// (`update_predelete_joints` → `display_predelete_joints`), and deletion itself
/// (single-player `delete_joints` walks the same `PredeleteJoints`; multiplayer's
/// `read_grab_intent` is separately launch-gated). Clearing it here states the rule
/// once instead of bolting a launch check onto each of those consumers.
///
/// It also matters most on **touch**, where `mobile::apply_pointer` holds the
/// modifier on permanently while empty-handed — so on a phone the delete zone and
/// the red joints were painted over the vehicle for the whole ascent.
fn lock_out_building_in_flight(
    local: Res<LaunchLocal>,
    multiplayer: Option<Res<SuppressLocalParts>>,
    orb: Query<&NetLaunch>,
    mut locked_out: ResMut<BuildingLockedOut>,
    mut modifiers: Query<&mut Modifying>,
) {
    let launched = room_launched(&local, multiplayer.is_some(), &orb);
    // Change-guarded so a settled flight doesn't wake every reader each frame.
    if locked_out.0 != launched {
        locked_out.0 = launched;
    }
    if !launched {
        return;
    }
    for mut modifying in &mut modifiers {
        if modifying.0 {
            modifying.0 = false;
        }
    }
}

/// Whether the (multiplayer) room's assembly has launched, read straight off the
/// replicated [`NetLaunch`]. For MP-only systems that don't carry [`LaunchLocal`].
pub(crate) fn net_launched(orb: &Query<&NetLaunch>) -> bool {
    orb.iter().next().is_some_and(|l| l.launched)
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

/// Release the client's *predicted* ground joints on the scheduled blastoff tick — the
/// predicted twin of the server's [`fire_scheduled_launches`], and the other half of
/// making liftoff tick-exact.
///
/// `bind_replicated_joints` rebuilds every replicated joint as **real Avian physics** in
/// the predicted world, ground clamps included (they name the local ground through the
/// `GROUND_JOINT_ID` sentinel). The server cuts those at blastoff by despawning the joint
/// entities — but that is replication, so it arrives ~1 RTT later. Until it did, the client
/// spent the opening ticks of every flight firing its engines against a pad clamp the
/// server had already released: not merely a late burn, but the two sims solving a
/// *different set of constraints*, which is the worst shape of prediction disagreement.
///
/// Only the constraint is dropped, never the entity — it is server-owned and replicated,
/// and despawning it locally would fight replication. `bind_replicated_joints` cannot
/// re-add it either: that is gated on `Without<JointAnchorBody>`, and the marker stays.
///
/// A rollback that replays ticks from before blastoff replays them without the clamp,
/// since the removal is structural rather than per-tick state. That is harmless here: the
/// assembly is at rest on the pad in those ticks, and the server is about to cut the same
/// joint on the same tick anyway.
fn release_predicted_ground_joints(
    mut commands: Commands,
    timeline: Res<lightyear::prelude::LocalTimeline>,
    orb: Query<&NetLaunch>,
    joints: Query<(Entity, &NetJoint), With<SphericalJoint>>,
) {
    let Some(launch) = orb.iter().next() else {
        return;
    };
    if !launch.launched_at(timeline.tick()) {
        return;
    }
    for (entity, joint) in &joints {
        if joint.body1 == GROUND_JOINT_ID || joint.body2 == GROUND_JOINT_ID {
            commands.entity(entity).remove::<SphericalJoint>();
        }
    }
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
        // Fire the banner once, on a launch edge we actually WITNESSED counting down —
        // never when merely (re)joining an already-launched room. `orb` is absent while
        // the replicated world is torn down (a tab-background reconnect) or before it
        // first replicates (a fresh iOS reload), and absence must NOT read as "not
        // launched" — doing so re-armed the banner every reconnect, so it flashed
        // "Blastoff!" again on every foreground. Gate firing on having first seen the
        // room un-launched (`mp_seen_prelaunch`); a client that arrives already-launched
        // never sees that and stays quiet.
        match orb.iter().next().map(|l| l.launched) {
            // Un-launched: arm (and re-arm after a room reset, so a relaunch fires again).
            Some(false) => local.mp_banner = MpBanner::Armed,
            Some(true) if local.mp_banner == MpBanner::Armed => {
                local.banner = BLASTOFF_BANNER_SECS;
                local.mp_banner = MpBanner::Fired;
            }
            // Some(true) already fired, or orb absent (disconnected / not yet
            // replicated): leave the latch untouched so a reconnect can't re-fire.
            _ => {}
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

/// The frame left behind by a client-predicted floating-origin rebase (see
/// [`predict_room_rebase`]) that no replicated [`NetRoomFrame`] sample covers yet:
/// the frame right after the predicted rebase, stamped with the tick it fired —
/// literally a locally-authored `NetRoomFrame` sample. Predicted physics reads the
/// frame through [`predicted_frame_at`], which prefers this over the (pre-rebase,
/// ~1 RTT stale) replicated sample. Cleared the moment a replicated sample at or
/// past its tick arrives — from then on the server's frame is the truth. Holding
/// only the newest predicted rebase is deliberate: two rebases inside one RTT can
/// only happen in the trigger→reset descent edge, and dropping the older one just
/// degrades to the documented one-rollback fallback.
#[derive(Resource, Default)]
pub struct PredictedRebase(Option<NetRoomFrame>);

/// The room frame advanced to `tick` via [`NetRoomFrame::frame_at`], reading the
/// predicted-rebase sample when one applies (it post-dates the replicated sample by
/// construction — see [`PredictedRebase`]). The `tick` bound keeps the read correct
/// for any caller at any tick, independent of the prune in `predict_room_rebase`
/// having run first.
fn predicted_frame_at(
    net: Option<&NetRoomFrame>,
    rebase: &PredictedRebase,
    tick: Tick,
) -> (DVec3, Vec3) {
    rebase
        .0
        .as_ref()
        .filter(|frame| tick - Tick(frame.tick) >= 0)
        .or(net)
        .map(|frame| frame.frame_at(tick))
        .unwrap_or_default()
}

/// Predict the room's floating-origin rebase on the predicted world — the client half
/// of the server's `rebase_room_frames`, sharing its decision function
/// ([`rebase_room_frame`]) so both peers rebase at the same tick with the same shift.
///
/// Why: the rebase used to reach clients as a rollback by construction — the server
/// shifts every room entity ~2 km in one tick, the confirmed samples arrive, and the
/// comparator can only see a km-scale "misprediction" (one rollback + one trigger per
/// body at every rebase, the dominant remaining rollback source on a clean flight).
/// But the decision is a pure function of sim state the two peers agree on to sub-mm,
/// so the client can run it on its predicted parts at the same tick the server does:
/// both sides shift together and nothing ever reaches the comparator. A mispredicted
/// trigger tick (the anchor crossing the threshold within the sims' sub-mm
/// disagreement of a tick boundary) just degrades to the old one-rollback behaviour.
///
/// Replay-safe by re-derivation: entries at or after the current tick are dropped at
/// the top of every run, so a rollback replay that walks back through a predicted
/// rebase discards the abandoned-future record and re-decides from the replayed
/// (confirmed-reset) state — re-shifting at the same tick if the server really
/// rebased, or not, if it didn't.
///
/// Ordering matches the server: first in the FixedUpdate chain, before anything that
/// computes world-space force targets for the same tick (gravity/thrust) — a thrust
/// point computed pre-shift but applied post-shift would be a km-scale lever arm.
///
/// The anchor mirrors the server's: volume-weighted mean position/velocity of the
/// room's largest assembly (the replicated [`InLargestAssembly`] markers — the same
/// membership the server's union-find computes), falling back to all parts when no
/// marker is present. The float-reduction order differs from the server's ECS order,
/// but the resulting ULP-scale anchor disagreement is dwarfed by the sims' existing
/// sub-mm drift, and both are absorbed by the shift being ~identical, not identical.
fn predict_room_rebase(
    frames: Query<&NetRoomFrame>,
    timeline: Res<lightyear::prelude::LocalTimeline>,
    mut rebase: ResMut<PredictedRebase>,
    // The anchor read and the shift write overlap on `Position`/`LinearVelocity`
    // (B0001) — sequence them.
    mut set: ParamSet<(
        Query<
            (&Position, &LinearVelocity, &NetPart, Has<InLargestAssembly>),
            // `Without<Asteroid>` mirrors the server's anchor exactly. It matters only in
            // the no-assembly fallback below (with markers present, rocks are unmarked and
            // already excluded) — but that fallback is precisely the broken-up-stack case,
            // where the rocks are still in the room and the two peers must not choose
            // different anchors while the frame is being decided.
            (With<Predicted>, Without<bad_spaceship_shared::net::Asteroid>),
        >,
        Query<
            (&mut Position, &mut LinearVelocity),
            (With<Predicted>, Or<(With<NetPart>, With<NetPlayer>)>),
        >,
    )>,
) {
    let Some(net) = frames.iter().next().copied() else {
        // No room state (boot / disconnect teardown): nothing to predict against.
        rebase.0 = None;
        return;
    };
    let tick = timeline.tick();
    // Prune (wrapping i32 tick compares): drop the entry once a replicated sample
    // covers it (server truth arrived for that tick range), and drop an entry at or
    // after the current tick (an abandoned future left by a rollback reset — this
    // run re-derives this tick's decision from the replayed state).
    //
    // The covered-arm's promptness rests on an invariant of the server's
    // `rebase_room_frames`: an ACTIVE frame republishes every tick (its offset
    // integrates, so `set_if_neq` always fires) — after a real rebase the sample
    // tick advances past the knot within ~1 RTT. The one mode where that fails is a
    // rebase the client predicted but the server declined (possible only if the
    // anchor membership churned inside the ~RTT marker-replication window — static
    // in flight): the server keeps publishing the inactive default, only the
    // replay-arm can kill the knot, and until the fresher markers arrive each
    // replay re-predicts the same wrong rebase — a bounded, self-healing burst of
    // km-scale rollbacks (the pre-#179 cost of every rebase) rather than one.
    // Eager covering also means a rollback reaching BEHIND a just-covered rebase
    // back-extrapolates the post-rebase sample across the discontinuity for the
    // 1–2 replayed ticks before it (a frame that never existed) — worth at most one
    // extra mispredicted-replay rollback in that packet-skew corner, which is the
    // price of holding a single knot instead of a history.
    if let Some(pending) = &rebase.0 {
        if Tick(pending.tick) - Tick(net.tick) <= 0 || tick - Tick(pending.tick) <= 0 {
            rebase.0 = None;
        }
    }

    // The anchor, mirroring the server's: the largest assembly (here the replicated
    // `InLargestAssembly` markers — written by the same union-find the server's
    // anchor re-runs), falling back to all parts when no marker is present (the
    // server's no-assembly fallback; markers exist iff a ≥2-part assembly does, so
    // the rules coincide). Membership parity is temporal, not structural: the
    // markers replicate ~1 RTT behind the server's fresh union-find, so joint/part
    // churn within that window of a trigger crossing can mispredict the shift —
    // accepted because flight membership is static and the markers are the
    // codebase's canonical membership seam (COM orb, launch, thrust all read them).
    // `.any()` short-circuits, so the common marked case costs barely more than one
    // pass.
    let parts = set.p0();
    let any_marked = parts.iter().any(|(.., marked)| marked);
    let anchor = weighted_anchor(
        parts
            .iter()
            .filter(|(.., marked)| *marked || !any_marked)
            .map(|(position, linear, part, _)| (position.0, linear.0, part_volume(part.shape))),
    );
    let Some((anchor_pos, anchor_vel)) = anchor else {
        return;
    };
    let (offset, velocity) = predicted_frame_at(Some(&net), &rebase, tick);
    let Some(outcome) = rebase_room_frame(offset, velocity, anchor_pos, anchor_vel) else {
        return;
    };
    for (mut position, mut linear) in &mut set.p1() {
        position.0 -= outcome.dpos;
        linear.0 -= outcome.dvel;
    }
    println!(
        "[c-rebase] tick={} shift {:?} boost {:?} -> offset {:?} vel {:?}",
        tick.0, outcome.dpos, outcome.dvel, outcome.offset, outcome.velocity
    );
    rebase.0 = Some(NetRoomFrame {
        offset: outcome.offset.to_array(),
        velocity: outcome.velocity.to_array(),
        tick: tick.0,
    });
}

/// Planet gravity for the predicted multiplayer bodies — the same radial correction as
/// [`apply_sp_gravity`], but true position folds in the room's floating-origin offset
/// (the replicated [`NetRoomFrame`] advanced to this tick — see `frame_at`) so `r` is the
/// real distance from the centre while the co-moving frame keeps the body near the local
/// origin. Only `Predicted` bodies are locally
/// simulated (interpolated remotes are replication-driven), so only they get the force;
/// the server applies the identical field, so prediction converges.
///
/// Avatars are keyed on `NetPlayer` (which every avatar carries), NOT on `Character`:
/// only the OWNER's avatar gets `Character` (`insert_remote_avatar_body` deliberately
/// omits it for remote players), so filtering on it silently left every *other* rider
/// weightless on this client while the server pulled them down — a full 1 g of force
/// disagreement on every remote avatar, every tick, i.e. guaranteed rollback churn.
/// The server's twin (`apply_server_gravity`) covers `Or<(NetPart, ServerAvatar)>`.
fn apply_mp_gravity(
    gravity: Res<Gravity>,
    // The frame advanced to THIS tick, predicted rebases included
    // (`predicted_frame_at`) — never the visual `ClientRoomFrame`, whose render-frame
    // smoothing stood metres away from the server's frame and fed every predicted
    // body a measurably wrong `r`.
    frames: Query<&NetRoomFrame>,
    rebase: Res<PredictedRebase>,
    timeline: Res<lightyear::prelude::LocalTimeline>,
    mut bodies: Query<
        (&Position, Forces),
        (With<Predicted>, Or<(With<NetPart>, With<NetPlayer>)>),
    >,
) {
    let offset =
        predicted_frame_at(frames.iter().next(), &rebase, timeline.tick()).0.as_vec3();
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
    // Escape-cutoff hysteresis state (see `escape_cutoff`): held across ticks so the throttle
    // can't flicker at the boundary, cleared with the plan when the launch ends.
    mut cut: Local<bool>,
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
    mut autopilot: ResMut<Autopilot>,
    // The rider's mass, for the ascent plan: locked aboard at launch, so their weight
    // flies with the stack and belongs in the planned thrust-to-weight.
    rider: Query<&ComputedMass, With<Character>>,
    stick: Res<crate::pilot::PilotStick>,
) {
    if local.sp != SpPhase::Launched {
        fuel.0 = 0.0; // idle: keep the readout at zero until the next launch fires
        *sp_plan = None; // re-plan the ascent for the next launch
        *cut = false; // reset the cutoff hysteresis for the next launch
        apparent.0 = None;
        autopilot.0 = None;
        return;
    }
    let Some((members, _)) = main_assembly(&parts, &joints) else {
        apparent.0 = None;
        autopilot.0 = None;
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
        autopilot.0 = None;
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
    // Total flown mass — riders included (locked aboard at launch): feeds the ascent
    // plan, the riders' apparent-up, and the autopilot snapshot's vehicle.
    let total_mass: f32 = parts
        .iter()
        .filter(|(entity, ..)| members.contains(entity))
        .map(|(_, _, m)| m.value())
        .sum::<f32>()
        + rider.iter().map(|m| m.value()).sum::<f32>();
    // Fuel-optimal ascent guidance: a per-assembly pitchover (optimized once at launch
    // from this stack's thrust-to-weight — heavy haulers lean, engine-dense stacks go
    // vertical), flown as a pitch program with an escape-energy throttle cutoff. No
    // floating origin in single player, so the true planet-frame state is just the local
    // COM + velocity.
    let program = sp_plan.get_or_insert_with(|| {
        PitchProgram::plan(com, spin.linear_velocity, geometry.len(), gravity.0, total_mass, None)
    });
    let autopilot_command = program_guidance(com, spin.linear_velocity, program, &mut cut);
    // The pilot's stick, folded in exactly where the server folds it in (see
    // `steer_guidance`) so single player and multiplayer fly the same law.
    let guidance = steer_guidance(autopilot_command, com, spin.linear_velocity, stick.value);
    let net_force = apply_thrust(
        com,
        gravity.0,
        &geometry,
        &spin,
        time.delta_secs(),
        &mut integral,
        guidance,
        // Single player has no floating-origin frame, so true state == local.
        com,
        spin.linear_velocity,
        &mut fuel.0,
        &mut set.p2(),
    );
    // Apparent up for the riders (see `ApparentUp`); single-player true == local.
    apparent.0 = (total_mass > 0.0).then(|| apparent_up(net_force, total_mass, com));
    autopilot.0 = (total_mass > 0.0).then(|| {
        AutopilotSnapshot::new(
            com.as_dvec3(),
            bevy::math::DVec3::ZERO,
            spin.linear_velocity,
            geometry.len(),
            gravity.0,
            total_mass,
            program,
            geometry.first().map(|(entity, position, ..)| (*entity, *position)),
            guidance.throttle,
            net_force,
            spread(
                parts
                    .iter()
                    .filter(|(entity, ..)| members.contains(entity))
                    .map(|(_, transform, _)| transform.translation()),
            ),
        )
    });
}

/// Where the flown steering deflection comes from in multiplayer, bundled as one
/// `SystemParam` because the choice between its two sources is the whole point.
///
/// **If we are the pilot, fly our own tick-stamped input** — `ActionState<NetInput>`, not
/// the live `PilotStick` resource. lightyear buffers the action state per tick and restores
/// it during a rollback replay, so a replayed tick re-flies the deflection that tick
/// actually had; the resource holds only what the stick reads *now*, which would re-fly the
/// present into the past and diverge a little further with every rollback. It is the same
/// discipline as `AttitudeIntegral` — hidden per-tick state belongs on something that rolls
/// back — and the same reason the server reads its copy off the input rather than a
/// resource.
///
/// **If we are not, fly the replicated level** ([`NetLaunch::steer`]). A spectator or a
/// second rider has no channel to the pilot's hands, so their prediction runs ~1 RTT behind
/// the real command while it is moving. That is the ordinary standing of any remote
/// player's intent here and the position rollback corrects it; there is no seed for a human
/// hand to replicate instead.
#[derive(bevy::ecs::system::SystemParam)]
struct PilotInput<'w, 's> {
    local: Query<'w, 's, &'static LocalId, With<Connected>>,
    own: Query<'w, 's, &'static ActionState<NetInput>, With<InputMarker<NetInput>>>,
}

impl PilotInput<'_, '_> {
    fn stick(&self, launch: &NetLaunch) -> Vec2 {
        let mine = launch.pilot != 0
            && crate::net::my_netcode_id(&self.local) == Some(launch.pilot);
        let raw = if mine {
            self.own.iter().next().map(|state| state.0.steer).unwrap_or_default()
        } else {
            launch.steer
        };
        Vec2::from(raw).clamp_length_max(1.0)
    }
}

/// Apply balanced thrust to the multiplayer assembly's **predicted** rockets each physics
/// tick while the room is launched. Membership + pose come from the replicated
/// `InLargestAssembly` markers and the predicted Avian `Position`/`Rotation` (not
/// `GlobalTransform`, which `lightyear_avian` drives out of the fixed schedule).
fn apply_mp_thrust(
    time: Res<Time>,
    // The launch autopilot's per-assembly PID integral state (see `assembly_burn`).

    // The ascent plan mirror: the pitch program rebuilt from the server's replicated
    // planning seed (keyed by that seed, so a re-plan on the server rebuilds here too).
    mut mp_plan: Local<Option<PitchProgram>>,
    // Escape-cutoff hysteresis state (see `escape_cutoff`): stops the *predicted* engine
    // chattering at the `E ≈ 0` boundary (the flame flicker), cleared when the room idles.
    mut cut: Local<bool>,
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
                &NetPart,
                Option<&AttitudeIntegral>,
                Option<&EscapeCut>,
            ),
            (With<NetPart>, With<Predicted>, With<InLargestAssembly>),
        >,
        Query<
            (Entity, Forces, &mut Gimbal, Option<&mut FlameThrottle>),
            (With<RocketEngine>, With<Predicted>),
        >,
        // Write-back of the rolled-back attitude integral (see `AttitudeIntegral`).
        Query<(&mut AttitudeIntegral, &mut EscapeCut), (With<RocketEngine>, With<Predicted>)>,
    )>,
    // Tick-exact frame for the physics (see `apply_mp_gravity` — same rule): the
    // visual `ClientRoomFrame` must not feed thrust/drag/guidance.
    frames: Query<&NetRoomFrame>,
    rebase: Res<PredictedRebase>,
    gravity: Res<Gravity>,
    mut fuel: ResMut<FuelUsed>,
    mut apparent: ResMut<ApparentUp>,
    mut autopilot: ResMut<Autopilot>,
    // Riders for the ascent plan AND the COM/inertia measurement (all avatars in the room
    // are locked aboard at launch, and all are predicted): matches the server's
    // rider-inclusive plan *and* its rider-inclusive `measure_assembly_spin`. Sorted by
    // the replicated `client_id` so multi-rider reductions match the server's order too.
    // `Without<RocketEngine>` keeps this disjoint from the `Forces` query in the ParamSet
    // (which takes `LinearVelocity` mutably) — B0001 otherwise, same guard the server uses.
    // Every predicted avatar in the room — keyed on `NetPlayer` (which only avatars
    // carry), NOT on `Character`: only the OWNER's avatar gets `Character`
    // (`insert_remote_avatar_body` deliberately omits it for remote players), so filtering
    // on it silently dropped every *other* rider's mass from the COM/inertia and the
    // thrust-to-weight — measured as an exactly-1.000 kg mass gap vs the server on every
    // single tick of a two-player flight, which trims the burn about the wrong centre.
    riders: Query<
        (&Position, &LinearVelocity, &ComputedMass, &NetPlayer),
        (With<Predicted>, Without<RocketEngine>, Without<NetPart>),
    >,
    // Tick-keyed burn trace (`BS_BURN_TRACE`), so the client's and server's burn inputs
    // can be diffed at the SAME tick number instead of inferred by elimination.
    timeline: Res<lightyear::prelude::LocalTimeline>,
    pilot: PilotInput,
) {
    // Tick-exact, NOT the replicated `launched` level: the level is replicate-only, so
    // reading it here started the predicted burn a link-dependent number of ticks after
    // the server's, with nothing to replay the gap (see `NetLaunch::launched_at`).
    let Some(launch) = orb.iter().next().filter(|l| l.launched_at(timeline.tick())) else {
        fuel.0 = 0.0; // idle: keep the readout at zero until the room launches
        *mp_plan = None; // re-plan on the next launch
        *cut = false; // reset the cutoff hysteresis for the next launch
        apparent.0 = None;
        autopilot.0 = None;
        return;
    };
    // The server's whole planning seed, replicated so the rebuilt program is identical
    // rather than merely similar (a default seed — straight up — for the tick or two
    // before the first value arrives; that window is inside the vertical kick phase,
    // where the real program commands ~0 too).
    let seed = launch.plan;
    let need_plan = mp_plan.as_ref().map(|p| p.seed) != Some(seed);
    // The assembly's COM + motion state, via the shared measurement (see
    // `measure_assembly_spin`) so the trim matches the server exactly; collect
    // the member rockets' poses alongside (`Gimbal` marks the rockets — it rides
    // `insert_rocket_physics`). The mass sum feeds only a plan rebuild, so it's
    // gathered only on the (once-per-launch) tick that needs it.
    let (measured, geometry, part_mass_total, integral_seed, cut_seed, radius) = {
        let members = set.p0();
        // **Cross-world stable member order.** Everything below is an order-sensitive
        // float reduction: `measure_assembly_spin` mass-weights positions/velocities into
        // the COM + inertia, the mass total is a plain sum, and `geometry` fixes the
        // per-rocket order of the thrust least-squares solve (whose clamped refinement
        // pass is order-dependent). Float addition isn't associative, so iterating in ECS
        // query (spawn) order — which the server and a replication-fed client do NOT
        // share — gives each peer a slightly different COM and therefore a different
        // balanced thrust, which steers the assembly apart. Sorting by the replicated
        // `NetPart::id` makes both peers reduce in the identical sequence.
        let mut ordered: Vec<_> = members.iter().collect();
        ordered.sort_unstable_by_key(|(_, _, _, _, _, _, _, net, _, _)| net.id);
        // Locked riders fly with the assembly, so their weight belongs in the COM +
        // inertia the attitude controller balances about — exactly as the server does it
        // (`apply_room_rocket_thrust` chains its riders into the same measurement). The
        // client used to measure a PARTS-ONLY COM while the server measured parts+riders,
        // so the two peers trimmed thrust about different centres and the predicted
        // attitude drifted from the confirmed one every tick — a standing divergence no
        // amount of rollback could reconcile. Riders are rotation-locked, so they
        // contribute mass + linear motion but no body spin (`Vec3::ZERO` angular), and
        // they are chained AFTER the parts to match the server's reduction order.
        let mut ordered_riders: Vec<_> = riders.iter().collect();
        ordered_riders.sort_unstable_by_key(|(_, _, _, player)| player.client_id);
        let samples = || {
            ordered
                .iter()
                .map(|(_, position, _, linear, angular, part_mass, _, _, _, _)| {
                    (position.0, linear.0, angular.0, part_mass.value())
                })
                .chain(ordered_riders.iter().map(|(position, linear, mass, _)| {
                    (position.0, linear.0, Vec3::ZERO, mass.value())
                }))
        };
        let geometry: Vec<(Entity, Vec3, Quat, bevy::math::Vec2)> = ordered
            .iter()
            .filter_map(|(entity, position, rotation, _, _, _, gimbal, _, _, _)| {
                gimbal.map(|g| (*entity, position.0, rotation.0, g.0))
            })
            .collect();
        // Needed every tick now (apparent-up divides the net thrust by it), not just
        // on plan-rebuild ticks.
        let part_mass_total: f32 =
            ordered.iter().map(|(_, _, _, _, _, part_mass, _, _, _, _)| part_mass.value()).sum();
        // The assembly's live PID integral: read from its lowest-`NetPart::id` member, so
        // which rocket holds the authoritative copy is cross-world stable. This value rides
        // a rolled-back component, so a replay resumes from the integral the replayed tick
        // actually had instead of re-integrating on top of the current one.
        let integral_seed = ordered
            .iter()
            .find_map(|(_, _, _, _, _, _, _, _, integral, _)| integral.map(|i| i.0))
            .unwrap_or(Vec3::ZERO);
        let cut_seed = ordered
            .iter()
            .find_map(|(_, _, _, _, _, _, _, _, _, cut)| cut.map(|c| c.0))
            .unwrap_or(false);
        // The craft's own size, for the chase camera's framing (see `AutopilotSnapshot`).
        let radius = spread(ordered.iter().map(|(_, position, ..)| position.0));
        (measure_assembly_spin(samples), geometry, part_mass_total, integral_seed, cut_seed, radius)
    };
    let Some((com, spin)) = measured else {
        apparent.0 = None;
        autopilot.0 = None;
        return;
    };
    // True planet-frame state folds in the room's floating-origin frame (offset +
    // co-moving velocity), so the guidance sees real altitude/velocity under a rebase.
    let (frame_offset, frame_velocity) =
        predicted_frame_at(frames.iter().next(), &rebase, timeline.tick());
    let true_com = com + frame_offset.as_vec3();
    let true_vel = spin.linear_velocity + frame_velocity;
    let total_mass = part_mass_total + riders.iter().map(|(_, _, mass, _)| mass.value()).sum::<f32>();
    // Rebuild the pitch program when the replicated seed (re)arrives, from that seed
    // ALONE — no locally measured state. Re-planning from the client's own live position,
    // velocity and mass is what the seed replaces: the program is a table sampled off a
    // forward-simulated trajectory, so seeding it a few ticks later (once the angle
    // arrived) built a genuinely different table, not a slightly-late one.
    if need_plan {
        *mp_plan = Some(PitchProgram::build(seed));
    }
    // Same rollback discipline as the integral: latch from the replayed tick's state.
    let mut cut_latch = cut_seed;
    let autopilot_command =
        program_guidance(true_com, true_vel, mp_plan.as_ref().unwrap(), &mut cut_latch);
    // Fold in the pilot's stick at the same seam the server does, from the source that
    // rolls back correctly for whoever we are (see `PilotInput`).
    let guidance = steer_guidance(autopilot_command, true_com, true_vel, pilot.stick(launch));
    if burn_trace() {
        let plan_probe = mp_plan.as_ref().unwrap().probe();
        println!(
            "[burn] C tick={:?} com={:.6},{:.6},{:.6} ang={:.6},{:.6},{:.6} lin={:.6},{:.6},{:.6} inert={:.6} m={:.6} int={:.6},{:.6},{:.6} thr={:.6} dir={:.6},{:.6},{:.6} n={} ns={} a1k={:.9} off={:.6},{:.6},{:.6}",
            timeline.tick(),
            com.x, com.y, com.z,
            spin.angular_velocity.x, spin.angular_velocity.y, spin.angular_velocity.z,
            spin.linear_velocity.x, spin.linear_velocity.y, spin.linear_velocity.z,
            spin.inertia,
            total_mass,
            integral_seed.x, integral_seed.y, integral_seed.z,
            guidance.throttle,
            guidance.thrust_dir.x, guidance.thrust_dir.y, guidance.thrust_dir.z,
            geometry.len(),
            // See the server twin: the fixed-speed probe compares the two pitch programs
            // directly, and `off` the two floating-origin frames.
            plan_probe.0,
            plan_probe.1,
            frame_offset.x, frame_offset.y, frame_offset.z,
        );
    }
    // Integrate from the ROLLED-BACK value, not a `Local` that accumulates once per replay
    // (see `AttitudeIntegral`) — this is what keeps the predicted burn on the server's
    // trajectory instead of drifting a little further apart with every rollback.
    let mut integral = integral_seed;
    let net_force = apply_thrust(
        com,
        gravity.0,
        &geometry,
        &spin,
        time.delta_secs(),
        &mut integral,
        guidance,
        // True (frame-folded) state — the same drag the server applies, so prediction
        // converges.
        true_com,
        true_vel,
        &mut fuel.0,
        &mut set.p1(),
    );
    // Persist the integral onto every member rocket so the rollback history carries it
    // (all members hold the same value; the lowest-id one is read back next tick).
    for (mut stored_integral, mut stored_cut) in &mut set.p2() {
        stored_integral.0 = integral;
        stored_cut.0 = cut_latch;
    }
    // Apparent up for the riders (see `ApparentUp`), from the true (frame-folded) state —
    // same formula as the server so the predicted movement basis matches.
    apparent.0 = (total_mass > 0.0).then(|| apparent_up(net_force, total_mass, true_com));
    let program = mp_plan.as_ref().unwrap();
    autopilot.0 = (total_mass > 0.0).then(|| {
        AutopilotSnapshot::new(
            frame_offset + com.as_dvec3(),
            frame_offset,
            true_vel,
            geometry.len(),
            gravity.0,
            total_mass,
            program,
            geometry.first().map(|(entity, position, ..)| (*entity, *position)),
            guidance.throttle,
            net_force,
            radius,
        )
    });
}

/// Per-tick flame reset — see the registration comment and
/// [`FlameThrottle`](crate::render_main_pass::flame_material::FlameThrottle).
fn reset_flame_targets(mut throttles: Query<&mut FlameThrottle>) {
    for mut throttle in &mut throttles {
        throttle.target = 0.0;
    }
}

/// Feed every character's [`FeltUp`] filter one sample of this tick's apparent-up
/// direction (see [`ApparentUp`]): the launched assembly's plumb-line direction while
/// launched, plain world-up otherwise. One system for both modes — the thrust systems
/// (whichever ran) already published the target.
///
/// Covers EVERY avatar this client simulates: the single-player character (`Character`,
/// no `NetPlayer`) and, in multiplayer, every predicted avatar (`NetPlayer`) — remote
/// riders included. Keying on `Character` alone skipped remote riders, because only the
/// OWNER's avatar gets it (`insert_remote_avatar_body` omits it); the server tilts every
/// avatar (`With<ServerAvatar>`), so a remote rider's `Rotation` — and the `Position`
/// this pivots about the foot — diverged on every tick of a turning ascent. The
/// `Confirmed` copies carry `NetPlayer` too but have no `Collider` (only predicted
/// avatars get a body), so they never match this query.
fn sample_felt_up(
    mut commands: Commands,
    apparent: Res<ApparentUp>,
    mut characters: Query<
        (Entity, Option<&mut FeltUp>, &mut Rotation, &mut Position, &Collider, Option<&NetPlayer>),
        Or<(With<Character>, With<NetPlayer>)>,
    >,
    // Tick-keyed avatar-pose trace (`BS_BURN_TRACE`) — see the server twin.
    timeline: Option<Res<lightyear::prelude::LocalTimeline>>,
) {
    let target = apparent.0.unwrap_or(Vec3::Y);
    for (entity, felt, mut rotation, mut position, collider, net_player) in &mut characters {
        let pivot = capsule_bottom_center(collider);
        let up_before = felt.as_ref().map(|f| f.up);
        let used =
            drive_felt_up(&mut commands, entity, felt, &mut rotation, &mut position, pivot, target);
        if burn_trace() {
            if let Some(u) = up_before {
                println!(
                    "[felt] C tgt={:.6},{:.6},{:.6} up={:.6},{:.6},{:.6}",
                    target.x, target.y, target.z, u.x, u.y, u.z
                );
            }
            if let (Some(tl), Some(np), Some(up)) = (timeline.as_ref(), net_player, used) {
                println!(
                    "[av] C tick={:?} id={} u={:.6},{:.6},{:.6} p={:.6},{:.6},{:.6} r={:.6},{:.6},{:.6},{:.6}",
                    tl.tick(),
                    np.client_id,
                    up.x,
                    up.y,
                    up.z,
                    position.0.x,
                    position.0.y,
                    position.0.z,
                    rotation.0.x,
                    rotation.0.y,
                    rotation.0.z,
                    rotation.0.w
                );
            }
        }
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
    // The assembly's TRUE (frame-folded) state, for the aerodynamic drag (see the
    // shared `map::apply_assembly_drag` for the physics); single-player true == local.
    true_com: Vec3,
    true_vel: Vec3,
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
    if burn_trace() {
        // Per-rocket applied force, indexed by position in `geometry` — which BOTH peers
        // sort by `NetPart::id`, so index i is the same physical rocket on each side.
        for (i, b) in burns.iter().enumerate() {
            let rot = geometry.get(i).map(|g| g.2).unwrap_or(Quat::IDENTITY);
            println!(
                "[force] C i={} f={:.6},{:.6},{:.6} p={:.6},{:.6},{:.6} g={:.6},{:.6} r={:.6},{:.6},{:.6},{:.6}",
                i, b.force.x, b.force.y, b.force.z, b.point.x, b.point.y, b.point.z,
                b.gimbal.x, b.gimbal.y, rot.x, rot.y, rot.z, rot.w
            );
        }
    }
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
    // Charge the whole assembly's drag to the first rocket (see `apply_assembly_drag`).
    if let Ok((_, mut forces, _, _)) = rocket_forces.get_mut(geometry[0].0) {
        bad_spaceship_shared::map::apply_assembly_drag(&mut forces, com, true_com, true_vel);
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
        // Countdown ("3/2/1") plays on the pad below the Lock button; the "Blastoff!"
        // linger plays once flying, so it drops below the launch HUD like the Lock button.
        let banner_y = if launched { BANNER_Y_FLIGHT } else { BANNER_Y_IDLE };
        egui::Area::new(egui::Id::new("bs_launch_banner"))
            .anchor(Align2::CENTER_TOP, egui::vec2(0.0, banner_y))
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
        // High on the pad; below the (now tall, ~5-line) flight HUD once launched.
        let lock_y = if launched { LOCK_BUTTON_Y_FLIGHT } else { LOCK_BUTTON_Y_IDLE };
        egui::Area::new(egui::Id::new("bs_lock_button"))
            .anchor(Align2::CENTER_TOP, egui::vec2(0.0, lock_y))
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
        .anchor(Align2::CENTER_TOP, egui::vec2(0.0, LAUNCH_BUTTON_Y))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `lock_out_building_in_flight` once over a single-player world in the given
    /// phase, and report `(the build modifier survived, building is locked out)`.
    fn build_state(phase: SpPhase) -> (bool, bool) {
        let mut app = App::new();
        app.insert_resource(LaunchLocal {
            sp: phase,
            ..default()
        })
        .init_resource::<BuildingLockedOut>()
        .add_systems(Update, lock_out_building_in_flight);
        let player = app.world_mut().spawn(Modifying(true)).id();
        app.update();
        (
            app.world().get::<Modifying>(player).unwrap().0,
            app.world().resource::<BuildingLockedOut>().0,
        )
    }

    #[test]
    fn building_is_yours_on_the_pad_and_locked_out_in_flight() {
        // On the pad — and even mid-countdown — the delete gesture is still yours,
        // and the touch overlay keeps its grab/action buttons.
        assert_eq!(build_state(SpPhase::Idle), (true, false));
        assert_eq!(build_state(SpPhase::Countdown { remaining: 1.5 }), (true, false));
        // After blastoff the modifier stands down — that one flag is what hides the
        // delete sphere, keeps joints from flaring red, and makes a click a no-op —
        // and the lockout flag pulls the two dead buttons off the rider's screen.
        assert_eq!(build_state(SpPhase::Launched), (false, true));
    }
}
