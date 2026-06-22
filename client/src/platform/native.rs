use crate::AppState;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, PrimaryWindow};

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, show_cursor)
            .add_systems(OnEnter(AppState::InGameMenu), show_cursor)
            .add_systems(OnEnter(AppState::InGame), hide_cursor)
            .add_systems(Update, toggle_menu_on_key);
    }
}

fn show_cursor(mut windows: Query<&mut Window, With<PrimaryWindow>>) {
    // Bevy 0.15 moved the cursor fields off `Window` into a `cursor_options` struct.
    // Bevy 0.16 made `Query::single_mut` fallible (returns `Result`).
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    window.cursor_options.grab_mode = CursorGrabMode::None;
    window.cursor_options.visible = true;
}

fn hide_cursor(mut windows: Query<&mut Window, With<PrimaryWindow>>) {
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    window.cursor_options.grab_mode = CursorGrabMode::Locked;
    window.cursor_options.visible = false;
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
