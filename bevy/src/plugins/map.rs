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
const PLATFORM_THICKNESS_M: f32 = 0.1; // meters

fn add_lighting(mut commands: Commands) {
    commands.spawn(LightComponents {
        translation: Translation::new(0.0, 8.0, 0.0), // meters
        ..Default::default()
    });
}

fn add_platform(mut commands: Commands) {
    let rigid_body = RigidBodyBuilder::new_static().translation(0.0, 0.0, 0.0);
    let collider = ColliderBuilder::cuboid(
        PLATFORM_WIDTH_M / 2.0,
        PLATFORM_THICKNESS_M / 2.0,
        PLATFORM_WIDTH_M / 2.0,
    );
    commands.spawn((rigid_body, collider));
}
