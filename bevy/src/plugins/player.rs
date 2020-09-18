use bevy::{
    input::mouse::{MouseMotion, MouseWheel},
    prelude::*,
};
use config_from_file_macro::ConfigFromFileMacro;
use config_from_file_macro_derive::ConfigFromFileMacro;
use serde::Deserialize;

use crate::plugins::character;

use std::f32::consts::PI;

const CONFIG_FILE: &str = "assets/config/player.ron";

const DEG_TO_RADIANS: f32 = PI / 180.;
const MIN_CAMERA_PITCH_DEGREES: f32 = 1.;
const MAX_CAMERA_PITCH_DEGREES: f32 = 179.;
const MIN_CAMERA_PITCH: f32 = MIN_CAMERA_PITCH_DEGREES * DEG_TO_RADIANS;
const MAX_CAMERA_PITCH: f32 = MAX_CAMERA_PITCH_DEGREES * DEG_TO_RADIANS;

pub struct PlayerPlugin;
use character::CharacterPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.init_resource::<State>()
            .add_startup_system(setup.system())
            .add_system(process_mouse_events.system())
            .add_system(process_keyboard_events.system())
            .add_system(update_camera.system())
            .add_plugin(CharacterPlugin);
    }
}

#[derive(ConfigFromFileMacro, Deserialize)]
struct Config {
    zoom_sensitivity: f32,
    look_sensitivity: f32,

    min_camera_distance: f32,
    max_camera_distance: f32,
}

#[derive(Default)]
pub struct KeyboardDirectionalInput(pub Vec2);

struct Camera {
    distance: f32,
    pitch: f32,
    entity: Option<Entity>,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            distance: 20.,
            pitch: 30.0f32.to_radians(),
            entity: None,
        }
    }
}

impl Camera {
    fn new(camera_entity: Option<Entity>) -> Self {
        Camera {
            entity: camera_entity,
            ..Default::default()
        }
    }
}

#[derive(Default, Bundle)]
struct Player {
    yaw: f32,
    camera: Camera,
}

impl Player {
    fn new(camera_entity: Option<Entity>) -> Self {
        Player {
            camera: Camera::new(camera_entity),
            ..Default::default()
        }
    }
}

#[derive(Default)]
struct State {
    mouse_motion_event_reader: EventReader<MouseMotion>,
    mouse_wheel_event_reader: EventReader<MouseWheel>,
}

fn setup(mut commands: Commands) {
    let config = Config::new(CONFIG_FILE);

    let camera_entity = commands
        .spawn(Camera3dComponents::default())
        .current_entity();

    character::spawn(&mut commands);

    let player_entity = commands
        .with(Player::new(camera_entity))
        .with(config)
        .with(KeyboardDirectionalInput::default())
        .current_entity();

    commands.push_children(player_entity.unwrap(), &[camera_entity.unwrap()]);
}

fn process_mouse_events(
    time: Res<Time>,
    mut state: ResMut<State>,
    mouse_motion_events: Res<Events<MouseMotion>>,
    mouse_wheel_events: Res<Events<MouseWheel>>,
    mut query: Query<(&mut Player, &mut Rotation, &Config)>,
) {
    let mut look = Vec2::zero();
    for event in state.mouse_motion_event_reader.iter(&mouse_motion_events) {
        look = event.delta;
    }

    let mut zoom_delta = 0.;
    for event in state.mouse_wheel_event_reader.iter(&mouse_wheel_events) {
        zoom_delta = event.y;
    }

    for (mut player, mut rotation, config) in &mut query.iter() {
        player.yaw += look.x() * time.delta_seconds;
        player.camera.pitch = (player.camera.pitch
            - look.y() * time.delta_seconds * config.look_sensitivity)
            .max(MIN_CAMERA_PITCH)
            .min(MAX_CAMERA_PITCH);
        player.camera.distance = (player.camera.distance
            - zoom_delta * time.delta_seconds * config.zoom_sensitivity)
            .max(config.min_camera_distance)
            .min(config.max_camera_distance);
        rotation.0 = Quat::from_rotation_y(-player.yaw);
    }
}

fn process_keyboard_events(
    keyboard_input: Res<Input<KeyCode>>,
    _player: &Player,
    mut keyboard_directional_input: Mut<KeyboardDirectionalInput>,
) {
    keyboard_directional_input.0 = Vec2::zero();

    if keyboard_input.pressed(KeyCode::W) {
        *keyboard_directional_input.0.y_mut() += 1.;
    }
    if keyboard_input.pressed(KeyCode::S) {
        *keyboard_directional_input.0.y_mut() -= 1.;
    }
    if keyboard_input.pressed(KeyCode::D) {
        *keyboard_directional_input.0.x_mut() += 1.;
    }
    if keyboard_input.pressed(KeyCode::A) {
        *keyboard_directional_input.0.x_mut() -= 1.;
    }

    if keyboard_directional_input.0 != Vec2::zero() {
        keyboard_directional_input.0.normalize();
    }
}

fn update_camera(
    mut player_query: Query<&mut Player>,
    camera_query: Query<(&mut Translation, &mut Rotation)>,
) {
    for player in &mut player_query.iter() {
        if let Some(camera_entity) = player.camera.entity {
            let cam_pos = Vec3::new(0., player.camera.pitch.cos(), -player.camera.pitch.sin())
                .normalize()
                * player.camera.distance;
            if let Ok(mut cam_trans) = camera_query.get_mut::<Translation>(camera_entity) {
                cam_trans.0 = cam_pos;
            }

            if let Ok(mut camera_rotation) = camera_query.get_mut::<Rotation>(camera_entity) {
                let look = Mat4::face_toward(cam_pos, Vec3::zero(), Vec3::new(0.0, 1.0, 0.0));
                camera_rotation.0 = look.to_scale_rotation_translation().1;
            }
        }
    }
}
