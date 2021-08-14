use bevy::render::pass::ClearColor;
use bevy::{app::ScheduleRunnerSettings, prelude::*};
use bevy_rapier3d::physics::NoUserData;
use bevy_rapier3d::{physics::RapierPhysicsPlugin, render::RapierRenderPlugin};

#[cfg(target_arch = "wasm32")]
use bevy_rapier3d::prelude::IntegrationParameters;
#[cfg(target_arch = "wasm32")]
use bevy_web_fullscreen::FullViewportPlugin;

use std::time::Duration;
#[macro_use]
mod utils;

mod plugins;

#[bevy_main]
fn main() {
    let args = utils::parse_args();

    let mut app = App::build();

    if args.is_server {
        app.insert_resource(ScheduleRunnerSettings::run_loop(Duration::from_secs_f64(
            1.0 / 60.,
        )))
        .add_plugins(MinimalPlugins);
    } else {
        #[cfg(target_arch = "wasm32")]
        app.add_plugins(bevy_webgl2::DefaultPlugins);
        #[cfg(not(target_arch = "wasm32"))]
        app.add_plugins(DefaultPlugins);
        app.add_state(AppState::Initial)
            .add_plugin(plugins::UiPlugin)
            .add_plugin(RapierPhysicsPlugin::<NoUserData>::default())
            .add_plugin(RapierRenderPlugin)
            .insert_resource(ClearColor(Color::rgb(0.99, 0.99, 0.95)))
            .add_plugins(plugins::EnvironmentPluginGroup)
            .add_plugin(plugins::PlayerPlugin);
        #[cfg(target_arch = "wasm32")]
        app.add_startup_system(set_initial_fps.system())
            .add_plugin(FullViewportPlugin);
    }

    app.insert_resource(args);

    app.run();
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum AppState {
    Initial,
    InGame,
    InGameMenu,
}

pub const APP_STATE: &str = "app_state";

#[cfg(target_arch = "wasm32")]
const CONFIG_DIR: include_dir::Dir = include_dir::include_dir!("assets/config");

#[cfg(target_arch = "wasm32")]
fn set_initial_fps(mut integration_parameters: ResMut<IntegrationParameters>) {
    integration_parameters.dt = 1.0 / 30.0;
}
