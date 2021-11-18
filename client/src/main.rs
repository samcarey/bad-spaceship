use bad_spaceship_shared::{
    character, CommonPlugins, OrbitingCamera, Player, PlayerCameraOrbitCenter,
};
pub mod highlight;
use bad_spaceship_shared::player;
use bevy::pbr::AmbientLight;
use bevy::prelude::*;
use bevy::render::camera::Camera;
use bevy::render::pass::ClearColor;

use bevy_rapier3d::render::RapierRenderPlugin;
#[cfg(target_arch = "wasm32")]
use bevy_web_fullscreen::FullViewportPlugin;
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
    let mut app = App::build();

    app.insert_resource(AmbientLight {
        color: Color::WHITE,
        brightness: 1.0 / 6.0,
    });

    app.insert_resource(WindowDescriptor {
        title: "Bad Spaceship".to_string(),
        ..Default::default()
    });

    #[cfg(target_arch = "wasm32")]
    app.add_plugins(bevy_webgl2::DefaultPlugins);
    #[cfg(not(target_arch = "wasm32"))]
    app.add_plugins(DefaultPlugins);

    app.add_state(AppState::Initial)
        .add_plugin(UiPlugin)
        .add_plugin(InputPlugin)
        .insert_resource(ClearColor(Color::rgb(0.99, 0.99, 0.95)))
        .add_plugin(PlatformPlugin)
        .add_plugin(HighlightPlugin)
        .add_plugin(RenderMainPassPlugin)
        .add_plugin(RenderSecondaryPassPlugin)
        .add_plugins(CommonPlugins)
        .add_startup_system(load_configs.system())
        .add_system(add_camera_to_player.system());

    app.add_plugin(RapierRenderPlugin);

    #[cfg(target_arch = "wasm32")]
    app.add_plugin(FullViewportPlugin);

    app.run();
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum AppState {
    Initial,
    InGame,
    InGameMenu,
}

pub const APP_STATE: &str = "app_state";

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

    asset_server.watch_for_changes().unwrap();
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
