//! Server-side netcode (lightyear) — the dedicated authoritative host.
//!
//! Added only in multiplayer mode (the `BS_MULTIPLAYER` env var); single-player
//! headless runs are byte-identical to before.
//!
//! **Per-room world isolation.** One server process hosts many lobby rooms, kept
//! separate via lightyear's room visibility (`Rooms`): an entity is only
//! replicated to clients sharing a room with it. Each client reports its lobby
//! code (carried on `NetInput`); the server maps the code to a `RoomId` and a
//! distinct Avian collision-layer bit, then spawns that room its own set of
//! parts. The single shared Avian world keeps the rooms from physically
//! interacting via collision layers (each room's parts collide only with
//! same-room parts and the ground), so grab/attach/joints are all room-scoped.
//!
//! Transport: plain `ws://` via `with_no_encryption()` so local/native testing
//! needs no TLS certs. Production / browser clients need `wss://` — swap in a
//! real `Identity` (see `with_identity`) behind a public TLS endpoint.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::SystemTime;

use avian3d::prelude::{
    AngularVelocity, CollisionLayers, Collisions, ComputedCenterOfMass, ComputedMass, Forces,
    Gravity, LinearVelocity, Position, Rotation, SphericalJoint, WriteRigidBodyForces,
};
use bad_spaceship_shared::assembly::largest_assembly_per_room;
use bad_spaceship_shared::launch::{
    balanced_assembly_thrust, measure_assembly_spin, LAUNCH_COUNTDOWN_SECS,
};
use bad_spaceship_shared::character::{spawn_position, CharacterMovement, InitialPose, ServerAvatar};
use bad_spaceship_shared::net::{
    apply_hold_spring, apply_net_input, focused_part, monster_index, sanitize_name,
    ClientPanicReport, InLargestAssembly, NetCenterOfMass, NetFacing, NetHold, NetInput, NetJoint,
    NetLaunch, NetName, NetPart, NetPlayer, PartShape, ProtocolPlugin, RequestLaunch, ResetPosition,
    RollbackReport, SaveGame, SetAvatar, SetName, GROUND_JOINT_ID, MONSTER_COUNT, TICK,
};
use bad_spaceship_shared::map::GROUND_LAYER;
use bad_spaceship_shared::part::{
    local_contact_anchor, part_state_diverged, spawn_random_part, spawn_random_rocket,
    spawn_rocket_engine, spawn_saved_cuboid, RocketEngine, SuppressLocalParts, DELETE_RADIUS,
    NUM_PARTS, NUM_ROCKET_ENGINES, PART_FALL_Y, ROCKET_VOLUME,
};
use bad_spaceship_shared::{Grass, SuppressLocalPlayer, Yaw};
use bevy::prelude::*;

use crate::save::{
    self, SaveAvatar, SaveBody, SaveFile, SaveJoint, SavePart, SaveShape, SaveWorld, AUTOSAVE_SECS,
};
use lightyear::prelude::input::native::ActionState;
use lightyear::prelude::input::InputBuffer;
use lightyear::prelude::server::*;
use lightyear::prelude::*;

/// Per-avatar tally of how many simulated ticks the server had a *fresh* input for
/// vs. how many it had to fall back on the last-known input (the input for that tick
/// was late or lost). This is the signal for sizing how far the client timeline
/// should lead the server: if `late` is a meaningful fraction of `total`, that
/// client's inputs aren't arriving in time and its sync margin is too tight. Reset
/// each telemetry window by [`flush_telemetry`] via [`Self::take`].
#[derive(Component, Default)]
struct LateInputStats {
    /// Ticks the input for the current tick was missing → server reused the last input.
    late: u32,
    /// Ticks the avatar had a live input buffer (`late` is counted out of this).
    total: u32,
}

/// Count late/lost inputs per avatar, every simulated tick. Runs in `FixedUpdate`,
/// i.e. after lightyear's `update_action_state` (FixedPreUpdate) has consumed this
/// tick's input from the buffer. The distinction (see `InputBuffer::get` vs
/// `get_predict`): `get_predict(tick)` is `Some` once the client is live (there is
/// at least a last-known input to fall back on); `get(tick)` is `Some` only if the
/// input *for this exact tick* is present. Present-via-fallback but not exact ⇒ the
/// server simulated this tick on a stale input because the real one hadn't arrived —
/// a buffer underrun, which a larger client lead would have prevented.
fn count_late_inputs(
    timeline: Res<LocalTimeline>,
    mut avatars: Query<(&InputBuffer<ActionState<NetInput>, NetInput>, &mut LateInputStats)>,
) {
    let tick = timeline.tick();
    for (buffer, mut stats) in &mut avatars {
        if buffer.get_predict(tick).is_some() {
            stats.total += 1;
            if buffer.get(tick).is_none() {
                stats.late += 1;
            }
        }
    }
}

/// One per-client telemetry row (a ~2s window). `None` columns are written as SQL
/// NULL: rtt/jitter before the first ping samples land, the rollback fields when no
/// report arrived this window, late-input when the avatar had no live input buffer.
struct Sample {
    ts_ms: i64,
    sha: &'static str,
    /// The client link entity's bits — the same id the live `[tel]` log line prints,
    /// stable for the session so rows correlate across windows.
    client: i64,
    rtt_ms: Option<f64>,
    jitter_ms: Option<f64>,
    samples: Option<i64>,
    rollbacks: Option<u32>,
    rollback_ticks: Option<u32>,
    max_pos_err_mm: Option<u32>,
    pos_triggers: Option<u32>,
    late_inputs: Option<u32>,
    input_ticks: Option<u32>,
}

/// SQLite telemetry sink. `Connection` is `Send` but not `Sync`, so wrap it in a
/// `Mutex` to satisfy `Resource` (only one system ever touches it, so contention is nil).
#[derive(Resource)]
struct TelemetryDb(Mutex<rusqlite::Connection>);

impl TelemetryDb {
    fn insert(&self, row: &Sample) {
        let conn = self.0.lock().unwrap();
        if let Err(e) = conn.execute(
            "INSERT INTO samples (ts_ms, sha, client, rtt_ms, jitter_ms, samples, \
             rollbacks, rollback_ticks, max_pos_err_mm, pos_triggers, late_inputs, input_ticks) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                row.ts_ms,
                row.sha,
                row.client,
                row.rtt_ms,
                row.jitter_ms,
                row.samples,
                row.rollbacks,
                row.rollback_ticks,
                row.max_pos_err_mm,
                row.pos_triggers,
                row.late_inputs,
                row.input_ticks,
            ],
        ) {
            eprintln!("[tel] insert failed: {e}");
        }
    }
}

/// Open (or create) the telemetry db at `BS_TELEMETRY_DB` (default `telemetry.db` in
/// the process cwd — under the versioned deploy that's the per-version dir, next to
/// `server.log`, so each build gets its own db). On any failure the resource is
/// simply not inserted and `flush_telemetry` degrades to printing only. WAL +
/// `synchronous=NORMAL` keep the per-window write cheap without risking torn rows.
fn open_telemetry_db(mut commands: Commands) {
    let path = std::env::var("BS_TELEMETRY_DB").unwrap_or_else(|_| "telemetry.db".to_string());
    match rusqlite::Connection::open(&path) {
        Ok(conn) => {
            if let Err(e) = conn.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; \
                 CREATE TABLE IF NOT EXISTS samples ( \
                     ts_ms          INTEGER NOT NULL, \
                     sha            TEXT    NOT NULL, \
                     client         INTEGER NOT NULL, \
                     rtt_ms         REAL, \
                     jitter_ms      REAL, \
                     samples        INTEGER, \
                     rollbacks      INTEGER, \
                     rollback_ticks INTEGER, \
                     max_pos_err_mm INTEGER, \
                     pos_triggers   INTEGER, \
                     late_inputs    INTEGER, \
                     input_ticks    INTEGER \
                 ); \
                 CREATE INDEX IF NOT EXISTS idx_samples_client_ts ON samples(client, ts_ms);",
            ) {
                eprintln!("[tel] schema init failed: {e}");
                return;
            }
            println!("[tel] telemetry db at {path}");
            commands.insert_resource(TelemetryDb(Mutex::new(conn)));
        }
        Err(e) => eprintln!("[tel] failed to open db {path}: {e}"),
    }
}

/// Format an optional telemetry value for the live `[tel]` log line: `-` for a value
/// that's absent this window (keeps the line compact and the columns aligned).
fn o<T: core::fmt::Display>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".to_string())
}

/// Telemetry flush (every ~2s): one wide row per connected client combining latency
/// (`PingManager`), the client's reported rollback load (`RollbackReport`), and the
/// server-measured late-input counts ([`LateInputStats`]). Written to the SQLite sink
/// for later analysis *and* printed as a single `[tel]` line so live `tail -f
/// server.log` still shows everything. `MinimalPlugins` has no `LogPlugin`, hence
/// `println!` (captured by launchd into the version's `server.log`).
fn flush_telemetry(
    time: Res<Time>,
    mut acc: Local<f32>,
    db: Option<Res<TelemetryDb>>,
    mut links: Query<
        (Entity, &PingManager, &mut MessageReceiver<RollbackReport>),
        (With<ClientOf>, With<Connected>),
    >,
    mut avatars: Query<(&ControlledBy, &mut LateInputStats)>,
) {
    *acc += time.delta_secs();
    if *acc < 2.0 {
        return;
    }
    *acc = 0.0;

    // Map each client link → its avatar's accumulated late-input counts. The avatar
    // carries the `InputBuffer`/`LateInputStats`; `ControlledBy.owner` is the link
    // entity whose `PingManager`/`RollbackReport` the rest of the row comes from.
    let mut late: HashMap<Entity, (u32, u32)> = HashMap::new();
    for (controlled, mut stats) in &mut avatars {
        let LateInputStats { late: l, total: t } = core::mem::take(&mut *stats);
        let entry = late.entry(controlled.owner).or_default();
        entry.0 += l;
        entry.1 += t;
    }

    let ts_ms = save::now_unix_ms() as i64;
    let sha = bad_spaceship_shared::net::BS_VERSION;

    for (entity, ping, mut receiver) in &mut links {
        let client = entity.to_bits() as i64;
        let (rtt_ms, jitter_ms, samples) = if ping.latency_samples_recv() > 0 {
            (
                Some(ping.rtt().as_secs_f64() * 1000.0),
                Some(ping.jitter().as_secs_f64() * 1000.0),
                Some(ping.latency_samples_recv() as i64),
            )
        } else {
            (None, None, None)
        };
        // Unreliable channel → only the newest sample matters; the counters are
        // cumulative, so an older one would just report a smaller total.
        let report = receiver.receive().last();
        let rollbacks = report.as_ref().map(|r| r.rollbacks);
        let rollback_ticks = report.as_ref().map(|r| r.rollback_ticks);
        let max_pos_err_mm = report.as_ref().map(|r| r.max_pos_err_mm);
        let pos_triggers = report.as_ref().map(|r| r.pos_triggers);
        let (late_inputs, input_ticks) = late.get(&entity).copied().unzip();

        println!(
            "[tel] client={} rtt={}ms jitter={}ms samples={} rb={} rbt={} errmm={} trig={} late={}/{}",
            entity.to_bits(),
            o(rtt_ms.map(|v| format!("{v:.1}"))),
            o(jitter_ms.map(|v| format!("{v:.1}"))),
            o(samples),
            o(rollbacks),
            o(rollback_ticks),
            o(max_pos_err_mm),
            o(pos_triggers),
            o(late_inputs),
            o(input_ticks),
        );

        if let Some(db) = &db {
            db.insert(&Sample {
                ts_ms,
                sha,
                client,
                rtt_ms,
                jitter_ms,
                samples,
                rollbacks,
                rollback_ticks,
                max_pos_err_mm,
                pos_triggers,
                late_inputs,
                input_ticks,
            });
        }
    }
}

