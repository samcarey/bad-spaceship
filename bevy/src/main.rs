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
        .add_plugins(DefaultPlugins)
        .add_plugin(RapierPhysicsPlugin)
        .add_plugin(RapierRenderPlugin)
        .add_plugin(plugins::MapPlugin)
        .add_plugin(plugins::PlayerPlugin)
        .run();
}
