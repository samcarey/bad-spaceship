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

use bad_spaceship_shared::net::{NetPlayer, NetTransform, PlayerInput, ProtocolPlugin};
use bevy::prelude::*;
use lightyear::prelude::input::native::ActionState;
use lightyear::prelude::*;
use lightyear::prelude::server::*;

/// Movement speed applied to player input, in world units per second.
const PLAYER_SPEED: f32 = 6.0;

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
        app.add_systems(Startup, (start_server, spawn_demo_bot));
        // One server-owned, replicated player per client that connects.
        app.add_observer(spawn_player_for_client);
        // A persistent, server-driven player that orbits so a single client can
        // see live replication (motion streamed over the wire) without a second
        // device — useful because mobile browsers suspend background tabs, so two
        // tabs on one phone never connect simultaneously.
        app.add_systems(Update, move_demo_bot);
        // Apply each client's replicated input to its player, authoritatively.
        app.add_systems(FixedUpdate, apply_player_input);
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

/// Marks the always-present, server-driven demo player.
#[derive(Component)]
struct DemoBot;

/// Spawn the orbiting demo player once at startup, replicated to everyone.
fn spawn_demo_bot(mut commands: Commands) {
    commands.spawn((
        NetPlayer { client_id: 0 },
        NetTransform::from_transform(&Transform::from_xyz(3.0, 2.0, 0.0)),
        Replicate::to_clients(NetworkTarget::All),
        DemoBot,
    ));
}

/// Drive the demo player in a slow circle each frame; the changed `NetTransform`
/// replicates to every connected client, so its cube visibly moves.
fn move_demo_bot(time: Res<Time>, mut bot: Query<&mut NetTransform, With<DemoBot>>) {
    let t = time.elapsed_secs();
    let pose = Transform::from_xyz(t.cos() * 3.0, 2.0, t.sin() * 3.0)
        .with_rotation(Quat::from_rotation_y(t));
    for mut net in &mut bot {
        *net = NetTransform::from_transform(&pose);
    }
}

/// When a client finishes connecting (`Connected` added to its link entity),
/// spawn a player entity owned by the server and replicated to everyone.
fn spawn_player_for_client(
    trigger: On<Add, Connected>,
    mut commands: Commands,
    mut count: Local<u32>,
) {
    let client = trigger.entity;
    // Fan players out along x so distinct clients are visibly separate (until
    // input/real Character positions drive them in a later phase).
    let x = (*count as f32) * 2.5 - 2.5;
    *count += 1;
    commands.spawn((
        NetPlayer { client_id: client.to_bits() },
        NetTransform::from_transform(&Transform::from_xyz(x, 2.0, 0.0)),
        Replicate::to_clients(NetworkTarget::All),
        // Bind this player to the connecting client so that client's networked
        // input drives it. The server auto-adds the `InputBuffer`/`ActionState`
        // when input arrives; seeding `ActionState` here lets `apply_player_input`
        // match the entity immediately. `SessionBased` despawns it on disconnect.
        ControlledBy { owner: client, lifetime: Lifetime::SessionBased },
        ActionState::<PlayerInput>::default(),
    ));
    info!("client {client:?} connected — spawned replicated player at x={x}");
}

/// Integrate each player's current input into its authoritative pose. Runs in
/// `FixedUpdate` (server tick); the changed `NetTransform` replicates to clients.
fn apply_player_input(
    time: Res<Time>,
    mut players: Query<(&ActionState<PlayerInput>, &mut NetTransform)>,
) {
    let dt = time.delta_secs();
    for (state, mut net) in &mut players {
        let dir = state.0.move_dir;
        net.translation[0] += dir.x * PLAYER_SPEED * dt;
        net.translation[2] += dir.y * PLAYER_SPEED * dt;
    }
}
