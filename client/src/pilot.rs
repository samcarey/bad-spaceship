//! Flying the rocket: the pilot's stick, and the chase camera it is flown from.
//!
//! Whoever presses Launch flies the ascent — for as long as they stay welded to it
//! (`NetLaunch::pilot`; unlock and the server hands the stick to the next rider). From
//! blastoff until the room stops flying, that player's view snaps to a chase camera behind
//! the craft and their **movement stick becomes the steering stick**: push it and the
//! flight bends, let go and the autopilot has it back on the next tick.
//!
//! The **look stick keeps looking**. It swings the chase camera around the craft while it
//! is held and drifts back to dead astern when released, so a pilot can check their flank
//! without giving up the default view or having to fly it back. It is a camera offset and
//! nothing else — where you look has no bearing on where you steer, which is what lets both
//! thumbs do their own job at once.
//!
//! Reusing the movement stick is deliberate: a locked rider cannot walk, so it and the
//! keyboard and the gamepad's left stick have nothing else to mean during a flight. Routing
//! them here gets touch, desktop and controller steering from one path — no second input
//! scheme to keep in sync, and no controls to learn.
//!
//! **The stick steers the autopilot's command, it does not replace it.** The deflection is
//! folded into the guidance direction in `shared::guidance::steer_guidance`, upstream of
//! the throttle/gimbal allocator — so a pilot input is executed by exactly the machinery
//! that executes the autopilot's, within the same structural limits, and releasing it
//! leaves nothing behind to unwind. Everything in this file is therefore about *reading a
//! human*: which fraction of a screen-space push, in which frame.
//!
//! **The frame is the planet's, not the vehicle's** — `guidance::flight_frame`, shared with
//! the steering law so that "push right" and "the world tips right" are one rotation rather
//! than two implementations that agree. A launch stack rolls freely under its own gimbals,
//! so a control or camera frame carried on the body's axes would invert the stick and roll
//! the horizon mid-climb. Keyed to the planet, the horizon stays level from the pad to a
//! horizontal burn and the stick means the same thing throughout.

use bad_spaceship_shared::guidance::flight_frame;
use bad_spaceship_shared::net::NetLaunch;
use bad_spaceship_shared::{
    DirectionalInput, InputEvents, MouseMotionDelta, OrbitingCamera, PlayerCameraOrbitCenter,
};
use bevy::prelude::*;
use lightyear::prelude::{Connected, LocalId};

use crate::input::get_look;
use crate::launch::{Autopilot, LaunchLocal};
use crate::net::my_netcode_id;

pub struct PilotPlugin;

impl Plugin for PilotPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AtTheControls>()
            .init_resource::<PilotStick>()
            .init_resource::<PilotLook>()
            .add_systems(
                Update,
                (
                    take_the_controls,
                    read_pilot_stick,
                    // After every writer of the look delta — the mouse (`get_look`) and the
                    // touch look-stick (`mobile::apply_pointer`, in `InputEvents`) — since
                    // it is a per-frame rate that the last writer wins.
                    read_pilot_look.after(get_look).after(InputEvents),
                    // Mount/unmount is a structural change (the camera leaves and rejoins
                    // the avatar's orbit rig), so it follows the state it reacts to.
                    mount_pilot_camera,
                )
                    .chain(),
            )
            // Same slot as the trajectory line's `follow_rendered_pose`, and for the same
            // reason: the pose the camera must sit behind only exists after frame
            // interpolation has run, and the transform written here must still be
            // propagated this frame.
            .add_systems(
                PostUpdate,
                drive_pilot_camera
                    .after(avian3d::prelude::PhysicsSystems::Writeback)
                    .before(TransformSystems::Propagate),
            );
    }
}

/// Whether this client is flying its room's assembly right now — the room has lifted off
/// and this player is its pilot.
///
/// In single player there is only one player, so whoever launched is flying by
/// construction and the flag is just "we are under way".
#[derive(Resource, Default)]
pub struct AtTheControls(pub bool);

/// Run condition for everything that must stand down while the stick is live — chiefly the
/// walking touch controls, whose thumb is now on the joystick.
pub fn piloting(controls: Res<AtTheControls>) -> bool {
    controls.0
}

/// The pilot's steering deflection this frame: `[right, up]`, clamped to the unit disc, in
/// the planet-relative [`flight_frame`]. Zero whenever we do not have the controls, so a
/// bystander's idle keys can never reach a room's guidance.
#[derive(Resource, Default)]
pub struct PilotStick {
    pub value: Vec2,
}

