//! Touch-screen controls for phones/tablets (primarily the web build).
//!
//! The game's input is fully platform-agnostic: every control writes into a
//! component or message on the player entity that `shared/` consumes — movement →
//! `KeyboardDirectionalInput`, look → `MouseMotionDelta`, pick-up/drop/attach/delete →
//! `PlayerClick`, the rotate/delete modifier → `Modifying`. This module feeds
//! those exact sinks from
//! `bevy::input::touch::Touches`. winit 0.30 delivers touch natively on web (the
//! same path keyboard/mouse take since the hand-rolled DOM input layer was
//! removed), so no browser glue is needed and it compiles on native too.
//!
//! Layout: two joysticks anchored in the bottom corners (move left, look right).
//! Each floats to the touch-down point — its deflection is measured from where the
//! thumb lands, not the fixed center — and shows a faint home ring when idle. Move
//! feeds the analog response curve; look is rate-control (cumulative) on both axes
//! (yaw + pitch). While moving and not actively looking, the pitch auto-levels back
//! toward the default. A vertical stack of three buttons sits centered between the
//! sticks, anchored at the bottom: jump (bottom),
//! grab (DROP/GRAB), and the action button on top (Join Parts when holding / Delete
//! Joints when empty-handed). A small pause sits top-right. Rotating a held part is
//! done by dragging the free area (no rotate button); the delete zone is always
//! live when empty-handed. `apply_pointer` derives the `Modifying` flag from hold
//! state + per-tap intent, so one `PlayerClick` routes to pickup/drop/attach/delete
//! without a toggle.
//!
//! Activation is auto-detected: `MobileActive` flips on the first touch, after
//! which the on-screen overlay and the touch systems turn on. Desktop/mouse users
//! never see it. Two desktop assumptions don't hold on touch and are handled here:
//! there is no pointer lock (so `web.rs`'s pointer-lock menu toggle is disabled in
//! mobile mode — see `web.rs`) and no mouse click to enter the game (so the first
//! tap drives `Initial → InGame`, and an on-screen Pause button drives the menu).
//!
//! Coordinates: Bevy reports touch positions in window *logical* pixels (top-left
//! origin), the same space `Window::width()/height()` report and — at egui zoom
//! factor 1.0, which is always the case on touch since Ctrl +/- can't be pressed —
//! the same space egui points use. So one layout in logical pixels serves both
//! hit-testing (`classify_touches`) and drawing (`draw_overlay`).

use crate::input::{get_look, get_modifying, process_keyboard_input};
use crate::AppState;
use bad_spaceship_shared::{
    Holding, InputEvents, KeyboardDirectionalInput, LookPitch, Modifying, MouseMotionDelta,
    PlayerClick, UpdateJointsLabel, INITIAL_CAMERA_PITCH,
};
use bevy::{input::touch::Touches, prelude::*, window::PrimaryWindow};
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};

/// Full-deflection look speed (both axes), as a per-frame look delta
/// (`MouseMotionDelta`). With `look_sensitivity = 0.42` (player.player.ron) the look
/// integrator turns at `delta * sensitivity` rad/s. Tunable.
const LOOK_SPEED: f32 = 4.5;

/// Auto-level rate: while moving and not actively looking, the pitch eases back to
/// the default at this fraction-per-second (exponential approach, ~1.4 s constant).
const PITCH_DRIFT_RATE: f32 = 0.7;

pub struct MobilePlugin;

impl Plugin for MobilePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MobileActive>()
            .init_resource::<TouchControls>()
            .init_resource::<ControlLayout>()
            .add_systems(
                Update,
                (
                    detect_touch,
                    start_game_on_touch.run_if(in_state(AppState::Initial)),
                    compute_layout,
                    classify_touches
                        .after(compute_layout)
                        .run_if(mobile_active)
                        .run_if(in_state(AppState::InGame)),
                    apply_movement
                        .after(classify_touches)
                        .after(process_keyboard_input)
                        .run_if(mobile_active)
                        .run_if(in_state(AppState::InGame)),
                    // Drives the camera-look delta and the modifier directly
                    // (replacing the winit `get_look` path, suppressed on mobile),
                    // and routes a free-area drag into part rotation while holding.
                    // It is the authority on `Modifying`, so it must run *before* the
                    // systems that read it for the click: `toggle_holding` (after
                    // `InputEvents`) and `update_predelete_joints` (in
                    // `UpdateJointsLabel`). Hence `in_set(InputEvents)` +
                    // `before(UpdateJointsLabel)`.
                    apply_pointer
                        .in_set(InputEvents)
                        .before(UpdateJointsLabel)
                        .after(classify_touches)
                        .after(get_look)
                        .after(get_modifying)
                        .run_if(mobile_active)
                        .run_if(in_state(AppState::InGame)),
                ),
            )
            .add_systems(
                EguiPrimaryContextPass,
                draw_overlay
                    .run_if(mobile_active)
                    .run_if(in_state(AppState::InGame)),
            )
            .add_systems(OnExit(AppState::InGame), reset_controls);
    }
}

