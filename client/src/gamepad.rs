//! PS5 / standard gamepad controls.
//!
//! Like `mobile.rs`, this feeds the platform-agnostic input sinks the rest of the
//! game consumes — movement → `GameStickDirectionalInput`, look → `MouseMotionDelta`,
//! the rotate/delete modifier → `Modifying`, pick-up/drop/attach/delete →
//! `PlayerClick`. Bevy reads gamepads through `bevy_gilrs`, which has a `web-sys`
//! backend over the browser Gamepad API, so this works on the wasm build too (the
//! `web` feature enables `bevy/bevy_gilrs`). That's what lets a controller paired to
//! a phone — a DualSense on an iPhone, say — drive the whole game with no keyboard,
//! mouse, or touch.
//!
//! Mapping (DualSense labels in brackets), chosen to mirror the desktop mouse+Shift
//! scheme so the two stay in sync:
//!   - left stick           → move (analog, with a radial dead zone)
//!   - right stick          → look (camera), or trackball-rotate a held part while
//!                            the modifier is held (the downstream `set_part_rotation`
//!                            / `mouse_motion` route on `Modifying`, same as the mouse)
//!   - South [✕]            → jump
//!   - right trigger [R1/R2] → click (`PlayerClick`): pickup / drop / attach / delete
//!   - left trigger  [L1/L2] → modifier (`Modifying`), like holding Shift
//!   - Start [Options]      → toggle the pause menu (and any button starts the game)
//!
//! Two desktop assumptions break for a controller-only session and are handled the
//! same way `mobile.rs` handles them for touch: there's no mouse click to enter the
//! game (so any button drives `Initial → InGame`) and — on iOS web — no pointer lock
//! (so `web.rs` reads `GamepadActive` to skip the throwing `request_pointer_lock`
//! and the pointer-lock menu toggle; the Start button drives the menu instead).

use crate::input::{get_look, get_modifying, process_keyboard_input};
use crate::AppState;
use bad_spaceship_shared::{
    GameStickDirectionalInput, InputEvents, Modifying, MouseMotionDelta, PlayerClick,
    UpdateJointsLabel,
};
use bevy::prelude::*;

/// Full-deflection right-stick look speed, as a per-frame `MouseMotionDelta` rate
/// (mirrors `mobile::LOOK_SPEED`). With `look_sensitivity = 0.42` (player.player.ron)
/// the camera turns at `rate * sensitivity` rad/s — so ~`9 * 0.42 ≈ 3.8` rad/s
/// (~216°/s) at full deflection. Tunable.
const LOOK_SPEED: f32 = 9.0;

/// Radial dead zone applied to both analog sticks: below this the stick reads zero
/// (kills resting drift), above it the remaining range is rescaled back to 0..1 so
/// fine control near the edge of the dead zone isn't lost.
const STICK_DEADZONE: f32 = 0.12;

/// Buttons that count as "a button" for starting the game / detecting that a
/// controller is in use (the face buttons, the menu buttons, and the shoulders).
const ACTIVATION_BUTTONS: [GamepadButton; 8] = [
    GamepadButton::South,
    GamepadButton::East,
    GamepadButton::West,
    GamepadButton::North,
    GamepadButton::Start,
    GamepadButton::Select,
    GamepadButton::LeftTrigger,
    GamepadButton::RightTrigger,
];

pub struct GamepadPlugin;

impl Plugin for GamepadPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GamepadActive>().add_systems(
            Update,
            (
                detect_gamepad,
                start_game_on_button.run_if(in_state(AppState::Initial)),
                toggle_menu_on_start,
                // Writes its own dedicated `GameStickDirectionalInput` sink, so it
                // only needs to land after the keyboard writer for tidy ordering.
                gamepad_movement
                    .in_set(InputEvents)
                    .after(process_keyboard_input),
                // Authority on the gamepad's look + modifier + click. It must run
                // *after* the winit `get_look`/`get_modifying` writers (it composes on
                // top of them rather than being clobbered) and *before* the systems
                // that read `Modifying` for the click — `toggle_holding` (after
                // `InputEvents`) and `update_predelete_joints` (in `UpdateJointsLabel`)
                // — exactly the ordering `mobile::apply_pointer` uses.
                gamepad_pointer
                    .in_set(InputEvents)
                    .before(UpdateJointsLabel)
                    .after(get_look)
                    .after(get_modifying)
                    .run_if(in_state(AppState::InGame)),
            ),
        );
    }
}

/// Set `true` the first time a controller is touched. Read by `web.rs` to disable
/// the iOS-incompatible pointer-lock path for a controller-only session (mirrors
/// `MobileActive`'s role for touch).
#[derive(Resource, Default)]
pub struct GamepadActive(pub bool);

/// Radial dead zone with edge rescaling: zero inside the dead zone, then the
/// remaining magnitude is stretched back to 0..1 so motion starts smoothly from 0.
fn deadzone(v: f32) -> f32 {
    if v.abs() <= STICK_DEADZONE {
        0.0
    } else {
        v.signum() * (v.abs() - STICK_DEADZONE) / (1.0 - STICK_DEADZONE)
    }
}