/// The pilot's temporary look offset from dead astern (radians), swung by the look stick or
/// the mouse and eased back to zero the moment it is released.
///
/// It is deliberately *not* the character's `Yaw`/`LookPitch`. Those drive the avatar's
/// orbit rig and its replicated facing, and they persist — which is exactly wrong here: a
/// pilot glances at a rock and needs the default view back without flying it back, and the
/// glance must not survive into the next thing they do.
#[derive(Resource, Default)]
pub struct PilotLook {
    yaw: f32,
    pitch: f32,
}

/// How far the look stick can swing the camera off dead astern before it stops (rad). Yaw
/// is unbounded — the pilot may look anywhere, including straight back at where they came
/// from — but pitch stops short of the poles, where an up-vector rolled to the planet stops
/// defining a view at all.
const MAX_LOOK_PITCH: f32 = 1.05;

/// Rate (per second) the look offset drifts back to dead astern once released. Fast enough
/// that the default view is never more than a moment away, slow enough to read as the
/// camera settling rather than snapping.
const LOOK_RETURN_RATE: f32 = 2.2;

/// Below this per-frame delta the look input counts as released and the drift takes over.
const LOOK_IDLE: f32 = 1e-3;

/// Decide whether this client has the stick. The pilot is named on the replicated launch
/// state, so every peer agrees on the answer — including, importantly, the peers who do
/// *not* have it and must keep their hands off the flight.
fn take_the_controls(
    mut controls: ResMut<AtTheControls>,
    local: Query<&LocalId, With<Connected>>,
    orb: Query<&NetLaunch>,
    launch: Res<LaunchLocal>,
    multiplayer: Option<Res<bad_spaceship_shared::part::SuppressLocalParts>>,
    mut stick: ResMut<PilotStick>,
) {
    let flying = if multiplayer.is_some() {
        match (orb.iter().next(), my_netcode_id(&local)) {
            // A pilot of `0` is a flight nobody pressed the button for — a mid-flight save
            // resumed from disk. It flies itself; nobody inherits the stick.
            (Some(state), Some(me)) => state.launched && state.pilot != 0 && state.pilot == me,
            _ => false,
        }
    } else {
        launch.sp_launched()
    };
    if controls.0 && !flying {
        // Handing the controls back mid-deflection would leave the last stick value on the
        // wire for as long as nothing overwrote it.
        *stick = PilotStick::default();
    }
    controls.0 = flying;
}

/// Read the stick: the movement intent, which the touch move-stick, the keyboard and the
/// gamepad all already feed, and which a rider welded to a flying deck has no other use for.
fn read_pilot_stick(
    controls: Res<AtTheControls>,
    movement: Query<&DirectionalInput>,
    mut stick: ResMut<PilotStick>,
) {
    stick.value = if controls.0 {
        movement
            .iter()
            .next()
            // `DirectionalInput` is the look-relative walk vector: x strafes, z goes
            // forward. As a steering stick those are exactly right/up.
            .map(|dir| Vec2::new(dir.0.x, dir.0.z).clamp_length_max(1.0))
            .unwrap_or(Vec2::ZERO)
    } else {
        Vec2::ZERO
    };
}

/// Swing the chase camera with the look input while it is held, and drift it back to dead
/// astern when it is let go.
///
/// Reads the same per-frame `MouseMotionDelta` the walking camera does — the one sink the
/// look stick, the mouse and the gamepad's right stick all write — at the same sensitivity,
/// so looking around from the cockpit feels like looking around on foot.
fn read_pilot_look(
    time: Res<Time>,
    controls: Res<AtTheControls>,
    deltas: Query<&MouseMotionDelta>,
    configs: Res<Assets<bad_spaceship_shared::player::Config>>,
    mut look: ResMut<PilotLook>,
) {
    if !controls.0 {
        *look = PilotLook::default();
        return;
    }
    let sensitivity = configs.iter().next().map(|(_, c)| c.look_sensitivity).unwrap_or(0.42);
    let delta = deltas.iter().next().map(|d| d.0).unwrap_or(Vec2::ZERO);
    if delta.length() > LOOK_IDLE {
        look.yaw += delta.x * time.delta_secs() * sensitivity;
        look.pitch = (look.pitch + delta.y * time.delta_secs() * sensitivity)
            .clamp(-MAX_LOOK_PITCH, MAX_LOOK_PITCH);
    } else {
        // Frame-rate-independent exponential drift home.
        let alpha = 1.0 - (-LOOK_RETURN_RATE * time.delta_secs()).exp();
        look.yaw -= look.yaw * alpha;
        look.pitch -= look.pitch * alpha;
    }
}