/// TEMPORARY: print a client's forwarded panic (`[panic] …`) into the version's
/// `server.log`. wasm client panics otherwise only reach the phone's browser console,
/// unreachable from the build box; the client stashes the message and forwards it on the
/// next connect. Remove with [`ClientPanicReport`].
fn log_client_panics(
    mut links: Query<
        (Entity, &mut MessageReceiver<ClientPanicReport>),
        (With<ClientOf>, With<Connected>),
    >,
) {
    for (entity, mut receiver) in &mut links {
        for ClientPanicReport(msg) in receiver.receive() {
            println!("[panic] client={} {}", entity.to_bits(), msg);
        }
    }
}

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
        // Owns the Avian `Position`↔`Transform` sync (its sub-plugins are disabled
        // in multiplayer by `add_physics`) and the rollback wiring. State
        // replication (server `Position` is truth) → no `rollback_resources`.
        app.add_plugins(lightyear_avian3d::prelude::LightyearAvianPlugin {
            replication_mode: lightyear_avian3d::plugin::AvianReplicationMode::Position,
            update_syncs_manually: false,
            rollback_resources: false,
        });
        // Room-based interest management: scope replication so a client only sees
        // entities sharing one of its rooms.
        app.add_plugins(RoomPlugin);
        app.init_resource::<RoomRegistry>();
        // Per-room rocket-launch countdown state (see `LaunchRegistry`).
        app.init_resource::<LaunchRegistry>();
        // Server-authoritative session resume: remember each player's last position
        // (keyed by its persistent `resume_id`) so a reconnect after an iOS reload
        // lands back in place rather than at the origin.
        app.init_resource::<ResumeRegistry>();
        // The server owns the part world per room, so suppress the shared
        // single-set spawner (`spawn_initial_parts`/`spawn_part`/
        // `replace_fallen_parts` in `PartPlugin`); the per-room spawner below
        // replaces it.
        app.insert_resource(SuppressLocalParts);
        // The server simulates one `ServerAvatar` body per connected client, so
        // suppress the stray local single-player character `CommonPlugins` would spawn.
        app.insert_resource(SuppressLocalPlayer);
        app.add_systems(Startup, start_server);
        // Open the telemetry db once at startup (degrades to log-only if it fails).
        app.add_systems(Startup, open_telemetry_db);
        // One server-owned, replicated player per client that connects.
        app.add_observer(spawn_player_for_client);
        // Tally late/lost inputs per simulated tick (FixedUpdate, after lightyear's
        // input-buffer read), then flush a combined per-client telemetry row (latency
        // + rollback load + late inputs) to the db + `[tel]` log line every ~2s.
        app.add_systems(FixedUpdate, count_late_inputs);
        app.add_systems(Update, flush_telemetry);
        app.add_systems(Update, log_client_panics);
        // Refill a room's parts that fall off its platform, and mirror each avatar's
        // look yaw into its replicated facing. Parts and joints replicate their state
        // directly (predicted Avian `Position`/`Rotation`; `NetJoint` data) — nothing
        // to stream per-frame.
        app.add_systems(
            Update,
            (sync_avatar_facing, sync_net_hold, update_assembly_center_of_mass),
        );
        // Part recycling runs in `FixedUpdate`, NOT `Update`: it also catches parts
        // whose state went non-finite / absurd (a diverging constraint solve), and
        // those MUST be despawned before the *next* Avian step — its broadphase
        // asserts on a NaN AABB and panics the whole server. `FixedUpdate` precedes
        // the physics step (`FixedPostUpdate`) on every tick, while an `Update`
        // system can be skipped between two back-to-back fixed steps in a lagging
        // frame — exactly when an explosion is underway.
        app.add_systems(FixedUpdate, replace_fallen_room_parts);
        // Assign each client (and its avatar) to its reported room on the first
        // input, lazily creating the room's world, and apply client rename +
        // avatar-pick + reset-position requests.
        app.add_systems(
            Update,
            (assign_rooms, apply_name_changes, apply_avatar_changes, apply_position_resets),
        );
        // Rocket launch: accept a client's launch request for its room, run that room's
        // countdown, and at blastoff cut the assembly's ground joints. Publishing the
        // countdown into each room's orb `NetLaunch` lets every client draw the banner.
        app.add_systems(
            Update,
            (handle_launch_requests, tick_room_launches, publish_room_launch).chain(),
        );
        // Session resume: continuously remember live avatars' positions (the reconnect
        // restore itself happens at connect, in `spawn_player_for_client`).
        app.add_systems(Update, record_resume_positions);
        // Save games: a rolling per-room autosave every `AUTOSAVE_SECS`, plus the
        // player-named manual save (`SaveGame` over the control channel). Loading
        // happens at room creation (`assign_rooms` consumes the matchmaker-staged
        // pending file).
        app.add_systems(Update, (autosave_rooms, apply_manual_saves));
        // Opt-in flight recorder (`BS_RECORD`): one world snapshot + all inputs per
        // simulated tick per occupied room, as JSONL, for frame-by-frame analysis
        // of a reproduced bug. `FixedLast` = after the physics step, so each line
        // is the tick's *outcome* alongside the inputs that produced it.
        if std::env::var("BS_RECORD").is_ok() {
            app.init_resource::<RecordingRegistry>();
            app.add_systems(FixedLast, record_room_frames);
        }
        // Bridge each client's per-tick input intent into its avatar's movement
        // inputs (`DirectionalInput`/`Yaw`), before the shared movement systems
        // read them on the same sim tick — so the server simulates the character
        // authoritatively from intent. Grab/hold run in Update where Avian's
        // `Forces` helper accumulates (matching single-player `position_held_part`).
        // Grab-latch + the hold/orient spring run in `FixedUpdate` (was `Update`)
        // so the spring force lands in the same tick as the Avian step that
        // consumes it — phase-aligned with the client's *predicted* hold spring
        // (`predict_hold`), so the two worlds diverge only by round-trip and don't
        // generate constant rollback churn from a fixed schedule offset. `server_attach`
        // runs in the same chain (it mutates the same `HeldPart`) and is tick-aligned
        // so its retry window samples one contact graph per sim tick.
        app.add_systems(
            FixedUpdate,
            (
                apply_net_input.before(CharacterMovement),
                (server_grab, server_hold, server_attach, server_delete).chain(),
                // Balanced rocket thrust for launched rooms — a continuous force, so it
                // runs per physics tick like the hold spring.
                apply_room_rocket_thrust,
            ),
        );
    }
}

/// A lobby room on the server: its lightyear `RoomId` (replication visibility)
/// and the single Avian collision-layer bit isolating its parts in the one
/// shared physics world.
#[derive(Clone, Copy)]
struct Room {
    id: RoomId,
    /// A single set bit in `1..=31` (bit 0 is the default layer the ground sits
    /// on, so rooms never use it).
    bit: u32,
}

/// Maps each lobby code to its allocated [`Room`]. Rooms are created lazily the
/// first time a client reports a code.
#[derive(Resource, Default)]
struct RoomRegistry {
    by_code: HashMap<[u8; 6], Room>,
}

impl RoomRegistry {
    /// Look up the room for a code, allocating a new `RoomId` + collision bit on
    /// first sighting. Returns the room and whether it was just created (so the
    /// caller can spawn its world once).
    fn get_or_create(&mut self, code: [u8; 6], allocator: &mut RoomAllocator) -> (Room, bool) {
        if let Some(room) = self.by_code.get(&code) {
            return (*room, false);
        }
        // Bits 1..=31 isolate rooms in the one Avian world (bit 0 is the ground's
        // default layer). Wrap after 31 rooms — parts in two rooms that recycle
        // the same bit would collide, the documented cap for this slice.
        let bit = 1u32 << (1 + self.by_code.len() as u32 % 31);
        let room = Room { id: allocator.allocate(), bit };
        self.by_code.insert(code, room);
        (room, true)
    }
}

/// How long the server remembers a disconnected player's last position so a
/// reconnecting client (same persistent `NetInput::resume_id`) resumes where it
/// left off instead of respawning at the origin. iOS tab-suspension drops the
/// socket abruptly (no clean disconnect event), so the position is recorded
/// continuously while the player is live and the last value survives the drop. Past
/// this window the record is swept and the player is treated as gone. Generous (30
/// min) so stepping away for a while — or a long enough background that iOS evicts
/// the page and forces a full reload — still resumes in place; the records are tiny
/// (one `Vec3` + timestamp per player).
const RESUME_GRACE_SECS: u64 = 1800;

