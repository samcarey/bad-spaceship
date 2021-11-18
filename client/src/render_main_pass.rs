use bad_spaceship_shared::{map::PLATFORM_WIDTH_M, part::Holdable, Character, Grass};
use bevy::{
    prelude::*,
    render::mesh::{Indices, VertexAttributeValues},
};
use bevy_rapier3d::prelude::ColliderShape;
use rand::Rng;

pub struct RenderMainPassPlugin;

impl Plugin for RenderMainPassPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(add_lighting.system())
            .add_system(assign_parts.system())
            .add_system(assign_grass.system())
            .add_system(assign_characters.system());
    }
}

fn add_lighting(mut commands: Commands) {
    commands.spawn().insert_bundle(LightBundle {
        transform: Transform::from_translation(Vec3::new(0.0, 8.0, 0.0)), // meters
        ..Default::default()
    });
}

struct AssignedMaterial;

const COLOR_MIN: f32 = 0.2;
const COLOR_MAX: f32 = 0.7;

fn assign_parts(
    mut commands: Commands,
    unassigned: Query<(Entity, &ColliderShape), (With<Holdable>, Without<AssignedMaterial>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let mut rng = rand::thread_rng();
    for (entity, collider_shape) in unassigned.iter() {
        let dims = collider_shape.as_cuboid().unwrap().half_extents;
        commands
            .entity(entity)
            .insert_bundle(PbrBundle {
                mesh: meshes.add(Mesh::from(shape::Box {
                    max_x: dims.get(0).unwrap().clone(),
                    min_x: -dims.get(0).unwrap().clone(),
                    max_y: dims.get(1).unwrap().clone(),
                    min_y: -dims.get(1).unwrap().clone(),
                    max_z: dims.get(2).unwrap().clone(),
                    min_z: -dims.get(2).unwrap().clone(),
                })),
                material: materials.add(StandardMaterial {
                    base_color: Color::rgba(
                        rng.gen_range(COLOR_MIN..=COLOR_MAX),
                        rng.gen_range(COLOR_MIN..=COLOR_MAX),
                        rng.gen_range(COLOR_MIN..=COLOR_MAX),
                        1.0,
                    ),
                    roughness: rng.gen_range(0.0..=1.0),
                    metallic: rng.gen_range(0.0..=1.0),
                    reflectance: rng.gen_range(0.0..=1.0),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .insert(AssignedMaterial);
    }
}

fn assign_characters(
    mut commands: Commands,
    unassigned: Query<(Entity, &ColliderShape), (With<Character>, Without<AssignedMaterial>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    for (entity, collider_shape) in unassigned.iter() {
        commands
            .entity(entity)
            .insert_bundle(PbrBundle {
                mesh: meshes.add(Mesh::from(shape::Icosphere {
                    radius: collider_shape.as_ball().unwrap().radius,
                    subdivisions: 5,
                })),
                material: materials.add(StandardMaterial {
                    base_color: Color::rgb(0.8, 0.8, 0.8),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .insert(AssignedMaterial);
    }
}

fn assign_grass(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    asset_server: Res<AssetServer>,
    unassigned: Query<(Entity, &ColliderShape), (With<Grass>, Without<AssignedMaterial>)>,
) {
    for (entity, collider_shape) in unassigned.iter() {
        commands
            .entity(entity)
            .insert_bundle(PbrBundle {
                mesh: meshes.add(compute_mesh(&collider_shape)),
                material: materials.add(StandardMaterial {
                    base_color_texture: Some(asset_server.load("textures/grass.png")),
                    roughness: 1.0,
                    ..Default::default()
                }),
                transform: Transform::from_scale(Vec3::ONE),
                ..Default::default()
            })
            .insert(AssignedMaterial);
    }
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
