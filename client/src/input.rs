use bad_spaceship_shared::{
    player, GameStickDirectionalInput, InputEvents, KeyboardDirectionalInput, LeftClicked,
    Modifying, MouseMotionDelta, MouseWheelDelta, MouseWheelLabel, OrbitingCamera, PlayerClick,
};
use bevy::{
    input::mouse::MouseMotion,
    input::mouse::{MouseScrollUnit, MouseWheel},
    prelude::*,
};

use crate::AppState;

pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut bevy::prelude::App) {
        app.add_systems(
            Update,
            (
                process_keyboard_input.in_set(InputEvents),
                get_look.in_set(InputEvents),
                process_mouse_clicks
                    .in_set(InputEvents)
                    .run_if(in_state(AppState::InGame)),
                get_left_click,
                get_modifying,
                gamepad_system,
                mouse_wheel.in_set(MouseWheelLabel).after(InputEvents),
                zoom_camera.after(MouseWheelLabel),
            ),
        )
        .add_message::<PlayerClick>();
    }
}

fn process_keyboard_input(
    input: Res<ButtonInput<KeyCode>>,
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

    if *state.get() == AppState::InGame {
        // "W" keypress indicates forward movement
        if input.pressed(KeyCode::KeyW) {
            direction.z += 1.;
        }

        // "S" keypress indicates forward movement
        if input.pressed(KeyCode::KeyS) {
            direction.z -= 1.;
        }

        // "D" keypress indicates forward movement
        if input.pressed(KeyCode::KeyD) {
            direction.x += 1.;
        }

        // "A" keypress indicates forward movement
        if input.pressed(KeyCode::KeyA) {
            direction.x -= 1.;
        }

        //
        // "Spacebar" keypress indicates vertical jump / thrust.
        //
        //  TODO:   We need to control directional input here to isolate jump event vs. continuous
        //          upward thrust.
        //
        if input.pressed(KeyCode::Space) {
            direction.y += 1.;
        }
    }

    for mut keyboard_directional_input in query.iter_mut() {
        keyboard_directional_input.0 =
            (keyboard_directional_input.0 + direction).normalize_or_zero();
    }
}

pub fn get_look(
    mut mouse_motion_events: MessageReader<MouseMotion>,
    mut mouse_deltas: Query<&mut MouseMotionDelta>,
    state: Res<bevy::prelude::State<AppState>>,
) {
    let motion = match *state.get() {
        AppState::InGame => match mouse_motion_events.read().last() {
            Some(event) => event.delta,
            None => Vec2::ZERO,
        },
        _ => Vec2::ZERO,
    };
    for mut mouse_delta in mouse_deltas.iter_mut() {
        *mouse_delta = MouseMotionDelta(motion);
    }
}

pub fn process_mouse_clicks(
    mouse_button_input: Res<ButtonInput<MouseButton>>,
    mut player_clicks: MessageWriter<PlayerClick>,
    state: Res<State<AppState>>,
) {
    if *state.get() == AppState::InGame {
        if mouse_button_input.just_pressed(MouseButton::Left) {
            player_clicks.write(PlayerClick);
        }
    }
}

fn get_left_click(
    mouse_button_input: Res<ButtonInput<MouseButton>>,
    state: Res<State<AppState>>,
    mut clicked_query: Query<&mut LeftClicked>,
) {
    if let Some(mut clicked) = clicked_query.iter_mut().next() {
        clicked.0 = (*state.get() == AppState::InGame)
            && mouse_button_input.just_pressed(MouseButton::Left);
    }
}

fn get_modifying(
    input: Res<ButtonInput<KeyCode>>,
    mut players: Query<&mut Modifying>,
    state: Res<State<AppState>>,
) {
    if let Some(mut modifying) = players.iter_mut().next() {
        modifying.0 = (*state.get() == AppState::InGame)
            && (input.pressed(KeyCode::ShiftLeft) | input.pressed(KeyCode::ShiftRight));
    }
}

fn gamepad_system(
    // Bevy 0.15 reworked gamepads into entities: each connected pad is an entity
    // carrying a `Gamepad` component (with `.get`/`.just_pressed` accessors), so
    // the old `GamepadLobby` resource + connection-tracking system are gone.
    gamepads: Query<&Gamepad>,
    mut query: Query<&mut GameStickDirectionalInput>,
) {
    for mut gamepad_directional_input in query.iter_mut() {
        // Initialize gamepad direction to zero every frame then overwrite below if we have gamepad inputs
        gamepad_directional_input.0 = Vec3::ZERO;

        for gamepad in gamepads.iter() {
            // Left stick controls movement
            //  NOTE: Gamepad Stick X axis => left/right => movement x-component
            //                      Y axis => forward/backward => movement z-component
            if let Some(left_stick_x) = gamepad.get(GamepadAxis::LeftStickX) {
                if left_stick_x.abs() > 0.01 {
                    gamepad_directional_input.0.x = left_stick_x;
                }
            }
            if let Some(left_stick_y) = gamepad.get(GamepadAxis::LeftStickY) {
                if left_stick_y.abs() > 0.01 {
                    gamepad_directional_input.0.z = left_stick_y;
                }
            }

            // "South" button [PS4 "X"] designates "jump"
            //  NOTE: Jump => movement y-component
            if gamepad.just_pressed(GamepadButton::South) {
                gamepad_directional_input.0.y += 1.0;
            }
        }

        // Check here to see if any keypresses were registered.
        // If so, then normalize the vector components.
        gamepad_directional_input.0 = gamepad_directional_input.0.normalize_or_zero();
    }
}

fn mouse_wheel(
    mut mouse_wheel_events: MessageReader<MouseWheel>,
    mut players: Query<&mut MouseWheelDelta>,
) {
    if let Some(mut mouse_wheel_delta) = players.iter_mut().next() {
        mouse_wheel_delta.0 = 0.0;
        if let Some(mouse_wheel) = mouse_wheel_events.read().last() {
            mouse_wheel_delta.0 = match mouse_wheel.unit {
                MouseScrollUnit::Line => mouse_wheel.y,
                MouseScrollUnit::Pixel => mouse_wheel.y / 108.0,
            };
        }
    }
}

fn zoom_camera(
    time: Res<Time>,
    mut players: Query<(&mut OrbitingCamera, &mut MouseWheelDelta, &Modifying)>,
    mut camera_transforms: Query<&mut Transform, With<Camera>>,
    configs: ResMut<Assets<player::Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        if let Some((orbiting_camera, scroll, modifying)) = players.iter_mut().next() {
            if !modifying.0 {
                // Set the camera translation relative to the camera orbit center
                let mut camera_transform = camera_transforms.get_mut(orbiting_camera.0).unwrap();
                camera_transform.translation = -Vec3::Z
                    * (-camera_transform.translation.z
                        - scroll.0 * time.delta_secs() * config.zoom_sensitivity)
                        .max(config.min_camera_distance)
                        .min(config.max_camera_distance);
            }
        }
    }
}
