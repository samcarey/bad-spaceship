//! Headless test client ("bot") — a real multiplayer player with no rendering.
//!
//! Connects to the dedicated server exactly like the game client (same connect
//! token, same `NetInput` channel, same reliable `ControlChannel` messages), but
//! runs on `MinimalPlugins`: no window, no assets, no physics. That makes a full
//! server+client session scriptable and unattended — join a room (which creates
//! it server-side, consuming any staged pending save), idle as a live occupant
//! (occupancy is what arms the autosave and the `BS_RECORD` flight recorder),
//! optionally trigger the room's rocket launch, and exit after a fixed run time.
//! Verification then reads the recorder's JSONL, not a screen.
//!
//! Env:
//! - `BS_CONNECT`   server `host:port` (default `127.0.0.1:5001`)
//! - `BS_ROOM`      lobby room code to join (default: the shared default room)
//! - `BS_BOT_LAUNCH_SECS`  seconds after start to send one `RequestLaunch`
//!                         (unset / negative ⇒ never launch)
//! - `BS_BOT_SECS`  seconds to run before exiting (unset / 0 ⇒ run forever)

use bad_spaceship_shared::net::{
    ControlChannel, NetInput, NetPlayer, ProtocolPlugin, RequestLaunch, BS_PROTOCOL_ID, TICK,
};
use bevy::{app::ScheduleRunnerPlugin, prelude::*};
use lightyear::netcode::ConnectToken;
use lightyear::prelude::client::input::InputSystems as ClientInputSystems;
use lightyear::prelude::client::*;
use lightyear::prelude::input::native::{ActionState, InputMarker};
use lightyear::prelude::{
    Authentication, Connected, LocalId, MessageSender, PeerId, Predicted, PredictionManager,
};
use std::net::SocketAddr;
use std::time::Duration;

/// The bot's run script, read once from the environment (see the module docs).
#[derive(Resource)]
struct BotConfig {
    server: String,
    room: [u8; 6],
    launch_after: Option<f32>,
    run_secs: f32,
}

impl BotConfig {
    fn from_env() -> Self {
        // Pack the room code the way the client does: uppercase, ≤6 bytes, zero-pad
        // (all-zero = the shared default room).
        let mut room = [0u8; 6];
        if let Ok(code) = std::env::var("BS_ROOM") {
            for (slot, byte) in room.iter_mut().zip(code.to_ascii_uppercase().bytes()) {
                *slot = byte;
            }
        }
        let secs_var = |name: &str| {
            std::env::var(name).ok().and_then(|v| v.parse::<f32>().ok())
        };
        Self {
            server: std::env::var("BS_CONNECT").unwrap_or_else(|_| "127.0.0.1:5001".into()),
            room,
            launch_after: secs_var("BS_BOT_LAUNCH_SECS").filter(|s| *s >= 0.0),
            run_secs: secs_var("BS_BOT_SECS").unwrap_or(0.0),
        }
    }
}

fn main() {
    let mut app = App::new();
    app.add_plugins(
        // Tighter than the sim tick so input writing never starves the 60 Hz
        // fixed schedule.
        MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0 / 120.0))),
    );
    // lightyear uses Bevy states internally; MinimalPlugins omits StatesPlugin.
    app.add_plugins(bevy::state::app::StatesPlugin);
    // Order matters (as in the real client): plugin group → protocol → connect.
    app.add_plugins(ClientPlugins { tick_duration: TICK });
    app.add_plugins(ProtocolPlugin);
    app.insert_resource(BotConfig::from_env());
    app.add_systems(Startup, connect);
    app.add_systems(Update, (adopt_avatar, send_launch, exit_when_done));
    app.add_systems(
        FixedPreUpdate,
        write_input.in_set(ClientInputSystems::WriteClientInputs),
    );
    app.run();
}

/// Open the connection: the same dev connect token the game client builds
/// (fixed protocol id, all-zero key, random handshake `client_id`), plain `ws://`.
/// `PredictionManager` is required to receive the server's predicted entities.
fn connect(mut commands: Commands, config: Res<BotConfig>) {
    let server_addr: SocketAddr =
        config.server.parse().unwrap_or_else(|e| panic!("BS_CONNECT '{}': {e}", config.server));
    let token = ConnectToken::build(server_addr, BS_PROTOCOL_ID, rand::random::<u64>(), [0u8; 32])
        .timeout_seconds(3)
        .expire_seconds(30)
        .generate()
        .expect("connect token");
    let netcode = NetcodeClient::new(Authentication::Token(token), NetcodeConfig::default())
        .expect("netcode client");
    let url = format!("ws://{server_addr}");
    let io = WebSocketClientIo::from_url(ClientConfig::builder().with_no_encryption(), url.clone());
    let client = commands.spawn((netcode, io, PredictionManager::default())).id();
    commands.trigger(Connect { entity: client });
    println!("[bot] connecting to {url}");
}

/// Attach the input components to our own predicted avatar once it replicates —
/// the minimal slice of the client's `setup_predicted_avatar` (no body, no camera).
/// Only then does lightyear start sending our `ActionState` to the server.
fn adopt_avatar(
    mut commands: Commands,
    new: Query<(Entity, &NetPlayer), (With<Predicted>, Without<InputMarker<NetInput>>)>,
    local: Query<&LocalId, With<Connected>>,
) {
    let Some(PeerId::Netcode(my_id)) = local.iter().next().map(|l| l.0) else {
        return;
    };
    for (entity, player) in &new {
        if player.client_id != my_id {
            continue;
        }
        commands
            .entity(entity)
            .insert((InputMarker::<NetInput>::default(), ActionState::<NetInput>::default()));
        println!("[bot] adopted avatar {entity} (client id {my_id})");
    }
}

/// Forward an idle input every tick. The room code is the whole payload: the
/// server keys room creation (and any staged pending-save load) on it.
fn write_input(
    config: Res<BotConfig>,
    mut controlled: Query<&mut ActionState<NetInput>, With<InputMarker<NetInput>>>,
) {
    for mut state in &mut controlled {
        state.0 = NetInput { room: config.room, ..default() };
    }
}

/// One-shot `RequestLaunch` once the scripted delay elapses — the headless twin
/// of the slide-to-launch gesture (the server accepts it from any room member).
fn send_launch(
    time: Res<Time>,
    config: Res<BotConfig>,
    mut sent: Local<bool>,
    mut senders: Query<&mut MessageSender<RequestLaunch>, With<Connected>>,
) {
    let Some(after) = config.launch_after else {
        return;
    };
    if *sent || time.elapsed_secs() < after {
        return;
    }
    for mut sender in &mut senders {
        sender.send::<ControlChannel>(RequestLaunch);
        *sent = true;
        println!("[bot] sent RequestLaunch at t={:.1}s", time.elapsed_secs());
    }
}

/// Exit cleanly after the scripted run time (0 = run until killed).
fn exit_when_done(time: Res<Time>, config: Res<BotConfig>, mut exit: MessageWriter<AppExit>) {
    if config.run_secs > 0.0 && time.elapsed_secs() >= config.run_secs {
        println!("[bot] run time reached ({:.0}s), exiting", config.run_secs);
        exit.write(AppExit::Success);
    }
}
