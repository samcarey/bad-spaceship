use bad_spaceship_shared::{
    character, CommonPlugins, OrbitingCamera, Player, PlayerCameraOrbitCenter,
};
pub mod highlight;
use bad_spaceship_shared::player;
use bevy::pbr::AmbientLight;
use bevy::prelude::*;
use bevy::render::camera::Camera;

use highlight::HighlightPlugin;
use input::InputPlugin;
use platform::PlatformPlugin;
use render_main_pass::RenderMainPassPlugin;
use render_secondary_pass::RenderSecondaryPassPlugin;
use ui::UiPlugin;

mod input;
mod platform;
mod render_main_pass;
mod render_secondary_pass;
mod ui;

#[bevy_main]
fn main() {
    let mut app = App::new();

    app.insert_resource(AmbientLight {
        color: Color::WHITE,
        // Bevy 0.13's lighting overhaul made AmbientLight brightness a physical
        // lux value (default 80.0). The old 0.12 fill (~1/6 of full white) maps to
        // ~600 lux under 0.13's default camera exposure; the engine default of 80
        // left shadowed faces far too dark against the 10_000-lux directional.
        brightness: 600.0,
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
            PlatformPlugin,
            HighlightPlugin,
            RenderMainPassPlugin,
            RenderSecondaryPassPlugin,
            CommonPlugins,
        ))
        .insert_resource(ClearColor(Color::rgb(0.99, 0.99, 0.95)))
        .add_systems(Startup, load_configs)
        .add_systems(Update, add_camera_to_player);

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
    *handle = Some(asset_server.load("config\\character.character.ron"));
    *handle2 = Some(asset_server.load("config\\player.player.ron"));

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
                .push_children(&[camera_entity]);
        }
    }
}
