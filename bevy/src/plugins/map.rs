use bevy::prelude::*;
use bevy_rapier3d::rapier::dynamics::RigidBodyBuilder;
use bevy_rapier3d::rapier::geometry::ColliderBuilder;
pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app
            // .add_startup_system(add_camera.system())
            .add_startup_system(add_lighting.system())
            .add_startup_system(add_platform.system());
    }
}

const PLATFORM_SIZE_M: f32 = 15.0; // meters
const PLATFORM_HEIGHT_M: f32 = 0.1; // meters

fn add_lighting(mut commands: Commands) {
    commands.spawn(LightComponents {
        translation: Translation::new(4.0, 8.0, 4.0), // meters
        ..Default::default()
    });
}

fn add_platform(mut commands: Commands) {
    let rigid_body = RigidBodyBuilder::new_static().translation(0.0, -PLATFORM_HEIGHT_M, 0.0);
    let collider =
        ColliderBuilder::cuboid(PLATFORM_SIZE_M, PLATFORM_HEIGHT_M, PLATFORM_SIZE_M).friction(1.0);
    commands.spawn((rigid_body, collider));
}
