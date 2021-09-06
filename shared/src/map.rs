use bevy::prelude::*;
use bevy::render::mesh::{Indices, VertexAttributeValues};
use bevy_rapier3d::na::Point3;
use bevy_rapier3d::physics::ColliderPositionSync;
use bevy_rapier3d::prelude::RigidBodyMassProps;
use bevy_rapier3d::{
    physics::{ColliderBundle, RigidBodyBundle},
    prelude::ColliderShape,
};
pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(add_lighting.system())
            .add_startup_system(spawn_map.system());
    }
}

pub const PLATFORM_WIDTH_M: f32 = 50.0; // meters
pub const PLATFORM_THICKNESS_M: f32 = 3.0; // meters

fn add_lighting(mut commands: Commands) {
    commands.spawn().insert_bundle(LightBundle {
        transform: Transform::from_translation(Vec3::new(0.0, 8.0, 0.0)), // meters
        ..Default::default()
    });
}

fn compute_mesh(shape: &ColliderShape) -> Mesh {
    let mut mesh = Mesh::new(bevy::render::pipeline::PrimitiveTopology::TriangleList);
    let trimesh = shape.as_trimesh().unwrap();
    mesh.set_attribute(
        Mesh::ATTRIBUTE_POSITION,
        VertexAttributeValues::from(
            trimesh
                .vertices()
                .iter()
                .map(|vertex| [vertex.x, vertex.y, vertex.z])
                .collect::<Vec<_>>(),
        ),
    );
    // Compute vertex normals by averaging the normals
    // of every triangle they appear in.
    // NOTE: This is a bit shonky, but good enough for visualisation.
    let verts = trimesh.vertices();
    let mut normals: Vec<Vec3> = vec![Vec3::ZERO; trimesh.vertices().len()];
    for triangle in trimesh.indices().iter() {
        let ab = verts[triangle[1] as usize] - verts[triangle[0] as usize];
        let ac = verts[triangle[2] as usize] - verts[triangle[0] as usize];
        let normal = ab.cross(&ac);
        // Contribute this normal to each vertex in the triangle.
        for i in 0..3 {
            normals[triangle[i] as usize] += Vec3::new(normal.x, normal.y, normal.z);
        }
    }
    let normals: Vec<[f32; 3]> = normals
        .iter()
        .map(|normal| {
            let normal = normal.normalize();
            [normal.x, normal.y, normal.z]
        })
        .collect();
    mesh.set_attribute(Mesh::ATTRIBUTE_NORMAL, VertexAttributeValues::from(normals));
    // There's nothing particularly meaningful we can do
    // for this one without knowing anything about the overall topology.

    mesh.set_attribute(
        Mesh::ATTRIBUTE_UV_0,
        VertexAttributeValues::from(
            trimesh
                .vertices()
                .iter()
                .map(|&vertex| {
                    [
                        vertex.x / PLATFORM_WIDTH_M + 0.5,
                        vertex.z / PLATFORM_WIDTH_M + 0.5,
                    ]
                })
                .collect::<Vec<_>>(),
        ),
    );
    mesh.set_indices(Some(Indices::U32(
        trimesh
            .indices()
            .iter()
            .flat_map(|triangle| triangle.iter())
            .cloned()
            .collect(),
    )));
    mesh
}

fn spawn_map(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    // Create a bowl with a cosine cross-section
    let mut vertices: Vec<Point3<f32>> = Vec::new();
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

    let rigid_body = RigidBodyBundle {
        body_type: bevy_rapier3d::prelude::RigidBodyType::Static,
        mass_properties: RigidBodyMassProps {
            effective_inv_mass: 1.0,
            ..Default::default()
        },
        position: [0.0, 0.0, 0.0].into(),
        ..Default::default()
    };
    let trimesh = ColliderShape::trimesh(vertices, indices);
    let collider = ColliderBundle {
        shape: trimesh.clone(),
        ..Default::default()
    };

    commands
        .spawn()
        .insert_bundle(rigid_body)
        .insert_bundle(collider)
        .insert_bundle(PbrBundle {
            mesh: meshes.add(compute_mesh(&trimesh)),
            material: materials.add(StandardMaterial {
                base_color_texture: Some(asset_server.load("textures/grass.png")),
                roughness: 1.0,
                ..Default::default()
            }),
            transform: Transform::from_scale(Vec3::ONE),
            ..Default::default()
        })
        .insert(ColliderPositionSync::Discrete);
}
