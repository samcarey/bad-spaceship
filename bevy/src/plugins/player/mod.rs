use std::f32;

use crate::{utils, AppState, APP_STATE};
use bevy::{input::mouse::MouseWheel, prelude::*, render::camera::Camera};
use bevy_rapier3d::physics::RigidBodyHandleComponent;
use rapier3d::dynamics::RigidBodySet;
use serde::Deserialize;

use super::{
    character,
    environment::part::{Holdable, TargetOrientation},
};

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
use native::{get_look, process_mouse_clicks, PlatformPlugin};
#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
use web::{get_look, process_mouse_clicks, PlatformPlugin};

const MAX_CAMERA_PITCH_DEGREES: f32 = 89.;
const MIN_CAMERA_PITCH_DEGREES: f32 = -89.;
const MIN_CAMERA_PITCH: f32 = MIN_CAMERA_PITCH_DEGREES * utils::DEG_TO_RADIANS;
const MAX_CAMERA_PITCH: f32 = MAX_CAMERA_PITCH_DEGREES * utils::DEG_TO_RADIANS;
const INITIAL_CAMERA_PITCH_DEGREES: f32 = 30.;
const INITIAL_CAMERA_PITCH: f32 = INITIAL_CAMERA_PITCH_DEGREES * utils::DEG_TO_RADIANS;

pub struct PlayerPlugin;
use character::CharacterPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.init_resource::<MouseWheelState>()
            .add_startup_system(setup.system())
            .on_state_update(
                APP_STATE,
                AppState::InGame,
                get_look.system().chain(process_mouse_events.system()),
            )
            .on_state_update(
                APP_STATE,
                AppState::InGame,
                process_mouse_clicks
                    .system()
                    .chain(initiate_holding.system()),
            )
            .on_state_update(
                APP_STATE,
                AppState::InGame,
                process_keyboard_events.system(),
            )
            // .add_system(update_camera.system())
            // .add_system(update_camera_distance.system())
            .add_system(respawn.system())
            .add_plugin(PlatformPlugin)
            .add_plugin(CharacterPlugin);
    }
}

#[derive(Deserialize, Copy, Clone)]
struct Config {
    zoom_sensitivity: f32,
    look_sensitivity: f32,

    min_camera_distance: f32,
    max_camera_distance: f32,
    camera_offset_character_size_ratio: (f32, f32, f32),
}

impl Default for Config {
    fn default() -> Self {
        config_from_file!("player.ron")
    }
}

#[derive(Default)]
pub struct KeyboardDirectionalInput(pub Vec3);

pub struct OrbitingCamera {
    pitch: f32,
    pub entity: Option<Entity>,
}

impl Default for OrbitingCamera {
    fn default() -> Self {
        OrbitingCamera {
            pitch: INITIAL_CAMERA_PITCH,
            entity: None,
        }
    }
}

impl OrbitingCamera {
    fn new(camera_entity: Option<Entity>) -> Self {
        OrbitingCamera {
            entity: camera_entity,
            ..Default::default()
        }
    }
}

#[derive(Default, Bundle)]
pub struct Player {
    pub camera: OrbitingCamera,
}

#[derive(Default)]
pub struct FocusedInteractable {
    pub current: Option<Entity>,
    pub previous: Option<Entity>,
    pub previous_color: Option<Color>,
}

#[derive(Default)]
pub struct Yaw(pub f32);

impl Player {
    fn new(camera_entity: Option<Entity>) -> Self {
        Player {
            camera: OrbitingCamera::new(camera_entity),
        }
    }
}

#[derive(Default)]
pub struct CameraOrbitCenter;

#[derive(Bundle, Default)]
pub struct CameraOrbitCenterBundle {
    pub transform: Transform,
    pub global_transform: GlobalTransform,
    camera_orbit_center: CameraOrbitCenter,
}

#[derive(Default)]
pub struct HoldPoint;

#[derive(Bundle, Default)]
pub struct HoldPointBundle {
    pub transform: Transform,
    pub global_transform: GlobalTransform,
    hold_point: HoldPoint,
}

#[derive(Default)]
pub struct Holding(pub bool);

