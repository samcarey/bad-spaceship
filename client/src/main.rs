use bad_spaceship_shared::{
    character, CommonPlugins, OrbitingCamera, Player, PlayerCameraOrbitCenter,
};
pub mod highlight;
use bad_spaceship_shared::player;
use bevy::asset::ChangeWatcher;
use bevy::pbr::AmbientLight;
use bevy::prelude::*;
use bevy::render::camera::Camera;
use std::time::Duration;

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
        brightness: 1.0 / 6.0,
    });

    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Bad Spaceship".to_string(),
                    // Resize the WASM canvas to fill its parent (the browser
                    // viewport); replaces the old `bevy_web_fullscreen` plugin.
                    fit_canvas_to_parent: true,
                    ..default()
                }),
                ..default()
            })
            // Hot-reload config RON assets. Bevy 0.11 replaced the
            // `watch_for_changes: bool` flag with an optional debounced watcher.
            .set(AssetPlugin {
                watch_for_changes: ChangeWatcher::with_delay(Duration::from_millis(200)),
                ..default()
            }),
    )
        .add_state::<AppState>()
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
    *handle = Some(asset_server.load("config\\character.ron"));
    *handle2 = Some(asset_server.load("config\\player.ron"));

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
