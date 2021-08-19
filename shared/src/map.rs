use bevy::prelude::*;
use bevy_rapier3d::na::Point3;
use bevy_rapier3d::physics::ColliderPositionSync;
use bevy_rapier3d::prelude::RigidBodyMassProps;
use bevy_rapier3d::render::ColliderDebugRender;
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

fn spawn_map(mut commands: Commands) {
    // Create a bowl with a cosine cross-section
    let mut vertices: Vec<Point3<f32>> = Vec::new();
    let mut indices: Vec<[u32; 3]> = Vec::new();
    let segments = 32;
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
            vertices.push(Point3::new(x, y, z));
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
    let collider = ColliderBundle {
        shape: ColliderShape::trimesh(vertices, indices),
        ..Default::default()
    };

    commands
        .spawn()
        .insert_bundle(rigid_body)
        .insert_bundle(collider)
        .insert_bundle((
            ColliderDebugRender::default(),
            ColliderPositionSync::Discrete,
        ));
}
