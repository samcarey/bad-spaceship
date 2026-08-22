//! The single-player asteroid field.
//!
//! The multiplayer field lives on the server (`run_asteroid_field`), which owns the world
//! and replicates its rocks like any other part. Single player has no server, so it runs
//! the same field itself — the same split, for the same reason, as the launch countdown and
//! the rocket thrust: *one feel, two owners.*
//!
//! Everything that decides **what** the field does — when it opens, how fast it ramps, how
//! big and how close the rocks come — is the shared [`bad_spaceship_shared::asteroid`]
//! curve, so the two owners cannot drift into being two different games. What is duplicated
//! here is only the bookkeeping neither mode can share: finding the flown assembly (the
//! local joint graph, rather than a replicated membership marker) and spawning a plain local
//! body instead of a replicated one.

use bad_spaceship_shared::asteroid::{rock_is_spent, FieldClock, MAX_LIVE_ROCKS};
use bad_spaceship_shared::net::Asteroid;
use bad_spaceship_shared::part::{spawn_asteroid, Holdable, SuppressLocalParts};
use avian3d::prelude::{ComputedMass, LinearVelocity, Position, SphericalJoint};
use bevy::prelude::*;

use crate::launch::LaunchLocal;
use crate::render_main_pass::rock::asteroid_visual;
use crate::render_secondary_pass::main_assembly;

pub struct SinglePlayerAsteroidPlugin;

impl Plugin for SinglePlayerAsteroidPlugin {
    fn build(&self, app: &mut App) {
        // `SuppressLocalParts` marks the modes where somebody else owns the world; the
        // field follows the parts.
        app.add_systems(
            FixedUpdate,
            run_local_asteroid_field.run_if(not(resource_exists::<SuppressLocalParts>)),
        );
    }
}

/// Age the field's clock, spawn what the curve calls for, sweep what has gone past.
///
/// Runs in `FixedUpdate` like every other force and spawn in the flight path: the clock
/// drives a difficulty curve, and a curve advanced at frame rate would make the field
/// harder on a fast machine.
fn run_local_asteroid_field(
    time: Res<Time>,
    mut commands: Commands,
    launch: Res<LaunchLocal>,
    mut clock: Local<FieldClock>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    // Shaped to match `main_assembly`, which is the one place the local joint graph is
    // walked; velocities are looked up per member alongside, exactly as `apply_sp_thrust`
    // does it.
    parts: Query<(Entity, &GlobalTransform, &ComputedMass), With<Holdable>>,
    velocities: Query<&LinearVelocity>,
    joints: Query<&SphericalJoint>,
    rocks: Query<(Entity, &Position, &LinearVelocity), With<Asteroid>>,
) {
    if !launch.sp_launched() {
        // A landed or reset flight starts its next field from the beginning, and takes its
        // rubble with it — leftover boulders drifting around the pad are not scenery, they
        // are a hazard the player never chose to fly into.
        if clock.elapsed != 0.0 {
            for (entity, ..) in &rocks {
                commands.entity(entity).despawn();
            }
        }
        *clock = FieldClock::default();
        return;
    }
    let Some((members, _)) = main_assembly(&parts, &joints) else {
        return;
    };
    // Plain centroid + mean velocity, matching the server's field: it only needs to know
    // roughly where the ship is, and unlike a mass-weighted centre it can't be dragged off
    // by one heavy part when a hit breaks the stack up.
    let mut centre = Vec3::ZERO;
    let mut velocity = Vec3::ZERO;
    let mut count = 0.0;
    for (entity, transform, _) in &parts {
        if members.contains(&entity) {
            centre += transform.translation();
            velocity += velocities.get(entity).map(|v| v.0).unwrap_or_default();
            count += 1.0;
        }
    }
    if count == 0.0 {
        return;
    }
    let (centre, velocity) = (centre / count, velocity / count);

    let mut live = 0;
    for (entity, position, rock_vel) in &rocks {
        if rock_is_spent(position.0, rock_vel.0, centre, velocity) {
            commands.entity(entity).despawn();
        } else {
            live += 1;
        }
    }

    let Some(d) = clock.tick(time.delta_secs()) else {
        return;
    };
    if live >= MAX_LIVE_ROCKS {
        return;
    }
    // Single player has no floating-origin frame, so true coordinates are local ones.
    let (entity, radius, seed) =
        spawn_asteroid(&mut commands, centre, velocity, centre, velocity, d);
    let (mesh, material) = asteroid_visual(radius, seed, &mut meshes, &mut materials);
    // Deliberately NOT `Holdable`: that marker is what makes a body part of the player's
    // world — grabbable, jointable, and (here) a member of the assembly the launch flies
    // and the camera frames. A rock is none of those, and leaving it off is the whole
    // reason the local field needs no exclusions anywhere else.
    commands.entity(entity).insert((Asteroid, mesh, material));
}