/// Set to `true` the first time a touch is seen; turns on the overlay + touch
/// systems and (read from `web.rs`) disables the pointer-lock menu toggle.
#[derive(Resource, Default)]
pub struct MobileActive(pub bool);

fn mobile_active(active: Res<MobileActive>) -> bool {
    active.0
}

/// Per-frame touch state: which finger owns which fixed stick/button, the stick
/// knob positions (for drawing), the stick outputs, and the latched modifier.
#[derive(Resource, Default)]
struct TouchControls {
    /// Finger on the movement stick, the touch-down point its deflection is measured
    /// from (relative/floating, not the fixed center), and the knob position.
    move_touch: Option<u64>,
    move_origin: Vec2,
    move_knob: Vec2,
    /// Stick output mapped to the movement basis (x = strafe, z = forward).
    move_dir: Vec3,
    /// Finger on the look stick, its touch-down origin, and knob position.
    look_touch: Option<u64>,
    look_origin: Vec2,
    look_knob: Vec2,
    /// Look stick → per-frame rate delta (both axes), written to `MouseMotionDelta`
    /// (cumulative): x = yaw, y = pitch.
    look_vec: Vec2,
    /// Finger dragging the free area (anywhere not on a stick/button). While a part
    /// is held this trackball-rotates it; otherwise it's inert. Plus its per-frame
    /// delta (fed to the part-rotation path).
    rotate_touch: Option<u64>,
    rotate_delta: Vec2,
    /// Finger held on the jump button.
    jump_touch: Option<u64>,
    /// Per-frame button taps (set in `classify_touches`, consumed by `apply_pointer`
    /// to pick the modifier for that click). grab → grab/drop, action → join/delete.
    grab_tap: bool,
    action_tap: bool,
}

impl TouchControls {
    fn clear_fingers(&mut self) {
        self.move_touch = None;
        self.move_dir = Vec3::ZERO;
        self.look_touch = None;
        self.look_vec = Vec2::ZERO;
        self.rotate_touch = None;
        self.rotate_delta = Vec2::ZERO;
        self.jump_touch = None;
    }
}

/// Fixed-position control geometry in window-logical pixels, recomputed each frame
/// from the window size so the layout tracks rotation/resize. Two fixed joysticks
/// in the bottom corners (move left, look right), a jump button low-center between
/// them, the click + shift buttons centered above them, and a small pause top-right.
#[derive(Resource, Default)]
struct ControlLayout {
    width: f32,
    height: f32,
    btn_r: f32,
    pause_r: f32,
    joystick_radius: f32,
    move_center: Vec2,
    look_center: Vec2,
    /// Vertical center stack (top→bottom): action (join/delete), grab, jump.
    action: Vec2,
    grab: Vec2,
    jump: Vec2,
    pause: Vec2,
}

impl ControlLayout {
    fn recompute(&mut self, w: f32, h: f32) {
        let small = w.min(h);
        let jr = (small * 0.18).clamp(60.0, 130.0);
        let r = (small * 0.07).clamp(30.0, 46.0);
        let edge = 20.0;
        self.width = w;
        self.height = h;
        self.joystick_radius = jr;
        self.btn_r = r;
        self.pause_r = r * 0.7;
        // Fixed sticks in the bottom corners.
        self.move_center = Vec2::new(jr + edge, h - jr - edge);
        self.look_center = Vec2::new(w - jr - edge, h - jr - edge);
        // Vertical stack centered between the two sticks, anchored at the bottom and
        // growing upward: jump at the bottom, grab above it, action (join/delete) on
        // top.
        let cx = w * 0.5;
        let gap = 2.0 * r + 10.0;
        self.jump = Vec2::new(cx, h - r - edge);
        self.grab = Vec2::new(cx, self.jump.y - gap);
        self.action = Vec2::new(cx, self.grab.y - gap);
        self.pause = Vec2::new(w - self.pause_r - 12.0, self.pause_r + 12.0);
    }

    /// Generous circular hit-test (touch target a bit larger than the drawn circle).
    fn hit(&self, center: Vec2, p: Vec2) -> bool {
        p.distance(center) <= self.btn_r + 8.0
    }

