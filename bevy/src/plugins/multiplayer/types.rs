use bevy::prelude::*;
use serde::{Deserialize, Serialize};

pub const SERVER_PORT: u16 = 14192;
pub const BOARD_WIDTH: u32 = 1000;
pub const BOARD_HEIGHT: u32 = 1000;

pub struct Pawn {
    pub controller: u32,
}
pub struct Ball {
    pub velocity: Vec3,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClientMessage {
    Hello(String),
    Direction(Direction),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GameStateMessage {
    pub frame: u32,
    pub balls: Vec<(u32, Vec3, Vec3)>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Bundle)]
pub struct Player {
    id: u32,
    transform: Mat4,
}

// #[derive(Serialize, Deserialize, Debug, Clone)]
// pub struct ClientMessage {
//     pub player: Player,
// }

// #[derive(Serialize, Deserialize, Debug, Clone)]
// pub struct GameStateMessage {
//     pub frame: u32,
//     pub players: Vec<Player>,
// }

#[derive(Default)]
pub struct NetworkBroadcast {
    pub frame: u32,
}
