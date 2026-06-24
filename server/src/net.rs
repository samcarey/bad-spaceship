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

use std::net::SocketAddr;

use bad_spaceship_shared::net::{NetPlayer, NetTransform, PlayerInput, ProtocolPlugin, TICK};
use bevy::prelude::*;
use lightyear::prelude::input::native::ActionState;
use lightyear::prelude::*;
use lightyear::prelude::server::*;

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
        // Clients render an interpolated copy, smoothing the orbit motion.
        InterpolationTarget::to_clients(NetworkTarget::All),
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
/// spawn a player entity owned by the server and replicated to everyone. The
/// initial pose is a placeholder — the client's first `PlayerInput` (its real
/// character pose) overwrites it within a tick, so distinct clients separate
/// naturally without any per-connection fan-out.
fn spawn_player_for_client(trigger: On<Add, Connected>, mut commands: Commands) {
    let client = trigger.entity;
    commands.spawn((
        NetPlayer { client_id: client.to_bits() },
        NetTransform::from_transform(&Transform::from_xyz(0.0, 2.0, 0.0)),
        Replicate::to_clients(NetworkTarget::All),
        // Clients (including the owner) render an interpolated copy, smoothing
        // the replicated pose between confirmed snapshots.
        InterpolationTarget::to_clients(NetworkTarget::All),
        // Bind this player to the connecting client so that client's networked
        // input drives it. The server auto-adds the `InputBuffer`/`ActionState`
        // when input arrives; seeding `ActionState` here lets `apply_player_input`
        // match the entity immediately. `SessionBased` despawns it on disconnect.
        ControlledBy { owner: client, lifetime: Lifetime::SessionBased },
        ActionState::<PlayerInput>::default(),
    ));
    info!("client {client:?} connected — spawned replicated player");
}

/// Mirror each client's forwarded character pose into its authoritative
/// `NetTransform`. Runs in `FixedUpdate` (server tick); the changed
/// `NetTransform` then replicates to all clients.
fn apply_player_input(mut players: Query<(&ActionState<PlayerInput>, &mut NetTransform)>) {
    for (state, mut net) in &mut players {
        // The all-zero default is the unsent seed, not a real pose (a real pose's
        // rotation is a unit quaternion, never `[0,0,0,0]`), so skip it and keep
        // the spawn pose until the client's first input arrives.
        if state.0 == PlayerInput::default() {
            continue;
        }
        net.translation = state.0.translation;
        net.rotation = state.0.rotation;
    }
}