    /// Whether a touch falls in a fixed stick's grab zone (a bit larger than the
    /// drawn ring, so the thumb doesn't have to land dead-center).
    fn in_stick(&self, center: Vec2, p: Vec2) -> bool {
        p.distance(center) <= self.joystick_radius * 1.15
    }
}

fn detect_touch(touches: Res<Touches>, mut active: ResMut<MobileActive>) {
    if !active.0 && touches.iter_just_pressed().next().is_some() {
        active.0 = true;
        crate::tlog!("detect_touch: MobileActive on");
    }
}

/// No mouse click exists on touch, so the first tap takes the place of
/// `ui.rs:capture_mouse_on_click` and enters the game.
fn start_game_on_touch(
    touches: Res<Touches>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    if touches.iter_just_pressed().next().is_some() {
        crate::tlog!("start_game_on_touch: first tap -> InGame");
        next_state.set(AppState::InGame);
    }
}

fn compute_layout(
    mut layout: ResMut<ControlLayout>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    layout.recompute(window.width(), window.height());
}

fn reset_controls(mut controls: ResMut<TouchControls>) {
    controls.clear_fingers();
}

/// Assigns newly-pressed fingers to roles, releases roles whose finger lifted,
/// fires the momentary buttons (grab / modify-toggle / pause), and recomputes the
/// joystick vector from the active move finger.
fn classify_touches(
    touches: Res<Touches>,
    layout: Res<ControlLayout>,
    mut controls: ResMut<TouchControls>,
    mut clicks: MessageWriter<PlayerClick>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    // Per-frame button taps, recomputed each frame.
    controls.grab_tap = false;
    controls.action_tap = false;

    // Release any role whose finger is no longer down.
    let is_active = |id: Option<u64>| -> bool {
        id.map(|id| touches.iter().any(|t| t.id() == id))
            .unwrap_or(false)
    };
    if !is_active(controls.move_touch) {
        controls.move_touch = None;
        controls.move_dir = Vec3::ZERO;
    }
    if !is_active(controls.look_touch) {
        controls.look_touch = None;
        controls.look_vec = Vec2::ZERO;
    }
    if !is_active(controls.jump_touch) {
        controls.jump_touch = None;
    }
    if !is_active(controls.rotate_touch) {
        controls.rotate_touch = None;
        controls.rotate_delta = Vec2::ZERO;
    }

    // Assign newly-pressed fingers. Buttons take priority over the stick zones, so
    // a finger landing on a button never grabs a stick.
    for touch in touches.iter_just_pressed() {
        let p = touch.position();
        let id = touch.id();
        if layout.hit(layout.pause, p) {
            next_state.set(AppState::InGameMenu);
            controls.clear_fingers();
            continue;
        }
        if layout.hit(layout.jump, p) {
            controls.jump_touch = Some(id);
            continue;
        }
        // Both action buttons fire a `PlayerClick`; the modifier set by
        // `apply_pointer` (from these tap flags) routes it to grab/drop vs
        // join/delete.
        if layout.hit(layout.grab, p) {
            crate::tlog!("hit grab");
            controls.grab_tap = true;
            clicks.write(PlayerClick);
            continue;
        }
        if layout.hit(layout.action, p) {
            crate::tlog!("hit action");
            controls.action_tap = true;
            clicks.write(PlayerClick);
            continue;
        }
        // Sticks: grab whichever zone the finger lands in (one finger each), and
        // record the touch-down point as the deflection origin (the stick floats to
        // where the thumb lands rather than measuring from the fixed center).
        if controls.move_touch.is_none() && layout.in_stick(layout.move_center, p) {
            controls.move_touch = Some(id);
            controls.move_origin = p;
            controls.move_knob = p;
        } else if controls.look_touch.is_none() && layout.in_stick(layout.look_center, p) {
            controls.look_touch = Some(id);
            controls.look_origin = p;
            controls.look_knob = p;
        } else if controls.rotate_touch.is_none() {
            // Anywhere else (the free area above the controls): a drag here
            // trackball-rotates a held part (see `apply_pointer`).
            controls.rotate_touch = Some(id);
        }
    }

    // Per-frame delta of the free-area finger (incremental trackball drag).
    if let Some(id) = controls.rotate_touch {
        controls.rotate_delta = touches
            .iter()
            .find(|t| t.id() == id)
            .map(|t| t.delta())
            .unwrap_or(Vec2::ZERO);
    }

    // Movement stick: deflection measured from the touch-down origin (not the fixed
    // center) → curved analog speed.
    if let Some(id) = controls.move_touch {
        if let Some(touch) = touches.iter().find(|t| t.id() == id) {
            let clamped =
                (touch.position() - controls.move_origin).clamp_length_max(layout.joystick_radius);
            controls.move_knob = controls.move_origin + clamped;
            let speed = response_curve(clamped.length() / layout.joystick_radius);
            let dir2 = clamped.normalize_or_zero() * speed;
            // Screen y is downward, so forward (+z, "W") is -y; +x ("D") is +x.
            controls.move_dir = Vec3::new(dir2.x, 0.0, -dir2.y);
        }
    }

    // Look stick: both axes are rate-control (cumulative) and feed `MouseMotionDelta`
    // (x = yaw, y = pitch), measured from the touch-down origin. Pitch is inverted
    // (stick up → look down), so the y rate keeps screen-space sign.
    if let Some(id) = controls.look_touch {
        if let Some(touch) = touches.iter().find(|t| t.id() == id) {
            let clamped =
                (touch.position() - controls.look_origin).clamp_length_max(layout.joystick_radius);
            controls.look_knob = controls.look_origin + clamped;
            let norm = clamped / layout.joystick_radius;
            let yaw_rate = response_curve(norm.x.abs()) * norm.x.signum() * LOOK_SPEED;
            let pitch_rate = response_curve(norm.y.abs()) * norm.y.signum() * LOOK_SPEED;
            controls.look_vec = Vec2::new(yaw_rate, pitch_rate);
        }
    }
}

