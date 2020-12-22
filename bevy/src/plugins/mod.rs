pub use self::character::CharacterPlugin;
pub use self::map::MapPlugin;
pub use self::player::PlayerPlugin;
pub use self::ui::UiPlugin;

mod character;
mod map;
mod player;
mod ui;

cfg_if::cfg_if! {
    if #[cfg(not(target_arch = "wasm32"))] {
        mod multiplayer;
        pub use self::multiplayer::{client::ClientPlugin, server::ServerPlugin, MultiplayerPlugins};
    }
}
