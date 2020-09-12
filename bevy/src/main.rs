use bevy::prelude::*;
use bevy::render::pass::ClearColor;
use bevy_rapier3d::{physics::RapierPhysicsPlugin, render::RapierRenderPlugin};
mod plugins;

fn main() {
    App::build()
        .add_resource(ClearColor(Color::rgb(
            0xF9 as f32 / 255.0,
            0xF9 as f32 / 255.0,
            0xFF as f32 / 255.0,
        )))
        .add_default_plugins()
        .add_plugin(RapierPhysicsPlugin)
        .add_plugin(RapierRenderPlugin)
        .add_plugin(plugins::MapPlugin)
        .add_startup_system(setup_graphics.system())
        .add_plugin(plugins::PlayerPlugin)
        .run();
}

fn setup_graphics(mut commands: Commands) {
    commands
        .spawn(LightComponents {
            translation: Translation::new(1000.0, 100.0, 2000.0),
            ..Default::default()
        })
        .spawn(Camera3dComponents {
            transform: Transform::new_sync_disabled(Mat4::face_toward(
                Vec3::new(-30.0, 30.0, 100.0),
                Vec3::new(0.0, 10.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            )),
            ..Default::default()
        });
}
