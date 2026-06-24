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
use bevy::ecs::entity::{EntityMapper, MapEntities};
use bevy::prelude::*;
use lightyear::prelude::*;
use serde::{Deserialize, Serialize};

/// Identifies which connected client controls a replicated player entity.
#[derive(Component, Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct NetPlayer {
    pub client_id: u64,
}

/// Replicated pose. Bevy's `Transform` isn't `Serialize`, and lightyear's
/// `.replicate()` requires it, so we replicate this plain-`f32` mirror instead
/// and map it to/from `Transform` on each side (server writes it from the
/// authoritative sim; the client applies it to the rendered entity).
#[derive(Component, Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default)]
pub struct NetTransform {
    pub translation: [f32; 3],
    /// Rotation quaternion, `[x, y, z, w]`.
    pub rotation: [f32; 4],
}

impl NetTransform {
    pub fn from_transform(t: &Transform) -> Self {
        Self {
            translation: t.translation.to_array(),
            rotation: t.rotation.to_array(),
        }
    }

    pub fn to_transform(&self) -> Transform {
        Transform {
            translation: Vec3::from_array(self.translation),
            rotation: Quat::from_array(self.rotation),
            ..default()
        }
    }
}

/// Per-tick player intent, sent client → server as a lightyear native input
/// (registered via `InputPlugin::<PlayerInput>` in `ProtocolPlugin`). Native
/// inputs must be `Serialize`/`Deserialize`/`Clone`/`PartialEq`/`Debug`/`Default`
/// + `Reflect` + `MapEntities`.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default, Reflect)]
pub struct PlayerInput {
    pub move_dir: Vec2,
    pub yaw: f32,
    pub pitch: f32,
    pub jump: bool,
    pub grab: bool,
    pub modifying: bool,
}

// No entities are referenced by `PlayerInput`, so the mapping is a no-op — but
// the trait is a required bound for native inputs.
impl MapEntities for PlayerInput {
    fn map_entities<M: EntityMapper>(&mut self, _entity_mapper: &mut M) {}
}

/// Registers the shared protocol. Add to BOTH the client and server apps, AFTER
/// their respective lightyear plugin group.
pub struct ProtocolPlugin;

impl Plugin for ProtocolPlugin {
    fn build(&self, app: &mut App) {
        // lightyear 0.27 builder API (the older `register_component` is
        // deprecated). `.replicate()` marks the component for World replication.
        // Thin slice: replicate the player marker + its transform, server → client.
        app.component::<NetPlayer>().replicate();
        app.component::<NetTransform>().replicate();

        // Register `PlayerInput` as a networked native input. `InputPlugin` is
        // role-agnostic: it adds the client input plugin under lightyear's
        // `client` feature and the server one under `server`, so a single
        // registration here wires both binaries (each compiles only its half).
        app.add_plugins(input::native::InputPlugin::<PlayerInput>::default());
    }
}