/// Maps raw joystick deflection (0..1) to a movement speed (0..1): a small
/// deadzone kills origin jitter, then a quadratic ramp keeps fine control near
/// center while still reaching full speed at the rim. Tune `DEADZONE`/`EXPO`.
fn response_curve(deflection: f32) -> f32 {
    const DEADZONE: f32 = 0.12;
    const EXPO: f32 = 2.0;
    if deflection <= DEADZONE {
        return 0.0;
    }
    let t = ((deflection - DEADZONE) / (1.0 - DEADZONE)).clamp(0.0, 1.0);
    t.powf(EXPO)
}

/// Adds the joystick direction (and jump) into `KeyboardDirectionalInput`, the same
/// sink `process_keyboard_input` feeds; `combine_directional_inputs` reads and
/// zeroes it each frame. Unlike the WASD path we must *preserve* the sub-unit
/// magnitude (analog speed) the response curve produced, so clamp the length
/// instead of normalizing it to 1.
fn apply_movement(
    controls: Res<TouchControls>,
    mut query: Query<&mut KeyboardDirectionalInput>,
) {
    let mut dir = controls.move_dir;
    if controls.jump_touch.is_some() {
        dir.y += 1.0;
    }
    if dir == Vec3::ZERO {
        return;
    }
    for mut input in query.iter_mut() {
        input.0 = (input.0 + dir).clamp_length_max(1.0);
    }
}

/// Single mobile writer of the look/rotate delta (`MouseMotionDelta`) and the
/// modifier (`Modifying`), replacing the suppressed winit `get_look`/`get_modifying`
/// path. Downstream, `Modifying` selects what the click does and what's shown:
/// `toggle_holding` (holding+modify = attach, holding+!modify = drop, !holding+
/// !modify = pickup), `delete_joints`/`update_predelete_joints` (!holding+modify =
/// delete zone), and `set_part_rotation`/`mouse_motion` (rotate held part vs turn
/// camera). So the modifier is derived from state + the per-tap intent:
/// - grab tap → modifier off  → pickup (empty) / drop (holding)
/// - action tap → modifier on → delete (empty) / attach (holding)
/// - free-area drag while holding → modifier on → trackball-rotate the part
/// - otherwise empty-handed → modifier on → delete zone always visible
/// - otherwise holding → modifier off → look stick turns the camera, drop ready
fn apply_pointer(
    time: Res<Time>,
    controls: Res<TouchControls>,
    holders: Query<&Holding>,
    mut deltas: Query<&mut MouseMotionDelta>,
    mut modifiers: Query<&mut Modifying>,
    mut pitches: Query<&mut LookPitch>,
) {
    let holding = holders.iter().next().map(|h| h.0).unwrap_or(false);
    let rotating = holding && controls.rotate_touch.is_some();
    let delta = if rotating {
        controls.rotate_delta
    } else {
        controls.look_vec
    };
    for mut d in deltas.iter_mut() {
        d.0 = delta;
    }
    // Auto-level: while moving horizontally and not actively looking, ease the pitch
    // back toward the default (so the view recenters as you walk). `mouse_motion`
    // only adds the look delta's y (0 here) on top, so it won't fight this.
    if controls.move_dir != Vec3::ZERO && controls.look_touch.is_none() {
        let t = (PITCH_DRIFT_RATE * time.delta_secs()).min(1.0);
        for mut pitch in pitches.iter_mut() {
            pitch.0 += (INITIAL_CAMERA_PITCH - pitch.0) * t;
        }
    }
    let modify = if controls.grab_tap {
        false
    } else if controls.action_tap {
        true
    } else if rotating {
        true
    } else {
        !holding
    };
    for mut m in modifiers.iter_mut() {
        m.0 = modify;
    }
}

