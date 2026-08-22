//! Flying the rocket: the pilot's stick, and the chase camera it is flown from.
//!
//! Whoever presses Launch flies the ascent (`NetLaunch::pilot`). From blastoff until the
//! room stops flying, that player's view snaps to a chase camera behind the craft and the
//! whole screen becomes a joystick: touch anywhere and a stick appears under your thumb,
//! push it, and the flight bends. Let go and the autopilot has it back on the next tick.
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
use bad_spaceship_shared::{DirectionalInput, OrbitingCamera, PlayerCameraOrbitCenter};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{egui, EguiContexts};
use lightyear::prelude::{Connected, LocalId};

use crate::launch::{Autopilot, LaunchLocal};
use crate::net::my_netcode_id;
use crate::ui::EguiDrawSystems;

pub struct PilotPlugin;

impl Plugin for PilotPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AtTheControls>()
            .init_resource::<PilotStick>()
            .add_systems(
                Update,
                (
                    take_the_controls,
                    read_pilot_stick,
                    // Mount/unmount is a structural change (the camera leaves and rejoins
                    // the avatar's orbit rig), so it follows the state it reacts to.
                    mount_pilot_camera,
                )
                    .chain(),
            )
            .add_systems(
                bevy_egui::EguiPrimaryContextPass,
                draw_pilot_stick.in_set(EguiDrawSystems),
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

/// The pilot's stick this frame.
///
/// `value` is what flies: `[right, up]` on the screen, clamped to the unit disc, in the
/// planet-relative [`flight_frame`]. `origin`/`knob` exist only to draw it.
#[derive(Resource, Default)]
pub struct PilotStick {
    /// Where the finger went down (window-logical pixels), which is where the stick is
    /// drawn. `None` when no finger is on the screen — including when the deflection is
    /// coming from the keyboard or a gamepad instead, which have nowhere to draw.
    origin: Option<Vec2>,
    /// Where the finger is now, for the knob.
    knob: Vec2,
    /// The deflection the burn flies.
    pub value: Vec2,
}

/// How far the finger must travel from where it landed for full deflection (window-logical
/// pixels, scaled by the short edge so a phone and a desktop feel alike).
///
/// Generous on purpose: this stick asks for a *lean*, not a target, so the useful precision
/// is in the first half of the travel. A tight radius makes every rock dodge an
/// over-correction.
const STICK_TRAVEL_FRACTION: f32 = 0.22;
const MIN_STICK_TRAVEL: f32 = 70.0;

/// Full-deflection travel for this window. Read by the reader and the painter alike, so the
/// knob can't sit somewhere the deflection doesn't agree with.
fn stick_travel(window: &Window) -> f32 {
    (window.width().min(window.height()) * STICK_TRAVEL_FRACTION).max(MIN_STICK_TRAVEL)
}

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

/// Read the stick: a floating touch joystick planted wherever the finger landed, or —
/// when nothing is touching the screen — the movement intent, which is already fed by the
/// keyboard and the gamepad and is otherwise idle while welded to a flying deck.
///
/// Reusing the movement intent for the fallback is deliberate. A locked rider cannot walk,
/// so `W`/`A`/`S`/`D` and the left gamepad stick have nothing else to mean during a flight;
/// routing them here gets desktop and controller steering with no second input path to keep
/// in sync, and no key bindings to invent.
fn read_pilot_stick(
    controls: Res<AtTheControls>,
    touches: Res<Touches>,
    windows: Query<&Window, With<PrimaryWindow>>,
    movement: Query<&DirectionalInput>,
    mut stick: ResMut<PilotStick>,
) {
    if !controls.0 {
        *stick = PilotStick::default();
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    let travel = stick_travel(window);

    // One finger owns the stick: the first one still down. Anywhere on the screen — this
    // is a cockpit, not a control layout, and the pilot shouldn't have to find a target.
    if let Some(touch) = touches.iter().next() {
        let origin = *stick.origin.get_or_insert(touch.start_position());
        stick.knob = touch.position();
        let offset = stick.knob - origin;
        // Screen y grows downward; the stick's y is up.
        stick.value = Vec2::new(offset.x, -offset.y).clamp_length_max(travel) / travel;
        return;
    }
    stick.origin = None;
    stick.value = movement
        .iter()
        .next()
        .map(|dir| Vec2::new(dir.0.x, dir.0.z).clamp_length_max(1.0))
        .unwrap_or(Vec2::ZERO);
}

/// Draw the stick under the pilot's thumb — a ring at the touch-down point and a knob at
/// the finger. Only when a finger is actually down: a keyboard or gamepad deflection has no
/// screen position, and a stick drawn in the middle of the view for it would be a lie about
/// where the input came from.
fn draw_pilot_stick(
    mut contexts: EguiContexts,
    controls: Res<AtTheControls>,
    stick: Res<PilotStick>,
    windows: Query<&Window, With<PrimaryWindow>>,
) -> Result {
    let (Some(origin), true) = (stick.origin, controls.0) else {
        return Ok(());
    };
    let Ok(window) = windows.single() else {
        return Ok(());
    };
    let travel = stick_travel(window);
    let ctx = contexts.ctx_mut()?;
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("pilot-stick"),
    ));
    let centre = egui::pos2(origin.x, origin.y);
    let knob = centre + egui::vec2(stick.value.x * travel, -stick.value.y * travel);
    painter.circle_stroke(centre, travel, egui::Stroke::new(2.0, egui::Color32::from_white_alpha(70)));
    painter.circle_filled(knob, travel * 0.28, egui::Color32::from_white_alpha(40));
    painter.circle_stroke(
        knob,
        travel * 0.28,
        egui::Stroke::new(2.0, egui::Color32::from_white_alpha(150)),
    );
    Ok(())
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
    let eye = target - forward * (distance * elevation.cos()) + up * (distance * elevation.sin());
    // Aim by tilting up off the craft rather than at a point some distance ahead of it: the
    // craft then sits at a *fixed* angle below the crosshair whatever the chase distance,
    // where a fixed lead point swings it from centred to off the bottom edge as the
    // distance changes with the build.
    let right = forward.cross(up).normalize_or(Vec3::X);
    let to_craft = (target - eye).normalize_or(forward);
    let look = Quat::from_axis_angle(right, CRAFT_BELOW_CENTRE_DEG.to_radians()) * to_craft;
    *camera = Transform::from_translation(eye).looking_to(look, up);
}
