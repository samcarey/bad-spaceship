use std::time::Duration;

use bad_spaceship_shared::{character, player, CommonPlugins};
use bevy::{app::ScheduleRunnerPlugin, asset::AssetPlugin, prelude::*};

mod net;

fn main() {
    let mut app = App::new();
    app
        // Bevy 0.11 merged ScheduleRunnerSettings into ScheduleRunnerPlugin;
        // override the one MinimalPlugins adds to keep the fixed 60 Hz loop.
        .add_plugins(MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(
            Duration::from_secs_f64(1.0 / 60.),
        )))
        // AssetServerSettings was folded into AssetPlugin in Bevy 0.9; 0.12
        // renamed `asset_folder` to `file_path` and swapped the `ChangeWatcher`
        // for a simple `watch_for_changes_override` flag.
        .add_plugins(AssetPlugin {
            file_path: "../client/assets".to_string(),
            watch_for_changes_override: Some(true),
            ..default()
        })
        .add_plugins(CommonPlugins)
        .add_systems(Startup, load_configs);

    // Opt-in multiplayer host: set BS_MULTIPLAYER to run as the authoritative
    // netcode server. Unset → the headless single-player sim, unchanged.
    if std::env::var("BS_MULTIPLAYER").is_ok() {
        app.add_plugins(net::NetServerPlugin);
    }

    app.run();
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
    *handle = Some(asset_server.load("..\\assets\\config\\character.character.ron"));
    *handle2 = Some(asset_server.load("..\\..\\assets\\config\\player.player.ron"));

    // TODO: Fix this
    // Theoretically this should work instead of the above, but it doesn't...
    // *handles = Some(asset_server.load_folder("config").unwrap());
}
