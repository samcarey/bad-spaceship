use bevy::prelude::*;
use bevy::render::pass::ClearColor;
use bevy_rapier3d::{physics::RapierPhysicsPlugin, render::RapierRenderPlugin};
mod plugins;
mod utils;

fn main() {
    simple_logger::SimpleLogger::from_env()
        .init()
        .expect("A logger was already initialized");
    let args = utils::parse_args();

    App::build()
        .add_resource(args)
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
        .add_plugin(plugins::UiPlugin)
        .add_plugins_with(plugins::MultiplayerPlugins, |group| {
            if args.is_server {
                group.disable::<plugins::ClientPlugin>()
            } else {
                group.disable::<plugins::ServerPlugin>()
            }
        })
        .run();
}
