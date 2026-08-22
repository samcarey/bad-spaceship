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
//! - `BS_BOT_LOCK_SECS`   seconds after start to send one `SetLocked(true)` — the
//!                         headless twin of the "Lock" button (the server welds
//!                         the avatar to whatever it's touching; a launch is only
//!                         granted once every player is locked to the assembly,
//!                         so ride scripts lock before they launch)
//! - `BS_BOT_RESET_SECS`   seconds after start to send one `ResetRoom` — the
//!                         headless twin of the menu's confirmed "Reset Room"
//!                         (unset / negative ⇒ never reset)
//! - `BS_BOT_SECS`  seconds to run before exiting (unset / 0 ⇒ run forever)
//! - `BS_BOT_RIDE`  autopilot: walk the avatar onto the rocket platform (via the
//!                  step block) and only then allow the launch — a real
//!                  character riding the ascent, verified from the recorder.
//! - `BS_BOT_WANDER`  with `BS_BOT_RIDE`: after liftoff, walk around on the
//!                  platform (deterministic golden-angle waypoints around the
//!                  platform's replicated position) instead of standing still —
//!                  reproduces a human rider shifting their weight mid-flight.
//! - `BS_BOT_JUMPY`  with `BS_BOT_WANDER`: also hop every few seconds while
//!                  riding — a human rider WILL press jump on an ascending
//!                  rocket, which is exactly how absolute-velocity jumps fling
//!                  them off.
//! - `BS_BOT_RESUME_ID`  persistent resume id (u64), sent in the connect
//!                  token's user_data + every input like the real client — for
//!                  testing the server's session-resume behaviour across rooms.

use avian3d::prelude::Position;
use bad_spaceship_shared::time_scale;
use bad_spaceship_shared::net::{
    resume_user_data, room_code_bytes, ControlChannel, NetInput, NetLaunch, NetPart, NetPlayer,
    PartShape, ProtocolPlugin, RequestLaunch, ResetRoom, SetLocked, BS_PROTOCOL_ID, TICK,
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
    lock_after: Option<f32>,
    reset_after: Option<f32>,
    run_secs: Option<f32>,
    ride: bool,
    wander: bool,
    jumpy: bool,
    resume_id: u64,
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
            lock_after: secs_var("BS_BOT_LOCK_SECS").filter(|s| *s >= 0.0),
            reset_after: secs_var("BS_BOT_RESET_SECS").filter(|s| *s >= 0.0),
            run_secs: secs_var("BS_BOT_SECS").filter(|s| *s > 0.0),
            ride: std::env::var("BS_BOT_RIDE").is_ok(),
            wander: std::env::var("BS_BOT_WANDER").is_ok(),
            jumpy: std::env::var("BS_BOT_JUMPY").is_ok(),
            resume_id: std::env::var("BS_BOT_RESUME_ID")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        }
    }
}

fn main() {
    let mut app = App::new();
    app.add_plugins(
        // Tighter than the sim tick so input writing never starves the 60 Hz
        // fixed schedule.
        MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(
            1.0 / (120.0 * time_scale()),
        ))),
    );
    // lightyear uses Bevy states internally; MinimalPlugins omits StatesPlugin.
    app.add_plugins(bevy::state::app::StatesPlugin);
    // `BS_LOG` turns lightyear's own tracing on. `MinimalPlugins` has no `LogPlugin`, so
    // by default every netcode diagnostic — an expired token, a refused protocol id, a
    // link that never completes its handshake — is silently discarded, and a bot that
    // fails to connect looks exactly like a bot with nothing to say. Opt-in because the
    // measurement runs (`BS_BURN_TRACE`) want a clean stdout.
    if std::env::var("BS_LOG").is_ok() {
        app.add_plugins(bevy::log::LogPlugin {
            filter: std::env::var("BS_LOG").unwrap_or_default(),
            ..default()
        });
    }
    // Order matters (as in the real client): plugin group → protocol → connect.
    app.add_plugins(ClientPlugins { tick_duration: TICK });
    app.add_plugins(ProtocolPlugin);
    app.insert_resource(BotConfig::from_env());
    app.init_resource::<Boarded>();
    app.add_systems(Startup, connect);
    app.add_systems(First, apply_time_scale.before(bevy::time::TimeSystems));
    app.add_systems(Update, (adopt_avatar, send_launch, send_lock, send_reset, exit_when_done));
    app.add_systems(
        FixedPreUpdate,
        write_input.in_set(ClientInputSystems::WriteClientInputs),
    );
    if bad_spaceship_shared::launch::burn_trace() {
        app.add_systems(FixedUpdate, trace_blastoff_tick);
    }
    app.run();
}

