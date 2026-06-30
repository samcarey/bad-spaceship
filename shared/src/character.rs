use avian3d::prelude::{Collider, Collisions, LinearVelocity, LockedAxes, Mass, Position, RigidBody};
use bevy::prelude::*;
use bevy::reflect::TypePath;

use serde::Deserialize;

use crate::{
    Character, DirectionalInput, GameStickDirectionalInput, KeyboardDirectionalInput, Player, Yaw,
};

pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut App) {
        // Input *merging* stays in `Update`: it's sampled once per render frame
        // from the device input written there (and `combine` zeroes the keyboard
        // accumulator, which is only safe once per frame). The velocity-applying
        // systems move to `FixedUpdate` so movement advances exactly once per
        // simulation tick. This is a prerequisite for the upcoming client-side
        // prediction + rollback (which replays per-tick inputs deterministically),
        // and it removes the old frame-rate dependence — movement used to run in
        // `Update`, so a 120 Hz display literally moved the character faster.
        app.add_systems(
            Update,
            (
                combine_directional_inputs,
                // Suppressed in multiplayer — the predicted networked avatar (client)
                // / the per-client `ServerAvatar` (server) is the character instead.
                spawn.run_if(not(resource_exists::<crate::SuppressLocalPlayer>)),
                build_server_avatar,
            ),
        )
            .add_systems(
                FixedUpdate,
                (
                    touching_ground,
                    walk_based_on_input.after(touching_ground),
                    // walk + jump both write `LinearVelocity` (different axes, but
                    // the same component), so order them explicitly for a
                    // deterministic result under rollback replay.
                    jump_based_on_input
                        .after(touching_ground)
                        .after(walk_based_on_input),
                )
                    .in_set(CharacterMovement),
            )
            // Run the fixed timestep at the netcode tick rate (60 Hz) so single-
            // player physics + movement match the multiplayer simulation tick
            // (Bevy's default `Time<Fixed>` is 64 Hz). Avian steps on this too.
            .insert_resource(Time::<Fixed>::from_duration(crate::net::TICK))
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

impl Config {
    /// The character ball's diameter, in metres. Exposed so the client's
    /// predicted-avatar setup can build the same body the single-player `spawn`
    /// does, from the loaded config asset.
    pub fn size(&self) -> f32 {
        self.size
    }
}

#[derive(Default, Bundle)]
struct CharacterBundle {
    character: Character,
    // Avian splits rapier's single `Velocity` into separate `LinearVelocity` /
    // `AngularVelocity`; the character only reads/writes linear velocity.
    linear_velocity: LinearVelocity,
    touching_ground: TouchingGround,
}

/// Insert the core character physics body onto an entity. Shared by the
/// single-player `spawn`, the server's `build_server_avatar`, and the client's
/// predicted-avatar setup. Does NOT set `Transform`/`Position` — the caller sets
/// the spawn pose (single-player/server) or replication provides it (client
/// predicted). The sphere collider, rotation lock, unit mass, and the
/// movement-input component (`DirectionalInput`) plus `Character`/velocity/
/// ground-contact (`CharacterBundle`) match what every controllable character needs.
pub fn insert_character_body(entity: &mut EntityCommands, size: f32) {
    entity.insert((
        RigidBody::Dynamic,
        LockedAxes::ROTATION_LOCKED,
        // Avian's sphere constructor (rapier's "ball"). Avian collides all collider
        // pairs by default, so rapier's `ActiveCollisionTypes` opt-in is dropped.
        Collider::sphere(size / 2.0),
        // Pin mass to 1.0; movement sets velocity directly so this only scales how
        // the character shoves parts on contact.
        Mass(1.0),
        CharacterBundle::default(),
        DirectionalInput::default(),
    ));
}

fn spawn(
    mut commands: Commands,
    players_without_characters: Query<Entity, (With<Player>, Without<Character>)>,
    configs: Res<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        for player_entity in players_without_characters.iter() {
            let mut entity = commands.entity(player_entity);
            insert_character_body(&mut entity, config.size);
            // Bevy 0.15: bare `Transform` (it now requires `GlobalTransform`).
            entity.insert(Transform::from_xyz(0.0, 10.0, 0.0));
        }
    }
}

/// System set wrapping the `FixedUpdate` movement systems, so the server's
/// input-bridge (which writes `DirectionalInput`/`Yaw` from the networked input)
/// can be ordered `.before` them.
#[derive(SystemSet, Clone, Hash, Debug, PartialEq, Eq)]
pub struct CharacterMovement;

/// Marks a server-side networked avatar that needs its character body assembled —
/// the multiplayer equivalent of a local `Player`. The server adds this to each
/// client's replicated avatar; `build_server_avatar` then gives it the same Avian
/// body the single-player `spawn` builds, so the server simulates it authoritatively
/// from the client's input intent.
#[derive(Default, Component)]
pub struct ServerAvatar;

/// An optional spawn position for a `ServerAvatar`, honored by `build_server_avatar`
/// instead of the origin. The server sets it for a *reconnecting* client (the
/// position resolved at connect from the resume id in the connect token), so the
/// avatar is assembled directly at its remembered spot — its first replicated
/// `Position` is the saved one, with no origin→saved easing on the client.
#[derive(Component, Clone, Copy)]
pub struct InitialPose(pub Vec3);

/// Assemble the character body for each `ServerAvatar` that doesn't have one yet,
/// once the character `Config` (its size) is loaded. Mirrors `spawn`, but driven by
/// the networked marker and seeded with the movement-input component the server's
/// bridge writes (`DirectionalInput`) plus `Yaw` (the bridge writes it from the
/// client's look angle each tick).
fn build_server_avatar(
    mut commands: Commands,
    avatars: Query<(Entity, Option<&InitialPose>), (With<ServerAvatar>, Without<Character>)>,
    configs: Res<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        for (entity, initial) in avatars.iter() {
            // A reconnecting client's avatar is built directly at its remembered
            // position (`InitialPose`, resolved at connect from the resume id), so its
            // first replicated pose is the saved one — no origin→saved ease. A fresh
            // client spawns at the origin (a tiny settle, NOT the single-player y=10
            // drop-in, which a predicting client would mispredict in slow motion at the
            // chaotic connect moment). Seed both Transform and Position because Avian's
            // transform-sync is disabled in multiplayer.
            let pos = initial.map(|p| p.0).unwrap_or(Vec3::ZERO);
            let mut e = commands.entity(entity);
            insert_character_body(&mut e, config.size);
            e.insert((
                Transform::from_translation(pos),
                Position(pos),
                Yaw::default(),
            ));
            e.remove::<InitialPose>();
        }
    }
}

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
        // Clamp (not normalize) so analog inputs keep their sub-unit magnitude for
        // variable speed — the touch joystick's response curve relies on this.
        // Keyboard and gamepad both arrive at unit length already, so capping the
        // max leaves them (and diagonal WASD) exactly as before.
        directional_input.0 = directional_input.0.clamp_length_max(1.0);

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
            // Per-tick blend toward the desired velocity. Now that this runs in
            // `FixedUpdate` at a fixed 60 Hz (was per-render-frame in `Update`),
            // this is a deterministic per-tick factor; retune it (and `max_speed`)
            // if the fixed-tick feel differs from the old variable-frame-rate feel.
            velocity.0 += horizontal_velocity_change * 0.13;
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
