use bevy::render::pass::ClearColor;
use bevy::{app::ScheduleRunnerSettings, prelude::*};
use bevy_rapier3d::{physics::RapierPhysicsPlugin, render::RapierRenderPlugin};
use std::time::Duration;
#[macro_use]
mod utils;

mod plugins;

#[bevy_main]
fn main() {
    // simple_logger::SimpleLogger::from_env()
    //     .init()
    //     .expect("A logger was already initialized");
    let args = utils::parse_args();

    let mut app = App::build();

    if args.is_server {
        app.add_resource(ScheduleRunnerSettings::run_loop(Duration::from_secs_f64(
            1.0 / 60.,
        )))
        .add_plugins(MinimalPlugins);
    } else {
        app.add_plugins(DefaultPlugins);
        #[cfg(target_arch = "wasm32")]
        app.add_plugin(bevy_webgl2::WebGL2Plugin);
        app.add_resource(State::new(AppState::InGame))
            .add_stage_after(stage::UPDATE, APP_STATE, StateStage::<AppState>::default())
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

    app.add_resource(args);

    cfg_if::cfg_if! {
        if #[cfg(not(target_arch = "wasm32"))] {
            // app.add_plugins_with(plugins::MultiplayerPlugins, |group| {
            //     if args.is_server {
            //         group.disable::<plugins::ClientPlugin>()
            //     } else {
            //         group.disable::<plugins::ServerPlugin>()
            //     }
            // })
        }
    }
    //
;

    app.run();
}

#[derive(Clone)]
pub enum AppState {
    InGame,
    InGameMenu,
}

pub const APP_STATE: &str = "app_state";

#[cfg(target_arch = "wasm32")]
const CONFIG_DIR: include_dir::Dir = include_dir::include_dir!("assets/config");