/// Remembers each player's last avatar position, keyed by the client's persistent
/// `NetInput::resume_id`. Written continuously by `record_resume_positions` (so an
/// abrupt iOS drop still leaves the last position behind); read once on the first
/// input after a reconnect by `assign_rooms`. This is the "server remembers the
/// user" half of the session-resume: the reload rebuilds the wasm client, the client
/// re-sends its persisted id, and the server places it back where it was.
#[derive(Resource, Default)]
struct ResumeRegistry {
    /// resume id → (last position, room code it was recorded in, when).
    by_id: HashMap<u64, (Vec3, [u8; 6], SystemTime)>,
}

/// The room code a resumed position came from, riding on the avatar until its
/// first input reveals which room it is actually joining (`assign_rooms`). A
/// remembered position is only valid in *its own* room: restoring it into a
/// different room teleported players to their old spot — e.g. loading a fresh
/// save after falling off a rocket respawned them mid-air at altitude instead
/// of at the new room's spawn.
#[derive(Component, Clone, Copy)]
struct ResumeRoom([u8; 6]);

/// The room a player avatar belongs to (server-side), so its grab is scoped to
/// that room's parts.
#[derive(Component, Clone, Copy)]
struct RoomMember(RoomId);

/// The room a replicated part belongs to (its `RoomId` + collision bit), so a
/// fallen part is replaced into the same room/layer.
#[derive(Component, Clone, Copy)]
struct PartRoom {
    id: RoomId,
    bit: u32,
}

/// Tags the per-room center-of-mass orb entity with the room it reports for, so
/// `update_assembly_center_of_mass` can write that room's largest-assembly COM into
/// its replicated [`NetCenterOfMass`].
#[derive(Component, Clone, Copy)]
struct OrbRoom(RoomId);

/// The part a networked player is currently holding (server-authoritative).
#[derive(Component, Default)]
struct HeldPart(Option<Entity>);

/// How long (ticks) the server keeps trying to satisfy an attach intent after the
/// join button is pressed. A held part floats on its spring and is usually only
/// *intermittently* touching the part you're pressing it against, so a one-shot
/// intent processed on a single tick mostly misses (you had to mash the button).
/// This window joins as soon as contact exists within ~0.5s of the press.
const ATTACH_WINDOW_TICKS: u32 = 30;

/// Per-player attach-intent latch. A **rising edge** on the networked `attach`
/// intent (`prev` tracks the previous value) arms `pending` for [`ATTACH_WINDOW_TICKS`];
/// while armed, `server_attach` joins the held part to whatever it's touching the
/// first tick contact exists, then clears `pending`. Rising-edge arming lets the
/// client re-send the intent across several ticks (packet-loss robustness) without
/// re-arming, and clearing on success prevents a second joint from the same press.
#[derive(Component, Default)]
struct AttachState {
    pending: u32,
    prev: bool,
}

/// Assign each connected client (and its avatar) to the room it reported, the
/// first time a real input arrives. Lazily creates the room's world (parts) on
/// first sighting of a code. Until assigned, the avatar carries no
/// `Rooms` filter, so it's visible to its own client (which bootstraps the
/// input/control loop) — the assignment then scopes it.
fn assign_rooms(
    mut commands: Commands,
    mut registry: ResMut<RoomRegistry>,
    mut allocator: ResMut<RoomAllocator>,
    // For a room created from a saved game: the ground entity (saved ground joints
    // re-anchor to it) and the launch registry (a launched save resumes thrusting).
    grounds: Query<Entity, With<Grass>>,
    mut launches: ResMut<LaunchRegistry>,
    players: Query<
        (Entity, &ActionState<NetInput>, &ControlledBy, &NetName, Option<&ResumeRoom>),
        Without<RoomMember>,
    >,
    // Already-assigned avatars' names, so a fresh join picks a default that's unique
    // within its room.
    named: Query<(&NetName, &RoomMember)>,
    // For revoking a cross-room resume position (see `ResumeRoom`): teleport the
    // already-built body back to a fresh spawn. Scoped to avatars so this system
    // doesn't declare conflicting access to every rigid body's pose (which would
    // serialize it against the physics-adjacent systems every tick).
    mut bodies: Query<
        (&mut Position, &mut LinearVelocity, &mut AngularVelocity),
        With<ServerAvatar>,
    >,
) {
    // The `players` query is `Without<RoomMember>`, so on the vast majority of ticks
    // nobody is joining — skip the whole name-bookkeeping scan then.
    if players.iter().next().is_none() {
        return;
    }
    // The default-name numbers already taken in each room (from existing avatars).
    // Multiple clients can cross the first-input line on the same frame, so newly
    // assigned numbers are folded back in as we go to keep this run's picks distinct.
    let mut used: HashMap<RoomId, HashSet<u32>> = HashMap::new();
    for (name, member) in &named {
        if let Some(n) = default_name_number(&name.0) {
            used.entry(member.0).or_default().insert(n);
        }
    }
    for (entity, state, controlled, name, resume_room) in &players {
        // Wait for the first real input — the all-zero seed carries no room (a
        // real input always has a unit-quaternion rotation, never `[0,0,0,0]`).
        if state.0 == NetInput::default() {
            continue;
        }
        // A resumed position is only meaningful in the room it was recorded in.
        // If this client's first input reveals a *different* room (e.g. it loaded
        // a fresh save), revoke the optimistic connect-time restore: drop the
        // pending `InitialPose` (body not built yet) and/or teleport the built
        // body to a fresh spawn — otherwise the player starts mid-air wherever
        // they last were in the old room.
        if let Some(ResumeRoom(recorded)) = resume_room {
            if *recorded != state.0.room {
                commands.entity(entity).remove::<InitialPose>();
                if let Ok((mut position, mut linear, mut angular)) = bodies.get_mut(entity) {
                    position.0 = spawn_position();
                    linear.0 = Vec3::ZERO;
                    angular.0 = Vec3::ZERO;
                }
                println!("[resume] revoked cross-room resume (recorded room != joined room)");
            }
            commands.entity(entity).remove::<ResumeRoom>();
        }
        let (room, is_new) = registry.get_or_create(state.0.room, &mut allocator);
        if is_new {
            // A room created by the matchmaker's "load saved game" flow has a pending
            // save staged under its code — rebuild that world; otherwise spawn the
            // normal random one.
            match save::take_pending(&save::code_string(state.0.room)) {
                Some(world) => spawn_room_world_from_save(
                    &mut commands,
                    room,
                    &world,
                    grounds.iter().next(),
                    &mut launches,
                ),
                None => spawn_room_world(&mut commands, room),
            }
        }
        // Scope this avatar and this client to the room (`Rooms` is immutable, so
        // `insert` replaces any prior membership). The avatar is a real dynamic body
        // in the one shared Avian world, so isolate it to the room's collision layer
        // too (membership = room bit, filter = room bit + ground's default bit 0) —
        // otherwise it would shove *every* room's blocks. Matches `tag_room_part`,
        // so same-room avatars/parts/ground interact and cross-room ones don't.
        commands.entity(entity).insert((
            Rooms::single(room.id),
            RoomMember(room.id),
            CollisionLayers::from_bits(room.bit, room.bit | GROUND_LAYER),
        ));
        commands.entity(controlled.owner).insert(Rooms::single(room.id));
        // Give a still-unnamed avatar the lowest free "Player N" in its room, reserving
        // it for the run. Skip if it already carries a name — a rename that raced ahead
        // of room assignment must not be clobbered by a default.
        if name.0.is_empty() {
            let room_used = used.entry(room.id).or_default();
            let number = (1u32..).find(|n| !room_used.contains(n)).unwrap();
            room_used.insert(number);
            commands.entity(entity).insert(NetName(format!("Player {number}")));
            info!("client {:?} joined room {:?} as Player {number}", controlled.owner, room.id);
        } else {
            info!("client {:?} joined room {:?} as {}", controlled.owner, room.id, name.0);
        }
    }
}

/// Parse the number out of a default `"Player N"` name, or `None` for a custom name.
/// Used by `assign_rooms` to find which default numbers are taken in a room (so it
/// can hand the next joiner the lowest free one).
fn default_name_number(name: &str) -> Option<u32> {
    name.strip_prefix("Player ").and_then(|n| n.parse::<u32>().ok())
}

/// Apply each client's `SetName` rename to its avatar's replicated [`NetName`].
/// The message rides the reliable `ControlChannel` and lands on the client's link
/// entity; map that link to the avatar it `ControlledBy`, sanitise the requested
/// name, and (if non-empty) write it — from where it re-replicates to everyone in
/// the room. `set_if_neq` avoids re-replicating an unchanged name.
fn apply_name_changes(
    mut links: Query<(Entity, &mut MessageReceiver<SetName>), (With<ClientOf>, With<Connected>)>,
    mut avatars: Query<(&ControlledBy, &mut NetName)>,
) {
    for (link, mut receiver) in &mut links {
        // Sequenced-reliable: only the newest rename this window matters.
        let Some(msg) = receiver.receive().last() else {
            continue;
        };
        let name = sanitize_name(&msg.0);
        if name.is_empty() {
            continue;
        }
        for (controlled, mut net_name) in &mut avatars {
            if controlled.owner == link {
                net_name.set_if_neq(NetName(name.clone()));
            }
        }
    }
}

/// Apply each client's `SetAvatar` pick to its avatar's replicated [`NetPlayer::monster`].
/// Mirrors [`apply_name_changes`]: the message rides the reliable `ControlChannel`, lands
/// on the client's link entity, and is mapped to the avatar it `ControlledBy`. The index
/// is reduced modulo [`MONSTER_COUNT`] (defensive against a malformed pick), and only
/// written when it changed — the `if` guard reads through `Deref` without dirtying the
/// component, so an unchanged pick doesn't re-replicate. The change replicates to every
/// client in the room, where the monster-dressing systems rebuild the visual.
fn apply_avatar_changes(
    mut links: Query<(Entity, &mut MessageReceiver<SetAvatar>), (With<ClientOf>, With<Connected>)>,
    mut avatars: Query<(&ControlledBy, &mut NetPlayer)>,
) {
    for (link, mut receiver) in &mut links {
        // Sequenced-reliable: only the newest pick this window matters.
        let Some(msg) = receiver.receive().last() else {
            continue;
        };
        let monster = msg.0 % MONSTER_COUNT;
        for (controlled, mut player) in &mut avatars {
            if controlled.owner == link && player.monster != monster {
                player.monster = monster;
            }
        }
    }
}

