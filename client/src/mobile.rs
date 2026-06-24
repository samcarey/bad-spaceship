//! Touch-screen controls for phones/tablets (primarily the web build).
//!
//! The game's input is fully platform-agnostic: every control writes into a
//! component or message on the player entity that `shared/` consumes — movement →
//! `KeyboardDirectionalInput`, look → `MouseMotion`, pick-up/drop/attach/delete →
//! `PlayerClick`, the rotate/delete modifier → `Modifying`. This module feeds
//! those exact sinks from
//! `bevy::input::touch::Touches`. winit 0.30 delivers touch natively on web (the
//! same path keyboard/mouse take since the hand-rolled DOM input layer was
//! removed), so no browser glue is needed and it compiles on native too.
//!
//! Layout: two fixed joysticks in the bottom corners — move (left) feeds the
//! analog response curve, look (right) is rate-control (a held deflection keeps
//! turning, emitting `MouseMotion` each frame). A jump button sits low-center
//! between them; the click + shift buttons sit centered above them with
//! context-specific labels (GRAB/DROP/ATTACH/DELETE and ROTATE/DELETE) keyed off
//! the player's hold state and the latched modifier. A small pause sits top-right.
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
use bad_spaceship_shared::{Holding, KeyboardDirectionalInput, Modifying, PlayerClick};
use bevy::{
    input::mouse::MouseMotion,
    input::touch::Touches,
    prelude::*,
    window::PrimaryWindow,
};
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};

/// Full-deflection look speed, as a synthetic per-frame `MouseMotion` delta. With
/// `look_sensitivity = 0.42` (player.player.ron) the look integrator turns at
/// `delta * sensitivity` rad/s, so ~7 → ~2.9 rad/s (~165°/s) at the rim. Tunable.
const LOOK_SPEED: f32 = 7.0;

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
                    apply_look
                        .after(classify_touches)
                        .before(get_look)
                        .run_if(mobile_active)
                        .run_if(in_state(AppState::InGame)),
                    apply_modify
                        .after(classify_touches)
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
    /// Finger on the (fixed, bottom-left) movement stick + its knob position.
    move_touch: Option<u64>,
    move_knob: Vec2,
    /// Stick output mapped to the movement basis (x = strafe, z = forward).
    move_dir: Vec3,
    /// Finger on the (fixed, bottom-right) look stick + its knob position.
    look_touch: Option<u64>,
    look_knob: Vec2,
    /// Per-frame synthetic `MouseMotion` delta the look stick emits (rate control).
    look_vec: Vec2,
    /// Finger held on the jump button.
    jump_touch: Option<u64>,
    /// Latched rotate/delete modifier (Shift equivalent), toggled by its button.
    modify_on: bool,
}

impl TouchControls {
    fn clear_fingers(&mut self) {
        self.move_touch = None;
        self.move_dir = Vec3::ZERO;
        self.look_touch = None;
        self.look_vec = Vec2::ZERO;
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
    jump: Vec2,
    click: Vec2,
    shift: Vec2,
    pause: Vec2,
}

impl ControlLayout {
    fn recompute(&mut self, w: f32, h: f32) {
        let small = w.min(h);
        let jr = (small * 0.18).clamp(60.0, 130.0);
        let r = (small * 0.075).clamp(30.0, 56.0);
        let edge = 20.0;
        self.width = w;
        self.height = h;
        self.joystick_radius = jr;
        self.btn_r = r;
        self.pause_r = r * 0.7;
        // Fixed sticks in the bottom corners.
        self.move_center = Vec2::new(jr + edge, h - jr - edge);
        self.look_center = Vec2::new(w - jr - edge, h - jr - edge);
        // Jump low-center, between the sticks.
        self.jump = Vec2::new(w * 0.5, h - r - edge);
        // Click + shift centered above the sticks, flanking the centerline.
        let pair_dx = r + 14.0;
        let cluster_y = (self.move_center.y - jr - r - 12.0).max(r + edge);
        self.click = Vec2::new(w * 0.5 - pair_dx, cluster_y);
        self.shift = Vec2::new(w * 0.5 + pair_dx, cluster_y);
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
        if layout.hit(layout.click, p) {
            crate::tlog!("hit click");
            clicks.write(PlayerClick);
            continue;
        }
        if layout.hit(layout.shift, p) {
            controls.modify_on = !controls.modify_on;
            crate::tlog!("hit shift -> {}", controls.modify_on);
            continue;
        }
        // Fixed sticks: grab whichever zone the finger lands in (one finger each).
        if controls.move_touch.is_none() && layout.in_stick(layout.move_center, p) {
            controls.move_touch = Some(id);
        } else if controls.look_touch.is_none() && layout.in_stick(layout.look_center, p) {
            controls.look_touch = Some(id);
        }
    }

