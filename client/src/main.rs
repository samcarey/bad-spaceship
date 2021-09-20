use bad_spaceship_shared::character;
use bad_spaceship_shared::config::ConfigPlugin;
use bad_spaceship_shared::contact::ContactPlugin;
pub mod highlight;
use bad_spaceship_shared::map::MapPlugin;
use bad_spaceship_shared::part::PartPlugin;
use bad_spaceship_shared::player::{self, PlayerPlugin};
use bevy::prelude::*;
use bevy::render::pass::ClearColor;
use bevy_rapier3d::physics::NoUserData;
use bevy_rapier3d::{physics::RapierPhysicsPlugin, render::RapierRenderPlugin};

#[cfg(target_arch = "wasm32")]
use bevy_web_fullscreen::FullViewportPlugin;
use highlight::HighlightPlugin;
use input::InputPlugin;
use platform::PlatformPlugin;
use ui::UiPlugin;

mod input;
mod platform;
mod ui;

#[bevy_main]
fn main() {
    let mut app = App::build();

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
        .add_plugin(RapierPhysicsPlugin::<NoUserData>::default())
        .add_plugin(RapierRenderPlugin)
        .insert_resource(ClearColor(Color::rgb(0.99, 0.99, 0.95)))
        .add_plugin(MapPlugin)
        .add_plugin(PartPlugin)
        .add_plugin(PlayerPlugin)
        .add_plugin(PlatformPlugin)
        .add_plugin(ConfigPlugin)
        .add_plugin(ContactPlugin)
        .add_plugin(HighlightPlugin)
        .add_startup_system(load_configs.system());
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
