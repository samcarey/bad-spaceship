use bad_spaceship_shared::KeyboardDirectionalInput;
use bevy::prelude::*;

use crate::AppState;

pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut bevy::prelude::AppBuilder) {
        app.add_system(process_keyboard_input.system());
    }
}

fn process_keyboard_input(
    // For Web, it's unclear when it will use the native or web capture of keyboard events.
    // Therefore try to capture the native input and add to it any web input if there is any.
    // `other_keyboard_input` will come either a native (always Vec3::ZERO) or Web source
    keyboard_input: Res<Input<KeyCode>>,
    mut query: Query<&mut KeyboardDirectionalInput>,
    state: Res<State<AppState>>,
) {
    //
    // Note: keyboard_directional_input vector components match Bevy/Rapier vector definitions:
    //  Horizontal = (X,Z)
    //  Vertical = Y
    //

    // Initialize to zero every time - if a key is pressed then it will overwrite in the section below.
    let mut direction = Vec3::ZERO;

    if *state.current() == AppState::InGame {
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
    }

    for mut keyboard_directional_input in query.iter_mut() {
        // Sum with whatever other input is also being applied (e.g. web)
        keyboard_directional_input.0 =
            (keyboard_directional_input.0 + direction).normalize_or_zero();
    }
}
