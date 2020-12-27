use crate::utils::html_body;
use crate::{AppState, APP_STATE};
use bevy::input::mouse::MouseButtonInput;
use bevy::prelude::*;

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.init_resource::<TrackInputState>()
            .on_state_exit(APP_STATE, AppState::InGameMenu, hide_cursor.system())
            .on_state_update(APP_STATE, AppState::InGame, capture_mouse_on_click.system());
    }
}

#[derive(Default)]
struct TrackInputState {
    mousebtn: EventReader<MouseButtonInput>,
}

fn hide_cursor() {
    html_body::get().request_pointer_lock();
}

fn capture_mouse_on_click(
    mut state: ResMut<TrackInputState>,
    ev_mousebtn: Res<Events<MouseButtonInput>>,
) {
    for _ev in state.mousebtn.iter(&ev_mousebtn) {
        html_body::get().request_pointer_lock();
        break;
    }
}