/// Take the camera out of the avatar's orbit rig while flying, and put it back after.
///
/// The chase pose is a **world** pose, and the camera's rig is parented to a rider whose
/// body tilts to its own felt-up and whose head turns with the look controls — so leaving
/// it mounted would mean cancelling all of that out through a parent transform that is only
/// available one frame stale. Detaching is both simpler and exact. It is also reversible in
/// one line, which matters: the same camera entity lives for the whole app (see
/// `player::spawn_camera`), so it has to go back where it was found.
fn mount_pilot_camera(
    mut commands: Commands,
    controls: Res<AtTheControls>,
    players: Query<(&OrbitingCamera, &PlayerCameraOrbitCenter)>,
    // Whether the camera is currently under a parent. The mount state is *read from the
    // world*, not remembered in a `Local`: a dropped connection despawns the avatar and
    // takes the whole orbit rig with it (see `player::spawn_camera`), and the fresh one
    // re-parents the camera behind this system's back — leaving a remembered flag claiming
    // the camera is detached while it is not, and a world-space pose composing on top of
    // the rig's. Comparing against the truth costs one lookup and cannot desynchronise.
    parented: Query<Has<ChildOf>>,
    autopilot: Res<Autopilot>,
) {
    // The chase pose needs something to chase; without a flown assembly there is nothing to
    // sit behind and the ordinary orbit camera is the right one.
    let want = controls.0 && autopilot.0.is_some();
    let Some((camera, orbit)) = players.iter().next() else {
        return;
    };
    let Ok(has_parent) = parented.get(camera.0) else {
        return;
    };
    // Chasing means detached, and not chasing means back under the rig — so the camera is
    // already where it belongs exactly when `has_parent == !want`.
    if has_parent != want {
        return;
    }
    if want {
        commands.entity(camera.0).remove::<ChildOf>();
    } else {
        commands.entity(orbit.0).add_children(&[camera.0]);
        // Back under the rig, the camera's transform is LOCAL again — and the chase pose
        // left a world rotation in it, which would compose with the rig's look basis and
        // leave the view cocked at whatever angle the flight ended on. `zoom_camera` writes
        // the translation every frame; the rotation belongs to the rig, so hand it back a
        // clean identity.
        commands.entity(camera.0).insert(Transform::IDENTITY);
    }
}

/// Camera distance behind the craft, as a multiple of the craft's own radius — so a
/// three-block hopper and a forty-part tower are both framed without a per-save number...
const CHASE_DISTANCE: f32 = 5.0;
/// ...within these bounds, because "multiples of the radius" degenerates at both ends: a
/// lone engine would put the camera inside its own exhaust, and a sprawling build would put
/// it a hundred metres back where an incoming rock is a speck.
const MIN_CHASE_DISTANCE: f32 = 32.0;
const MAX_CHASE_DISTANCE: f32 = 90.0;

/// How far the camera is lifted off the craft's flight axis — **as an angle**, not a
/// distance.
///
/// This is the "slight lift so they can see over it", and it has to be angular because what
/// it must clear is a *cone*: the exhaust plume streams straight back along the very axis
/// the camera sits behind, and it grows with the stack. A lift expressed in metres (or in
/// craft radii) clears the plume for one build and sits inside it for the next — measured
/// directly, at 1.4 radii of lift a four-engine ascent filled the entire frame with flame
/// and the craft was invisible. An angle is scale-free, so one number holds for every
/// build.
const CHASE_ELEVATION_DEG: f32 = 34.0;

/// How far below the centre of the view the craft rides. This is the other half of seeing
/// over it: with the craft in the lower part of the frame, the rest is the sky ahead —
/// which is where the rocks come from and the only thing worth aiming at. Kept well inside
/// the half-FOV so the stack never slides off the bottom edge.
const CRAFT_BELOW_CENTRE_DEG: f32 = 12.0;

