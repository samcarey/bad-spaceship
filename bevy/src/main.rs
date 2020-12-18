use bevy::render::pass::ClearColor;
use bevy::{app::ScheduleRunnerSettings, prelude::*};
use bevy_rapier3d::{physics::RapierPhysicsPlugin, render::RapierRenderPlugin};
mod plugins;
mod utils;
use std::time::Duration;

fn main() {
    simple_logger::SimpleLogger::from_env()
        .init()
        .expect("A logger was already initialized");
    let args = utils::parse_args();

    let mut app = App::build();

    if args.is_server {
        app.add_resource(ScheduleRunnerSettings::run_loop(Duration::from_secs_f64(
            1.0,
        )))
        .add_plugins(MinimalPlugins);
    } else {
        app.add_plugins(DefaultPlugins)
            .add_plugin(plugins::UiPlugin)
            .add_plugin(RapierRenderPlugin)
            .add_resource(ClearColor(Color::rgb(
                0xF9 as f32 / 255.0,
                0xF9 as f32 / 255.0,
                0xFF as f32 / 255.0,
            )))
            .add_plugin(RapierPhysicsPlugin)
            .add_plugin(plugins::MapPlugin)
            .add_plugin(plugins::PlayerPlugin);
    }

    app.add_resource(args)
        .add_plugins_with(plugins::MultiplayerPlugins, |group| {
            if args.is_server {
                group.disable::<plugins::ClientPlugin>()
            } else {
                group.disable::<plugins::ServerPlugin>()
            }
        });

    app.run();
}
