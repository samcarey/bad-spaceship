use bad_spaceship_shared::{
    utils::TransformExt, CameraOrbitCenter, InputEvents, KeyboardDirectionalInput,
    MouseMotionDelta, PartRotation, PlayerClick, WebKeyCode, WebMouseButton,
};
use bevy::{input::mouse::MouseMotion, input::mouse::MouseWheel, prelude::*};

use crate::AppState;

pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut bevy::prelude::AppBuilder) {
        app.add_system(get_part_rotation.system())
            .add_system(process_keyboard_input.system().label(InputEvents))
            .init_resource::<Input<WebKeyCode>>()
            .init_resource::<Input<WebMouseButton>>()
            .add_system(get_look.system().label(InputEvents))
            .add_system_set(
                SystemSet::on_update(AppState::InGame)
                    .with_system(process_mouse_clicks.system().label(InputEvents)),
            )
            .add_event::<PlayerClick>();
    }
}

struct MergedKeyboardInput<'a> {
    native_keyboard_input: &'a Res<'a, Input<KeyCode>>,
    web_keyboard_input: &'a Res<'a, Input<WebKeyCode>>,
}

impl<'a> MergedKeyboardInput<'a> {
    pub fn pressed(&self, input: KeyCode) -> bool {
        self.native_keyboard_input.pressed(input)
            || self.web_keyboard_input.pressed(WebKeyCode(input))
    }
}

fn process_keyboard_input(
    keyboard_input: Res<Input<KeyCode>>,
    web_keyboard_input: Res<Input<WebKeyCode>>,
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

    if *state.current() == AppState::InGame {
        // "W" keypress indicates forward movement
        if input.pressed(KeyCode::W) {
            direction.z += 1.;
        }

        // "S" keypress indicates forward movement
        if input.pressed(KeyCode::S) {
            direction.z -= 1.;
        }

        // "D" keypress indicates forward movement
        if input.pressed(KeyCode::D) {
            direction.x += 1.;
        }

        // "A" keypress indicates forward movement
        if input.pressed(KeyCode::A) {
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

fn get_part_rotation(
    native_keyboard_input: Res<Input<KeyCode>>,
    web_keyboard_input: Res<Input<WebKeyCode>>,
    mut mouse_wheel_events: EventReader<MouseWheel>,
    mut players: Query<(&mut PartRotation, &Children)>,
    camera_orbit_centers: Query<&GlobalTransform, With<CameraOrbitCenter>>,
    mouse_deltas: Query<&MouseMotionDelta>,
) {
    if let Some((mut rotation, player_children)) = players.iter_mut().next() {
        rotation.0 = Quat::default();
        let input = MergedKeyboardInput {
            native_keyboard_input: &native_keyboard_input,
            web_keyboard_input: &web_keyboard_input,
        };
        if input.pressed(KeyCode::LShift) | input.pressed(KeyCode::RShift) {
            for child in player_children.iter() {
                if let Ok(camera_orbit_center) = camera_orbit_centers.get(*child) {
                    for mouse_wheel in mouse_wheel_events.iter() {
                        rotation.0 = Quat::from_axis_angle(
                            camera_orbit_center.forward(),
                            mouse_wheel.y / 10.,
                        ) * rotation.0;
                    }
                    for mouse_delta in mouse_deltas.iter() {
                        if mouse_delta.0 != Vec2::ZERO {
                            let rotation_input = camera_orbit_center.rotation.mul_vec3(Vec3::new(
                                -mouse_delta.0.x,
                                -mouse_delta.0.y,
                                0.0,
                            ));
                            let rotation_axis = rotation_input
                                .cross(camera_orbit_center.forward())
                                .normalize();
                            rotation.0 = Quat::from_axis_angle(
                                rotation_axis,
                                rotation_input.length() / 100.,
                            ) * rotation.0;
                        }
                    }
                }
            }
        }
    }
}

pub fn get_look(
    mut mouse_motion_events: EventReader<MouseMotion>,
    mut mouse_deltas: Query<&mut MouseMotionDelta>,
    state: Res<bevy::prelude::State<AppState>>,
) {
    let motion = match state.current() {
        AppState::InGame => match mouse_motion_events.iter().last() {
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
    native_mouse_button_input: Res<Input<MouseButton>>,
    web_mouse_button_input: Res<Input<WebMouseButton>>,
    mut player_clicks: EventWriter<PlayerClick>,
    state: Res<State<AppState>>,
) {
    if *state.current() == AppState::InGame {
        if native_mouse_button_input.just_pressed(MouseButton::Left)
            || web_mouse_button_input.pressed(WebMouseButton(MouseButton::Left))
        {
            player_clicks.send(PlayerClick);
        }
    }
}
