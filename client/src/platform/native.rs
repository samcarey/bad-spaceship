use crate::AppState;
use bevy::prelude::*;

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut App) {
        app.add_startup_system(show_cursor)
            .add_system_set(SystemSet::on_enter(AppState::InGameMenu).with_system(show_cursor))
            .add_system_set(SystemSet::on_enter(AppState::InGame).with_system(hide_cursor))
            .add_system(toggle_menu_on_key);
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

fn toggle_menu_on_key(
    input: Res<Input<KeyCode>>,
    mut state: ResMut<bevy::prelude::State<AppState>>,
) {
    if input.just_pressed(KeyCode::Escape) {
        match state.current() {
            AppState::InGame => state.set(AppState::InGameMenu).unwrap(),
            AppState::InGameMenu => state.set(AppState::InGame).unwrap(),
            _ => {}
        }
    }
}
