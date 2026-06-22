use bevy::prelude::*;
use bevy::reflect::{TypePath, TypeUuid};
use bevy::transform::TransformSystem;
use bevy_rapier3d::{
    na::{UnitQuaternion, Vector3},
    plugin::RapierContext,
    prelude::{
        ActiveCollisionTypes, AdditionalMassProperties, Collider, LockedAxes, MassProperties,
        RigidBody, Velocity,
    },
};

use serde::Deserialize;

use crate::{
    Character, DirectionalInput, GameStickDirectionalInput, KeyboardDirectionalInput, Player, Yaw,
};

pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                touching_ground,
                combine_directional_inputs.in_set(CombineInputs),
                walk_based_on_input
                    .after(CombineInputs)
                    .after(touching_ground),
                jump_based_on_input
                    .after(CombineInputs)
                    .after(touching_ground),
                spawn,
            ),
        )
        // Must run after Rapier's writeback (which resets this ROTATION_LOCKED
        // body's rotation to identity, before PostUpdate) and before transform
        // propagation, so the yaw reaches the child camera-orbit hierarchy.
        .add_systems(
            PostUpdate,
            rotate_character_based_on_input.before(TransformSystem::TransformPropagate),
        )
        .add_asset::<Config>();
    }
}

// Bevy 0.11's `Asset` bound now also requires `TypePath`.
#[derive(Deserialize, Clone, TypeUuid, TypePath, Debug)]
#[uuid = "39cadc56-aa9c-4543-8640-a018b74b5051"]
pub struct Config {
    size: f32,
    max_speed: f32,
    jump_force: f32,
}

#[derive(Default, Bundle)]
struct CharacterBundle {
    character: Character,
    velocity: Velocity,
    touching_ground: TouchingGround,
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
                .insert(RigidBody::Dynamic)
                .insert(TransformBundle::from(Transform::from_xyz(0.0, 10.0, 0.0)))
                .insert(LockedAxes::ROTATION_LOCKED)
                .insert(Collider::ball(config.size / 2.0))
                // .insert(ActiveEvents::COLLISION_EVENTS)
                .insert(ActiveCollisionTypes::default() | ActiveCollisionTypes::STATIC_STATIC)
                .insert(CharacterBundle::default())
                .insert(AdditionalMassProperties::MassProperties(MassProperties {
                    mass: 1.0,
                    ..Default::default()
                }));
        }
    }
}

#[derive(SystemSet, Clone, Hash, Debug, PartialEq, Eq)]
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
        directional_input.0 = directional_input.0.normalize_or_zero();

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

#[derive(Default, Component)]
struct TouchingGround(bool);

fn touching_ground(
    mut query: Query<(Entity, &mut TouchingGround)>,
    rapier_context: Res<RapierContext>,
) {
    for (entity, mut touching_ground) in query.iter_mut() {
        touching_ground.0 = false;
        // There's a function called "any_active_contact" that used to work for this,
        // but doesn't anymore, so I'm just doing it manually until I figure what is wrong.
        for contact in rapier_context.contacts_with(entity) {
            if let Some((_, contact)) = contact.find_deepest_contact() {
                if contact.dist() < 0.002 {
                    touching_ground.0 = true;
                    break;
                }
            }
        }
    }
}

fn walk_based_on_input(
    mut query: Query<(
        &mut DirectionalInput,
        &Transform,
        &mut Velocity,
        &TouchingGround,
    )>,
    configs: Res<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        for (directional_input, transform, mut velocity, touching_ground) in query.iter_mut() {
            let current_velocity: Vec3 = velocity.linvel.into();
            let forward = transform.back() * directional_input.0.z;
            let right = transform.left() * directional_input.0.x;
            let desired_velocity = Vec3::from(forward + right) * config.max_speed;
            let current_horizontal_velocity =
                Vec3::new(current_velocity.x, 0.0, current_velocity.z);
            let mut horizontal_velocity_change = if desired_velocity != Vec3::ZERO {
                velocity_adjustment(
                    current_velocity,
                    desired_velocity,
                    current_horizontal_velocity,
                )
            } else {
                -current_horizontal_velocity
            };
            if !touching_ground.0 {
                horizontal_velocity_change *= 0.13; // slowing down even more when in air
            }
            velocity.linvel += horizontal_velocity_change * 0.13; // tuning factor
        }
    }
}

fn jump_based_on_input(
    mut query: Query<(
        &mut DirectionalInput,
        &Transform,
        &mut Velocity,
        &TouchingGround,
    )>,
    configs: Res<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        for (directional_input, transform, mut velocity, touching_ground) in query.iter_mut() {
            if directional_input.0.y != 0. {
                if touching_ground.0 {
                    let current_velocity: Vec3 = velocity.linvel.into();
                    let up = transform.up() * directional_input.0.y;
                    let desired_velocity = Vec3::from(up) * config.jump_force;
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
                    velocity.linvel += vertical_velocity;
                }
            }
        }
    }
}

fn rotate_character_based_on_input(mut query: Query<(&mut Transform, &Yaw)>) {
    for (mut transform, yaw) in query.iter_mut() {
        let rotation = UnitQuaternion::from_axis_angle(&Vector3::y_axis(), -yaw.0);
        transform.rotation = rotation.into();
    }
}
