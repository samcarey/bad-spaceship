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
//! - `BS_BOT_RIDE`  autopilot: walk the avatar onto the rocket platform (via the
//!                  step block) and only then allow the launch — a real
//!                  character riding the ascent, verified from the recorder.

use avian3d::prelude::Position;
use bad_spaceship_shared::net::{
    room_code_bytes, ControlChannel, NetInput, NetPlayer, ProtocolPlugin, RequestLaunch,
    BS_PROTOCOL_ID, TICK,
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
    run_secs: Option<f32>,
    ride: bool,
}

/// Ride-autopilot progress: how long the avatar has been standing on the
/// platform (ticks). The launch is gated on this in ride mode.
#[derive(Resource, Default)]
struct Boarded(u32);

/// Ticks the autopilot must stand centred on the platform before the launch is
/// considered boarded (and the autopilot freezes into "just ride").
const BOARD_SETTLE_TICKS: u32 = 60;

impl BotConfig {
    fn from_env() -> Self {
        let secs_var = |name: &str| {
            std::env::var(name).ok().and_then(|v| v.parse::<f32>().ok())
        };
        Self {
            server: std::env::var("BS_CONNECT").unwrap_or_else(|_| "127.0.0.1:5001".into()),
            room: room_code_bytes(std::env::var("BS_ROOM").ok().as_deref().unwrap_or("")),
            launch_after: secs_var("BS_BOT_LAUNCH_SECS").filter(|s| *s >= 0.0),
            run_secs: secs_var("BS_BOT_SECS").filter(|s| *s > 0.0),
            ride: std::env::var("BS_BOT_RIDE").is_ok(),
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
    app.init_resource::<Boarded>();
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

/// Forward an input every tick. The room code is the whole payload for an idle
/// bot: the server keys room creation (and any staged pending-save load) on it.
///
/// In ride mode (`BS_BOT_RIDE`) this is a tiny autopilot over the "Rocket Ride"
/// save's known geometry, driving the avatar from its own replicated `Position`:
/// walk to the step block at (3.4, 0), hop on, jump across onto the platform
/// (top ≈ 1.37 — only reachable off the step), then stand at the centre. The
/// launch is gated on `Boarded` so the rocket lifts off with the rider aboard.
/// Movement basis matches `walk_based_on_input`: with `yaw = atan2(-dx, dz)`
/// the target is dead ahead and `move_xz = [0, throttle]` walks toward it.
fn write_input(
    config: Res<BotConfig>,
    mut boarded: ResMut<Boarded>,
    mut controlled: Query<
        (&mut ActionState<NetInput>, Option<&Position>),
        With<InputMarker<NetInput>>,
    >,
) {
    for (mut state, position) in &mut controlled {
        let mut input = NetInput { room: config.room, ..default() };
        // Once launch has been earned (see `send_launch`), just stand and ride —
        // the platform drifts in world space, so pre-boarding geometry is
        // meaningless after liftoff.
        if config.ride && boarded.0 < BOARD_SETTLE_TICKS {
            if let Some(position) = position {
                // `Position` is the capsule CENTRE (contact + 0.75): bowl floor
                // ≈ -0.7, step top ≈ 0.1, platform top ≈ 2.1.
                let pos = position.0;
                let on_platform = pos.y > 1.9;
                let target = if on_platform || pos.y > -0.3 {
                    Vec3::new(0.0, 0.0, 0.0) // platform centre (from the step: jump across)
                } else {
                    Vec3::new(3.4, 0.0, 0.0) // the step block first
                };
                let delta = target - pos;
                let dist = Vec2::new(delta.x, delta.z).length();
                if on_platform && dist < 0.45 {
                    boarded.0 += 1; // stand at the centre; launch unlocks at BOARD_SETTLE_TICKS
                } else {
                    let mut dir = Vec2::new(delta.x, delta.z).normalize_or_zero();
                    // Ground approach must go AROUND the pad, never through it: a
                    // character shoving a rocket at walk speed topples the whole
                    // assembly (seen on the recorder: the stack ended upside down
                    // 4 m away, then "launched" itself into the ground). Blend in
                    // a radial repulsion from the pad while crossing its vicinity.
                    if pos.y <= -0.3 && dist > 1.5 {
                        let radial = Vec2::new(pos.x, pos.z);
                        if radial.length() < 3.6 {
                            dir = (dir + radial.normalize_or_zero() * 1.4).normalize_or_zero();
                        }
                    }
                    input.yaw = f32::atan2(-dir.x, dir.y);
                    input.move_xz = [0.0, (dist / 2.0).clamp(0.2, 1.0)];
                    // Hop whenever climbing is called for: onto the step from the
                    // bowl, and from the step across+up onto the platform.
                    input.jump =
                        (pos.y <= -0.3 && dist < 2.2) || (-0.3..1.9).contains(&pos.y);
                }
            }
        }
        state.0 = input;
    }
}

/// One-shot `RequestLaunch` once the scripted delay elapses — the headless twin
/// of the slide-to-launch gesture (the server accepts it from any room member).
/// In ride mode it additionally waits until the avatar has stood on the platform
/// for a second, so the rocket lifts off with the rider aboard.
fn send_launch(
    time: Res<Time>,
    config: Res<BotConfig>,
    boarded: Res<Boarded>,
    mut sent: Local<bool>,
    mut senders: Query<&mut MessageSender<RequestLaunch>, With<Connected>>,
) {
    let Some(after) = config.launch_after else {
        return;
    };
    if *sent || time.elapsed_secs() < after || (config.ride && boarded.0 < BOARD_SETTLE_TICKS) {
        return;
    }
    for mut sender in &mut senders {
        sender.send::<ControlChannel>(RequestLaunch);
        *sent = true;
        println!("[bot] sent RequestLaunch at t={:.1}s (boarded {} ticks)", time.elapsed_secs(), boarded.0);
    }
}

/// Exit cleanly after the scripted run time (0 = run until killed).
fn exit_when_done(time: Res<Time>, config: Res<BotConfig>, mut exit: MessageWriter<AppExit>) {
    let Some(run_secs) = config.run_secs else {
        return;
    };
    if time.elapsed_secs() >= run_secs {
        println!("[bot] run time reached ({run_secs:.0}s), exiting");
        exit.write(AppExit::Success);
    }
}
