use bad_spaceship_shared::map::MapPlugin;
use bad_spaceship_shared::part::PartPlugin;
use bad_spaceship_shared::player::PlayerPlugin;
use bevy::prelude::*;
use bevy::render::pass::ClearColor;
use bevy_rapier3d::physics::NoUserData;
use bevy_rapier3d::{physics::RapierPhysicsPlugin, render::RapierRenderPlugin};

#[cfg(target_arch = "wasm32")]
use bevy_rapier3d::prelude::IntegrationParameters;
#[cfg(target_arch = "wasm32")]
use bevy_web_fullscreen::FullViewportPlugin;
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
        .add_plugin(PlatformPlugin);
    #[cfg(target_arch = "wasm32")]
    app.add_startup_system(set_initial_fps.system())
        .add_plugin(FullViewportPlugin);

    app.run();
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum AppState {
    Initial,
    InGame,
    InGameMenu,
}

pub const APP_STATE: &str = "app_state";

#[cfg(target_arch = "wasm32")]
fn set_initial_fps(mut integration_parameters: ResMut<IntegrationParameters>) {
    integration_parameters.dt = 1.0 / 30.0;
}
