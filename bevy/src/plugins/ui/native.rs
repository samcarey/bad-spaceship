use crate::{AppState, APP_STATE};
use bevy::input::mouse::MouseButtonInput;
use bevy::prelude::*;

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.init_resource::<TrackInputState>()
            .add_startup_system(show_cursor.system())
            .on_state_enter(APP_STATE, AppState::InGameMenu, show_cursor.system())
            .on_state_enter(APP_STATE, AppState::InGame, hide_cursor.system())
            .on_state_update(
                APP_STATE,
                AppState::Initial,
                capture_mouse_on_click.system(),
            );
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

#[derive(Default)]
struct TrackInputState {
    mousebtn: EventReader<MouseButtonInput>,
}

fn capture_mouse_on_click(
    mut input_state: ResMut<TrackInputState>,
    ev_mousebtn: Res<Events<MouseButtonInput>>,
    mut state: ResMut<State<AppState>>,
) {
    for _ev in input_state.mousebtn.iter(&ev_mousebtn) {
        state.set_next(AppState::InGame).unwrap();
    }
}