/// Teleport a client's avatar back to a fresh spawn position on a `ResetPosition`
/// request (the "Reset Position" menu action). Server-authoritative: move the
/// avatar's `Position` and zero its velocity, so the reset replicates/corrects on the
/// owner's predicted body. Maps the client link to its avatar via `ControlledBy`, the
/// same way `apply_name_changes` does. `spawn_position` is the shared spawn rule, so a
/// reset lands on the same valid on-platform disc a fresh join uses.
fn apply_position_resets(
    mut links: Query<
        (Entity, &mut MessageReceiver<ResetPosition>),
        (With<ClientOf>, With<Connected>),
    >,
    mut avatars: Query<
        (&ControlledBy, &mut Position, &mut LinearVelocity, &mut AngularVelocity),
        With<ServerAvatar>,
    >,
) {
    for (link, mut receiver) in &mut links {
        // Drain the window; act once if any reset was requested (a single teleport is
        // idempotent, so coalescing repeats is correct).
        if receiver.receive().count() == 0 {
            continue;
        }
        for (controlled, mut position, mut linear, mut angular) in &mut avatars {
            if controlled.owner == link {
                position.0 = spawn_position();
                linear.0 = Vec3::ZERO;
                angular.0 = Vec3::ZERO;
            }
        }
    }
}

/// Continuously remember each live, room-assigned avatar's position keyed by its
/// persistent `resume_id`, and sweep records past the grace window. Throttled —
/// position barely moves in a fraction of a second and this bounds per-frame cost.
/// Gated on `RoomMember` (only fully-joined avatars). The reconnect *restore* happens
/// at connect (`spawn_player_for_client` → `InitialPose`), so the body is built
/// directly at the remembered spot and there's no transient origin for this recorder
/// to capture.
fn record_resume_positions(
    time: Res<Time>,
    mut throttle: Local<f32>,
    mut resume: ResMut<ResumeRegistry>,
    avatars: Query<(&ActionState<NetInput>, &Position), With<RoomMember>>,
) {
    *throttle -= time.delta_secs();
    if *throttle > 0.0 {
        return;
    }
    *throttle = 0.25;
    let now = SystemTime::now();
    for (state, position) in &avatars {
        if state.0.resume_id != 0 {
            resume.by_id.insert(state.0.resume_id, (position.0, state.0.room, now));
        }
    }
    // Drop records for players who didn't return within the grace window.
    resume.by_id.retain(|_, (_, _, at)| {
        at.elapsed().map(|e| e.as_secs() < RESUME_GRACE_SECS).unwrap_or(false)
    });
}

/// Spawn a fresh room's world: its own set of parts (replicated + predicted +
/// collision-isolated to the room). Parts replicate immediately — a client that
/// joins mid-fall (or mid-shove) now receives their velocity too, so its predicted
/// copy falls in sync rather than drifting.
fn spawn_room_world(commands: &mut Commands, room: Room) {
    for _ in 0..NUM_PARTS {
        let (entity, half_extents, seed) = spawn_random_part(commands);
        tag_room_part(commands, entity, PartShape::Cuboid { half_extents: half_extents.to_array() }, seed, room);
    }
    // Rocket engines join the loose-parts pool (see `spawn_random_part` above): same
    // room-scoped replication + prediction, distinguished only by `PartShape::RocketEngine`
    // so each client rebuilds the cylinder+cone body instead of a cuboid. Rockets carry no
    // appearance seed (their striped body material is fixed) — pass 0.
    for _ in 0..NUM_ROCKET_ENGINES {
        let entity = spawn_random_rocket(commands);
        tag_room_part(commands, entity, PartShape::RocketEngine, 0, room);
    }
    spawn_room_orb(commands, room);
}

/// One center-of-mass orb per room: a server-owned, replicated marker whose
/// `NetCenterOfMass` the server rewrites as the room's largest assembly changes /
/// moves (`update_assembly_center_of_mass`). It carries no physics body — it's a
/// pure data holder the client renders a floating orb from. Scoped to the room so
/// only that room's clients receive it. The same entity carries the room's
/// launch/countdown state (`NetLaunch`), so a single replicated entity per room
/// tells clients both where the COM is and where the launch sequence is.
fn spawn_room_orb(commands: &mut Commands, room: Room) {
    commands.spawn((
        NetCenterOfMass::default(),
        NetLaunch::default(),
        Replicate::to_clients(NetworkTarget::All),
        Rooms::single(room.id),
        OrbRoom(room.id),
    ));
}

/// Spawn a fresh room's world from a saved snapshot instead of the random pool
/// (the load half of the save-game feature — see `crate::save`): respawn every
/// saved part at its saved pose/velocity, rebuild the joints (remapping saved
/// part indices to the new entities; ground endpoints to the shared `Grass`
/// entity), restore the launched flag, and spawn the room orb.
fn spawn_room_world_from_save(
    commands: &mut Commands,
    room: Room,
    world: &SaveWorld,
    ground: Option<Entity>,
    launches: &mut LaunchRegistry,
) {
    let entities: Vec<Entity> = world
        .parts
        .iter()
        .map(|p| {
            let pos = Vec3::from_array(p.position);
            let rot = Quat::from_array(p.rotation);
            let (entity, shape) = match p.shape {
                SaveShape::Cuboid { half_extents } => (
                    spawn_saved_cuboid(commands, Vec3::from_array(half_extents), p.seed),
                    PartShape::Cuboid { half_extents },
                ),
                SaveShape::RocketEngine => {
                    (spawn_rocket_engine(commands, pos), PartShape::RocketEngine)
                }
            };
            // The saved pose/velocity. Both `Transform` AND the Avian components
            // must be seeded — `lightyear_avian` owns the sync in multiplayer, so a
            // `Transform` alone would leave the body simulating from the origin
            // (see `spawn_random_part`).
            commands.entity(entity).insert((
                Transform::from_translation(pos).with_rotation(rot),
                Position(pos),
                Rotation(rot),
                LinearVelocity(Vec3::from_array(p.linear_velocity)),
                AngularVelocity(Vec3::from_array(p.angular_velocity)),
            ));
            tag_room_part(commands, entity, shape, p.seed, room);
            entity
        })
        .collect();

    for joint in &world.joints {
        let resolve = |body: SaveBody| match body {
            SaveBody::Part(i) => entities.get(i as usize).copied(),
            SaveBody::Ground => ground,
        };
        let (Some(b1), Some(b2)) = (resolve(joint.body1), resolve(joint.body2)) else {
            // A part index past the parts list (a corrupt/hand-edited file) or a
            // ground joint with no ground entity — drop the joint, keep the world.
            println!("[save] skipping joint with unresolvable endpoint ({joint:?})");
            continue;
        };
        let net_id = |body: SaveBody, e: Entity| match body {
            SaveBody::Ground => GROUND_JOINT_ID,
            SaveBody::Part(_) => e.to_bits(),
        };
        // The exact spawn the live attach path uses, minus the contact discovery —
        // the anchors come from the save.
        spawn_room_joint(
            commands,
            room.id,
            (b1, Vec3::from_array(joint.anchor1), net_id(joint.body1, b1)),
            (b2, Vec3::from_array(joint.anchor2), net_id(joint.body2, b2)),
        );
    }

    // A world saved after blastoff resumes with its rockets firing.
    if world.launched {
        launches.by_room.insert(room.id, RoomLaunch::Launched);
    }
    spawn_room_orb(commands, room);
}

/// Tag a freshly-spawned part for room-scoped replication: its shape + stable id
/// via `NetPart`, its pose via the predicted Avian `Position`/`Rotation`,
/// replicated + predicted, scoped to the room's `Rooms`, and isolated to the
/// room's collision layer (it collides only with same-room parts and the ground —
/// default bit 0).
fn tag_room_part(commands: &mut Commands, entity: Entity, shape: PartShape, seed: u32, room: Room) {
    commands.entity(entity).insert((
        // `id` is the part's stable cross-network identity (this entity's bits), so
        // a replicated `NetJoint` can name its two endpoints and the client can find
        // the matching *predicted* parts to joint locally.
        NetPart { shape, id: entity.to_bits(), seed },
        Replicate::to_clients(NetworkTarget::All),
        // Predict the loose blocks on every client in the room: each client
        // simulates them locally (so shoving one is instant) and rollback reconciles
        // against the server's authoritative Avian `Position`/`Rotation` (which ride
        // on the predicted components registered in `ProtocolPlugin`).
        PredictionTarget::to_clients(NetworkTarget::All),
        Rooms::single(room.id),
        PartRoom { id: room.id, bit: room.bit },
        CollisionLayers::from_bits(room.bit, room.bit | GROUND_LAYER),
    ));
}

/// Resolve each player's grab intent: on grab, latch the part (in the player's
/// room) the player is most directly looking at — the same selection as
/// single-player, cast from the forwarded orbit-center origin along the ray to
/// the hold target. On release, let go. The part stays dynamic throughout.
fn server_grab(
    mut players: Query<(&ActionState<NetInput>, &mut HeldPart, &RoomMember)>,
    parts: Query<(Entity, &Position, &PartRoom), With<NetPart>>,
) {
    for (state, mut held, member) in &mut players {
        if !state.0.grab {
            held.0 = None;
            continue;
        }
        if held.0.is_some() {
            continue;
        }
        let origin = Vec3::from_array(state.0.grab_origin);
        let look = (Vec3::from_array(state.0.hold_target) - origin).normalize_or_zero();
        held.0 = focused_part(
            origin,
            look,
            parts
                .iter()
                .filter(|(_, _, part_room)| part_room.id == member.0)
                .map(|(entity, p, _)| (entity, p.0)),
        );
    }
}

