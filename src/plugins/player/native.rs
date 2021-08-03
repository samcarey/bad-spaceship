use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.init_resource::<State>();
    }
}

#[derive(Default)]
pub struct State;

pub fn get_look(mut mouse_motion_events: EventReader<MouseMotion>) -> Vec2 {
    match mouse_motion_events.iter().last() {
        Some(event) => event.delta,
        None => Vec2::ZERO,
    }
}

pub fn process_mouse_clicks(mouse_button_input: Res<Input<MouseButton>>) -> Option<MouseButton> {
    if mouse_button_input.just_pressed(MouseButton::Left) {
        Some(MouseButton::Left)
    } else {
        None
    }
}
