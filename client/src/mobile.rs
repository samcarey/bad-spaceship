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
use bad_spaceship_shared::{KeyboardDirectionalInput, Modifying, PlayerClick};
use bevy::{
    input::mouse::MouseMotion,
    input::touch::Touches,
    prelude::*,
    window::PrimaryWindow,
};
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};

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

/// Per-frame touch state: which finger owns which role, the joystick geometry,
/// and the latched modifier toggle.
#[derive(Resource, Default)]
struct TouchControls {
    /// Finger driving the movement joystick, plus its origin (touch-down point)
    /// and current knob position (origin + clamped offset, for drawing).
    move_touch: Option<u64>,
    move_origin: Vec2,
    move_knob: Vec2,
    /// Joystick output mapped to the movement basis (x = strafe, z = forward).
    move_dir: Vec3,
    /// Finger driving look (right-half drag).
    look_touch: Option<u64>,
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
        self.jump_touch = None;
    }
}

/// Button centers + radii in window-logical pixels, recomputed each frame from the
/// window size so the layout tracks rotation/resize.
#[derive(Resource, Default)]
struct ControlLayout {
    width: f32,
    height: f32,
    btn_r: f32,
    pause_r: f32,
    joystick_radius: f32,
    jump: Vec2,
    grab: Vec2,
    modify: Vec2,
    pause: Vec2,
}

impl ControlLayout {
    fn recompute(&mut self, w: f32, h: f32) {
        let small = w.min(h);
        let r = (small * 0.07).clamp(28.0, 56.0);
        let gap = 2.0 * r + 18.0;
        self.width = w;
        self.height = h;
        self.btn_r = r;
        self.pause_r = r * 0.7;
        self.joystick_radius = (small * 0.18).clamp(60.0, 140.0);
        let margin = r + 16.0;
        // Action cluster anchored bottom-right (thumb reach), Pause top-right.
        self.jump = Vec2::new(w - margin, h - margin);
        self.grab = Vec2::new(self.jump.x - gap, self.jump.y);
        self.modify = Vec2::new(self.jump.x, self.jump.y - gap);
        self.pause = Vec2::new(w - self.pause_r - 12.0, self.pause_r + 12.0);
    }

    /// Generous circular hit-test (touch target a bit larger than the drawn circle).
    fn hit(&self, center: Vec2, p: Vec2) -> bool {
        p.distance(center) <= self.btn_r + 8.0
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
    }
    if !is_active(controls.jump_touch) {
        controls.jump_touch = None;
    }

    // Assign newly-pressed fingers. Button hits take priority over the
    // move/look zones, so a finger landing on a corner button never moves or looks.
    for touch in touches.iter_just_pressed() {
        let p = touch.position();
        let id = touch.id();
        crate::tlog!(
            "press id={id} pos=({:.0},{:.0}) win=({:.0}x{:.0})",
            p.x,
            p.y,
            layout.width,
            layout.height
        );
        if layout.hit(layout.pause, p) {
            crate::tlog!("hit pause");
            next_state.set(AppState::InGameMenu);
            controls.clear_fingers();
            continue;
        }
        if layout.hit(layout.jump, p) {
            crate::tlog!("hit jump");
            controls.jump_touch = Some(id);
            continue;
        }
        if layout.hit(layout.grab, p) {
            crate::tlog!("hit grab");
            clicks.write(PlayerClick);
            continue;
        }
        if layout.hit(layout.modify, p) {
            crate::tlog!("hit modify");
            controls.modify_on = !controls.modify_on;
            continue;
        }
        // Left half = movement joystick (originating where the finger lands),
        // right half = look drag. One finger per role.
        if p.x < layout.width * 0.5 {
            if controls.move_touch.is_none() {
                crate::tlog!("move start");
                controls.move_touch = Some(id);
                controls.move_origin = p;
                controls.move_knob = p;
                controls.move_dir = Vec3::ZERO;
            }
        } else if controls.look_touch.is_none() {
            crate::tlog!("look start");
            controls.look_touch = Some(id);
        }
    }

    // Recompute the joystick vector from the active move finger.
    if let Some(id) = controls.move_touch {
        if let Some(touch) = touches.iter().find(|t| t.id() == id) {
            let offset = touch.position() - controls.move_origin;
            let clamped = offset.clamp_length_max(layout.joystick_radius);
            controls.move_knob = controls.move_origin + clamped;
            let norm = clamped / layout.joystick_radius;
            // Screen y is downward, so forward (+z, "W") is -y; +x ("D") is +x.
            let mut v = Vec3::new(norm.x, 0.0, -norm.y);
            if v.length() < 0.15 {
                v = Vec3::ZERO; // deadzone to stop jitter near the origin
            }
            controls.move_dir = v;
        }
    }
}

/// Adds the joystick direction (and jump) into `KeyboardDirectionalInput` the same
/// way `process_keyboard_input` adds the WASD/Space direction; `combine_directional_inputs`
/// reads and zeroes it each frame.
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
        input.0 = (input.0 + dir).normalize_or_zero();
    }
}

/// Emits a synthetic `MouseMotion` from the look finger's per-frame delta; the
/// existing `get_look` consumes it into `MouseMotionDelta` (which also drives
/// held-part rotation while the modifier is on).
fn apply_look(
    controls: Res<TouchControls>,
    touches: Res<Touches>,
    mut motion: MessageWriter<MouseMotion>,
) {
    if let Some(id) = controls.look_touch {
        if let Some(touch) = touches.iter().find(|t| t.id() == id) {
            let delta = touch.delta();
            if delta != Vec2::ZERO {
                motion.write(MouseMotion { delta });
            }
        }
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
    painter.text(
        c,
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(r * 0.62),
        egui::Color32::WHITE,
    );
}

fn draw_overlay(
    mut contexts: EguiContexts,
    layout: Res<ControlLayout>,
    controls: Res<TouchControls>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("mobile_overlay"),
    ));

    // Movement joystick: only visible while a finger is on it.
    if controls.move_touch.is_some() {
        let base = egui::pos2(controls.move_origin.x, controls.move_origin.y);
        painter.circle_stroke(
            base,
            layout.joystick_radius,
            egui::Stroke::new(2.0, egui::Color32::from_white_alpha(90)),
        );
        let knob = egui::pos2(controls.move_knob.x, controls.move_knob.y);
        painter.circle_filled(
            knob,
            layout.joystick_radius * 0.36,
            egui::Color32::from_white_alpha(130),
        );
    }

    let r = layout.btn_r;
    draw_button(&painter, layout.jump, r, "JMP", controls.jump_touch.is_some());
    draw_button(&painter, layout.grab, r, "GRAB", false);
    draw_button(&painter, layout.modify, r, "ROT", controls.modify_on);
    draw_button(&painter, layout.pause, layout.pause_r, "II", false);

    Ok(())
}
