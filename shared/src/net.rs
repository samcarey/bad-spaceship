//! Multiplayer networking protocol (lightyear).
//!
//! This module defines the *protocol* — the components, channels, and input
//! types the client and server agree on. It deliberately does NOT add
//! lightyear's `ClientPlugins` / `ServerPlugins`: those live in the client and
//! server crates respectively, and lightyear requires the protocol to be
//! registered *after* the relevant plugin group. `ProtocolPlugin` is added by
//! both binaries after their plugin group.
//!
//! Scaffold status: this registers a representative component + channel and
//! defines the per-tick input type, proving the lightyear 0.27 dependency builds
//! into every target (native + wasm). Wiring the live connection, replicating
//! the Avian physics bodies (via `lightyear_avian3d`), and client-side
//! prediction/interpolation come next, where they can be tested on real
//! endpoints.
use bevy::prelude::*;
use lightyear::prelude::*;
use serde::{Deserialize, Serialize};

/// Identifies which connected client controls a replicated player entity.
#[derive(Component, Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct NetPlayer {
    pub client_id: u64,
}

/// Per-tick player intent, sent client → server. Registered as a networked
/// input once the client/server input plugins are wired up (Phase 2b).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default)]
pub struct PlayerInput {
    pub move_dir: Vec2,
    pub yaw: f32,
    pub pitch: f32,
    pub jump: bool,
    pub grab: bool,
    pub modifying: bool,
}

/// Registers the shared protocol. Add to BOTH the client and server apps, AFTER
/// their respective lightyear plugin group.
pub struct ProtocolPlugin;

impl Plugin for ProtocolPlugin {
    fn build(&self, app: &mut App) {
        // lightyear 0.27 builder API (the older `register_component` is
        // deprecated). `.replicate()` marks the component for World replication.
        app.component::<NetPlayer>().replicate();
    }
}