/// Float each held part to its holder's hold point (critically-damped
/// anti-gravity force, matching `position_held_part`) AND orient it toward the
/// hold rotation (critically-damped angular acceleration, matching
/// `orient_held_part`). The part stays dynamic, so it still collides. Both forces
/// go through the one `Forces` accessor to avoid an ambiguous double-write.
fn server_hold(
    players: Query<(&ActionState<NetInput>, &HeldPart)>,
    // Read the authoritative Avian `Position`/`Rotation` rather than `Transform`:
    // in multiplayer `lightyear_avian` owns the Position→Transform sync (the Avian
    // `PhysicsTransformPlugin` is disabled), so `Transform` can lag within the fixed
    // schedule; `Position`/`Rotation` are always current where the step reads them.
    mut parts: Query<(&Position, &Rotation, Forces), With<NetPart>>,
    gravity: Res<Gravity>,
) {
    for (state, held) in &players {
        let Some(part_entity) = held.0 else {
            continue;
        };
        let Ok((position, rotation, mut forces)) = parts.get_mut(part_entity) else {
            continue;
        };
        // The hold target/orientation arrive over the wire (the client forwards its
        // HoldPoint world position + tracked target rotation); the same spring the
        // client predicts locally (`predict_hold`) runs here authoritatively.
        apply_hold_spring(
            &mut forces,
            position.0,
            rotation.0,
            Vec3::from_array(state.0.hold_target),
            Quat::from_array(state.0.hold_rotation),
            gravity.0,
        );
    }
}

/// On the attach intent, joint the held part to whatever (other) part — or the
/// ground — it's touching, at the contact anchors — then release it (it's now part
/// of the assembly). Cross-room parts can't touch (collision layers isolate rooms)
/// and the ground is shared but the joint itself is room-tagged, so the join is
/// room-scoped automatically. Ports single-player's
/// `update_active_joints`/`attach`, recovering each body-local anchor via the shared
/// `local_contact_anchor` (the COM-relative anchor convention lives there). Joints
/// are server physics, so the joined parts move together and their replicated poses
/// tell the story (no joint replication needed).
fn server_attach(
    mut commands: Commands,
    collisions: Collisions,
    // Recover the anchors from the authoritative Avian `Rotation`, not `Transform`:
    // in multiplayer `lightyear_avian` owns the Position→Transform sync, so a body's
    // `Transform` can lag the rotation the contact anchors were computed against.
    rotations: Query<&Rotation>,
    coms: Query<&ComputedCenterOfMass>,
    net_parts: Query<(), With<NetPart>>,
    grounds: Query<(), With<Grass>>,
    // Existing joints (to tell which parts are joining for the FIRST time) and each
    // part's room (to spawn the replacement in the same room).
    joints: Query<&SphericalJoint>,
    part_rooms: Query<&PartRoom>,
    mut players: Query<(&ActionState<NetInput>, &HeldPart, &RoomMember, &mut AttachState)>,
) {
    // Parts that already had a joint before this tick. A part gaining its first
    // joint is consumed into a structure, so spawn a fresh random part to replace
    // it in the room's loose-parts pool (`commands.spawn` is deferred, so this
    // reflects the pre-attach state). Mirrors single-player's `attach`.
    let had_joint: Vec<Entity> = joints.iter().flat_map(|j| [j.body1, j.body2]).collect();
    for (state, held, member, mut attach) in &mut players {
        // Arm a retry window on the rising edge of the intent (see `AttachState`).
        if state.0.attach && !attach.prev {
            attach.pending = ATTACH_WINDOW_TICKS;
        }
        attach.prev = state.0.attach;
        if attach.pending == 0 {
            continue;
        }
        attach.pending -= 1;
        let Some(held_entity) = held.0 else {
            continue;
        };
        let held_room = part_rooms.get(held_entity).ok().map(|pr| Room { id: pr.id, bit: pr.bit });
        let mut attached = false;
        let mut replaced: Vec<Entity> = Vec::new();
        for pair in collisions.collisions_with(held_entity) {
            if !pair.is_touching() {
                continue;
            }
            let (c1, c2) = (pair.collider1, pair.collider2);
            // Only attach to another replicated part or the ground (not a character).
            let other = if c1 == held_entity { c2 } else { c1 };
            if net_parts.get(other).is_err() && grounds.get(other).is_err() {
                continue;
            }
            let rot = |e| rotations.get(e).map(|r| r.0).unwrap_or(Quat::IDENTITY);
            let com = |e| coms.get(e).map(|c| c.0).unwrap_or(Vec3::ZERO);
            for manifold in &pair.manifolds {
                for contact in &manifold.points {
                    let p1 = local_contact_anchor(rot(c1), com(c1), contact.anchor1);
                    let p2 = local_contact_anchor(rot(c2), com(c2), contact.anchor2);
                    // Default order body1=c2, body2=c1 — but a ground joint is
                    // normalized so the *part* is body1: the client anchors the
                    // joint gizmo to body1 (`position_replicated_joints` looks it
                    // up as a `NetPart`), and the ground endpoint is named by the
                    // `GROUND_JOINT_ID` sentinel (it has no `NetPart::id`).
                    let ((b1, a1), (b2, a2)) = if grounds.get(c2).is_ok() {
                        ((c1, p1), (c2, p2))
                    } else {
                        ((c2, p2), (c1, p1))
                    };
                    let net_id = |e: Entity| {
                        if grounds.get(e).is_ok() { GROUND_JOINT_ID } else { e.to_bits() }
                    };
                    spawn_room_joint(
                        &mut commands,
                        member.0,
                        (b1, a1, net_id(b1)),
                        (b2, a2, net_id(b2)),
                    );
                    attached = true;
                    // Replenish the pool for each *part* endpoint joining for the
                    // first time (the ground isn't a loose part — no replacement).
                    for endpoint in [held_entity, other] {
                        if net_parts.get(endpoint).is_ok()
                            && !had_joint.contains(&endpoint)
                            && !replaced.contains(&endpoint)
                        {
                            replaced.push(endpoint);
                            if let Some(room) = held_room {
                                let (new_entity, half_extents, seed) = spawn_random_part(&mut commands);
                                tag_room_part(
                                    &mut commands,
                                    new_entity,
                                    PartShape::Cuboid { half_extents: half_extents.to_array() },
                                    seed,
                                    room,
                                );
                            }
                        }
                    }
                }
            }
        }
        if attached {
            // Keep holding the part after joining (like single-player): you lift your
            // block and the one you joined hangs below it on the new joint. Just close
            // the retry window so this press joins exactly once.
            attach.pending = 0;
        }
    }
}

/// Spawn one authoritative room joint between two bodies, each given as
/// `(entity, body-local anchor, replicated net id)`: the server's `SphericalJoint`
/// plus its replicated `NetJoint` mirror (endpoints by stable id + anchors, so each
/// client can rebuild it as real predicted physics between its predicted parts — and
/// draw it), scoped to the room's visibility, plus the server-only `RoomMember` tag
/// `server_delete` scopes deletes by (rooms share coordinate space, so a distance
/// check alone could hit another room's joint). The **single** spawn point for both
/// the live attach path (`server_attach`) and the saved-world loader
/// (`spawn_room_world_from_save`), so loaded joints can never drift from
/// freshly-built ones.
fn spawn_room_joint(
    commands: &mut Commands,
    room: RoomId,
    (body1, anchor1, id1): (Entity, Vec3, u64),
    (body2, anchor2, id2): (Entity, Vec3, u64),
) {
    commands.spawn((
        SphericalJoint::new(body1, body2)
            .with_local_anchor1(anchor1)
            .with_local_anchor2(anchor2),
        NetJoint {
            body1: id1,
            body2: id2,
            anchor1: anchor1.to_array(),
            anchor2: anchor2.to_array(),
        },
        Replicate::to_clients(NetworkTarget::All),
        Rooms::single(room),
        RoomMember(room),
    ));
}

/// Per-player delete-intent latch. Tracks the previous `delete` value so the
/// despawn fires once on the **rising edge** of the gesture (the client asserts
/// the intent for several ticks for packet-loss robustness, like `attach`).
#[derive(Component, Default)]
struct DeleteState {
    prev: bool,
}

/// On the rising edge of the delete intent, despawn the joint inside the player's
/// delete zone — the empty-handed counterpart to `server_attach`. Mirrors
/// single-player's `update_predelete_joints`/`delete_joints`: a joint is "in the
/// zone" when its `body2` anchor is within `DELETE_RADIUS` of the hold point
/// (`hold_target`, forwarded on `NetInput`). Despawning the server joint entity
/// removes its replication, so every client drops the joint and the assembly
/// separates. Room-scoped via the joint's `RoomMember` (rooms share coordinate
/// space, so the distance check alone could match another room's joint).
fn server_delete(
    mut commands: Commands,
    bodies: Query<(&Position, &Rotation)>,
    joints: Query<(Entity, &SphericalJoint, &RoomMember)>,
    mut players: Query<(&ActionState<NetInput>, &RoomMember, &mut DeleteState)>,
) {
    for (state, member, mut del) in &mut players {
        let rising = state.0.delete && !del.prev;
        del.prev = state.0.delete;
        if !rising {
            continue;
        }
        let hold = Vec3::from_array(state.0.hold_target);
        for (joint_entity, joint, joint_room) in &joints {
            if joint_room.0 != member.0 {
                continue;
            }
            // World position of the `body2` anchor — the same point
            // `update_predelete_joints` measures against the hold point.
            let (Some(anchor2), Ok((pos, rot))) =
                (joint.local_anchor2(), bodies.get(joint.body2))
            else {
                continue;
            };
            let center = pos.0 + rot.0 * anchor2;
            if (center - hold).length() < DELETE_RADIUS {
                commands.entity(joint_entity).despawn();
            }
        }
    }
}

/// Mirror each avatar's look `Yaw` into its replicated `NetFacing`, so remote
/// clients can draw it facing where it looks. Only writes on change (a still avatar
/// stops generating facing traffic). `Yaw` itself is deliberately not replicated —
/// doing so overwrote the owning client's locally-driven look and broke its turning.
fn sync_avatar_facing(mut avatars: Query<(&Yaw, &mut NetFacing)>) {
    for (yaw, mut facing) in &mut avatars {
        if facing.0 != yaw.0 {
            facing.0 = yaw.0;
        }
    }
}

