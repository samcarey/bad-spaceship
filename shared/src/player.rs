use std::f32;

use bevy::{
    input::gamepad::{Gamepad, GamepadButton, GamepadEvent, GamepadEventType},
    input::mouse::MouseWheel,
    prelude::*,
    reflect::TypeUuid,
    render::camera::Camera,
    utils::HashSet,
};
use bevy_rapier3d::prelude::{ColliderShape, RigidBodyMassProps};
use serde::Deserialize;

use crate::{
    character::CharacterPlugin,
    part::{Holdable, TargetOrientation, TargetPosition},
    utils::DEG_TO_RADIANS,
    CameraOrbitCenter, Character, FocusedInteractable, GameStickDirectionalInput, HoldPoint,
    Holding, KeyboardDirectionalInput, MouseMotionDelta, Player, PlayerClick, PlayerToSpawn, Yaw,
    INITIAL_CAMERA_PITCH,
};

const MAX_CAMERA_PITCH_DEGREES: f32 = 89.;
const MIN_CAMERA_PITCH_DEGREES: f32 = -89.;
const MIN_CAMERA_PITCH: f32 = MIN_CAMERA_PITCH_DEGREES * DEG_TO_RADIANS;
const MAX_CAMERA_PITCH: f32 = MAX_CAMERA_PITCH_DEGREES * DEG_TO_RADIANS;

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.init_resource::<MouseWheelState>()
            .add_startup_system(spawn_camera.system())
            .add_system(spawn.system())
            .add_system_to_stage(CoreStage::PreUpdate, connection_system.system())
            .add_system(process_mouse_events.system())
            .add_system(toggle_holding.system())
            .add_system(gamepad_system.system())
            .init_resource::<GamepadLobby>()
            .add_system(despawn.system())
            .add_system(attach_camera_orbit.system())
            .add_event::<PlayerToSpawn>()
            .add_plugin(CharacterPlugin)
            .add_event::<PlayerClick>()
            .add_asset::<Config>();
    }
}

#[derive(Deserialize, Copy, Clone, TypeUuid)]
#[uuid = "39cadc56-aa9c-4543-8640-a018b74b5050"]
pub struct Config {
    zoom_sensitivity: f32,
    look_sensitivity: f32,

    min_camera_distance: f32,
    max_camera_distance: f32,
    camera_offset_character_size_ratio: (f32, f32, f32),
}

#[derive(Bundle, Default)]
pub struct CameraOrbitCenterBundle {
    pub transform: Transform,
    pub global_transform: GlobalTransform,
    camera_orbit_center: CameraOrbitCenter,
}

#[derive(Bundle, Default)]
pub struct HoldPointBundle {
    pub transform: Transform,
    pub global_transform: GlobalTransform,
    hold_point: HoldPoint,
}

#[derive(Default)]
struct MouseWheelState;

impl MouseWheelState {
    pub fn get_zoom_delta(
        &mut self,
        mouse_wheel_events: &mut EventReader<MouseWheel>,
    ) -> Option<f32> {
        match mouse_wheel_events.iter().last() {
            Some(event) => Some(event.y),
            None => None,
        }
    }
}

fn tuple_to_vec3(tuple: (f32, f32, f32)) -> Vec3 {
    let (x, y, z) = tuple;
    Vec3::new(x, y, z)
}

fn spawn_camera(mut commands: Commands) {
    let mut camera_transform =
        Transform::from_rotation(Quat::from_rotation_ypr(std::f32::consts::PI, 0.0, 0.0));
    camera_transform.translation = -Vec3::Z * 20.0;
    commands.spawn_bundle(PerspectiveCameraBundle {
        transform: camera_transform,
        ..Default::default()
    });
}

fn spawn(
    mut commands: Commands,
    cameras: Query<Entity, With<Camera>>,
    players: Query<(), With<Player>>,
) {
    if players.iter().next().is_none() {
        if let Some(camera) = cameras.iter().next() {
            commands.spawn_bundle((
                Player::new(Some(camera)),
                Yaw::default(),
                KeyboardDirectionalInput::default(),
                GameStickDirectionalInput::default(),
                FocusedInteractable::default(),
                Holding::default(),
                MouseMotionDelta::default(),
            ));
        }
    }
}

