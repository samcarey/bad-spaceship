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
    let body = html_body::get();
    body.request_pointer_lock();
}

fn capture_mouse_on_click(
    mut state: ResMut<TrackInputState>,
    ev_mousebtn: Res<Events<MouseButtonInput>>,
) {
    for _ev in state.mousebtn.iter(&ev_mousebtn) {
        let window = web_sys::window().expect("no global `window` exists");
        let document = window.document().expect("should have a document on window");
        let body = document.body().expect("document should have a body");
        body.request_pointer_lock();
    }
}