/// Rate (per second) the camera basis eases toward the live flight frame.
///
/// The frame itself is not smooth and shouldn't be: it is defined off the velocity
/// direction, which changes hard at the moment a rock lands and switches definition
/// entirely at [`TURN_SPEED`](bad_spaceship_shared::guidance::TURN_SPEED) as the ascent
/// leaves the pad. Steering must see that immediately — it is the actual flight direction —
/// but a camera that did would snap. The ease is on the *view* only.
const CAMERA_EASE_RATE: f32 = 4.0;

/// Sit the camera behind the craft, lifted, rolled to the planet.
fn drive_pilot_camera(
    time: Res<Time>,
    controls: Res<AtTheControls>,
    autopilot: Res<Autopilot>,
    look: Res<PilotLook>,
    mut eased: Local<Option<(Vec3, Vec3)>>,
    // The player's own camera by handle, NOT `Query<&mut Transform, With<Camera>>`: the
    // outline pass runs a second `Camera3d` (`outline::MaskCam`), so a `single_mut()` over
    // all cameras never matches and this would silently do nothing. `OrbitingCamera` is the
    // same handle `zoom_camera` writes through. The mask camera copies the main one's pose
    // itself, so it follows the chase view for free.
    players: Query<&OrbitingCamera>,
    bodies: Query<&Transform, Without<Camera>>,
    mut cameras: Query<&mut Transform, With<Camera>>,
) {
    let Some(snap) = autopilot.0.as_ref().filter(|_| controls.0) else {
        *eased = None;
        return;
    };
    let look = &*look;
    let Some(camera) = players.iter().next() else {
        return;
    };
    let Ok(mut camera) = cameras.get_mut(camera.0) else {
        return;
    };
    // The craft where it is actually *drawn*. The snapshot's position is the raw fixed-step
    // sample; predicted bodies are frame-interpolated after it, so following the raw value
    // saws the whole view against the rocket by a fraction of a tick of local motion — the
    // same correction, for the same reason, as the trajectory line's `follow_rendered_pose`.
    let local_com = (snap.true_pos - snap.frame_offset).as_vec3();
    let drawn = snap
        .anchor
        .and_then(|(body, raw)| bodies.get(body).ok().map(|drawn| drawn.translation - raw))
        .unwrap_or(Vec3::ZERO);
    let target = local_com + drawn;

    let (forward, _, up) = flight_frame(snap.true_pos.as_vec3(), snap.true_vel);
    let (forward, up) = match *eased {
        Some((prev_forward, prev_up)) => {
            let alpha = 1.0 - (-CAMERA_EASE_RATE * time.delta_secs()).exp();
            let forward = prev_forward.lerp(forward, alpha).normalize_or(forward);
            // Re-orthogonalise rather than easing `up` freely: the two must stay
            // perpendicular or the view shears.
            let up = prev_up.lerp(up, alpha);
            let up = (up - forward * forward.dot(up)).try_normalize().unwrap_or(up);
            (forward, up)
        }
        None => (forward, up),
    };
    *eased = Some((forward, up));

    // Behind and above, on a sphere of `distance` around the craft: `CHASE_ELEVATION_DEG`
    // off the flight axis, which is what keeps the lens out of the plume at any scale.
    let distance = (snap.radius * CHASE_DISTANCE).clamp(MIN_CHASE_DISTANCE, MAX_CHASE_DISTANCE);
    let elevation = CHASE_ELEVATION_DEG.to_radians();
    let right = forward.cross(up).normalize_or(Vec3::X);
    let astern = -forward * (distance * elevation.cos()) + up * (distance * elevation.sin());
    // The pilot's glance, as an orbit of that standing offset — so looking around never
    // moves the camera off its sphere, only around it, and letting go returns to exactly
    // the pose it left. Both signs are negated against the raw look input for the same
    // reason an orbit camera always is: to pan the *view* right the *eye* must swing left.
    let yawed = Quat::from_axis_angle(up, -look.yaw);
    let orbit = Quat::from_axis_angle(yawed * right, -look.pitch) * yawed;
    let eye = target + orbit * astern;
    // Aim by tilting up off the craft rather than at a point some distance ahead of it: the
    // craft then sits at a *fixed* angle below the crosshair whatever the chase distance,
    // where a fixed lead point swings it from centred to off the bottom edge as the
    // distance changes with the build.
    let to_craft = (target - eye).normalize_or(forward);
    let view_right = to_craft.cross(up).normalize_or(right);
    let aim = Quat::from_axis_angle(view_right, CRAFT_BELOW_CENTRE_DEG.to_radians()) * to_craft;
    *camera = Transform::from_translation(eye).looking_to(aim, up);
}