fn despawn(players: Query<(&Transform, Entity, &Children), With<Player>>, mut commands: Commands) {
    for (player_transform, player_entity, player_children) in players.iter() {
        if player_transform.translation.y < -30.0 {
            let camera_orbit_center = player_children.iter().next().unwrap();
            commands.entity(player_entity).despawn();
            commands.entity(*camera_orbit_center).despawn();
        }
    }
}

fn attach_camera_orbit(
    mut commands: Commands,
    cameras: Query<Entity, With<Camera>>,
    characters_without_players: Query<
        (Entity, &ColliderShape),
        (With<Character>, Without<Children>),
    >,
    configs: ResMut<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        if let Some(camera) = cameras.iter().next() {
            for (character_entity, character_collider) in characters_without_players.iter() {
                // This is simply a point that hovers above the character that the camera orbits around.
                // This is for the purpose of making it easier to see over obstructions.
                // For now we generate this as a PbrComponent, which is overkill for an invisible point,
                // so we'll want to simplify this later to something with only the necessary components.
                let mut camera_orbit_center_transform = Transform::from_translation(
                    tuple_to_vec3(config.camera_offset_character_size_ratio)
                        * character_collider.compute_local_bounding_sphere().radius
                        * 2.0,
                );
                camera_orbit_center_transform.rotation =
                    Quat::from_rotation_x(INITIAL_CAMERA_PITCH);
                let camera_orbit_center = commands
                    .spawn()
                    .insert_bundle(CameraOrbitCenterBundle {
                        transform: camera_orbit_center_transform,
                        ..Default::default()
                    })
                    .id();

                let hold_point = commands
                    .spawn()
                    .insert_bundle(HoldPointBundle {
                        transform: Transform::from_translation(Vec3::Z * 5.0),
                        ..Default::default()
                    })
                    .id();

                // Mount the camera center to the player
                commands
                    .entity(character_entity)
                    .push_children(&[camera_orbit_center]);

                // Mount the camera to the camera orbit center
                commands
                    .entity(camera_orbit_center)
                    .push_children(&[camera]);

                commands
                    .entity(camera_orbit_center)
                    .push_children(&[hold_point]);
            }
        }
    }
}

fn process_mouse_events(
    time: Res<Time>,
    mut mouse_wheel_state: ResMut<MouseWheelState>,
    mut mouse_wheel_events: EventReader<MouseWheel>,
    mut query: Query<(&mut Player, &mut Yaw, &MouseMotionDelta)>,
    mut camera_queries: QuerySet<(
        Query<&mut Transform, With<Camera>>,
        Query<&mut Transform, With<CameraOrbitCenter>>,
    )>,
    configs: ResMut<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        if let Some((mut player, mut yaw, mouse_delta)) = query.iter_mut().next() {
            let camera = &mut player.camera;

            yaw.0 = (yaw.0 + mouse_delta.0.x * time.delta_seconds() * config.look_sensitivity)
                % std::f32::consts::TAU;

            camera.pitch = (camera.pitch
                + mouse_delta.0.y * time.delta_seconds() * config.look_sensitivity)
                .max(MIN_CAMERA_PITCH)
                .min(MAX_CAMERA_PITCH);

            // By tilting the orbit center that the camera is attached to,
            // the camera itself is swung to the correct position
            if let Some(mut camera_orbit_center_transform) =
                camera_queries.q1_mut().iter_mut().next()
            {
                camera_orbit_center_transform.rotation = Quat::from_rotation_x(camera.pitch);
            }

            if let Some(zoom_delta) = mouse_wheel_state.get_zoom_delta(&mut mouse_wheel_events) {
                // Set the camera translation relative to the camera orbit center
                let mut camera_transform = camera_queries
                    .q0_mut()
                    .get_mut(camera.entity.unwrap())
                    .unwrap();
                camera_transform.translation = -Vec3::Z
                    * (-camera_transform.translation.z
                        - zoom_delta * time.delta_seconds() * config.zoom_sensitivity)
                        .max(config.min_camera_distance)
                        .min(config.max_camera_distance);
            }
        }
    }
}

