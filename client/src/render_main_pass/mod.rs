use bad_spaceship_shared::{
    map::PLATFORM_WIDTH_M,
    part::{Holdable, PartSeed, SuppressLocalParts},
    Grass,
};
mod ash_material;
mod grass_material;
pub mod metal_material;

// Bevy 0.17's render-crate split relocated several types out of `bevy_render`:
// `CascadeShadowConfigBuilder` → `bevy_light` (`bevy::light`), `Indices` /
// `VertexAttributeValues` → `bevy_mesh` (`bevy::mesh`), and `RenderAssetUsages`
// → `bevy_asset` (`bevy::asset`).
use bevy::{
    asset::{load_internal_asset, uuid_handle, RenderAssetUsages},
    light::CascadeShadowConfigBuilder,
    mesh::{Indices, VertexAttributeValues},
    pbr::ExtendedMaterial,
    prelude::*,
};
use ash_material::{spawn_ash_field, AshMaterial, ASH_SHADER_HANDLE};
use grass_material::{GrassExtension, GrassMaterial, GRASS_SHADER_HANDLE};
use metal_material::{part_visual, MetalMaterial, METAL_SHADER_HANDLE};
use avian3d::prelude::Collider;

/// The shared noise library both material shaders `#import` (see
/// `bad_spaceship::noise` in the WGSL); registering it makes the import path
/// resolvable when the material pipelines compile.
const NOISE_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("26176534-6a5e-4030-8788-5ebe6e74318d");

pub struct RenderMainPassPlugin;

impl Plugin for RenderMainPassPlugin {
    fn build(&self, app: &mut App) {
        // Embedded like the gizmo shader (`render_secondary_pass`): compiled
        // into the binary under weak handles, so the web build fetches nothing.
        // The noise library goes first — the material shaders `#import` it.
        load_internal_asset!(app, NOISE_SHADER_HANDLE, "../../assets/noise.wgsl", Shader::from_wgsl);
        load_internal_asset!(
            app,
            GRASS_SHADER_HANDLE,
            "../../assets/grass_material.wgsl",
            Shader::from_wgsl
        );
        load_internal_asset!(
            app,
            METAL_SHADER_HANDLE,
            "../../assets/metal_material.wgsl",
            Shader::from_wgsl
        );
        load_internal_asset!(
            app,
            ASH_SHADER_HANDLE,
            "../../assets/ash_material.wgsl",
            Shader::from_wgsl
        );
        app.add_plugins((
            MaterialPlugin::<GrassMaterial>::default(),
            MaterialPlugin::<MetalMaterial>::default(),
            MaterialPlugin::<AshMaterial>::default(),
        ))
            .add_systems(Startup, (add_lighting, spawn_ash_field))
            .add_systems(
                Update,
                (
                    // In multiplayer the replicated parts are drawn by the netcode
                    // (and marked Holdable for joint display), so skip the local
                    // part renderer to avoid double meshes.
                    assign_parts.run_if(not(resource_exists::<SuppressLocalParts>)),
                    assign_grass,
                ),
            );
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
            shadow_maps_enabled: true,
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

fn assign_parts(
    mut commands: Commands,
    unassigned: Query<
        (Entity, &Collider, &Transform, &GlobalTransform, &PartSeed),
        (With<Holdable>, Without<AssignedMaterial>),
    >,
    mut materials: ResMut<Assets<MetalMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    for (entity, collider_shape, transform, global_transform, seed) in unassigned.iter() {
        // Avian colliders expose their parry shape via `.shape()`; parry's
        // `Cuboid::half_extents` is a field (nalgebra `Vector3`, still indexable).
        let dims = collider_shape.shape().as_cuboid().unwrap().half_extents;
        // Mesh + seed-derived metal from the shared constructor — the same one
        // the multiplayer client runs on `NetPart` (`draw_replicated_parts`).
        let (mesh, material) = part_visual(
            Vec3::new(dims[0], dims[1], dims[2]),
            seed.0,
            &mut meshes,
            &mut materials,
        );
        commands
            .entity(entity)
            .insert((transform.clone(), global_transform.clone(), mesh, material))
            .insert(AssignedMaterial);
    }
}

fn assign_grass(
    mut commands: Commands,
    mut materials: ResMut<Assets<GrassMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
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
                MeshMaterial3d(materials.add(ExtendedMaterial {
                    base: StandardMaterial {
                        // The turf original ships ROUGHNESS 0.9 / SPECULAR 0.05;
                        // grass is a diffuse surface, so kill the specular sheen.
                        perceptual_roughness: 1.0,
                        reflectance: 0.05,
                        ..Default::default()
                    },
                    extension: GrassExtension::default(),
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
    // No UVs: the grass shader works in world-space XZ (the old texture tiling
    // derived its UVs from world position anyway).
    mesh.insert_indices(Indices::U32(
        tris.iter().flat_map(|t| t.iter().copied()).collect(),
    ));
    mesh
}
