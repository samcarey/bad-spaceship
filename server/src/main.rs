use std::time::Duration;

use bad_spaceship_shared::{character, player, CommonPlugins};
use bevy::{app::ScheduleRunnerPlugin, asset::AssetPlugin, prelude::*};

mod net;

fn main() {
    // Bevy resolves its asset root from `BEVY_ASSET_ROOT`, else `CARGO_MANIFEST_DIR`
    // (set by `cargo run`), else the *executable's* directory — never the working
    // directory. The deployed server runs as a bare binary in `bin/` under launchd
    // with its `WorkingDirectory` at the server crate, so without this it looks for
    // assets next to the binary and silently fails to load the character `Config`
    // (its size/speed/jump) — which the server now needs to simulate characters.
    // Anchor the root at the working directory so `../client/assets` resolves the
    // same way `cargo run` does.
    if std::env::var_os("BEVY_ASSET_ROOT").is_none() {
        if let Ok(cwd) = std::env::current_dir() {
            std::env::set_var("BEVY_ASSET_ROOT", cwd);
        }
    }
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
    // Forward slashes (the old `..\assets\…` backslash paths are treated as a
    // single literal filename on macOS native — the server box — so they silently
    // failed; it never mattered until the server began simulating characters, which
    // need the character `Config` via `build_server_avatar`). Relative to the asset
    // root (`../client/assets`).
    *handle = Some(asset_server.load("config/character.character.ron"));
    *handle2 = Some(asset_server.load("config/player.player.ron"));

    // TODO: Fix this
    // Theoretically this should work instead of the above, but it doesn't...
    // *handles = Some(asset_server.load_folder("config").unwrap());
}