fn get_hold_point_entity(
    player_children: &Children,
    camera_orbit_centers: Query<&Children>,
    hold_points: Query<Entity, With<HoldPoint>>,
) -> Option<Entity> {
    let mut held_entity: Option<Entity> = None;
    if let Some(camera_orbit_center) = player_children.iter().next() {
        if let Ok(potential_hold_points) = camera_orbit_centers.get(*camera_orbit_center) {
            for potential_hold_point in potential_hold_points.iter() {
                if let Ok(held_entity_component) = hold_points.get(*potential_hold_point) {
                    held_entity = Some(held_entity_component);
                }
            }
        }
    }
    held_entity
}

fn toggle_holding(
    mut clicks: EventReader<PlayerClick>,
    mut commands: Commands,
    mut players: Query<(&mut Holding, &FocusedInteractable, &Children), With<Player>>,
    camera_orbit_centers: Query<&Children>,
    hold_points: Query<Entity, With<HoldPoint>>,
    holdables: Query<(&GlobalTransform, &RigidBodyMassProps), With<Holdable>>,
) {
    if clicks.iter().next().is_some() {
        if let Some((mut holding, interactable, player_children)) = players.iter_mut().next() {
            if let Some(current_interactable) = interactable.current {
                if let Ok((original_orientation, mass_properties)) =
                    holdables.get(current_interactable)
                {
                    if let Some(hold_point_entity) =
                        get_hold_point_entity(player_children, camera_orbit_centers, hold_points)
                    {
                        if holding.0 {
                            holding.0 = false;
                            commands
                                .entity(current_interactable)
                                .remove_bundle::<(TargetPosition, TargetOrientation)>();
                        } else {
                            holding.0 = true;
                            commands.entity(current_interactable).insert_bundle((
                                TargetPosition::new(hold_point_entity),
                                TargetOrientation::new(
                                    &mass_properties,
                                    original_orientation.rotation,
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct GamepadLobby {
    gamepads: HashSet<Gamepad>,
}

fn connection_system(
    mut lobby: ResMut<GamepadLobby>,
    mut gamepad_event: EventReader<GamepadEvent>,
) {
    for event in gamepad_event.iter() {
        match &event {
            GamepadEvent(gamepad, GamepadEventType::Connected) => {
                lobby.gamepads.insert(*gamepad);
                println!("{:?} Connected", gamepad);
            }
            GamepadEvent(gamepad, GamepadEventType::Disconnected) => {
                lobby.gamepads.remove(gamepad);
                println!("{:?} Disconnected", gamepad);
            }
            _ => (),
        }
    }
}

fn gamepad_system(
    lobby: Res<GamepadLobby>,
    button_inputs: Res<Input<GamepadButton>>,
    axes: Res<Axis<GamepadAxis>>,
    mut query: Query<&mut GameStickDirectionalInput>,
) {
    for mut gamepad_directional_input in query.iter_mut() {
        //
        // Initialize gamepad direction to zero every frame then overwrite below if we have gamepad inputs
        //
        gamepad_directional_input.0 = Vec3::ZERO;

        //
        // confirm that the controller is connected
        //
        for gamepad in lobby.gamepads.iter().cloned() {
            //
            // Left stick controls movement
            //  NOTE: Gamepad Stick X axis => left/right => movement x-component
            //                      Y axis => forward/backward => movement z-component
            let left_stick_x = axes
                .get(GamepadAxis(gamepad, GamepadAxisType::LeftStickX))
                .unwrap();
            if left_stick_x.abs() > 0.01 {
                //println!("{:?} LeftStickX value is {}", gamepad, left_stick_x);
                gamepad_directional_input.0.x = left_stick_x;
            }
            let left_stick_y = axes
                .get(GamepadAxis(gamepad, GamepadAxisType::LeftStickY))
                .unwrap();
            if left_stick_y.abs() > 0.01 {
                //println!("{:?} LeftStickY value is {}", gamepad, left_stick_y);
                gamepad_directional_input.0.z = left_stick_y;
            }

            //
            // "South" button [PS4 "X"] designates "jump"
            //  NOTE: Jump => movement y-component
            //
            if button_inputs.just_pressed(GamepadButton(gamepad, GamepadButtonType::South)) {
                //println!("{:?} just pressed South", gamepad);
                gamepad_directional_input.0.y += 1.0;
            }
        }

        // Check here to see if any keypresses were registered.
        // If so, then normalize the vector components.
        if gamepad_directional_input.0 != Vec3::ZERO {
            gamepad_directional_input.0.normalize();
        }
    }
}