/// Tag each held part with a replicated [`NetHold`] (holder + the hold point/orientation
/// the server springs it toward) and strip it from parts that were released, so every
/// client can run the same hold spring on a held part instead of predicting it in
/// free-fall (the held-part sag/bob for non-holders). The target/orientation are the
/// holder's forwarded `hold_target`/`hold_rotation` — the same values `server_hold`
/// springs toward — so observers and the server agree. Only writes on change, so a held
/// part the holder isn't moving stops generating traffic.
fn sync_net_hold(
    mut commands: Commands,
    players: Query<(&HeldPart, &ActionState<NetInput>, &NetPlayer)>,
    mut tagged: Query<(Entity, &mut NetHold)>,
) {
    // The hold state for every part held this tick, keyed by part entity. If two
    // players somehow latch the same part (a contested grab — `server_grab` doesn't
    // exclude it), last-writer-wins on the holder; a degenerate case that resolves
    // when one of them releases.
    let mut held_now: HashMap<Entity, NetHold> = HashMap::new();
    for (held, state, net_player) in &players {
        if let Some(part) = held.0 {
            held_now.insert(
                part,
                NetHold {
                    holder: net_player.client_id,
                    target: state.0.hold_target,
                    rotation: state.0.hold_rotation,
                },
            );
        }
    }
    // Update parts that already carry a tag; drop the tag from any that were released.
    // `set_if_neq` only dirties (and so re-replicates) `NetHold` on an actual change, so
    // a held part the holder isn't moving stops generating traffic.
    for (entity, mut hold) in &mut tagged {
        match held_now.remove(&entity) {
            Some(new) => {
                hold.set_if_neq(new);
            }
            None => {
                commands.entity(entity).remove::<NetHold>();
            }
        }
    }
    // Insert tags for parts newly held this tick (those still left in the map).
    // `try_insert`, not `insert`: a held part can't fall, but if one is ever despawned
    // the same frame (e.g. `replace_fallen_room_parts`) the deferred insert would hit a
    // missing entity — `try_insert` no-ops instead of erroring.
    for (entity, hold) in held_now {
        commands.entity(entity).try_insert(hold);
    }
}

/// Replace a room's parts that have fallen off its platform, keeping each room
/// stocked (the server's per-room equivalent of single-player's
/// `replace_fallen_parts`, which is suppressed here). The replacement re-joins
/// the same room and collision layer.
///
/// Also recycles parts whose simulation state has *diverged* (see the shared
/// `part_state_diverged` — non-finite or absurd state from an exploding
/// constraint solve, e.g. a jointed rocket assembly at extreme altitude). A NaN
/// position panics Avian's next broadphase and kills the server for every room,
/// so divergence is caught here, one tick ahead, and the part is recycled like a
/// fallen one.
fn replace_fallen_room_parts(
    mut commands: Commands,
    parts: Query<(Entity, &Position, &LinearVelocity, &AngularVelocity, &PartRoom, &NetPart)>,
) {
    for (entity, position, linear, angular, part_room, part) in &parts {
        let diverged = part_state_diverged(position.0, linear.0, angular.0);
        if diverged {
            println!(
                "[part] recycling diverged part (pos {:?}, v {:?}, w {:?})",
                position.0, linear.0, angular.0
            );
        }
        if position.0.y < PART_FALL_Y || diverged {
            commands.entity(entity).despawn();
            let room = Room { id: part_room.id, bit: part_room.bit };
            // Respawn the same kind that fell so the pool's composition is stable.
            match part.shape {
                PartShape::Cuboid { .. } => {
                    let (new_entity, half_extents, seed) = spawn_random_part(&mut commands);
                    tag_room_part(
                        &mut commands,
                        new_entity,
                        PartShape::Cuboid { half_extents: half_extents.to_array() },
                        seed,
                        room,
                    );
                }
                PartShape::RocketEngine => {
                    let new_entity = spawn_random_rocket(&mut commands);
                    tag_room_part(&mut commands, new_entity, PartShape::RocketEngine, 0, room);
                }
            }
        }
    }
}

/// Recompute each room's **largest assembly** — the biggest connected component of
/// parts joined together through joints — and publish it: mark the member parts with
/// a replicated [`InLargestAssembly`] and write the assembly's (mass-weighted) center
/// of mass into the room's orb [`NetCenterOfMass`], so every client can draw a
/// floating white orb there.
///
/// Runs every frame, which covers "whenever a joint is created or deleted" (the only
/// time membership can change) *and* keeps the orb tracking the assembly as it moves.
/// The per-part marker only re-replicates when membership actually flips (guarded by
/// `Has<InLargestAssembly>`), and the orb position only re-replicates when it changes
/// (`set_if_neq`), so a settled world generates no traffic.
///
/// Parts never joint to the ground (`server_attach` attaches only to other `NetPart`s)
/// and cross-room parts can't collide (collision layers), so the graph is purely
/// part-to-part within one room — "blocks connected through the ground" simply can't
/// arise here. A lone part is not an assembly, so only components of ≥ 2 parts count.
fn update_assembly_center_of_mass(
    mut commands: Commands,
    parts: Query<(Entity, &Position, &NetPart, &PartRoom, Has<InLargestAssembly>)>,
    joints: Query<&SphericalJoint>,
    mut orbs: Query<(&OrbRoom, &mut NetCenterOfMass)>,
) {
    // Index every part so joints can reference them by position. Each entry carries
    // the part's world position, its mass weight (density is uniform across parts, so
    // the cuboid volume is proportional to mass), and its room.
    let mut index: HashMap<Entity, usize> = HashMap::new();
    let mut items: Vec<(Vec3, f32, RoomId)> = Vec::new();
    for (entity, position, part, room, _) in &parts {
        index.insert(entity, items.len());
        items.push((position.0, part_volume(part.shape), room.id));
    }

    // Joint edges as index pairs. A joint referencing a despawned part (a dangling
    // joint — Avian tolerates these) simply contributes no edge.
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for joint in &joints {
        if let (Some(&a), Some(&b)) = (index.get(&joint.body1), index.get(&joint.body2)) {
            edges.push((a, b));
        }
    }

    let assemblies = largest_assembly_per_room(&items, &edges);

    // Every index that belongs to some room's winning assembly (for the marker).
    let mut member_indices: HashSet<usize> = HashSet::new();
    for assembly in assemblies.values() {
        member_indices.extend(assembly.members.iter().copied());
    }

    // Add/remove the membership marker only where it actually changed, so it
    // re-replicates on joint create/delete rather than every frame.
    for (entity, _, _, _, is_marked) in &parts {
        let is_member = index.get(&entity).is_some_and(|i| member_indices.contains(i));
        if is_member && !is_marked {
            commands.entity(entity).insert(InLargestAssembly);
        } else if !is_member && is_marked {
            commands.entity(entity).remove::<InLargestAssembly>();
        }
    }

    // Publish each room's COM into its orb. When a room has no assembly, keep the last
    // position (the orb is hidden on `count == 0` anyway) and just zero the count.
    for (orb_room, mut com) in &mut orbs {
        let next = match assemblies.get(&orb_room.0) {
            Some(a) => NetCenterOfMass { position: a.com.to_array(), count: a.members.len() as u32 },
            None => NetCenterOfMass { position: com.position, count: 0 },
        };
        com.set_if_neq(next);
    }
}

/// A part's volume — the mass proxy under uniform density (full cuboid volume, or the
/// rocket's precomputed cylinder+cone volume). Shared by the assembly COM and the launch
/// COM so both weigh parts identically.
fn part_volume(shape: PartShape) -> f32 {
    match shape {
        PartShape::Cuboid { half_extents } => {
            let he = Vec3::from_array(half_extents);
            8.0 * he.x * he.y * he.z
        }
        PartShape::RocketEngine => ROCKET_VOLUME,
    }
}

// ---- Rocket launch (server-authoritative) -----------------------------------
//
// A player touching its room's largest assembly can swipe to launch (`RequestLaunch`).
// The server runs that room's countdown, and at blastoff cuts the assembly's ground
// joints and fires its rockets with balanced thrust. The countdown + launched flag ride
// on the room's orb entity (`NetLaunch`), so every client draws the same banner and
// applies the same thrust to its predicted rockets (smooth liftoff, minimal rollback).

/// Per-room launch state, keyed by `RoomId`. A room is absent until launch is requested;
/// `Counting` runs the pre-blastoff countdown, then `Launched` fires the rockets for the
/// rest of the session.
#[derive(Clone, Copy)]
enum RoomLaunch {
    Counting { remaining: f32 },
    Launched,
}

#[derive(Resource, Default)]
struct LaunchRegistry {
    by_room: HashMap<RoomId, RoomLaunch>,
}

impl LaunchRegistry {
    /// Whether a room has blasted off — the state the rocket thrust keys on and
    /// the save snapshot persists.
    fn is_launched(&self, room: RoomId) -> bool {
        matches!(self.by_room.get(&room), Some(RoomLaunch::Launched))
    }
}

/// Start a room's countdown when one of its members swipes to launch. Maps the requesting
/// client link → its avatar (`ControlledBy`) → its `RoomMember`, and arms the countdown if
/// the room isn't already counting down or launched (re-requests are ignored — launch is a
/// one-way room event).
fn handle_launch_requests(
    mut links: Query<
        (Entity, &mut MessageReceiver<RequestLaunch>),
        (With<ClientOf>, With<Connected>),
    >,
    avatars: Query<(&ControlledBy, &RoomMember)>,
    mut registry: ResMut<LaunchRegistry>,
) {
    for (link, mut receiver) in &mut links {
        if receiver.receive().count() == 0 {
            continue;
        }
        for (controlled, member) in &avatars {
            if controlled.owner == link {
                registry
                    .by_room
                    .entry(member.0)
                    .or_insert(RoomLaunch::Counting { remaining: LAUNCH_COUNTDOWN_SECS });
            }
        }
    }
}

/// Advance each room's countdown; at blastoff flip it to `Launched` and cut every joint
/// pinning that room's assembly to the ground (a part↔ground joint has one endpoint that
/// isn't a `NetPart`). Part-to-part joints stay intact so the assembly holds together.
fn tick_room_launches(
    time: Res<Time>,
    mut commands: Commands,
    mut registry: ResMut<LaunchRegistry>,
    joints: Query<(Entity, &SphericalJoint, &RoomMember)>,
    net_parts: Query<(), With<NetPart>>,
) {
    let dt = time.delta_secs();
    for (&room, launch) in registry.by_room.iter_mut() {
        let RoomLaunch::Counting { remaining } = launch else {
            continue;
        };
        *remaining -= dt;
        if *remaining > 0.0 {
            continue;
        }
        *launch = RoomLaunch::Launched;
        for (entity, joint, member) in &joints {
            if member.0 == room
                && (net_parts.get(joint.body1).is_err() || net_parts.get(joint.body2).is_err())
            {
                commands.entity(entity).despawn();
            }
        }
    }
}

