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

pub fn process_keyboard_events(keyboard_input: Res<Input<KeyCode>>) -> Vec3 {
    //
    // Note: keyboard_directional_input vector components match Bevy/Rapier vector definitions:
    //  Horizontal = (X,Z)
    //  Vertical = Y
    //

    // Initialize to zero every time - if a key is pressed then it will overwrite in the section below.
    let mut direction = Vec3::ZERO;

    // "W" keypress indicates forward movement
    if keyboard_input.pressed(KeyCode::W) {
        direction.z += 1.;
    }

    // "S" keypress indicates forward movement
    if keyboard_input.pressed(KeyCode::S) {
        direction.z -= 1.;
    }

    // "D" keypress indicates forward movement
    if keyboard_input.pressed(KeyCode::D) {
        direction.x += 1.;
    }

    // "A" keypress indicates forward movement
    if keyboard_input.pressed(KeyCode::A) {
        direction.x -= 1.;
    }

    //
    // "Spacebar" keypress indicates vertical jump / thrust.
    //
    //  TODO:   We need to control directional input here to isolate jump event vs. continuous
    //          upward thrust.
    //
    if keyboard_input.pressed(KeyCode::Space) {
        direction.y += 1.;
    }

    direction
}
