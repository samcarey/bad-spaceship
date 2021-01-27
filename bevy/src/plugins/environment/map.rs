use bevy::prelude::*;
use bevy_rapier3d::rapier::dynamics::RigidBodyBuilder;
use bevy_rapier3d::rapier::geometry::ColliderBuilder;
pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(add_lighting.system())
            .add_startup_system(add_platform.system());
    }
}

pub const PLATFORM_WIDTH_M: f32 = 50.0; // meters
pub const PLATFORM_THICKNESS_M: f32 = 1.0; // meters

fn add_lighting(commands: &mut Commands) {
    commands.spawn(LightBundle {
        transform: Transform::from_translation(Vec3::new(0.0, 8.0, 0.0)), // meters
        ..Default::default()
    });
}

fn add_platform(commands: &mut Commands) {
    let platform_rigid_body = RigidBodyBuilder::new_static().translation(0.0, 0.0, 0.0);
    let platform_collider = ColliderBuilder::cuboid(
        PLATFORM_WIDTH_M / 2.0,
        PLATFORM_THICKNESS_M / 2.0,
        PLATFORM_WIDTH_M / 2.0,
    );

    commands.spawn((platform_rigid_body, platform_collider));

    let mut camera_transform = Transform::from_translation(Vec3::new(
        PLATFORM_WIDTH_M * 2.,
        PLATFORM_WIDTH_M * 2.,
        PLATFORM_WIDTH_M * 2.,
    ));
    let look = Mat4::face_toward(camera_transform.translation, Vec3::zero(), Vec3::unit_y());
    camera_transform.rotation = look.to_scale_rotation_translation().1;
    commands.spawn(Camera3dBundle {
        transform: camera_transform,
        ..Default::default()
    });
}