#[derive(Default)]
struct MouseWheelState {
    mouse_wheel_event_reader: EventReader<MouseWheel>,
}

impl MouseWheelState {
    pub fn get_zoom_delta(&mut self, mouse_wheel_events: &Events<MouseWheel>) -> Option<f32> {
        match self
            .mouse_wheel_event_reader
            .iter(&mouse_wheel_events)
            .last()
        {
            Some(event) => Some(event.y),
            None => None,
        }
    }
}

fn tuple_to_vec3(tuple: (f32, f32, f32)) -> Vec3 {
    let (x, y, z) = tuple;
    Vec3::new(x, y, z)
}

fn setup(commands: &mut Commands) {
    let mut camera_transform =
        Transform::from_rotation(Quat::from_rotation_ypr(std::f32::consts::PI, 0.0, 0.0));
    camera_transform.translation = -Vec3::unit_z() * 20.0;
    let camera_entity = commands
        .spawn(Camera3dBundle {
            transform: camera_transform,
            ..Default::default()
        })
        .current_entity();
    spawn(commands, camera_entity);
}

fn respawn(
    players: Query<(&Transform, Entity, &Children), With<Player>>,
    camera_orbit_centers: Query<&Children, With<CameraOrbitCenter>>,
    cameras: Query<Entity, With<Camera>>,
    commands: &mut Commands,
) {
    for (player_transform, player_entity, player_children) in players.iter() {
        if player_transform.translation.y < -30.0 {
            let camera_orbit_center = player_children.iter().next().unwrap();

            let mut camera_option = None;
            for camera_orbit_center_child in camera_orbit_centers
                .get(*camera_orbit_center)
                .unwrap()
                .iter()
            {
                if let Ok(camera) = cameras.get(*camera_orbit_center_child) {
                    camera_option = Some(camera);
                }
            }
            let camera = camera_option.unwrap();

            commands
                .despawn(player_entity)
                .despawn(*camera_orbit_center);

            spawn(commands, Some(camera));
        }
    }
}

fn spawn(commands: &mut Commands, camera_entity: Option<Entity>) {
    let config: Config = config_from_file!("player.ron");

    let character_size = character::spawn(commands);

    let player_entity = commands
        .with(Player::new(camera_entity))
        .with(Yaw::default())
        .with(config)
        .with(KeyboardDirectionalInput::default())
        .with(FocusedInteractable::default())
        .with(Holding::default())
        .current_entity();

    // This is simply a point that hovers above the character that the camera orbits around.
    // This is for the purpose of making it easier to see over obstructions.
    // For now we generate this as a PbrComponent, which is overkill for an invisible point,
    // so we'll want to simplify this later to something with only the necessary components.
    let mut camera_orbit_center_transform = Transform::from_translation(
        tuple_to_vec3(config.camera_offset_character_size_ratio) * character_size,
    );
    camera_orbit_center_transform.rotation = Quat::from_rotation_x(INITIAL_CAMERA_PITCH);
    let camera_orbit_center = commands
        .spawn(CameraOrbitCenterBundle {
            transform: camera_orbit_center_transform,
            ..Default::default()
        })
        .current_entity();

    let hold_point = commands
        .spawn(HoldPointBundle {
            transform: Transform::from_translation(Vec3::unit_z() * 5.0),
            ..Default::default()
        })
        .current_entity();

    // Mount the camera center to the player
    commands.push_children(player_entity.unwrap(), &[camera_orbit_center.unwrap()]);

    // Mount the camera to the camera orbit center
    commands.push_children(camera_orbit_center.unwrap(), &[camera_entity.unwrap()]);

    commands.push_children(camera_orbit_center.unwrap(), &[hold_point.unwrap()]);
}

