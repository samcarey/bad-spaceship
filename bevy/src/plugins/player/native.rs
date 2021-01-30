use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.init_resource::<State>();
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

pub fn process_mouse_clicks(mouse_button_input: Res<Input<MouseButton>>) -> Option<MouseButton> {
    if mouse_button_input.just_pressed(MouseButton::Left) {
        Some(MouseButton::Left)
    } else {
        None
    }
}
