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
use core::time::Duration;

use bevy::ecs::entity::{EntityMapper, MapEntities};
use bevy::prelude::*;
use lightyear::prelude::*;
use serde::{Deserialize, Serialize};

/// The simulation tick interval (60 Hz). The client and server must agree on
/// this, so it lives in the shared protocol — both pass it to their lightyear
/// plugin group's `tick_duration`.
pub const TICK: Duration = Duration::from_millis(1000 / 60);

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

/// Per-tick client → server message carrying the controlling client's current
/// character pose (world space). The client owns its local character sim, so
/// rather than re-simulating movement on the server (which drifts and feels
/// wrong vs the real physics character), the client forwards its authoritative
/// pose and the server mirrors it into the replicated `NetTransform` — so every
/// other client sees the avatar exactly track the character (offset only by
/// network round-trip, smoothed later by interpolation).
///
/// Sent via lightyear's native-input channel (registered with
/// `InputPlugin::<PlayerInput>` in `ProtocolPlugin`); native inputs must be
/// `Serialize`/`Deserialize`/`Clone`/`PartialEq`/`Debug`/`Default` + `Reflect` +
/// `MapEntities`.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default, Reflect)]
pub struct PlayerInput {
    pub translation: [f32; 3],
    /// Rotation quaternion, `[x, y, z, w]`.
    pub rotation: [f32; 4],
    /// The client's intent to be holding a part: while true the server grabs the
    /// nearest part in front of the player and holds it at the player's hold point.
    pub grab: bool,
}

// No entities are referenced by `PlayerInput`, so the mapping is a no-op — but
// the trait is a required bound for native inputs.
impl MapEntities for PlayerInput {
    fn map_entities<M: EntityMapper>(&mut self, _entity_mapper: &mut M) {}
}

/// Replicated cuboid shape of a part (full extents = 2 × `half_extents`). The
/// server replicates this once per part so a client that doesn't simulate parts
/// can rebuild the render mesh; the part's live pose rides on `NetTransform`.
#[derive(Component, Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default)]
pub struct NetPart {
    pub half_extents: [f32; 3],
}

/// Interpolate between two replicated poses: lerp the translation, slerp the
/// rotation. Used by lightyear's interpolation for `NetTransform`.
fn lerp_net_transform(start: NetTransform, other: NetTransform, t: f32) -> NetTransform {
    let translation = Vec3::from_array(start.translation)
        .lerp(Vec3::from_array(other.translation), t)
        .to_array();
    let rotation = Quat::from_array(start.rotation)
        .slerp(Quat::from_array(other.rotation), t)
        .to_array();
    NetTransform { translation, rotation }
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
        // Replicate the pose and register linear interpolation for it: the client
        // renders `Interpolated` copies whose `NetTransform` lightyear eases
        // between confirmed snapshots each frame, smoothing the round-trip motion
        // trail. `NetTransform` isn't `Ease`, so supply a custom lerp.
        app.component::<NetTransform>()
            .replicate()
            .add_interpolation_with(lerp_net_transform);
        // The part shape is constant, so it only needs replicating (no interp).
        app.component::<NetPart>().replicate();

        // Register `PlayerInput` as a networked native input. `InputPlugin` is
        // role-agnostic: it adds the client input plugin under lightyear's
        // `client` feature and the server one under `server`, so a single
        // registration here wires both binaries (each compiles only its half).
        app.add_plugins(input::native::InputPlugin::<PlayerInput>::default());
    }
}
