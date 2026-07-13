//! The mini-planet: a giant low-poly magma sphere below the grass platform, the
//! cliff walls dropping to it, and the green "there's your world" outline that
//! appears around the sphere once the rocket blasts off.
//!
//! Purely client-side visuals. The platform (`Grass`) is unchanged — it's now the
//! top of a mesa; `PLANET_DROP` metres below its edge is the sphere's surface. Both
//! the sphere and the cliffs are parented to the `Grass` entity so they ride the
//! floating-origin frame during flight (the client slides `Grass` to `-offset`),
//! receding smoothly below the climbing rocket. The gameplay consequences of the
//! planet (touching it before blastoff → respawn, an assembly crashing into it →
//! room reset) are server-side y-thresholds in `server::net`, keyed off the shared
//! `PLANET_SURFACE_Y` — the planet has no 30 km collider.

use bad_spaceship_shared::{
    map::{PLANET_CENTER_Y, PLANET_RADIUS, PLANET_SURFACE_Y, PLATFORM_WIDTH_M},
    net::NetLaunch,
    part::SuppressLocalParts,
    Grass,
};
use bevy::{asset::RenderAssetUsages, mesh::PrimitiveTopology, prelude::*};

use crate::launch::LaunchLocal;
use crate::outline::{Outlined, PlanetOutline};
use crate::render_main_pass::magma_material::{magma_material, MagmaMaterial};

pub struct PlanetPlugin;

impl Plugin for PlanetPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (spawn_planet, toggle_planet_outline));
    }
}

/// The magma sphere entity (child of `Grass`). Marks it for the green outline.
#[derive(Component)]
struct PlanetSphere;

/// Set on the `Grass` entity once its planet + cliffs are spawned, so it runs once.
#[derive(Component)]
struct PlanetSpawned;

/// Icosphere subdivision count — deliberately coarse: the low-poly facets (flat-
/// shaded) are the intended stylised look, and the procedural magma supplies the
/// fine surface detail. A 30 km sphere reads as round enough from altitude while
/// the near-field facets keep it a "little world".
const PLANET_SUBDIVISIONS: u32 = 24;

/// Spawn the magma sphere + cliff skirt as children of the ground, once it exists.
fn spawn_planet(
    mut commands: Commands,
    grass: Query<Entity, (With<Grass>, Without<PlanetSpawned>)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<MagmaMaterial>>,
) {
    let Ok(ground) = grass.single() else {
        return;
    };

    // Low-poly sphere, flat-shaded for crisp facets. `ico` fails only past 80
    // subdivisions (far above ours), so this can't error at our count.
    let sphere = meshes.add(
        Sphere::new(PLANET_RADIUS)
            .mesh()
            .ico(PLANET_SUBDIVISIONS)
            .expect("planet icosphere subdivisions in range")
            .with_duplicated_vertices()
            .with_computed_flat_normals(),
    );
    let cliffs = meshes.add(cliff_mesh());
    // One material instance shared by the sphere and the cliffs (same black rock +
    // molten rivulets); the cliffs' vertical faces make the magma read as dripping.
    let material = materials.add(magma_material());

    // `Grass` carries `Visibility` from its spawn (`map::spawn_map`), so parenting
    // these meshes under it is visibility-consistent from the first frame.
    commands.entity(ground).insert(PlanetSpawned).with_children(|parent| {
        parent.spawn((
            Mesh3d(sphere),
            MeshMaterial3d(material.clone()),
            // Centre the sphere so its top surface sits `PLANET_DROP` below the pad.
            Transform::from_xyz(0.0, PLANET_CENTER_Y, 0.0),
            PlanetSphere,
        ));
        parent.spawn((
            Mesh3d(cliffs),
            MeshMaterial3d(material),
            Transform::IDENTITY,
        ));
    });
}

/// The cliff skirt: the four vertical walls of the square platform footprint,
/// dropping from the platform lip down to the planet surface. Built non-indexed
/// with per-face outward normals (flat-shaded rock), each wall wound so its front
/// face points outward regardless of corner order.
fn cliff_mesh() -> Mesh {
    // The platform lip sits at +half the bowl thickness; anchor the cliff top there
    // so it meets the grass edge with no gap, and drop to the planet surface.
    const TOP_Y: f32 = 1.5;
    let hw = PLATFORM_WIDTH_M / 2.0;
    let bot = PLANET_SURFACE_Y;

    // Square corners (x, z), counter-clockwise seen from above.
    let corners = [
        Vec2::new(-hw, -hw),
        Vec2::new(hw, -hw),
        Vec2::new(hw, hw),
        Vec2::new(-hw, hw),
    ];

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(24);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(24);

    for i in 0..4 {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        // Outward normal of this wall = horizontal, perpendicular to the edge,
        // pointing away from the centre.
        let edge = b - a;
        let mut out = Vec3::new(edge.y, 0.0, -edge.x).normalize();
        if out.dot(Vec3::new(a.x, 0.0, a.y)) < 0.0 {
            out = -out;
        }
        let tl = Vec3::new(a.x, TOP_Y, a.y);
        let tr = Vec3::new(b.x, TOP_Y, b.y);
        let bl = Vec3::new(a.x, bot, a.y);
        let br = Vec3::new(b.x, bot, b.y);
        // Two triangles; flip winding if it faces inward, so the outside is drawn.
        for tri in [[tl, bl, tr], [tr, bl, br]] {
            let geo = (tri[1] - tri[0]).cross(tri[2] - tri[0]);
            let (p0, p1, p2) = if geo.dot(out) >= 0.0 {
                (tri[0], tri[1], tri[2])
            } else {
                (tri[0], tri[2], tri[1])
            };
            for v in [p0, p1, p2] {
                positions.push([v.x, v.y, v.z]);
                normals.push([out.x, out.y, out.z]);
            }
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh
}

/// Ring the planet in green once the room has blasted off (single-player from
/// [`LaunchLocal`], multiplayer from the replicated [`NetLaunch`]); clear it on a
/// reset. Reuses the grabbable-outline pipeline — [`PlanetOutline`] flips its
/// colour to green.
fn toggle_planet_outline(
    mut commands: Commands,
    planet: Query<(Entity, Has<Outlined>), With<PlanetSphere>>,
    local: Res<LaunchLocal>,
    multiplayer: Option<Res<SuppressLocalParts>>,
    orb: Query<&NetLaunch>,
) {
    let launched = if multiplayer.is_some() {
        orb.iter().next().is_some_and(|l| l.launched)
    } else {
        local.sp_launched()
    };
    for (entity, has) in &planet {
        if launched && !has {
            commands.entity(entity).insert((Outlined, PlanetOutline));
        } else if !launched && has {
            commands
                .entity(entity)
                .remove::<Outlined>()
                .remove::<PlanetOutline>();
        }
    }
}
