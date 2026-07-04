use crate::AppState;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, show_cursor)
            .add_systems(OnEnter(AppState::InGameMenu), show_cursor)
            .add_systems(OnEnter(AppState::InGame), hide_cursor)
            .add_systems(Update, toggle_menu_on_key);
    }
}

/// No-op on native: panics print to stderr, which the build box reads directly (no need
/// to round-trip through `localStorage` + the server). Mirrors the web signatures so the
/// callers stay platform-agnostic.
pub fn install_panic_hook() {}

/// See `install_panic_hook` — native has nothing stored.
pub fn take_stored_panic() -> Option<String> {
    None
}

/// Persisted display name — a no-op on native (no `localStorage`; the reset path uses
/// a server teleport, not a reload, so there's nothing to persist across). Mirrors the
/// web signature so `ui.rs` can call it unconditionally.
pub fn store_name(_name: &str) {}

/// See `store_name` — native has no persisted name.
pub fn stored_name() -> Option<String> {
    None
}

/// Persisted avatar pick — a no-op on native (no `localStorage`), mirroring the web
/// signature so `ui.rs` can call it unconditionally.
pub fn store_avatar(_monster: u8) {}

/// See `store_avatar` — native has no persisted avatar.
pub fn stored_avatar() -> Option<u8> {
    None
}

/// Copy the movement-tuning settings string somewhere the tester can grab it. Native
/// has no clipboard dep (dropped with bevy_egui's clipboard feature), so print it to
/// stdout — the desktop/build-box tester copies it from the terminal. Mirrors the web
/// signature so `ui::show_movement_panel` can call it unconditionally.
pub fn copy_to_clipboard(text: &str) {
    println!("[movement settings]\n{text}");
}

// Bevy 0.15 moved the cursor fields off `Window` into a `cursor_options` struct;
// Bevy 0.17 promoted that struct to its own `CursorOptions` component on the
// window entity, so query it directly. (`Query::single_mut` is fallible since 0.16.)
fn show_cursor(mut cursors: Query<&mut CursorOptions, With<PrimaryWindow>>) {
    let Ok(mut cursor) = cursors.single_mut() else {
        return;
    };
    cursor.grab_mode = CursorGrabMode::None;
    cursor.visible = true;
}

fn hide_cursor(mut cursors: Query<&mut CursorOptions, With<PrimaryWindow>>) {
    let Ok(mut cursor) = cursors.single_mut() else {
        return;
    };
    cursor.grab_mode = CursorGrabMode::Locked;
    cursor.visible = false;
}

fn toggle_menu_on_key(
    input: Res<ButtonInput<KeyCode>>,
    state: Res<State<AppState>>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    if input.just_pressed(KeyCode::Escape) {
        match *state.get() {
            AppState::InGame => next_state.set(AppState::InGameMenu),
            AppState::InGameMenu => next_state.set(AppState::InGame),
            _ => {}
        }
    }
}
