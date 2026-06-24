//! Client-side netcode (lightyear) — connects to the dedicated server and
//! renders the players it replicates.
//!
//! Added only when a connect target is configured (`BS_CONNECT=host:port` on
//! native); otherwise the client is the unchanged single-player game. On wasm
//! `std::env::var` is always `Err`, so the web build stays single-player for
//! now — web multiplayer needs `wss://` (a TLS endpoint) and reading the room
//! off `window.__BS_NET__`, which is the next step after this native slice.
//!
//! Thin slice: connect over plain `ws://` (no TLS), then for every player the
//! server replicates, draw a cube at its replicated `NetTransform`.

use core::time::Duration;

use bad_spaceship_shared::net::{NetPlayer, NetTransform, ProtocolPlugin};
use bevy::prelude::*;
use lightyear::prelude::client::*;

/// 60 Hz, matching the server tick.
const TICK: Duration = Duration::from_millis(1000 / 60);

/// The server to connect to, or `None` for single-player. Native reads
/// `BS_CONNECT` (e.g. `127.0.0.1:5001`); always `None` on wasm for now.
pub fn multiplayer_target() -> Option<String> {
    std::env::var("BS_CONNECT").ok()
}

pub struct NetClientPlugin;

impl Plugin for NetClientPlugin {
    fn build(&self, app: &mut App) {
        // Order matters: plugin group → protocol → spawn the client entity.
        app.add_plugins(ClientPlugins { tick_duration: TICK });
        app.add_plugins(ProtocolPlugin);
        app.add_systems(Startup, connect);
        // Give every replicated player a visible body, then keep its transform
        // in sync with the replicated pose.
        app.add_systems(Update, (draw_replicated_players, apply_net_transform));
    }
}

/// Attach a mesh to any replicated player that doesn't have one yet.
fn draw_replicated_players(
    mut commands: Commands,
    new_players: Query<Entity, (With<NetPlayer>, Without<Mesh3d>)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for entity in &new_players {
        commands.entity(entity).insert((
            Mesh3d(meshes.add(Cuboid::new(1.0, 2.0, 1.0))),
            MeshMaterial3d(materials.add(Color::srgb(0.9, 0.35, 0.35))),
        ));
    }
}

/// Apply the replicated pose to the rendered transform.
fn apply_net_transform(mut q: Query<(&NetTransform, &mut Transform), Changed<NetTransform>>) {
    for (net, mut transform) in &mut q {
        *transform = net.to_transform();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn connect(mut commands: Commands) {
    use lightyear::prelude::Authentication;
    use std::net::SocketAddr;

    let Some(addr_str) = multiplayer_target() else {
        return;
    };
    let server_addr: SocketAddr = match addr_str.parse() {
        Ok(addr) => addr,
        Err(e) => {
            error!("BS_CONNECT ('{addr_str}') is not a host:port address: {e}");
            return;
        }
    };

    // Dev auth: a fixed protocol id + zero key, matching the server's
    // `NetcodeConfig::default()`. Production issues a real ConnectToken from the
    // matchmaker instead of `Manual`.
    let auth = Authentication::Manual {
        server_addr,
        client_id: rand::random::<u64>(),
        // The netcode private key (`Key = [u8; 32]`). Dev/loopback uses the
        // all-zero key, matching the server's `NetcodeConfig::default()`.
        private_key: [0u8; 32],
        protocol_id: 0,
    };
    let netcode = match NetcodeClient::new(auth, NetcodeConfig::default()) {
        Ok(n) => n,
        Err(e) => {
            error!("failed to build netcode client: {e:?}");
            return;
        }
    };

    let url = format!("ws://{server_addr}");
    let io = WebSocketClientIo::from_url(ClientConfig::builder().with_no_encryption(), url.clone());
    let client = commands.spawn((netcode, io)).id();
    commands.trigger(Connect { entity: client });
    info!("connecting to multiplayer server at {url}");
}

#[cfg(target_arch = "wasm32")]
fn connect() {
    // Reached only if `multiplayer_target()` returns Some on wasm, which it
    // currently never does. Web multiplayer (wss:// + window.__BS_NET__) lands
    // in the next step.
    warn!("web multiplayer is not wired up yet (needs a wss:// endpoint)");
}
