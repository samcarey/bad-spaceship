use crate::{AppState, APP_STATE};
use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;

use super::{FocusedInteractable, Holding, Player};

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.init_resource::<State>().on_state_update(
            APP_STATE,
            AppState::InGame,
            process_mouse_clicks.system(),
        );
    }
}

#[derive(Default)]
pub struct State {
    mouse_motion_event_reader: EventReader<MouseMotion>,
}

pub fn get_look(mut state: ResMut<State>, mouse_motion_events: Res<Events<MouseMotion>>) -> Vec2 {
    match state
        .mouse_motion_event_reader
        .iter(&mouse_motion_events)
        .last()
    {
        Some(event) => event.delta,
        None => Vec2::zero(),
    }
}

fn process_mouse_clicks(
    mouse_button_input: Res<Input<MouseButton>>,
    mut players: Query<(&mut Holding, &FocusedInteractable), With<Player>>,
) {
    if mouse_button_input.just_pressed(MouseButton::Left) {
        let (mut holding, interactable) = players.iter_mut().next().unwrap();
        if let Some(_current_interactable) = interactable.current {
            holding.0 = !holding.0;
        }
    }
}
