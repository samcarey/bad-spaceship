use avian3d::prelude::{Collider, Collisions, LinearVelocity, LockedAxes, Mass, RigidBody};
use bevy::prelude::*;
use bevy::reflect::TypePath;

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
        .init_asset::<Config>();
    }
}

// Bevy 0.12's asset rework replaced `TypeUuid` with the `Asset` derive
// (which still requires `TypePath`); the type id is derived, not a manual UUID.
#[derive(Asset, Deserialize, Clone, TypePath, Debug)]
pub struct Config {
    size: f32,
    max_speed: f32,
    jump_force: f32,
}

#[derive(Default, Bundle)]
struct CharacterBundle {
    character: Character,
    // Avian splits rapier's single `Velocity` into separate `LinearVelocity` /
    // `AngularVelocity`; the character only reads/writes linear velocity.
    linear_velocity: LinearVelocity,
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
                // Bevy 0.15: bare `Transform` (it now requires `GlobalTransform`).
                .insert(Transform::from_xyz(0.0, 10.0, 0.0))
                .insert(LockedAxes::ROTATION_LOCKED)
                // Avian's sphere constructor (rapier's "ball"). Avian collides all
                // collider pairs by default, so rapier's `ActiveCollisionTypes`
                // opt-in is dropped.
                .insert(Collider::sphere(config.size / 2.0))
                .insert(CharacterBundle::default())
                // Pin mass to 1.0 (rapier's `AdditionalMassProperties` did this);
                // movement sets velocity directly so this only scales how the
                // character shoves parts on contact.
                .insert(Mass(1.0));
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
    // Avian's `Collisions` system param yields only the touching contact pairs
    // (a convenience view over the `ContactGraph`); `collisions_with` is the
    // rapier `contact_pairs_with` equivalent.
    collisions: Collisions,
) {
    for (entity, mut touching_ground) in query.iter_mut() {
        touching_ground.0 = false;
        for pair in collisions.collisions_with(entity) {
            if let Some(contact) = pair.find_deepest_contact() {
                // Avian's `penetration` is positive when the bodies overlap — the
                // opposite sign from rapier's `dist()`. The old `dist < 0.002`
                // "in contact" test becomes `penetration > -0.002`.
                if contact.penetration > -0.002 {
                    touching_ground.0 = true;
                    break;
                }
            }
        }
    }
}

fn walk_based_on_input(
    mut query: Query<(&mut DirectionalInput, &Yaw, &mut LinearVelocity, &TouchingGround)>,
    configs: Res<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        for (directional_input, yaw, mut velocity, touching_ground) in query.iter_mut() {
            // `LinearVelocity` is a `Vec3` newtype (rapier's `Velocity.linvel`).
            let current_velocity: Vec3 = velocity.0;
            // The character body is a ROTATION_LOCKED ball whose rotation Rapier
            // owns, so movement is derived from the look `Yaw` directly instead of
            // the body transform. This matches the old basis: `back()` = +Z and
            // `left()` = -X, both yawed by `-yaw` (see `mouse_motion`).
            let look = Quat::from_rotation_y(-yaw.0);
            let forward = look * Vec3::Z * directional_input.0.z;
            let right = look * Vec3::NEG_X * directional_input.0.x;
            let desired_velocity = (forward + right) * config.max_speed;
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
            velocity.0 += horizontal_velocity_change * 0.13; // tuning factor
        }
    }
}

fn jump_based_on_input(
    mut query: Query<(
        &mut DirectionalInput,
        &Transform,
        &mut LinearVelocity,
        &TouchingGround,
    )>,
    configs: Res<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        for (directional_input, transform, mut velocity, touching_ground) in query.iter_mut() {
            if directional_input.0.y != 0. {
                if touching_ground.0 {
                    let current_velocity: Vec3 = velocity.0;
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
                    velocity.0 += vertical_velocity;
                }
            }
        }
    }
}
