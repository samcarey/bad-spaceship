use bevy::prelude::*;
use bevy::reflect::TypeUuid;
use bevy_rapier3d::{
    na::{Isometry, UnitQuaternion, Vector3},
    physics::{ColliderBundle, ColliderPositionSync, IntoEntity, RigidBodyBundle},
    prelude::{
        ActiveEvents, ColliderHandle, ColliderShape, ContactEvent, RigidBodyMassProps,
        RigidBodyMassPropsFlags, RigidBodyPosition, RigidBodyVelocity,
    },
    render::ColliderDebugRender,
};

use serde::Deserialize;

use crate::{
    utils::TransformExt, Character, DirectionalInput, GameStickDirectionalInput,
    KeyboardDirectionalInput, Player, Yaw,
};

pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_system(combine_directional_inputs.system().label(CombineInputs))
            .add_system(walk_based_on_input.system().after(CombineInputs))
            .add_system(jump_based_on_input.system().after(CombineInputs))
            .add_system_to_stage(
                CoreStage::PostUpdate,
                rotate_character_based_on_input.system(),
            )
            .add_system(touching_ground.system())
            .add_system(spawn.system())
            .add_asset::<Config>();
    }
}

#[derive(Default)]
struct Touching(Vec<ColliderHandle>);

impl Touching {
    pub fn index(&self, handle: &ColliderHandle) -> Option<usize> {
        self.0.iter().position(|x| *x == *handle)
    }

    pub fn touching(&self) -> bool {
        !self.0.is_empty()
    }
}

#[derive(Deserialize, Clone, TypeUuid, Debug)]
#[uuid = "39cadc56-aa9c-4543-8640-a018b74b5051"]
pub struct Config {
    size: f32,
    max_speed: f32,
    jump_force: f32,
}

