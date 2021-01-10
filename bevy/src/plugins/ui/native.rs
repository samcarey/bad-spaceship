use crate::{AppState, APP_STATE};
use bevy::prelude::*;

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(show_cursor.system())
            .on_state_update(APP_STATE, AppState::InGame, open_menu_on_key.system())
            .on_state_update(APP_STATE, AppState::InGameMenu, close_menu_on_key.system())
            .on_state_enter(APP_STATE, AppState::InGameMenu, show_cursor.system())
            .on_state_enter(APP_STATE, AppState::InGame, hide_cursor.system());
    }
}

fn show_cursor(mut windows: ResMut<Windows>) {
    let window = windows.get_primary_mut().unwrap();
    window.set_cursor_lock_mode(false);
    window.set_cursor_visibility(true);
}

fn hide_cursor(mut windows: ResMut<Windows>) {
    let window = windows.get_primary_mut().unwrap();
    window.set_cursor_lock_mode(true);
    window.set_cursor_visibility(false);
}

fn close_menu_on_key(input: ChangedRes<Input<KeyCode>>, mut state: ResMut<State<AppState>>) {
    if input.just_pressed(KeyCode::Escape) {
        state.set_next(AppState::InGame).unwrap();
    }
}

fn open_menu_on_key(input: ChangedRes<Input<KeyCode>>, mut state: ResMut<State<AppState>>) {
    if input.just_pressed(KeyCode::Escape) {
        state.set_next(AppState::InGameMenu).unwrap();
    }
}
