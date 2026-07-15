use bad_spaceship_shared::{
    character, CommonPlugins, OrbitingCamera, Player, PlayerCameraOrbitCenter,
};
use bad_spaceship_shared::player;
// Bevy 0.17 split the renderer into focused crates: light types moved from
// `bevy_pbr` to `bevy_light` (facade `bevy::light`). Bevy 0.18 then split the
// scene-wide ambient light off the `AmbientLight` *component* into a dedicated
// `GlobalAmbientLight` *resource* (the component now only overrides per-camera).
use bevy::light::GlobalAmbientLight;
use bevy::prelude::*;

use gamepad::GamepadPlugin;
use input::InputPlugin;
use launch::LaunchPlugin;
use mobile::MobilePlugin;
use platform::PlatformPlugin;
use render_main_pass::RenderMainPassPlugin;
use render_secondary_pass::RenderSecondaryPassPlugin;
use ui::UiPlugin;

mod gamepad;
mod input;
mod launch;
mod mobile;
mod monster;
mod net;
mod outline;
mod planet;
mod platform;
mod render_main_pass;
mod render_secondary_pass;
mod trajectory;
mod ui;

#[bevy_main]
fn main() {
    // TEMPORARY: capture wasm client panics into localStorage (forwarded to the server
    // on next connect) so a browser-console-only crash is visible from the build box.
    // Installed before anything can panic. No-op on native (stderr is readable). Remove
    // with `ClientPanicReport`.
    platform::install_panic_hook();

    let mut app = App::new();

    app.insert_resource(GlobalAmbientLight {
        // Warm, sooty fill to match the ash sky — the whole scene sits in a dull
        // reddish-orange bounce, as if the light is the volcano's glow scattered
        // through the haze rather than clean daylight.
        color: Color::srgb(0.85, 0.42, 0.30),
        // Bevy 0.13's lighting overhaul made ambient brightness a physical lux
        // value (default 80.0). With the directional sun now faint and diffuse
        // (see render_main_pass), the ambient carries more of the scene, so it's
        // nudged up from the old 360 fill to keep shadowed faces readable under
        // the tinted, dimmer key light.
        brightness: 500.0,
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
            outline::OutlinePlugin,
            planet::PlanetPlugin,
            LaunchPlugin,
            trajectory::TrajectoryPlugin,
            RenderMainPassPlugin,
            monster::MonsterPlugin,
            RenderSecondaryPassPlugin,
            CommonPlugins,
        ))
        // Dark, reddish-grey ash sky — the murk of a nearby erupting volcano, the
        // air thick with soot lit a dull ember-red. Replaces the old near-white sky.
        .insert_resource(ClearColor(Color::srgb(0.17, 0.125, 0.115)))
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
    /// The cursor-free state: mouse-look released so the on-screen controls (the
    /// top-left hamburger etc.) are clickable. Toggled by Escape (desktop), the
    /// browser pointer-lock exit (web), and the gamepad Start button. The center
    /// pause-menu window this state used to draw was deliberately removed — its
    /// remaining job is cursor/input management; the name is historical.
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