/// Mirror each room's launch state onto its orb `NetLaunch` so it replicates to every
/// client in the room (countdown banner + predicted thrust). Rooms with no launch entry
/// report the idle default. `set_if_neq` keeps a settled/idle room quiet.
fn publish_room_launch(registry: Res<LaunchRegistry>, mut orbs: Query<(&OrbRoom, &mut NetLaunch)>) {
    for (orb_room, mut launch) in &mut orbs {
        let next = match registry.by_room.get(&orb_room.0) {
            Some(RoomLaunch::Counting { remaining }) => {
                NetLaunch { remaining: remaining.max(0.0), launched: false }
            }
            Some(RoomLaunch::Launched) => NetLaunch { remaining: 0.0, launched: true },
            None => NetLaunch::default(),
        };
        launch.set_if_neq(next);
    }
}

/// Apply balanced rocket thrust to every launched room's assembly rockets each physics
/// tick. Reuses the replicated `InLargestAssembly` membership + `PartRoom` grouping the COM
/// system maintains: computes each launched room's mass-weighted COM + rotational state
/// from its members and the balanced per-rocket forces via the shared
/// `balanced_assembly_thrust` (whose PD stability assist needs the spin measurement).
fn apply_room_rocket_thrust(
    registry: Res<LaunchRegistry>,
    gravity: Res<Gravity>,
    rocket_geometry: Query<
        (Entity, &Position, &Rotation, &PartRoom),
        (With<InLargestAssembly>, With<RocketEngine>),
    >,
    // `Forces` takes `AngularVelocity` mutably inside, so the member read and the
    // force write cannot coexist as sibling queries (B0001) — sequence them.
    mut set: ParamSet<(
        Query<(&Position, &AngularVelocity, &ComputedMass, &PartRoom), With<InLargestAssembly>>,
        Query<(Entity, Forces), With<RocketEngine>>,
    )>,
) {
    // Group launched rooms' member rockets by room.
    let mut per_room: HashMap<RoomId, Vec<(Entity, Vec3, Quat)>> = HashMap::new();
    for (entity, position, rotation, room) in &rocket_geometry {
        if registry.is_launched(room.id) {
            per_room.entry(room.id).or_default().push((entity, position.0, rotation.0));
        }
    }
    if per_room.is_empty() {
        return;
    }

    // Balanced thrust per room → (force, point) per rocket entity. The COM +
    // rotational state come from the shared `measure_assembly_spin` (the same
    // measurement the client's predicted twin makes, from the same `ComputedMass`).
    let mut to_apply: HashMap<Entity, (Vec3, Vec3)> = HashMap::new();
    {
        let members = set.p0();
        for (room, rockets) in &per_room {
            let samples = || {
                members
                    .iter()
                    .filter(|(.., r)| r.id == *room)
                    .map(|(position, angular, mass, _)| (position.0, angular.0, mass.value()))
            };
            let Some((com, spin)) = measure_assembly_spin(samples) else {
                continue;
            };
            for thrust in balanced_assembly_thrust(com, gravity.0, rockets, &spin) {
                to_apply.insert(thrust.entity, (thrust.force, thrust.point));
            }
        }
    }
    for (entity, mut forces) in &mut set.p1() {
        if let Some((force, point)) = to_apply.get(&entity) {
            forces.apply_force_at_point(*force, *point);
        }
    }
}

// ---- Save games ---------------------------------------------------------------
//
// The snapshot half of the save-game feature (format + disk I/O in `crate::save`;
// the load half is `spawn_room_world_from_save`). Two writers: a rolling per-room
// autosave every `AUTOSAVE_SECS`, and the player's named manual save (`SaveGame`
// over the control channel), which the autosave never touches.

/// The pose + identity data a room snapshot reads off every part.
type SnapshotParts<'w, 's> = Query<
    'w,
    's,
    (
        &'static NetPart,
        &'static PartRoom,
        &'static Position,
        &'static Rotation,
        &'static LinearVelocity,
        &'static AngularVelocity,
    ),
>;

/// Every joint's replicated data + room tag (`RoomMember` also rides on avatars,
/// but only joints carry `NetJoint`).
type SnapshotJoints<'w, 's> = Query<'w, 's, (&'static NetJoint, &'static RoomMember)>;

/// Every room-assigned player's snapshot state (un-assigned avatars have no
/// `RoomMember` yet and are naturally excluded).
type SnapshotAvatars<'w, 's> = Query<
    'w,
    's,
    (
        &'static NetPlayer,
        &'static NetName,
        &'static RoomMember,
        &'static Position,
        &'static Yaw,
        &'static LinearVelocity,
        &'static HeldPart,
    ),
    With<ServerAvatar>,
>;

/// Snapshot one room's world into the save schema. Parts are ordered by their
/// stable `NetPart::id`, so an unchanged world snapshots identically (the
/// autosave's skip-if-unchanged hash relies on it). Joints and held-part
/// references name parts by index into that order; a dangling reference (an
/// endpoint despawned this frame) is dropped.
fn snapshot_room(
    room: RoomId,
    launched: bool,
    avatars: &SnapshotAvatars,
    parts: &SnapshotParts,
    joints: &SnapshotJoints,
) -> SaveWorld {
    let mut room_parts: Vec<_> =
        parts.iter().filter(|(_, part_room, ..)| part_room.id == room).collect();
    room_parts.sort_by_key(|(part, ..)| part.id);
    let index: HashMap<u64, u32> = room_parts
        .iter()
        .enumerate()
        .map(|(i, (part, ..))| (part.id, i as u32))
        .collect();

    let save_parts = room_parts
        .iter()
        .map(|(part, _, position, rotation, linear, angular)| SavePart {
            shape: match part.shape {
                PartShape::Cuboid { half_extents } => SaveShape::Cuboid { half_extents },
                PartShape::RocketEngine => SaveShape::RocketEngine,
            },
            seed: part.seed,
            position: position.0.to_array(),
            rotation: rotation.0.to_array(),
            linear_velocity: linear.0.to_array(),
            angular_velocity: angular.0.to_array(),
        })
        .collect();

    let body = |id: u64| {
        if id == GROUND_JOINT_ID {
            Some(SaveBody::Ground)
        } else {
            index.get(&id).copied().map(SaveBody::Part)
        }
    };
    let save_joints = joints
        .iter()
        .filter(|(_, member)| member.0 == room)
        .filter_map(|(joint, _)| {
            Some(SaveJoint {
                body1: body(joint.body1)?,
                body2: body(joint.body2)?,
                anchor1: joint.anchor1,
                anchor2: joint.anchor2,
            })
        })
        .collect();

    // The players, as analysis context (ignored on load — see `SaveAvatar`).
    let save_avatars = avatars
        .iter()
        .filter(|(_, _, member, ..)| member.0 == room)
        .map(|(player, name, _, position, yaw, linear, held)| SaveAvatar {
            client_id: player.client_id,
            name: name.0.clone(),
            position: position.0.to_array(),
            yaw: yaw.0,
            linear_velocity: linear.0.to_array(),
            held_part: held.0.and_then(|e| index.get(&e.to_bits()).copied()),
        })
        .collect();

    SaveWorld { parts: save_parts, joints: save_joints, avatars: save_avatars, launched }
}

/// A snapshot's identity for the autosave's skip-if-unchanged check. Settled
/// bodies stop moving exactly (Avian sleeps them), so an untouched room hashes
/// stably and generates no disk traffic. Serializes straight into the hasher —
/// no intermediate String for the (common) unchanged-room case.
fn world_hash(world: &SaveWorld) -> u64 {
    use std::hash::Hasher;
    struct HashWriter(std::collections::hash_map::DefaultHasher);
    impl std::io::Write for HashWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Default::default());
    let _ = serde_json::to_writer(&mut writer, world);
    writer.0.finish()
}

/// Every `AUTOSAVE_SECS`, atomically replace each **occupied** room's rolling
/// autosave (`auto-<CODE>.json`). Unoccupied rooms freeze — their last autosave
/// (at most one interval stale) stands until someone rejoins. A room whose world
/// hasn't changed since the last write is skipped entirely.
fn autosave_rooms(
    time: Res<Time>,
    mut timer: Local<f32>,
    mut last_hash: Local<HashMap<RoomId, u64>>,
    registry: Res<RoomRegistry>,
    launches: Res<LaunchRegistry>,
    avatars: SnapshotAvatars,
    parts: SnapshotParts,
    joints: SnapshotJoints,
) {
    *timer += time.delta_secs();
    if *timer < AUTOSAVE_SECS {
        return;
    }
    *timer = 0.0;
    let occupied: HashSet<RoomId> = avatars.iter().map(|(_, _, member, ..)| member.0).collect();
    for (&code, room) in &registry.by_code {
        if !occupied.contains(&room.id) {
            continue;
        }
        let world = snapshot_room(room.id, launches.is_launched(room.id), &avatars, &parts, &joints);
        let hash = world_hash(&world);
        if last_hash.get(&room.id) == Some(&hash) {
            continue;
        }
        let code = save::code_string(code);
        let file = SaveFile::new(code.clone(), code.clone(), "auto", world);
        match save::write_save(&save::auto_file_name(&code), &file) {
            Ok(()) => {
                last_hash.insert(room.id, hash);
            }
            Err(e) => println!("[save] autosave {code} failed: {e}"),
        }
    }
}

/// Write a player-named manual save of the sender's room (`manual-<CODE>-<slug>.json`
/// — a separate file per name, never touched by the autosave). Maps the client link
/// to its avatar's room like every other control-channel handler; the name gets the
/// same `sanitize_name` rules as a rename (blank ⇒ ignored).
fn apply_manual_saves(
    mut links: Query<(Entity, &mut MessageReceiver<SaveGame>), (With<ClientOf>, With<Connected>)>,
    avatars: Query<(&ControlledBy, &RoomMember)>,
    registry: Res<RoomRegistry>,
    launches: Res<LaunchRegistry>,
    snapshot_avatars: SnapshotAvatars,
    parts: SnapshotParts,
    joints: SnapshotJoints,
) {
    for (link, mut receiver) in &mut links {
        // Sequenced-reliable: only the newest request this window matters.
        let Some(msg) = receiver.receive().last() else {
            continue;
        };
        let name = sanitize_name(&msg.0);
        if name.is_empty() {
            continue;
        }
        let Some(room_id) = avatars
            .iter()
            .find(|(controlled, _)| controlled.owner == link)
            .map(|(_, member)| member.0)
        else {
            continue;
        };
        // Reverse-map the room id to its lobby code (the registry is keyed by code;
        // rooms are few, so a scan is fine).
        let Some(&code) = registry.by_code.iter().find(|(_, r)| r.id == room_id).map(|(c, _)| c)
        else {
            continue;
        };
        let world =
            snapshot_room(room_id, launches.is_launched(room_id), &snapshot_avatars, &parts, &joints);
        let code = save::code_string(code);
        let file = SaveFile::new(name.clone(), code.clone(), "manual", world);
        match save::write_save(&save::manual_file_name(&code, &name), &file) {
            Ok(()) => println!("[save] manual save '{name}' written for room {code}"),
            Err(e) => println!("[save] manual save '{name}' for room {code} failed: {e}"),
        }
    }
}

