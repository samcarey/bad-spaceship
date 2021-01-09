use bevy::render::pass::ClearColor;
use bevy::{app::ScheduleRunnerSettings, prelude::*};
use bevy_rapier3d::{physics::RapierPhysicsPlugin, render::RapierRenderPlugin};
#[cfg(target_arch = "wasm32")]
use rapier3d::dynamics::IntegrationParameters;
use std::time::Duration;
#[macro_use]
mod utils;

mod plugins;

#[bevy_main]
fn main() {
    let args = utils::parse_args();

    let mut app = App::build();

    if args.is_server {
        app.add_resource(ScheduleRunnerSettings::run_loop(Duration::from_secs_f64(
            1.0 / 60.,
        )))
        .add_plugins(MinimalPlugins);
    } else {
        #[cfg(target_arch = "wasm32")]
        app.add_plugins(bevy_webgl2::DefaultPlugins);
        #[cfg(not(target_arch = "wasm32"))]
        app.add_plugins(DefaultPlugins);
        app.add_resource(State::new(AppState::Initial))
            .add_stage_after(stage::UPDATE, APP_STATE, StateStage::<AppState>::default())
            .add_plugin(plugins::UiPlugin)
            .add_plugin(RapierPhysicsPlugin)
            .add_plugin(RapierRenderPlugin)
            .add_resource(ClearColor(Color::rgb(
                0xF9 as f32 / 255.0,
                0xF9 as f32 / 255.0,
                0xFF as f32 / 255.0,
            )))
            .add_plugin(plugins::MapPlugin)
            .add_plugin(plugins::PlayerPlugin);
        #[cfg(target_arch = "wasm32")]
        app.add_startup_system(set_initial_fps.system());
    }

    app.add_resource(args);

    app.run();
}
#[derive(Clone)]
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
    integration_parameters.set_dt(1.0 / 30.0);
}
