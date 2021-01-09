use crate::utils::html_body;
use crate::{AppState, APP_STATE};
use bevy::prelude::*;

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.on_state_enter(APP_STATE, AppState::InGame, hide_cursor.system());
    }
}

fn hide_cursor() {
    html_body::get().request_pointer_lock();
}
