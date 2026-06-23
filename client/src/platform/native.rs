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