/// `BS_BURN_TRACE`: report, from a **real client's** timeline, the tick blastoff is due on
/// versus the tick the replicated `launched` level actually arrives.
///
/// The bot runs no physics, but this measurement does not need any — it needs a peer with
/// its own synced `LocalTimeline` and the replicated `NetLaunch`, which is exactly what a
/// bot is. The gap between the two printed ticks IS the lag that used to open every
/// flight: a client gating its burn on the level starts that many ticks after the server,
/// with nothing to replay the difference. Gating on the schedule makes the first number
/// match the server's blastoff tick exactly, no matter how late the second one lands.
fn trace_blastoff_tick(
    timeline: Res<lightyear::prelude::LocalTimeline>,
    launch: Query<&NetLaunch>,
    mut reported_due: Local<bool>,
    mut reported_flag: Local<bool>,
) {
    let Some(state) = launch.iter().next() else {
        return;
    };
    let tick = timeline.tick();
    if !*reported_due && state.launched_at(tick) {
        *reported_due = true;
        println!(
            "[sched] C scheduled={:?} fires_at_tick={tick:?}",
            state.blastoff_tick
        );
    }
    if !*reported_flag && state.launched {
        *reported_flag = true;
        println!("[sched] C launched-level arrived at tick={tick:?}");
    }
}