/// Live flight-recording writers, one per recorded room (see
/// `record_room_frames`). `None` = the file failed to open (e.g. unwritable
/// saves dir); the room is skipped without retrying every tick. Buffered so the
/// 60 Hz sim thread batches its writes instead of one syscall per line. Only
/// inserted when `BS_RECORD` is set.
#[derive(Resource, Default)]
struct RecordingRegistry {
    by_room: HashMap<RoomId, Option<std::io::BufWriter<std::fs::File>>>,
}

/// One recorded tick, serialized straight to the room's file in a single pass
/// (no intermediate `serde_json::Value` tree — this runs per tick per room).
#[derive(serde::Serialize)]
struct RecordedFrame<'a> {
    tick: u64,
    unix_ms: u64,
    inputs: Vec<RecordedInput<'a>>,
    world: &'a SaveWorld,
}

/// One player's raw input for a recorded tick.
#[derive(serde::Serialize)]
struct RecordedInput<'a> {
    client_id: u64,
    input: &'a NetInput,
}

/// Flight recorder (opt-in via `BS_RECORD`): every simulated tick, append one
/// JSON line per **occupied** room to its recording file —
///
/// ```json
/// {"tick":N,"unix_ms":T,"inputs":[{"client_id":..,"input":{..}}],"world":{..}}
/// ```
///
/// where `world` is the same versioned [`SaveWorld`] snapshot a save uses (taken
/// **after** the physics step — the tick's outcome) and `inputs` is each player's
/// raw `NetInput` for the tick. This is the machine-analysis complement to saves:
/// reproduce a bug while recording, then step/bisect/diff the JSONL tick-by-tick
/// to see exactly which frame — and which input — went wrong. A room's file opens
/// on its first recorded tick and is never rotated (recordings are debug
/// artifacts, enabled deliberately, not a production default).
fn record_room_frames(
    mut tick: Local<u64>,
    mut recordings: ResMut<RecordingRegistry>,
    registry: Res<RoomRegistry>,
    launches: Res<LaunchRegistry>,
    avatars: SnapshotAvatars,
    inputs: Query<(&NetPlayer, &RoomMember, &ActionState<NetInput>)>,
    parts: SnapshotParts,
    joints: SnapshotJoints,
) {
    use std::io::Write;
    *tick += 1;
    let occupied: HashSet<RoomId> = avatars.iter().map(|(_, _, member, ..)| member.0).collect();
    for (&code, room) in &registry.by_code {
        if !occupied.contains(&room.id) {
            continue;
        }
        // Resolve the room's writer BEFORE snapshotting, so a room whose file
        // couldn't open doesn't cost a discarded snapshot every tick.
        let writer = recordings.by_room.entry(room.id).or_insert_with(|| {
            save::open_recording(&save::code_string(code))
                .map(std::io::BufWriter::new)
                .map_err(|e| println!("[save] recording for room {:?} disabled: {e}", room.id))
                .ok()
        });
        let Some(file) = writer.as_mut() else {
            continue;
        };

        let world = snapshot_room(room.id, launches.is_launched(room.id), &avatars, &parts, &joints);
        let frame = RecordedFrame {
            tick: *tick,
            unix_ms: save::now_unix_ms(),
            inputs: inputs
                .iter()
                .filter(|(_, member, _)| member.0 == room.id)
                .map(|(player, _, state)| RecordedInput {
                    client_id: player.client_id,
                    input: &state.0,
                })
                .collect(),
            world: &world,
        };
        let write = serde_json::to_writer(&mut *file, &frame)
            .map_err(std::io::Error::from)
            .and_then(|()| file.write_all(b"\n"));
        if let Err(e) = write {
            println!("[save] recording write for room {:?} disabled: {e}", room.id);
            recordings.by_room.insert(room.id, None);
        }
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
        .spawn((
            // Match the client's version gate: only accept connect tokens whose
            // protocol id equals this build's BS_PROTOCOL_ID (same git commit).
            NetcodeServer::new(
                NetcodeConfig::default()
                    .with_protocol_id(bad_spaceship_shared::net::BS_PROTOCOL_ID),
            ),
            LocalAddr(addr),
            io,
        ))
        .id();
    commands.trigger(Start { entity: server });
    info!("multiplayer server listening on ws://{addr}");
}

/// When a client finishes connecting (`Connected` added to its link entity),
/// spawn a player entity owned by the server and replicated to it. The initial
/// pose is a placeholder — the client's first `NetInput` (its real character
/// pose) overwrites it within a tick. The avatar starts with no `Rooms` filter so
/// it's visible to its own client (bootstrapping the input/control loop);
/// `assign_rooms` scopes it once the first input reveals the room.
fn spawn_player_for_client(
    trigger: On<Add, Connected>,
    mut commands: Commands,
    remote: Query<&RemoteId>,
    tokens: Query<&TokenUserData>,
    mut resume: ResMut<ResumeRegistry>,
) {
    let client = trigger.entity;
    // The owning client's peer id: it predicts its own avatar; everyone else
    // interpolates it. (Predicting a remote player is impossible without its input.)
    let owner = remote.get(client).map(|r| r.0).unwrap_or(PeerId::Server);
    let client_id = client_identity(client, &remote);
    // The persistent resume id rides in the connect token's `user_data` (see the client's
    // `build_netcode_client`). Resolve the remembered position NOW, at connect — before
    // the avatar's body assembles and its first `Position` replicates — so a reconnecting
    // avatar is built directly at its saved spot (`InitialPose`), with no origin→saved
    // ease. Consume the record; `record_resume_positions` re-tracks the live avatar after.
    let rid = tokens
        .get(client)
        .ok()
        .map(|t| bad_spaceship_shared::net::resume_id_from_user_data(&t.0))
        .unwrap_or(0);
    let resume_pos = (rid != 0)
        .then(|| {
            resume.by_id.remove(&rid).and_then(|(pos, room, at)| {
                at.elapsed()
                    .map(|e| e.as_secs() < RESUME_GRACE_SECS)
                    .unwrap_or(false)
                    .then_some((pos, room))
            })
        })
        .flatten();
    // The monster is keyed off the *persistent* resume id so a reload keeps it;
    // clients without one (native) fall back to the per-session client id.
    let monster = monster_index(if rid != 0 { rid } else { client_id });
    let mut avatar = commands.spawn((
        NetPlayer { client_id, monster },
        // Replicate the avatar; its pose rides on Avian `Position`/`Rotation`
        // (`build_server_avatar` gives it a real body next frame, and the server
        // simulates it from the client's input intent).
        Replicate::to_clients(NetworkTarget::All),
        // Predict on the owner (zero input delay, rolled back), interpolate on others.
        PredictionTarget::to_clients(NetworkTarget::Single(owner)),
        InterpolationTarget::to_clients(NetworkTarget::AllExceptSingle(owner)),
        // Bind this player to the connecting client so that client's networked
        // input drives it. The server auto-adds the `InputBuffer`/`ActionState`
        // when input arrives; seeding `ActionState` here lets `apply_net_input`
        // match the entity immediately. `SessionBased` despawns it on disconnect.
        ControlledBy { owner: client, lifetime: Lifetime::SessionBased },
        ActionState::<NetInput>::default(),
        // Make this avatar a server-simulated character body (assembled once the
        // config loads).
        ServerAvatar,
        HeldPart::default(),
        AttachState::default(),
        DeleteState::default(),
        // Telemetry: server-measured late/lost-input tally for this client (the
        // `InputBuffer` lightyear adds on first input lands on this same entity).
        LateInputStats::default(),
        // Replicated facing (mirrored from the avatar's `Yaw` by
        // `sync_avatar_facing`) so remote clients can draw it facing its look.
        NetFacing::default(),
        // Display name — replicated from spawn (empty until `assign_rooms` picks a
        // unique per-room default), so the client never queries a nameless avatar.
        NetName::default(),
    ));
    if let Some((pos, room)) = resume_pos {
        // Optimistically build at the remembered spot (the common case — an iOS
        // reload rejoining the same room — must not slide in from the origin).
        // `assign_rooms` revokes it if the first input reveals a DIFFERENT room:
        // a remembered position means nothing in another room's world.
        avatar.insert((InitialPose(pos), ResumeRoom(room)));
        println!("[resume] client_id={client_id} reconnect -> spawn at {pos:?}");
    } else {
        println!("[resume] client_id={client_id} fresh connect (no remembered position)");
    }
    info!("client {client:?} connected — spawned replicated avatar");
}

/// The stable u64 a client chose in its netcode `Authentication`, read from the
/// link's `RemoteId`. We stamp it onto `NetPlayer.client_id` so a client can
/// recognise its *own* avatar by an id it knows for certain — rather than
/// lightyear's replicated `Controlled` marker, which leaks to an already-connected
/// client when a late joiner's avatar arrives (and would mis-tag that peer as
/// "ours", stacking it on our own avatar — the "first player can't see the second"
/// bug). Falls back to the link entity bits if the peer id isn't a netcode/steam/
/// local id (e.g. host-server).
fn client_identity(link: Entity, remote: &Query<&RemoteId>) -> u64 {
    match remote.get(link).map(|r| r.0) {
        Ok(PeerId::Netcode(id))
        | Ok(PeerId::Steam(id))
        | Ok(PeerId::Local(id))
        | Ok(PeerId::Entity(id)) => id,
        _ => link.to_bits(),
    }
}
