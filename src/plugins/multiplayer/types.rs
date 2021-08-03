use bevy::prelude::*;
use serde::{Deserialize, Serialize};

pub const SERVER_PORT: u16 = 14192;

#[derive(Serialize, Deserialize, Debug, Clone, Bundle)]
pub struct SerializablePlayer {
    pub id: u32,
    pub transform: Mat4,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClientMessage {
    pub player: SerializablePlayer,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GameStateMessage {
    pub frame: u32,
    pub game_state: GameState,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct GameState {
    pub players: Vec<SerializablePlayer>,
}

#[derive(Default)]
pub struct NetworkBroadcast {
    pub frame: u32,
}