/// Open the connection: the same dev connect token the game client builds
/// (fixed protocol id, all-zero key, random handshake `client_id`), plain `ws://`.
/// `PredictionManager` is required to receive the server's predicted entities.
fn connect(mut commands: Commands, config: Res<BotConfig>) {
    let server_addr: SocketAddr =
        config.server.parse().unwrap_or_else(|e| panic!("BS_CONNECT '{}': {e}", config.server));
    // The resume id rides in the token's user_data, exactly like the real client
    // (the server reads it at connect to restore a remembered position).
    let user_data = resume_user_data(config.resume_id);
    let token = ConnectToken::build(server_addr, BS_PROTOCOL_ID, rand::random::<u64>(), [0u8; 32])
        .timeout_seconds(3)
        .expire_seconds(30)
        .user_data(user_data)
        .generate()
        .expect("connect token");
    let netcode = NetcodeClient::new(Authentication::Token(token), NetcodeConfig::default())
        .expect("netcode client");
    let url = format!("ws://{server_addr}");
    let io = WebSocketClientIo::from_url(ClientConfig::builder().with_no_encryption(), url.clone());
    let mut client_entity = commands.spawn((netcode, io, PredictionManager::default()));
    // Accelerated runs (BS_TIME_SCALE): lightyear's input-lead margins are counted
    // in TICKS, so at N x wall speed their real-time value shrinks N x — loopback
    // RTT (~3 ms) becomes multiple ticks and EVERY input arrives late (server
    // telemetry: late=126/126 at 10 x). Scale the sync margins so the wall-clock
    // safety margin matches a 1 x run.
    let scale = time_scale() as f32;
    if scale > 1.0 {
        use lightyear::prelude::{InputTimelineConfig, SyncConfig};
        client_entity.insert(InputTimelineConfig::default().with_sync_config(SyncConfig {
            jitter_margin: scale.mul_add(1.5, 1.0),
            error_margin: 2.0,
            max_error_margin: 2.0 * scale.max(10.0),
            ..Default::default()
        }));
    }
    let client = client_entity.id();
    commands.trigger(Connect { entity: client });
    println!("[bot] connecting to {url} proto={BS_PROTOCOL_ID} ver={}", bad_spaceship_shared::net::BS_VERSION);
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
    mut wander_tick: Local<u32>,
    parts: Query<(&NetPart, &Position), (With<Predicted>, Without<InputMarker<NetInput>>)>,
    mut controlled: Query<
        (&mut ActionState<NetInput>, Option<&Position>),
        With<InputMarker<NetInput>>,
    >,
) {
    for (mut state, position) in &mut controlled {
        let mut input =
            NetInput { room: config.room, resume_id: config.resume_id, ..default() };
        // Once launch has been earned (see `send_launch`), stand and ride — or,
        // in wander mode, stroll around the platform like a fidgety human rider
        // (the platform's world position moves, so waypoints are computed off
        // its replicated `Position` each tick). Deterministic golden-angle
        // waypoints, changed every 1.5 s, radius alternating mid/edge.
        if config.ride && config.wander && boarded.0 >= BOARD_SETTLE_TICKS {
            if let (Some(position), Some((plat, half))) = (position, platform_xz(&parts)) {
                *wander_tick += 1;
                let phase = *wander_tick / 90;
                let angle = phase as f32 * 2.399963; // golden angle
                // Scale the stroll to the deck: the edge waypoint keeps ~0.45 m
                // (the capsule + a step) inside the deck's short half-width,
                // capped at the 0.9 m the big 4-rocket deck was tuned with —
                // on a small 1-2-rocket deck the old fixed 0.9 walked the
                // rider clean off the edge at blastoff.
                let edge = (half.min_element() - 0.45).clamp(0.15, 0.9);
                let radius = if phase % 2 == 0 { (edge * 0.5).min(0.4) } else { edge };
                let target = plat + radius * Vec2::new(angle.cos(), angle.sin());
                let delta = target - Vec2::new(position.0.x, position.0.z);
                if delta.length() > 0.2 {
                    let dir = delta.normalize_or_zero();
                    input.yaw = f32::atan2(-dir.x, dir.y);
                    input.move_xz = [0.0, (delta.length() * 1.5).clamp(0.2, 0.6)];
                }
                // A real rider WILL hit jump mid-ascent: hop for a few ticks
                // every ~4.5 s — but only near the deck centre. Jumping while
                // walking toward the edge drifts you overboard mid-air (~4.5 m
                // of relative drift per hop vs a 1.6 m half-width deck), which
                // is correct physics, not the regression under test.
                let near_center = (Vec2::new(position.0.x, position.0.z) - plat).length()
                    < (edge * 0.8).min(0.7);
                input.jump = config.jumpy && near_center && *wander_tick % 270 < 8;
            }
        } else if config.ride && boarded.0 < BOARD_SETTLE_TICKS {
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

/// The platform's replicated world XZ + its XZ half-extents: the largest-footprint
/// cuboid in view (the platform dwarfs the step block and any loose cubes).
fn platform_xz(
    parts: &Query<(&NetPart, &Position), (With<Predicted>, Without<InputMarker<NetInput>>)>,
) -> Option<(Vec2, Vec2)> {
    parts
        .iter()
        .filter_map(|(part, position)| match part.shape {
            PartShape::Cuboid { half_extents } => Some((
                half_extents[0] * half_extents[2],
                position.0,
                Vec2::new(half_extents[0], half_extents[2]),
            )),
            // Neither a rocket nor a rock is a deck to walk onto.
            PartShape::RocketEngine | PartShape::Asteroid { .. } => None,
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, p, half)| (Vec2::new(p.x, p.z), half))
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

/// One-shot `SetLocked(true)` once the scripted delay elapses — the headless twin
/// of the "Lock" button (the server welds the avatar to whatever it's touching).
/// In ride mode it waits until the avatar has stood on the platform for a second,
/// like `send_launch` — a launch is now only granted once every player is locked
/// to the assembly, so a ride script locks first, then launches.
fn send_lock(
    time: Res<Time>,
    config: Res<BotConfig>,
    boarded: Res<Boarded>,
    mut sent: Local<bool>,
    mut senders: Query<&mut MessageSender<SetLocked>, With<Connected>>,
) {
    let Some(after) = config.lock_after else {
        return;
    };
    if *sent || time.elapsed_secs() < after || (config.ride && boarded.0 < BOARD_SETTLE_TICKS) {
        return;
    }
    for mut sender in &mut senders {
        sender.send::<ControlChannel>(SetLocked(true));
        *sent = true;
        println!("[bot] sent SetLocked(true) at t={:.1}s (boarded {} ticks)", time.elapsed_secs(), boarded.0);
    }
}

/// One-shot `ResetRoom` once the scripted delay elapses — the headless twin of
/// the menu's confirmed "Reset Room" dialog (the server resets the whole room to
/// its initial conditions).
fn send_reset(
    time: Res<Time>,
    config: Res<BotConfig>,
    mut sent: Local<bool>,
    mut senders: Query<&mut MessageSender<ResetRoom>, With<Connected>>,
) {
    let Some(after) = config.reset_after else {
        return;
    };
    if *sent || time.elapsed_secs() < after {
        return;
    }
    for mut sender in &mut senders {
        sender.send::<ControlChannel>(ResetRoom);
        *sent = true;
        println!("[bot] sent ResetRoom at t={:.1}s", time.elapsed_secs());
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

/// Compose `BS_TIME_SCALE` onto whatever clock speed lightyear's sync set last
/// frame. lightyear's `update_virtual_time` (schedule `Last`) OVERWRITES
/// `Time<Virtual>`'s relative speed with its tick-sync correction (~1.0 +/- 5%,
/// `SyncConfig::speedup_factor`) — the crate has a TODO about composing with a
/// user-applied speed instead. Running in `First`, before Bevy's `TimeSystem`
/// advances the clock, we re-multiply the sync's value by the scale each frame:
/// the client then paces its ticks N x wall clock (matching an N x server) while
/// the sync's +/- 5% still trims on top. The `< scale/2` guard keeps the multiply
/// from compounding on frames where lightyear didn't overwrite (pre-connection).
fn apply_time_scale(mut time: ResMut<Time<Virtual>>) {
    let scale = time_scale() as f32;
    if scale != 1.0 {
        let current = time.relative_speed();
        if current < scale * 0.5 {
            time.set_relative_speed(current * scale);
        }
    }
}