    // Movement stick: deflection from the fixed center → curved analog speed.
    if let Some(id) = controls.move_touch {
        if let Some(touch) = touches.iter().find(|t| t.id() == id) {
            let clamped =
                (touch.position() - layout.move_center).clamp_length_max(layout.joystick_radius);
            controls.move_knob = layout.move_center + clamped;
            let speed = response_curve(clamped.length() / layout.joystick_radius);
            let dir2 = clamped.normalize_or_zero() * speed;
            // Screen y is downward, so forward (+z, "W") is -y; +x ("D") is +x.
            controls.move_dir = Vec3::new(dir2.x, 0.0, -dir2.y);
        }
    }

    // Look stick: deflection from the fixed center → rate-control look. The
    // deflection is in screen space (down = +y), which is exactly the convention
    // the drag-look path used, so it feeds `MouseMotion` with no sign juggling.
    if let Some(id) = controls.look_touch {
        if let Some(touch) = touches.iter().find(|t| t.id() == id) {
            let clamped =
                (touch.position() - layout.look_center).clamp_length_max(layout.joystick_radius);
            controls.look_knob = layout.look_center + clamped;
            let rate = response_curve(clamped.length() / layout.joystick_radius) * LOOK_SPEED;
            controls.look_vec = clamped.normalize_or_zero() * rate;
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

/// Emits the look stick's rate vector as a synthetic `MouseMotion` each frame; the
/// existing `get_look` consumes it into `MouseMotionDelta` (which also drives
/// held-part rotation while the modifier is on). Unlike drag-look, the stick keeps
/// turning while held at deflection (rate control), not only while the finger moves.
fn apply_look(controls: Res<TouchControls>, mut motion: MessageWriter<MouseMotion>) {
    if controls.look_vec != Vec2::ZERO {
        motion.write(MouseMotion {
            delta: controls.look_vec,
        });
    }
}

/// ORs the latched modifier toggle into `Modifying` after `get_modifying` (which
/// reads the absent Shift key and clears it every frame).
fn apply_modify(controls: Res<TouchControls>, mut players: Query<&mut Modifying>) {
    if controls.modify_on {
        for mut modifying in players.iter_mut() {
            modifying.0 = true;
        }
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
    // Shrink the font for longer labels so words like "ATTACH"/"ROTATE" fit inside
    // the circle (roughly fit the label width to the diameter).
    let font = (1.7 * r / label.len().max(1) as f32).clamp(11.0, r * 0.62);
    painter.text(
        c,
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(font),
        egui::Color32::WHITE,
    );
}

/// Draws a fixed stick: an always-visible ring at `center` plus a knob (at the
/// deflected position when held, recentered when idle) and a label above it.
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

    // Fixed sticks (knob recenters when idle).
    let move_knob = if controls.move_touch.is_some() {
        controls.move_knob
    } else {
        layout.move_center
    };
    let look_knob = if controls.look_touch.is_some() {
        controls.look_knob
    } else {
        layout.look_center
    };
    draw_stick(&painter, layout.move_center, layout.joystick_radius, move_knob, "MOVE");
    draw_stick(&painter, layout.look_center, layout.joystick_radius, look_knob, "LOOK");

    // Context-specific labels for the click + shift buttons. `holding` and the
    // latched `modify_on` select what a click does and what shift toggles into.
    let holding = holders.iter().next().map(|h| h.0).unwrap_or(false);
    let modifying = controls.modify_on;
    let click_label = match (holding, modifying) {
        (true, true) => "ATTACH",
        (true, false) => "DROP",
        (false, true) => "DELETE",
        (false, false) => "GRAB",
    };
    let shift_label = if holding { "ROTATE" } else { "DELETE" };

    let r = layout.btn_r;
    draw_button(&painter, layout.jump, r, "JUMP", controls.jump_touch.is_some());
    draw_button(&painter, layout.click, r, click_label, false);
    draw_button(&painter, layout.shift, r, shift_label, modifying);
    draw_button(&painter, layout.pause, layout.pause_r, "II", false);

    Ok(())
}