fn spawn(
    mut commands: Commands,
    players_without_characters: Query<Entity, (With<Player>, Without<Character>)>,
    configs: Res<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        for player_entity in players_without_characters.iter() {
            commands
                .entity(player_entity)
                .insert(Character)
                .insert_bundle(RigidBodyBundle {
                    body_type: bevy_rapier3d::prelude::RigidBodyType::Dynamic,
                    position: [0.0, 10., 0.].into(),
                    mass_properties: RigidBodyMassProps {
                        flags: RigidBodyMassPropsFlags::ROTATION_LOCKED,
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .insert_bundle(ColliderBundle {
                    shape: ColliderShape::ball(config.size / 2.0),
                    flags: (ActiveEvents::INTERSECTION_EVENTS | ActiveEvents::CONTACT_EVENTS)
                        .into(),
                    ..Default::default()
                })
                .insert_bundle((
                    Touching::default(),
                    ColliderDebugRender::with_id(0),
                    ColliderPositionSync::Discrete,
                ))
                .id();
        }
    }
}

fn touching_ground(
    mut players: Query<(Entity, &mut Touching)>,
    mut events: EventReader<ContactEvent>,
) {
    for contact_event in events.iter() {
        for (player_entity, mut touching) in players.iter_mut() {
            match contact_event {
                ContactEvent::Stopped(handle1, handle2) => {
                    if player_entity == handle1.entity() {
                        if let Some(index) = touching.index(&handle2) {
                            touching.0.remove(index);
                        }
                    } else if player_entity == handle2.entity() {
                        if let Some(index) = touching.index(&handle1) {
                            touching.0.remove(index);
                        }
                    }
                }
                ContactEvent::Started(handle1, handle2) => {
                    if player_entity == handle1.entity() {
                        if let None = touching.index(&handle2) {
                            touching.0.push(*handle2);
                        }
                    } else if player_entity == handle2.entity() {
                        if let None = touching.index(&handle1) {
                            touching.0.push(*handle1);
                        }
                    }
                }
            }
        }
    }
}

#[derive(SystemLabel, Clone, Hash, Debug, PartialEq, Eq)]
struct CombineInputs;

fn combine_directional_inputs(
    mut query: Query<(
        &mut KeyboardDirectionalInput,
        &GameStickDirectionalInput,
        &mut DirectionalInput,
    )>,
) {
    for (mut keyboard_directional_input, gamepad_directional_input, mut directional_input) in
        query.iter_mut()
    {
        directional_input.0 = Vec3::ZERO;
        directional_input.0.x = keyboard_directional_input.0.x + gamepad_directional_input.0.x;
        directional_input.0.y = keyboard_directional_input.0.y + gamepad_directional_input.0.y;
        directional_input.0.z = keyboard_directional_input.0.z + gamepad_directional_input.0.z;
        directional_input.0.normalize_or_zero();

        // Now that we've read this, reset it so it can be summed up again next frame
        keyboard_directional_input.0 = Vec3::ZERO;
    }
}

fn velocity_adjustment(
    current_velocity: Vec3,
    desired_velocity: Vec3,
    current_relevant_velocity: Vec3,
) -> Vec3 {
    let current_speed_along_desired_direction =
        current_velocity.dot(desired_velocity.normalize()).abs();
    let current_velocity_along_propulsion_direction = if current_relevant_velocity != Vec3::ZERO {
        current_speed_along_desired_direction * current_relevant_velocity.normalize()
    } else {
        Vec3::ZERO
    };
    desired_velocity - current_velocity_along_propulsion_direction
}

fn walk_based_on_input(
    mut query: Query<(
        &mut DirectionalInput,
        &Transform,
        &Touching,
        &mut RigidBodyVelocity,
        &RigidBodyMassProps,
    )>,
    configs: Res<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        for (directional_input, transform, touch_tracker, mut velocity, mass_properties) in
            query.iter_mut()
        {
            let current_velocity: Vec3 = velocity.linvel.clone_owned().into();

            // Compute our desired horizontal velocity vector based on keyboard inputs and move speed
            // Note: Horizontal plane = (x,z), Vertical plane = (y)
            let forward = transform.forward() * directional_input.0.z;
            let right = transform.right() * directional_input.0.x;
            let desired_velocity = Vec3::from(forward + right) * config.max_speed;

            // get a copy of the current velocity from rapier, isolated to horizontal components only
            // (ie, zero out current vertical [y] component)
            let current_horizontal_velocity =
                Vec3::new(current_velocity.x, 0.0, current_velocity.z);

            // To move the character, we increase the speed to match the maximum speed in whatever
            // direction is indicated by user keypress; or, if no keys pressed then we cancel out
            // any velocity to stop horizontally.
            let horizontal_velocity_change = if desired_velocity != Vec3::ZERO {
                velocity_adjustment(
                    current_velocity,
                    desired_velocity,
                    current_horizontal_velocity,
                )
            } else {
                -current_horizontal_velocity
            };

            let mut horizontal_impulse = mass_properties.mass() * horizontal_velocity_change * 0.13; // slowing down with fudge factor

            if !touch_tracker.touching() {
                horizontal_impulse *= 0.13; // slowing down even more when in air
            }

            // Apply the computed impulse to the character's rigid body
            velocity.apply_impulse(mass_properties, horizontal_impulse.into());
        }
    }
}

fn jump_based_on_input(
    mut query: Query<(
        &mut DirectionalInput,
        &Transform,
        &Touching,
        &mut RigidBodyVelocity,
        &RigidBodyMassProps,
    )>,
    configs: Res<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        for (directional_input, transform, touch_tracker, mut velocity, mass_properties) in
            query.iter_mut()
        {
            if touch_tracker.touching() && directional_input.0.y != 0. {
                let current_velocity: Vec3 = velocity.linvel.clone_owned().into();

                // Compute our desired horizontal velocity vector based on keyboard inputs and move speed
                // Note: Horizontal plane = (x,z), Vertical plane = (y)
                let up = transform.up() * directional_input.0.y;
                let desired_velocity = Vec3::from(up) * config.jump_force;

                // Get a copy of the current velocity from rapier, isolated to vertical component only
                // (ie, zero out current horizontal [x,z] components)
                let current_vertical_velocity = Vec3::new(0.0, current_velocity.y, 0.0);

                let vertical_velocity = if desired_velocity != Vec3::ZERO {
                    velocity_adjustment(
                        current_velocity,
                        desired_velocity,
                        current_vertical_velocity,
                    )
                } else {
                    Vec3::ZERO
                };

                // Apply the computed force to the character's rigid body
                let vertical_impulse = mass_properties.mass() * vertical_velocity;
                velocity.apply_impulse(mass_properties, vertical_impulse.into());
            }
        }
    }
}

fn rotate_character_based_on_input(mut query: Query<(&Transform, &Yaw, &mut RigidBodyPosition)>) {
    for (transform, yaw, mut position) in query.iter_mut() {
        let rotation = UnitQuaternion::from_axis_angle(&Vector3::y_axis(), -yaw.0);
        let new_position = Isometry::from_parts(transform.translation.into(), rotation);
        position.position = new_position;
    }
}
