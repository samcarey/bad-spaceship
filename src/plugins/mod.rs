pub use self::character::CharacterPlugin;
pub use self::environment::EnvironmentPluginGroup;
pub use self::player::PlayerPlugin;
pub use self::ui::UiPlugin;

mod character;
mod environment;
mod player;
mod ui;

// #[cfg(not(target_arch = "wasm32"))]
// mod multiplayer;
// #[cfg(not(target_arch = "wasm32"))]
// pub use self::multiplayer::{client::ClientPlugin, server::ServerPlugin, MultiplayerPlugins};
