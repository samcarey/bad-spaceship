use avian3d::prelude::{Collider, RigidBody};
use bevy::prelude::*;

use crate::Grass;
pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_map);
    }
}

pub const PLATFORM_WIDTH_M: f32 = 50.0; // meters
pub const PLATFORM_THICKNESS_M: f32 = 3.0; // meters

// ---------------------------------------------------------------------------
// Mini planet
// ---------------------------------------------------------------------------
//
// The 50 m grass platform is now the top of a mesa: cliffs drop from its edge
// down to a giant low-poly magma sphere `PLANET_DROP` metres below. The planet
// is a *client-side visual* (rendered with the magma material) plus a couple of
// server-side y-thresholds — it has no 30 km collider. Instead:
//   * a player who walks off the cliff falls past `PLANET_SURFACE_Y` and is
//     respawned by the normal fall path (the platform is the only thing you can
//     stand on before blastoff);
//   * a built assembly whose parts drop below `ASSEMBLY_CRASH_Y` (well below the
//     platform but above the loose-part cull line) has toppled off — a crash,
//     which resets the room.

/// Metres the planet's top surface sits below the platform base (world y = 0).
pub const PLANET_DROP: f32 = 20.0;
/// World-space y of the planet's top surface (directly below the platform).
pub const PLANET_SURFACE_Y: f32 = -PLANET_DROP;
/// 30 km diameter — a proper little world curving away to a near horizon.
pub const PLANET_RADIUS: f32 = 15_000.0;
/// World-space y of the sphere's centre (surface minus radius).
pub const PLANET_CENTER_Y: f32 = PLANET_SURFACE_Y - PLANET_RADIUS;
/// Height a grounded avatar respawns at once it falls off the cliffs — 2 m above the
/// planet surface, so it never visibly clips into the magma. Shared by the server
/// (`respawn_fallen_avatars`) and single-player (`player::despawn`) so the two respawn
/// heights stay in lockstep.
pub const PLANET_RESPAWN_Y: f32 = PLANET_SURFACE_Y + 2.0;

/// The Avian collision-layer bit the ground sits on: bit 0 (value 1), Avian's
/// default membership for a collider with no explicit `CollisionLayers` — like the
/// bowl spawned below. The multiplayer collision scheme reserves it for the ground
/// so rooms never use it: room parts/avatars (`server::net`) put their `room.bit`
/// in membership and add `GROUND_LAYER` to their *filter* (`room.bit | GROUND_LAYER`)
/// so they still land on the ground while staying isolated from other rooms; a
/// freshly-spawned, not-yet-roomed avatar (`build_server_avatar`) uses it as both
/// membership and filter to collide with the ground *only*.
pub const GROUND_LAYER: u32 = 1;

/// Build the ground bowl's trimesh collider (a cosine cross-section bowl with a flat
/// lip). `pub` so the joint-thinning tuning tests weld against the EXACT trimesh the
/// live ground uses, rather than a duplicated approximation that could silently drift.
pub fn bowl_collider() -> Collider {
    let mut vertices: Vec<Vec3> = Vec::new();
    let mut indices: Vec<[u32; 3]> = Vec::new();
    let segments = 16;
    let bowl_size = Vec3::new(PLATFORM_WIDTH_M, PLATFORM_THICKNESS_M, PLATFORM_WIDTH_M);
    for ix in 0..=segments {
        for iz in 0..=segments {
            // Map x and y into range [-1.0, 1.0];
            let shifted_z = (iz as f32 / segments as f32 - 0.5) * 2.0;
            let shifted_x = (ix as f32 / segments as f32 - 0.5) * 2.0;
            // Clamp radius at 1.0 or lower so the bowl has a flat lip near the corners.
            let clamped_radius = (shifted_z.powi(2) + shifted_x.powi(2)).sqrt().min(1.0);
            let x = shifted_x * bowl_size.x / 2.0;
            let z = shifted_z * bowl_size.z / 2.0;
            let y =
                ((clamped_radius - 0.5) * std::f32::consts::TAU / 2.0).sin() * bowl_size.y / 2.0;
            vertices.push([x, y, z].into());
        }
    }
    for ix in 0..segments {
        // Start of the two relevant rows of vertices.
        let row0 = ix * (segments + 1);
        let row1 = (ix + 1) * (segments + 1);

        for iz in 0..segments {
            // Two triangles making up a not-very-flat quad for each segment of the bowl.
            indices.push([row0 + iz + 0, row0 + iz + 1, row1 + iz + 0]);
            indices.push([row1 + iz + 0, row0 + iz + 1, row1 + iz + 1]);
        }
    }
    // Avian's fallible trimesh constructor is `try_trimesh`; the bowl mesh is always
    // valid (non-empty, matching index count), so unwrap it.
    Collider::try_trimesh(vertices, indices).expect("valid bowl trimesh")
}

fn spawn_map(mut commands: Commands) {
    commands
        .spawn_empty()
        // Avian renamed rapier's `RigidBody::Fixed` to `Static`.
        .insert(RigidBody::Static)
        // Bevy 0.15: `Transform` now requires `GlobalTransform`, so inserting the
        // bare component replaces the old `TransformBundle`. (Avian collides all
        // collider pairs by default, so rapier's `ActiveCollisionTypes` opt-in is gone.)
        .insert(Transform::from_xyz(0.0, 0.0, 0.0))
        // The client parents the mini-planet's visuals (sphere + cliffs) under this
        // entity so they ride the floating-origin frame. Give it `Visibility` from
        // the start so those child meshes never attach to a parent that lacks
        // `InheritedVisibility` (a first-frame B0004 warning otherwise). Inert on the
        // headless server.
        .insert(Visibility::default())
        .insert(bowl_collider())
        .insert(Grass);
}
