use std::time::Duration;

use bad_spaceship_shared::{character, player};
use bevy::{
    app::ScheduleRunnerSettings,
    asset::{AssetPlugin, AssetServerSettings},
    prelude::*,
};

fn main() {
    App::build()
        .insert_resource(ScheduleRunnerSettings::run_loop(Duration::from_secs_f64(
            1.0 / 60.,
        )))
        .add_plugins(MinimalPlugins)
        .add_plugin(AssetPlugin)
        .insert_resource(AssetServerSettings {
            asset_folder: "../client/assets".to_string(),
        })
        .add_startup_system(load_configs.system())
        .run();
}

fn load_configs(
    asset_server: Res<AssetServer>,
    // TODO: Fix this
    // mut handles: Local<Option<Vec<HandleUntyped>>>,
    mut handle: Local<Option<Handle<character::Config>>>,
    mut handle2: Local<Option<Handle<player::Config>>>,
) {
    // We're not going to use these handles,
    // but we need to store them or else the assets will be dropped
    *handle = Some(asset_server.load("..\\assets\\config\\character.ron"));
    *handle2 = Some(asset_server.load("..\\..\\assets\\config\\player.ron"));

    // TODO: Fix this
    // Theoretically this should work instead of the above, but it doesn't...
    // *handles = Some(asset_server.load_folder("config").unwrap());

    asset_server.watch_for_changes().unwrap();
}
