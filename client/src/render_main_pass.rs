use bad_spaceship_shared::{map::PLATFORM_WIDTH_M, part::Holdable, Character, Grass};
use bevy::{
    prelude::*,
    render::mesh::{Indices, VertexAttributeValues},
};
use bevy_rapier3d::prelude::Collider;
use rand::Rng;

pub struct RenderMainPassPlugin;

impl Plugin for RenderMainPassPlugin {
    fn build(&self, app: &mut App) {
        app.add_startup_system(add_lighting)
            .add_system(assign_parts)
            .add_system(assign_grass)
            .add_system(assign_characters);
    }
}

fn add_lighting(mut commands: Commands) {
    const HALF_SIZE: f32 = PLATFORM_WIDTH_M;
    commands.spawn(DirectionalLightBundle {
        directional_light: DirectionalLight {
            illuminance: 10_000.0,
            shadows_enabled: true,
            shadow_projection: OrthographicProjection {
                left: -HALF_SIZE,
                right: HALF_SIZE,
                bottom: -HALF_SIZE,
                top: HALF_SIZE,
                near: -HALF_SIZE,
                far: HALF_SIZE,
                ..Default::default()
            },
            ..Default::default()
        },
        transform: Transform {
            translation: Vec3::new(0.0, -2.0, 0.0),
            rotation: Quat::from_rotation_x(-std::f32::consts::FRAC_PI_4),
            ..Default::default()
        },
        ..Default::default()
    });
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
        let dims = collider_shape.as_cuboid().unwrap().half_extents();
        commands
            .entity(entity)
            .insert(PbrBundle {
                transform: transform.clone(),
                global_transform: global_transform.clone(),
                mesh: meshes.add(Mesh::from(shape::Box {
                    max_x: dims[0],
                    min_x: -dims[0],
                    max_y: dims[1],
                    min_y: -dims[1],
                    max_z: dims[2],
                    min_z: -dims[2],
                })),
                material: materials.add(StandardMaterial {
                    base_color: Color::rgba(
                        rng.gen_range(COLOR_MIN..=COLOR_MAX),
                        rng.gen_range(COLOR_MIN..=COLOR_MAX),
                        rng.gen_range(COLOR_MIN..=COLOR_MAX),
                        1.0,
                    ),
                    perceptual_roughness: rng.gen_range(0.0..=1.0),
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
            .insert(PbrBundle {
                transform: transform.clone(),
                global_transform: global_transform.clone(),
                mesh: meshes.add(Mesh::from(shape::Icosphere {
                    radius: collider_shape.as_ball().unwrap().radius(),
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
    unassigned: Query<
        (Entity, &Collider, &Transform, &GlobalTransform),
        (With<Grass>, Without<AssignedMaterial>),
    >,
) {
    for (entity, collider_shape, transform, global_transform) in unassigned.iter() {
        commands
            .entity(entity)
            .insert(PbrBundle {
                transform: transform.clone(),
                global_transform: global_transform.clone(),
                mesh: meshes.add(compute_mesh(&collider_shape)),
                material: materials.add(StandardMaterial {
                    base_color_texture: Some(asset_server.load("textures/grass.png")),
                    perceptual_roughness: 1.0,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .insert(AssignedMaterial);
    }
}

fn compute_mesh(shape: &Collider) -> Mesh {
    let mut mesh = Mesh::new(bevy::render::render_resource::PrimitiveTopology::TriangleList);
    let trimesh = shape.as_trimesh().unwrap();
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        VertexAttributeValues::from(
            trimesh
                .vertices()
                .map(|vertex| [vertex.x, vertex.y, vertex.z])
                .collect::<Vec<_>>(),
        ),
    );
    // Compute vertex normals by averaging the normals
    // of every triangle they appear in.
    // NOTE: This is a bit shonky, but good enough for visualisation.
    let verts = trimesh.vertices().collect::<Vec<_>>();
    let mut normals: Vec<Vec3> = vec![Vec3::ZERO; trimesh.vertices().len()];
    for triangle in trimesh.indices().iter() {
        let ab = verts[triangle[1] as usize] - verts[triangle[0] as usize];
        let ac = verts[triangle[2] as usize] - verts[triangle[0] as usize];
        let normal = ab.cross(ac);
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
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, VertexAttributeValues::from(normals));
    // There's nothing particularly meaningful we can do
    // for this one without knowing anything about the overall topology.

    mesh.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        VertexAttributeValues::from(
            trimesh
                .vertices()
                .map(|vertex| {
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