fn process_mouse_events(
    In(look): In<Vec2>,
    time: Res<Time>,
    mut mouse_wheel_state: ResMut<MouseWheelState>,
    mouse_wheel_events: Res<Events<MouseWheel>>,
    mut query: Query<(&mut Player, &Config, &mut Yaw)>,
    mut cameras: Query<&mut Transform, With<Camera>>,
    mut camera_orbit_centers: Query<&mut Transform, With<CameraOrbitCenter>>,
) {
    if let Some((mut player, config, mut yaw)) = query.iter_mut().next() {
        let camera = &mut player.camera;

        yaw.0 = (yaw.0 + look.x * time.delta_seconds() * config.look_sensitivity)
            % std::f32::consts::TAU;

        camera.pitch = (camera.pitch + look.y * time.delta_seconds() * config.look_sensitivity)
            .max(MIN_CAMERA_PITCH)
            .min(MAX_CAMERA_PITCH);

        // By tilting the orbit center that the camera is attached to,
        // the camera itself is swung to the correct position
        let mut camera_orbit_center_transform = camera_orbit_centers.iter_mut().next().unwrap();
        camera_orbit_center_transform.rotation = Quat::from_rotation_x(camera.pitch);

        if let Some(zoom_delta) = mouse_wheel_state.get_zoom_delta(&mouse_wheel_events) {
            // Set the camera translation relative to the camera orbit center
            let mut camera_transform = cameras.get_mut(camera.entity.unwrap()).unwrap();
            camera_transform.translation = -Vec3::unit_z()
                * (-camera_transform.translation.z
                    - zoom_delta * time.delta_seconds() * config.zoom_sensitivity)
                    .max(config.min_camera_distance)
                    .min(config.max_camera_distance);
        }
    }
}

fn process_keyboard_events(
    keyboard_input: Res<Input<KeyCode>>,
    mut query: Query<Mut<KeyboardDirectionalInput>>,
) {
    for mut keyboard_directional_input in query.iter_mut() {
        //
        // Note: keyboard_directional_input vector components match Bevy/Rapier vector definitions:
        //  Horizontal = (X,Z)
        //  Vertical = Y
        //

        // Initialize to zero every time - if a key is pressed then it will overwrite in the section below.
        keyboard_directional_input.0 = Vec3::zero();

        // "W" keypress indicates forward movement
        if keyboard_input.pressed(KeyCode::W) {
            keyboard_directional_input.0.z += 1.;
        }

        // "S" keypress indicates forward movement
        if keyboard_input.pressed(KeyCode::S) {
            keyboard_directional_input.0.z -= 1.;
        }

        // "D" keypress indicates forward movement
        if keyboard_input.pressed(KeyCode::D) {
            keyboard_directional_input.0.x += 1.;
        }

        // "A" keypress indicates forward movement
        if keyboard_input.pressed(KeyCode::A) {
            keyboard_directional_input.0.x -= 1.;
        }

        //
        // "Spacebar" keypress indicates vertical jump / thrust.
        //
        //  TODO:   We need to control directional input here to isolate jump event vs. continuous
        //          upward thrust.
        //
        if keyboard_input.pressed(KeyCode::Space) {
            keyboard_directional_input.0.y += 1.;
        }

        // Check here to see if any keypresses were registered.
        // If so, then normalize the vector components.
        if keyboard_directional_input.0 != Vec3::zero() {
            keyboard_directional_input.0.normalize();
        }
    }
}

fn initiate_holding(
    In(click): In<Option<MouseButton>>,
    commands: &mut Commands,
    mut players: Query<(&mut Holding, &FocusedInteractable), With<Player>>,
    holdables: Query<(&GlobalTransform, &RigidBodyHandleComponent), With<Holdable>>,
    mut bodies: ResMut<RigidBodySet>,
) {
    if let Some(_mouse_button) = click {
        if let Some((mut holding, interactable)) = players.iter_mut().next() {
            if let Some(current_interactable) = interactable.current {
                if let Ok((original_orientation, rb_handle)) = holdables.get(current_interactable) {
                    if let Some(rb) = bodies.get_mut(rb_handle.handle()) {
                        if holding.0 {
                            holding.0 = false;
                            commands.remove_one::<TargetOrientation>(current_interactable);
                            rb.angular_damping = 0.0;
                        } else {
                            holding.0 = true;
                            commands.insert(
                                current_interactable,
                                (TargetOrientation(original_orientation.rotation.clone()),),
                            );
                            rb.angular_damping = 1.0;
                        }
                    }
                }
            }
        }
    }
}