/// Whether any controller is currently giving "meaningful" input (a mapped button
/// just pressed, or the left stick pushed past the dead zone).
fn any_activation(gamepads: &Query<&Gamepad>) -> bool {
    gamepads.iter().any(|gamepad| {
        ACTIVATION_BUTTONS.iter().any(|b| gamepad.just_pressed(*b))
            || gamepad
                .get(GamepadAxis::LeftStickX)
                .is_some_and(|v| v.abs() > STICK_DEADZONE)
            || gamepad
                .get(GamepadAxis::LeftStickY)
                .is_some_and(|v| v.abs() > STICK_DEADZONE)
    })
}

fn detect_gamepad(gamepads: Query<&Gamepad>, mut active: ResMut<GamepadActive>) {
    if !active.0 && any_activation(&gamepads) {
        active.0 = true;
    }
}

/// No mouse click exists for a controller-only player, so any button takes the place
/// of `ui.rs:capture_mouse_on_click` and enters the game (like `mobile.rs`'s first
/// tap).
fn start_game_on_button(gamepads: Query<&Gamepad>, mut next_state: ResMut<NextState<AppState>>) {
    if gamepads
        .iter()
        .any(|g| ACTIVATION_BUTTONS.iter().any(|b| g.just_pressed(*b)))
    {
        next_state.set(AppState::InGame);
    }
}

/// Start [Options] toggles the pause menu both ways (the controller player has no
/// pointer to click the menu's resume button, and on iOS web the pointer-lock toggle
/// in `web.rs` is disabled for them — see `GamepadActive`).
fn toggle_menu_on_start(
    gamepads: Query<&Gamepad>,
    active: Res<GamepadActive>,
    state: Res<State<AppState>>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    if !active.0 {
        return;
    }
    if gamepads.iter().any(|g| g.just_pressed(GamepadButton::Start)) {
        match *state.get() {
            AppState::InGame => next_state.set(AppState::InGameMenu),
            AppState::InGameMenu => next_state.set(AppState::InGame),
            _ => {}
        }
    }
}

/// Left stick → analog movement (clamped, not normalized, so sub-unit deflection
/// gives variable speed — `combine_directional_inputs` clamps the keyboard+stick
/// sum); South → jump. Always runs (zeroing the sink outside `InGame`) so a stick
/// left deflected on pause doesn't drift the character.
fn gamepad_movement(
    gamepads: Query<&Gamepad>,
    mut query: Query<&mut GameStickDirectionalInput>,
    state: Res<State<AppState>>,
) {
    let mut dir = Vec3::ZERO;
    if *state.get() == AppState::InGame {
        for gamepad in gamepads.iter() {
            // Stick X → strafe (x); stick Y (up = +1 = forward) → forward (z).
            dir.x += deadzone(gamepad.get(GamepadAxis::LeftStickX).unwrap_or(0.0));
            dir.z += deadzone(gamepad.get(GamepadAxis::LeftStickY).unwrap_or(0.0));
            if gamepad.just_pressed(GamepadButton::South) {
                dir.y += 1.0;
            }
        }
    }
    for mut input in query.iter_mut() {
        input.0 = dir.clamp_length_max(1.0);
    }
}

/// Right stick → look delta, left/right triggers → modifier, right triggers → click.
/// Composes additively on top of the winit `get_look`/`get_modifying` writers (which
/// read zero with no mouse/keyboard), so it must be ordered after them.
fn gamepad_pointer(
    gamepads: Query<&Gamepad>,
    mut deltas: Query<&mut MouseMotionDelta>,
    mut modifiers: Query<&mut Modifying>,
    mut clicks: MessageWriter<PlayerClick>,
) {
    let mut look = Vec2::ZERO;
    let mut modify = false;
    let mut clicked = false;
    for gamepad in gamepads.iter() {
        let rx = deadzone(gamepad.get(GamepadAxis::RightStickX).unwrap_or(0.0));
        let ry = deadzone(gamepad.get(GamepadAxis::RightStickY).unwrap_or(0.0));
        // Stick up (+ry) looks up: a mouse moved forward gives a *negative*
        // `MouseMotionDelta.y` and looks up, so negate ry to match.
        look += Vec2::new(rx, -ry) * LOOK_SPEED;
        if gamepad.pressed(GamepadButton::LeftTrigger) || gamepad.pressed(GamepadButton::LeftTrigger2)
        {
            modify = true;
        }
        if gamepad.just_pressed(GamepadButton::RightTrigger)
            || gamepad.just_pressed(GamepadButton::RightTrigger2)
        {
            clicked = true;
        }
    }
    for mut delta in deltas.iter_mut() {
        delta.0 += look;
    }
    for mut modifying in modifiers.iter_mut() {
        modifying.0 = modifying.0 || modify;
    }
    if clicked {
        clicks.write(PlayerClick);
    }
}
