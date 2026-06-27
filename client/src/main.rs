use bad_spaceship_shared::{
    character, CommonPlugins, OrbitingCamera, Player, PlayerCameraOrbitCenter,
};
pub mod highlight;
use bad_spaceship_shared::player;
// Bevy 0.17 split the renderer into focused crates: light types moved from
// `bevy_pbr` to `bevy_light` (facade `bevy::light`). Bevy 0.18 then split the
// scene-wide ambient light off the `AmbientLight` *component* into a dedicated
// `GlobalAmbientLight` *resource* (the component now only overrides per-camera).
use bevy::light::GlobalAmbientLight;
use bevy::prelude::*;

use gamepad::GamepadPlugin;
use highlight::HighlightPlugin;
use input::InputPlugin;
use mobile::MobilePlugin;
use platform::PlatformPlugin;
use render_main_pass::RenderMainPassPlugin;
use render_secondary_pass::RenderSecondaryPassPlugin;
use ui::UiPlugin;

mod gamepad;
mod input;
mod mobile;
mod net;
mod platform;
mod render_main_pass;
mod render_secondary_pass;
mod ui;

#[bevy_main]
fn main() {
    let mut app = App::new();

    app.insert_resource(GlobalAmbientLight {
        color: Color::WHITE,
        // Bevy 0.13's lighting overhaul made ambient brightness a physical
        // lux value (default 80.0). The old 0.12 fill (~1/6 of full white) was
        // first remapped to ~600 lux, but with the directional sun that left the
        // whole scene reading brighter than 0.12; scaled to 360 (~0.6x, in step
        // with the directional in render_main_pass.rs) to dim back toward the
        // 0.12 look while keeping shadowed faces off pure black.
        brightness: 360.0,
        // Bevy 0.16 added a mixed-lighting field to every light; we use no
        // lightmaps, so the default (`true`) is correct.
        ..default()
    });

    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Bad Spaceship".to_string(),
                    // Bevy 0.13 removed `Window::fit_canvas_to_parent`; the WASM
                    // canvas is now sized to the viewport via CSS in index.html
                    // (`canvas { width/height: 100% }`).
                    ..default()
                }),
                ..default()
            })
            // Hot-reload config RON assets. Bevy 0.12's asset rework replaced the
            // debounced `ChangeWatcher` with a simple override flag; the actual
            // watcher is only active when the `file_watcher` feature is on (native).
            .set(AssetPlugin {
                watch_for_changes_override: Some(true),
                ..default()
            }),
    )
        .init_state::<AppState>()
        .add_plugins((
            UiPlugin,
            InputPlugin,
            MobilePlugin,
            GamepadPlugin,
            PlatformPlugin,
            HighlightPlugin,
            RenderMainPassPlugin,
            RenderSecondaryPassPlugin,
            CommonPlugins,
        ))
        .insert_resource(ClearColor(Color::srgb(0.99, 0.99, 0.95)))
        .add_systems(Startup, load_configs)
        .add_systems(Update, add_camera_to_player);

    // Avian physics — with the multiplayer transform-sync handling disabled when
    // we're connecting (so `lightyear_avian3d` can own it). Must precede
    // `NetClientPlugin` (which adds `LightyearAvianPlugin`).
    let multiplayer = net::multiplayer_target().is_some();
    bad_spaceship_shared::add_physics(&mut app, multiplayer);

    // Opt-in multiplayer: when a connect target is configured, add the netcode
    // client. Otherwise the app is the unchanged single-player game.
    if multiplayer {
        app.add_plugins(net::NetClientPlugin);
    }

    app.run();
}

#[derive(States, Default, Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum AppState {
    #[default]
    Initial,
    InGame,
    InGameMenu,
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
    // Forward slashes: valid on both the native filesystem and in the wasm
    // asset-fetch URL. (The old Windows-style backslashes only "worked" on wasm
    // because browsers normalise `\`→`/` in URLs; on native macOS the backslash is
    // a literal filename byte, so the config never loaded and no character body
    // could be assembled.)
    *handle = Some(asset_server.load("config/character.character.ron"));
    *handle2 = Some(asset_server.load("config/player.player.ron"));

    // TODO: Fix this
    // Theoretically this should work instead of the above, but it doesn't...
    // *handles = Some(asset_server.load_folder("config").unwrap());
}

fn add_camera_to_player(
    mut commands: Commands,
    cameras: Query<Entity, With<Camera>>,
    players: Query<(Entity, &PlayerCameraOrbitCenter), (With<Player>, Without<OrbitingCamera>)>,
) {
    if let Some(camera_entity) = cameras.iter().next() {
        if let Some((player, camera_orbit_center)) = players.iter().next() {
            commands
                .entity(player)
                .insert(OrbitingCamera(camera_entity));
            commands
                .entity(camera_orbit_center.0)
                .add_children(&[camera_entity]);
        }
    }
}
