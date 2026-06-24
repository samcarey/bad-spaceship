//! Server-side netcode (lightyear) — the dedicated authoritative host.
//!
//! Added only in multiplayer mode (the `BS_MULTIPLAYER` env var); single-player
//! headless runs are byte-identical to before. Thin slice: stand up a WebSocket
//! listener and, for each client that connects, spawn a server-owned player
//! entity replicated to all clients. Real gameplay (driving these from the
//! actual Character sim, parts, joints, prediction) is layered on next.
//!
//! Transport: plain `ws://` via `with_no_encryption()` so local/native testing
//! needs no TLS certs. Production / browser clients need `wss://` — swap in a
//! real `Identity` (see `with_identity`) behind a public TLS endpoint.

use core::time::Duration;
use std::net::SocketAddr;

use bad_spaceship_shared::net::{NetPlayer, NetTransform, ProtocolPlugin};
use bevy::prelude::*;
use lightyear::prelude::*;
use lightyear::prelude::server::*;

/// 60 Hz, matching the server's fixed simulation loop.
const TICK: Duration = Duration::from_millis(1000 / 60);

pub struct NetServerPlugin;

impl Plugin for NetServerPlugin {
    fn build(&self, app: &mut App) {
        // lightyear uses Bevy states internally (`init_state`), which needs the
        // `StateTransition` schedule. The client gets it from `DefaultPlugins`,
        // but the headless server runs `MinimalPlugins`, so add `StatesPlugin`
        // explicitly before the lightyear plugin group.
        app.add_plugins(bevy::state::app::StatesPlugin);
        // Order matters: plugin group → protocol → spawn the server entity.
        app.add_plugins(ServerPlugins { tick_duration: TICK });
        app.add_plugins(ProtocolPlugin);
        app.add_systems(Startup, start_server);
        // One server-owned, replicated player per client that connects.
        app.add_observer(spawn_player_for_client);
    }
}

/// Where the WebSocket server listens. `BS_SERVER_BIND` (host:port) or default.
fn bind_addr() -> SocketAddr {
    std::env::var("BS_SERVER_BIND")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "0.0.0.0:5001".parse().unwrap())
}

fn start_server(mut commands: Commands) {
    let addr = bind_addr();
    let io = WebSocketServerIo {
        config: ServerConfig::builder()
            .with_bind_address(addr)
            .with_no_encryption(),
    };
    let server = commands
        .spawn((NetcodeServer::new(NetcodeConfig::default()), LocalAddr(addr), io))
        .id();
    commands.trigger(Start { entity: server });
    info!("multiplayer server listening on ws://{addr}");
}

/// When a client finishes connecting (`Connected` added to its link entity),
/// spawn a player entity owned by the server and replicated to everyone.
fn spawn_player_for_client(trigger: On<Add, Connected>, mut commands: Commands) {
    let client = trigger.entity;
    commands.spawn((
        NetPlayer { client_id: client.to_bits() },
        NetTransform::from_transform(&Transform::from_xyz(0.0, 2.0, 0.0)),
        Replicate::to_clients(NetworkTarget::All),
    ));
    info!("client {client:?} connected — spawned replicated player");
}