fn draw_button(painter: &egui::Painter, center: Vec2, r: f32, label: &str, active: bool) {
    let c = egui::pos2(center.x, center.y);
    let fill = if active {
        egui::Color32::from_rgba_unmultiplied(80, 140, 220, 150)
    } else {
        egui::Color32::from_black_alpha(90)
    };
    painter.circle(
        c,
        r,
        fill,
        egui::Stroke::new(2.0, egui::Color32::from_white_alpha(170)),
    );
    // Support multi-line labels (e.g. "Delete\nJoints"): size the font to the
    // longest line's width and the line count's height so it fits the circle, then
    // stack the lines centered.
    let lines: Vec<&str> = label.split('\n').collect();
    let n = lines.len().max(1) as f32;
    let longest = lines.iter().map(|l| l.len()).max().unwrap_or(1).max(1) as f32;
    let font = (1.7 * r / longest).min(1.4 * r / n).clamp(9.0, r * 0.62);
    let line_h = font * 1.1;
    for (i, line) in lines.iter().enumerate() {
        let y = c.y - (n - 1.0) * 0.5 * line_h + i as f32 * line_h;
        painter.text(
            egui::pos2(c.x, y),
            egui::Align2::CENTER_CENTER,
            line,
            egui::FontId::proportional(font),
            egui::Color32::WHITE,
        );
    }
}

/// Draws a stick: a ring at `center` (the touch-down origin while held, or the home
/// position when idle) plus a knob at the deflected position, and a label above it.
fn draw_stick(painter: &egui::Painter, center: Vec2, radius: f32, knob: Vec2, label: &str) {
    let active = knob != center;
    let alpha = if active { 95 } else { 55 };
    painter.circle_stroke(
        egui::pos2(center.x, center.y),
        radius,
        egui::Stroke::new(2.0, egui::Color32::from_white_alpha(alpha)),
    );
    painter.circle_filled(
        egui::pos2(knob.x, knob.y),
        radius * 0.36,
        egui::Color32::from_white_alpha(alpha + 45),
    );
    painter.text(
        egui::pos2(center.x, center.y - radius - 10.0),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(14.0),
        egui::Color32::from_white_alpha(alpha + 50),
    );
}

fn draw_overlay(
    mut contexts: EguiContexts,
    layout: Res<ControlLayout>,
    controls: Res<TouchControls>,
    holders: Query<&Holding>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("mobile_overlay"),
    ));

    // Sticks float to the touch-down origin while held (their deflection is measured
    // from there); when idle they show a faint ring at the fixed home position.
    let (move_center, move_knob) = if controls.move_touch.is_some() {
        (controls.move_origin, controls.move_knob)
    } else {
        (layout.move_center, layout.move_center)
    };
    let (look_center, look_knob) = if controls.look_touch.is_some() {
        (controls.look_origin, controls.look_knob)
    } else {
        (layout.look_center, layout.look_center)
    };
    draw_stick(&painter, move_center, layout.joystick_radius, move_knob, "MOVE");
    draw_stick(&painter, look_center, layout.joystick_radius, look_knob, "LOOK");

    // Context labels: the top action button joins (holding) or deletes (empty),
    // the grab button drops (holding) or grabs (empty).
    let holding = holders.iter().next().map(|h| h.0).unwrap_or(false);
    let action_label = if holding { "Join\nParts" } else { "Delete\nJoints" };
    let grab_label = if holding { "DROP" } else { "GRAB" };

    let r = layout.btn_r;
    draw_button(&painter, layout.action, r, action_label, false);
    draw_button(&painter, layout.grab, r, grab_label, false);
    draw_button(&painter, layout.jump, r, "JUMP", controls.jump_touch.is_some());
    draw_button(&painter, layout.pause, layout.pause_r, "II", false);

    Ok(())
}
