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

const PLATFORM_WIDTH_M: f32 = 50.0; // meters
const PLATFORM_THICKNESS_M: f32 = 1.0; // meters

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
}
