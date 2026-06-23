use bad_spaceship_shared::{map::PLATFORM_WIDTH_M, part::Holdable, Character, Grass};
// Bevy 0.17's render-crate split relocated several types out of `bevy_render`:
// `CascadeShadowConfigBuilder` → `bevy_light` (`bevy::light`), `Indices` /
// `VertexAttributeValues` → `bevy_mesh` (`bevy::mesh`), and `RenderAssetUsages`
// → `bevy_asset` (`bevy::asset`).
use bevy::{
    asset::RenderAssetUsages,
    light::CascadeShadowConfigBuilder,
    mesh::{Indices, VertexAttributeValues},
    prelude::*,
};
use avian3d::prelude::Collider;
use rand::Rng;

pub struct RenderMainPassPlugin;

impl Plugin for RenderMainPassPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, add_lighting)
            .add_systems(Update, (assign_parts, assign_grass, assign_characters));
    }
}

fn add_lighting(mut commands: Commands) {
    // Bevy 0.15 replaced `DirectionalLightBundle` with the `DirectionalLight`
    // required-components marker; the cascade config and transform are now plain
    // sibling components in the spawned tuple.
    commands.spawn((
        DirectionalLight {
            // Scaled down from 10_000 (the value carried over verbatim from the
            // 0.12 build) because Bevy 0.13+ added a physically-based camera
            // `Exposure` that 0.12 lacked, making the same illuminance read
            // brighter. ~0.6x dims the whole scene back toward the 0.12 look;
            // tune alongside the ambient fill in main.rs.
            illuminance: 6_000.0,
            shadows_enabled: true,
            ..Default::default()
        },
        // Bevy 0.10 replaced DirectionalLight's manual `shadow_projection` with
        // cascaded shadow maps; a single cascade spanning the platform suffices.
        CascadeShadowConfigBuilder {
            num_cascades: 1,
            maximum_distance: PLATFORM_WIDTH_M * 2.0,
            ..Default::default()
        }
        .build(),
        Transform {
            translation: Vec3::new(0.0, -2.0, 0.0),
            rotation: Quat::from_rotation_x(-std::f32::consts::FRAC_PI_4),
            ..Default::default()
        },
    ));
}

#[derive(Component)]
struct AssignedMaterial;

const COLOR_MIN: f32 = 0.2;
const COLOR_MAX: f32 = 0.7;

fn assign_parts(
    mut commands: Commands,
    unassigned: Query<
        (Entity, &Collider, &Transform, &GlobalTransform),
        (With<Holdable>, Without<AssignedMaterial>),
    >,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let mut rng = rand::thread_rng();
    for (entity, collider_shape, transform, global_transform) in unassigned.iter() {
        // Avian colliders expose their parry shape via `.shape()`; parry's
        // `Cuboid::half_extents` is a field (nalgebra `Vector3`, still indexable).
        let dims = collider_shape.shape().as_cuboid().unwrap().half_extents;
        commands
            .entity(entity)
            .insert((
                transform.clone(),
                global_transform.clone(),
                // Bevy 0.15 replaced `PbrBundle` with the `Mesh3d` / `MeshMaterial3d`
                // required-components wrappers — `Handle<T>` is no longer a component.
                // Bevy 0.13 deprecated `shape::*` in favour of `bevy_math`
                // primitives; the collider half-extents map to a full-size cuboid.
                Mesh3d(meshes.add(Cuboid::new(dims[0] * 2.0, dims[1] * 2.0, dims[2] * 2.0))),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::srgb(
                        rng.gen_range(COLOR_MIN..=COLOR_MAX),
                        rng.gen_range(COLOR_MIN..=COLOR_MAX),
                        rng.gen_range(COLOR_MIN..=COLOR_MAX),
                    ),
                    perceptual_roughness: rng.gen_range(0.0..=1.0),
                    metallic: rng.gen_range(0.0..=1.0),
                    reflectance: rng.gen_range(0.0..=1.0),
                    ..Default::default()
                })),
            ))
            .insert(AssignedMaterial);
    }
}

