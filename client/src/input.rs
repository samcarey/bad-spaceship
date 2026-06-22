use bad_spaceship_shared::{
    player, GameStickDirectionalInput, InputEvents, KeyboardDirectionalInput, LeftClicked,
    Modifying, MouseMotionDelta, MouseWheelDelta, MouseWheelLabel, OrbitingCamera, PlayerClick,
    WebKeyCode, WebMouseButton,
};
use bevy::{
    input::gamepad::{GamepadConnection, GamepadConnectionEvent},
    input::mouse::MouseMotion,
    input::mouse::{MouseScrollUnit, MouseWheel},
    prelude::*,
    render::camera::Camera,
    utils::HashSet,
};

use crate::AppState;

pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut bevy::prelude::App) {
        app.add_systems(PreUpdate, connection_system)
            .add_systems(
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
            .init_resource::<ButtonInput<WebKeyCode>>()
            .init_resource::<ButtonInput<WebMouseButton>>()
            .add_event::<PlayerClick>()
            .init_resource::<GamepadLobby>();
    }
}

struct MergedKeyboardInput<'a> {
    native_keyboard_input: &'a Res<'a, ButtonInput<KeyCode>>,
    web_keyboard_input: &'a Res<'a, ButtonInput<WebKeyCode>>,
}

impl<'a> MergedKeyboardInput<'a> {
    pub fn pressed(&self, input: KeyCode) -> bool {
        self.native_keyboard_input.pressed(input)
            || self.web_keyboard_input.pressed(WebKeyCode(input))
    }
}

fn process_keyboard_input(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    web_keyboard_input: Res<ButtonInput<WebKeyCode>>,
    mut query: Query<&mut KeyboardDirectionalInput>,
    state: Res<State<AppState>>,
) {
    let input = MergedKeyboardInput {
        native_keyboard_input: &keyboard_input,
        web_keyboard_input: &web_keyboard_input,
    };
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
        // Sum with whatever other input is also being applied (e.g. web)
        keyboard_directional_input.0 =
            (keyboard_directional_input.0 + direction).normalize_or_zero();
    }
}

pub fn get_look(
    mut mouse_motion_events: EventReader<MouseMotion>,
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
    native_mouse_button_input: Res<ButtonInput<MouseButton>>,
    web_mouse_button_input: Res<ButtonInput<WebMouseButton>>,
    mut player_clicks: EventWriter<PlayerClick>,
    state: Res<State<AppState>>,
) {
    if *state.get() == AppState::InGame {
        if native_mouse_button_input.just_pressed(MouseButton::Left)
            || web_mouse_button_input.pressed(WebMouseButton(MouseButton::Left))
        {
            player_clicks.send(PlayerClick);
        }
    }
}

fn get_left_click(
    native_mouse_button_input: Res<ButtonInput<MouseButton>>,
    web_mouse_button_input: Res<ButtonInput<WebMouseButton>>,
    state: Res<State<AppState>>,
    mut clicked_query: Query<&mut LeftClicked>,
) {
    if let Some(mut clicked) = clicked_query.iter_mut().next() {
        clicked.0 = (*state.get() == AppState::InGame)
            && (native_mouse_button_input.just_pressed(MouseButton::Left)
                || web_mouse_button_input.pressed(WebMouseButton(MouseButton::Left)));
    }
}

fn get_modifying(
    native_keyboard_input: Res<ButtonInput<KeyCode>>,
    web_keyboard_input: Res<ButtonInput<WebKeyCode>>,
    mut players: Query<&mut Modifying>,
    state: Res<State<AppState>>,
) {
    if let Some(mut modifying) = players.iter_mut().next() {
        let input = MergedKeyboardInput {
            native_keyboard_input: &native_keyboard_input,
            web_keyboard_input: &web_keyboard_input,
        };
        modifying.0 = (*state.get() == AppState::InGame)
            && (input.pressed(KeyCode::ShiftLeft) | input.pressed(KeyCode::ShiftRight));
    }
}

#[derive(Default, Resource)]
struct GamepadLobby {
    gamepads: HashSet<Gamepad>,
}

fn connection_system(
    mut lobby: ResMut<GamepadLobby>,
    mut gamepad_events: EventReader<GamepadConnectionEvent>,
) {
    // Bevy 0.10 split the monolithic GamepadEvent/GamepadEventType into typed
    // events; connections now arrive as GamepadConnectionEvent.
    for event in gamepad_events.read() {
        match &event.connection {
            GamepadConnection::Connected(_) => {
                lobby.gamepads.insert(event.gamepad);
                println!("{:?} Connected", event.gamepad);
            }
            GamepadConnection::Disconnected => {
                lobby.gamepads.remove(&event.gamepad);
                println!("{:?} Disconnected", event.gamepad);
            }
        }
    }
}

fn gamepad_system(
    lobby: Res<GamepadLobby>,
    button_inputs: Res<ButtonInput<GamepadButton>>,
    axes: Res<Axis<GamepadAxis>>,
    mut query: Query<&mut GameStickDirectionalInput>,
) {
    for mut gamepad_directional_input in query.iter_mut() {
        // Initialize gamepad direction to zero every frame then overwrite below if we have gamepad inputs
        gamepad_directional_input.0 = Vec3::ZERO;

        // confirm that the controller is connected
        for gamepad in lobby.gamepads.iter().cloned() {
            // Left stick controls movement
            //  NOTE: Gamepad Stick X axis => left/right => movement x-component
            //                      Y axis => forward/backward => movement z-component
            let left_stick_x = axes
                .get(GamepadAxis {
                    gamepad,
                    axis_type: GamepadAxisType::LeftStickX,
                })
                .unwrap();
            if left_stick_x.abs() > 0.01 {
                //println!("{:?} LeftStickX value is {}", gamepad, left_stick_x);
                gamepad_directional_input.0.x = left_stick_x;
            }
            let left_stick_y = axes
                .get(GamepadAxis {
                    gamepad,
                    axis_type: GamepadAxisType::LeftStickY,
                })
                .unwrap();
            if left_stick_y.abs() > 0.01 {
                //println!("{:?} LeftStickY value is {}", gamepad, left_stick_y);
                gamepad_directional_input.0.z = left_stick_y;
            }

            // "South" button [PS4 "X"] designates "jump"
            //  NOTE: Jump => movement y-component
            if button_inputs.just_pressed(GamepadButton {
                gamepad,
                button_type: GamepadButtonType::South,
            }) {
                //println!("{:?} just pressed South", gamepad);
                gamepad_directional_input.0.y += 1.0;
            }
        }

        // Check here to see if any keypresses were registered.
        // If so, then normalize the vector components.
        gamepad_directional_input.0 = gamepad_directional_input.0.normalize_or_zero();
    }
}

fn mouse_wheel(
    mut mouse_wheel_events: EventReader<MouseWheel>,
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
                        - scroll.0 * time.delta_seconds() * config.zoom_sensitivity)
                        .max(config.min_camera_distance)
                        .min(config.max_camera_distance);
            }
        }
    }
}