fn assign_characters(
    mut commands: Commands,
    unassigned: Query<
        (Entity, &Collider, &Transform, &GlobalTransform),
        (With<Character>, Without<AssignedMaterial>),
    >,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    for (entity, collider_shape, transform, global_transform) in unassigned.iter() {
        commands
            .entity(entity)
            .insert((
                transform.clone(),
                global_transform.clone(),
                Mesh3d(meshes.add(
                    // parry's `Ball::radius` is a field (via Avian's `.shape()`).
                    Sphere::new(collider_shape.shape().as_ball().unwrap().radius)
                        .mesh()
                        .ico(5)
                        .unwrap(),
                )),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::srgb(0.8, 0.8, 0.8),
                    ..Default::default()
                })),
            ))
            .insert(AssignedMaterial);
    }
}

fn assign_grass(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    asset_server: Res<AssetServer>,
    unassigned: Query<
        (Entity, &Collider, &Transform, &GlobalTransform),
        (With<Grass>, Without<AssignedMaterial>),
    >,
) {
    for (entity, collider_shape, transform, global_transform) in unassigned.iter() {
        commands
            .entity(entity)
            .insert((
                transform.clone(),
                global_transform.clone(),
                Mesh3d(meshes.add(compute_mesh(&collider_shape))),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color_texture: Some(asset_server.load("textures/grass.png")),
                    perceptual_roughness: 1.0,
                    ..Default::default()
                })),
            ))
            .insert(AssignedMaterial);
    }
}

fn compute_mesh(shape: &Collider) -> Mesh {
    let mut mesh = Mesh::new(
        bevy::render::render_resource::PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    // Avian exposes the parry shape via `.shape()`. parry's `TriMesh::vertices()`
    // returns a slice of nalgebra `Point3<f32>` (rapier's view yielded glam-like
    // points); convert to glam `Vec3` once up front so the rest stays in glam.
    let trimesh = shape.shape().as_trimesh().unwrap();
    let verts: Vec<Vec3> = trimesh
        .vertices()
        .iter()
        .map(|v| Vec3::new(v.x, v.y, v.z))
        .collect();
    let tris: Vec<[u32; 3]> = trimesh.indices().to_vec();

    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        VertexAttributeValues::from(verts.iter().map(|v| [v.x, v.y, v.z]).collect::<Vec<_>>()),
    );
    // Compute vertex normals by averaging the normals
    // of every triangle they appear in.
    // NOTE: This is a bit shonky, but good enough for visualisation.
    let mut normals: Vec<Vec3> = vec![Vec3::ZERO; verts.len()];
    for triangle in tris.iter() {
        let ab = verts[triangle[1] as usize] - verts[triangle[0] as usize];
        let ac = verts[triangle[2] as usize] - verts[triangle[0] as usize];
        let normal = ab.cross(ac);
        // Contribute this normal to each vertex in the triangle.
        for i in 0..3 {
            normals[triangle[i] as usize] += normal;
        }
    }
    let normals: Vec<[f32; 3]> = normals
        .iter()
        .map(|normal| {
            let normal = normal.normalize();
            [normal.x, normal.y, normal.z]
        })
        .collect();
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, VertexAttributeValues::from(normals));
    // There's nothing particularly meaningful we can do
    // for this one without knowing anything about the overall topology.

    mesh.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        VertexAttributeValues::from(
            verts
                .iter()
                .map(|v| [v.x / PLATFORM_WIDTH_M + 0.5, v.z / PLATFORM_WIDTH_M + 0.5])
                .collect::<Vec<_>>(),
        ),
    );
    mesh.insert_indices(Indices::U32(
        tris.iter().flat_map(|t| t.iter().copied()).collect(),
    ));
    mesh
}
