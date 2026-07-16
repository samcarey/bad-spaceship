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
use std::time::{Duration, SystemTime};

use avian3d::prelude::{
    AngularVelocity, Collider, CollisionLayers, ComputedMass, Forces, Gravity, LinearVelocity,
    Position, Rotation, SphericalJoint, WriteRigidBodyForces,
};
use bad_spaceship_shared::assembly::largest_assembly_per_room;
use bad_spaceship_shared::guidance::{program_guidance, PitchProgram, DEFAULT_PITCHOVER};
use bad_spaceship_shared::launch::{
    assembly_burn, burn_impulse, measure_assembly_spin, LAUNCH_COUNTDOWN_SECS,
};
use bad_spaceship_shared::character::{
    apparent_up, drive_felt_up, spawn_position, CharacterMovement, FeltUp, InitialPose,
    ServerAvatar,
};
use bad_spaceship_shared::net::{
    apply_hold_spring, apply_net_input, focused_part, monster_index, sanitize_name,
    ClientPanicReport, InLargestAssembly, NetFacing, NetHold, NetInput, NetJoint, NetLockJoint,
    NetMoving,
    NetLaunch, NetName, NetPart, NetPlayer, NetRoomFrame, PartShape, ProtocolPlugin, RequestLaunch,
    ResetPosition, ResetRoom, RollbackReport, SaveGame, SetAvatar, SetLocked, SetName,
    GROUND_JOINT_ID, MONSTER_COUNT, TICK,
};
use bad_spaceship_shared::map::{
    apply_assembly_drag, apply_gravity_correction, radial_altitude, GROUND_LAYER, PLANET_CENTER,
    PLANET_RADIUS, PLANET_RESPAWN_Y,
};
use bad_spaceship_shared::part::{
    avatar_lock_contacts, capsule_bottom_center, despawn_player_lock_welds, part_gap_contacts,
    part_state_diverged, spawn_random_part, spawn_random_rocket, spawn_rocket_engine,
    spawn_saved_cuboid, Gimbal, LockJoint, RocketEngine, SuppressLocalParts, DELETE_RADIUS,
    NUM_PARTS, NUM_ROCKET_ENGINES, PART_FALL_Y, ROCKET_VOLUME,
};
use bad_spaceship_shared::{DirectionalInput, Grass, SuppressLocalPlayer, Yaw};
use bevy::math::DVec3;
use bevy::prelude::*;

use crate::save::{
    self, SaveAvatar, SaveBody, SaveFile, SaveFrame, SaveJoint, SavePart, SaveShape, SaveWorld,
    AUTOSAVE_SECS,
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

/// Consecutive ticks a client's exact input has been missing (server running on the
/// reused last input). Reset to 0 the moment a fresh input lands. Feeds
/// [`stop_stale_avatars`].
#[derive(Component, Default)]
struct StaleInputRun(u32);

/// After this many consecutive stale ticks, stop reusing the client's last movement
/// intent and zero it instead. ~165 ms at 60 Hz: long enough that ordinary jitter (a
/// few late ticks, where reusing a held direction is correct) never trips it, short
/// enough that a genuine connection stall *parks* the avatar instead of walking it off
/// the level on the last-seen "move forward". Momentary reuse of a stale direction is
/// invisible (you resume); reuse of a stale "keep walking" past a real stall is the
/// runaway a high-jitter phone hit.
const STALE_INPUT_STOP_TICKS: u32 = 10;

/// Zero a client avatar's movement/jump once its input has been stale too long
/// (`StaleInputRun` past [`STALE_INPUT_STOP_TICKS`]). Runs after [`apply_net_input`]
/// (which wrote `DirectionalInput` from the possibly-reused `ActionState`) and before
/// `CharacterMovement` reads it, on the server only — the owning client always has its
/// own input for the current tick locally, so its predicted avatar is never stale and
/// this can't perturb client prediction. Yaw (facing) is left untouched; only the
/// translational intent can run you off the map.
fn stop_stale_avatars(
    timeline: Res<LocalTimeline>,
    mut avatars: Query<(
        &InputBuffer<ActionState<NetInput>, NetInput>,
        &mut StaleInputRun,
        &mut DirectionalInput,
    )>,
) {
    let tick = timeline.tick();
    for (buffer, mut run, mut dir) in &mut avatars {
        // Stale = the buffer has a fallback (client is live) but not the exact tick.
        let stale = buffer.get_predict(tick).is_some() && buffer.get(tick).is_none();
        run.0 = if stale { run.0 + 1 } else { 0 };
        if run.0 > STALE_INPUT_STOP_TICKS {
            dir.0 = Vec3::ZERO;
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
        app.init_resource::<RoomAttitudeIntegrals>();
        app.init_resource::<RoomFuel>();
        app.init_resource::<RoomPolicy>();
        app.init_resource::<RoomApparentUp>();
        app.init_resource::<RoomEscaped>();
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
            (sync_avatar_facing, sync_avatar_moving, sync_net_hold, mark_largest_assembly),
        );
        // Part recycling runs in `FixedUpdate`, NOT `Update`: it also catches parts
        // whose state went non-finite / absurd (a diverging constraint solve), and
        // those MUST be despawned before the *next* Avian step — its broadphase
        // asserts on a NaN AABB and panics the whole server. `FixedUpdate` precedes
        // the physics step (`FixedPostUpdate`) on every tick, while an `Update`
        // system can be skipped between two back-to-back fixed steps in a lagging
        // frame — exactly when an explosion is underway.
        //
        // The floating-origin rebase leads the chain: the fall checks it precedes
        // read frame-consistent positions, and (see `rebase_room_frames`) it must
        // run before anything that computes world-space force targets this tick.
        app.init_resource::<RoomFrames>();
        app.add_systems(
            FixedUpdate,
            (
                rebase_room_frames,
                replace_fallen_room_parts,
                respawn_fallen_avatars,
                // Restocking within a second of a shortfall is plenty (see the fn doc);
                // scanning every joint each tick is not.
                ensure_spare_rocket
                    .run_if(bevy::time::common_conditions::on_timer(Duration::from_secs(1))),
            )
                .chain()
                .before(server_grab)
                .before(apply_room_rocket_thrust),
        );
        // Assign each client (and its avatar) to its reported room on the first
        // input, lazily creating the room's world, and apply client rename +
        // avatar-pick + reset-position + reset-room requests.
        app.init_resource::<InitialWorlds>();
        // Reset a room when its assembly crashes into the planet: the detector
        // (FixedUpdate, reads post-physics positions) flags the room, and
        // `apply_room_resets` drains the flag alongside client reset requests.
        app.init_resource::<PendingRoomResets>();
        app.add_systems(FixedUpdate, detect_assembly_crash);
        // Planet gravity: a per-tick radial correction on every dynamic body so gravity
        // points at the planet centre and weakens with altitude (see `gravity_at`). After
        // the fall/rebase chain (so it reads post-shift positions + each room's current
        // offset) and before the `Forces` consumers it shares the rockets with
        // (`server_hold` in the grab chain, `apply_room_rocket_thrust`).
        app.add_systems(
            FixedUpdate,
            apply_server_gravity
                .after(respawn_fallen_avatars)
                .before(server_grab)
                .before(apply_room_rocket_thrust),
        );
        // Re-attach locked riders that reconnected (`relock_resumed_riders`) or drifted
        // clear of the assembly (`keep_riders_aboard`). After the fall/respawn chain (so
        // the body has settled near its deck) and before the force writers, since both
        // teleport avatars — a pose change must land before this tick's physics step.
        app.add_systems(
            FixedUpdate,
            (relock_resumed_riders, keep_riders_aboard)
                .chain()
                .after(respawn_fallen_avatars)
                .before(apply_server_gravity),
        );
        app.add_systems(
            Update,
            (
                assign_rooms,
                apply_name_changes,
                apply_avatar_changes,
                apply_position_resets,
                apply_room_resets,
                // Demo hook (`BS_SPAWN_ON_DECK`): drop a fresh joiner straight onto
                // its room's assembly deck instead of the pad, so a "rocket built +
                // character onboard" save needs no walking. No-op unless the env var
                // is set, so real rooms are unaffected.
                spawn_demo_players_on_deck,
                // Lock/Unlock rider welds, plus the shared sweep that drops welds
                // whose avatar (disconnect) or part (recycle/reset) is gone.
                apply_lock_changes,
                bad_spaceship_shared::part::cleanup_lock_joints,
            ),
        );
        // Remember each fresh room's initial world for `ResetRoom`. `PostUpdate`:
        // the parts `assign_rooms` spawned are queryable (its commands flushed at
        // the end of `Update`) and still at their exact spawn poses (this frame's
        // physics already ran) — with no mid-`Update` sync point.
        app.add_systems(PostUpdate, capture_initial_worlds);
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
                // Safety net: park an avatar whose input has stalled instead of
                // reusing its last "keep walking" indefinitely (runaway on a lossy link).
                stop_stale_avatars
                    .after(apply_net_input)
                    .before(CharacterMovement),
                (server_grab, server_hold, server_attach, server_delete).chain(),
                // Balanced rocket thrust for launched rooms — a continuous force, so it
                // runs per physics tick like the hold spring.
                apply_room_rocket_thrust,
                // Feed each rider's felt-up window (camera + movement basis) from the
                // assembly attitude the burn just flew — the server twin of the client's
                // `sample_sp/mp_felt_up`, over the same replicated state.
                sample_felt_up.after(apply_room_rocket_thrust),
                // Structural damping across welded pairs — drains contact/joint pump
                // energy before it can run away (see `damp_weld_motion`).
                bad_spaceship_shared::part::damp_weld_motion.after(apply_room_rocket_thrust),
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
    /// resume id → (last position, room code it was recorded in, lock anchor if the
    /// rider was locked, when). The lock anchor lets a reconnecting locked rider re-weld
    /// to its deck point instead of returning free on a moving assembly (see
    /// `relock_resumed_riders`).
    by_id: HashMap<u64, (Vec3, [u8; 6], Option<LockAnchor>, SystemTime)>,
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

impl PartRoom {
    /// The [`Room`] descriptor this part's room tags fresh spawns with.
    fn room(&self) -> Room {
        Room { id: self.id, bit: self.bit }
    }
}

/// Tags the per-room state entity with the room it reports for, so the systems that
/// author that room's replicated state onto it — its floating-origin [`NetRoomFrame`]
/// (`rebase_room_frames`) and its launch/countdown [`NetLaunch`] (`publish_room_launch`)
/// — can find the right one.
#[derive(Component, Clone, Copy)]
struct RoomStateOf(RoomId);

// ---- Floating-origin rebase (per room) ----------------------------------------
//
// f32 starves the solver (and the client renderer) at extreme altitude — the old
// hard ceilings were ~33 km with a rider / ~524 km unmanned. So each room carries a
// floating-origin frame: when its assembly drifts `REBASE_TRIGGER_M` from the local
// origin, `rebase_room_frames` subtracts the assembly's position AND mean velocity
// from every entity in the room (a Galilean boost — physics is invariant), and
// accumulates both in the frame (`offset` integrates at `velocity` per tick, in
// f64). Ascent is then unbounded: the sim always runs near the origin at small
// local velocities. The frame replicates on the room orb (`NetRoomFrame`) so
// clients can move their ground to `-offset` and derive true altitude/speed.
//
// While a frame is active the true ground is far away, but the shared ground
// *collider* still sits at the local origin (it serves every room at once and
// cannot move per-room) — so active rooms drop `GROUND_LAYER` from their collision
// filters, and the rebase parks content `REBASE_REST_Y` above the origin so nothing
// overlaps the phantom bowl even transiently. When the room descends below
// `REBASE_RESET_M` true, the frame snaps back to exactly zero and the ground bit is
// restored — landings happen on the real ground, and the loose parts left behind at
// the pad (which fell out of the world while the frame flew) get recycled by the
// normal fall check the moment coordinates are true again.

/// Local drift of the room's assembly that triggers a rebase.
const REBASE_TRIGGER_M: f32 = 2000.0;

/// True distance from the origin below which an active frame resets to exactly
/// zero (real coordinates, real ground). Half of `REBASE_TRIGGER_M`, so the two
/// can't flap: right after a reset the local drift is under the trigger.
const REBASE_RESET_M: f64 = 1000.0;

/// Where a rebase parks the assembly above the local origin. Keeps the freshly
/// shifted content clear of the phantom ground collider (and of the client's
/// not-yet-moved local ground during the rollback that applies the shift).
const REBASE_REST_Y: f32 = 100.0;

/// A room's authoritative floating-origin frame: local + frame = true. `offset`
/// is f64 — it grows without bound and integrates every tick, and f32 would
/// accumulate error in exactly the quantity this feature exists to keep exact.
#[derive(Clone, Copy, Default)]
struct RoomFrame {
    offset: DVec3,
    velocity: Vec3,
}

impl RoomFrame {
    fn is_active(&self) -> bool {
        self.offset != DVec3::ZERO || self.velocity != Vec3::ZERO
    }
    fn net(&self) -> NetRoomFrame {
        NetRoomFrame { offset: self.offset.to_array(), velocity: self.velocity.to_array() }
    }
    fn save(&self) -> SaveFrame {
        SaveFrame { offset: self.offset.to_array(), velocity: self.velocity.to_array() }
    }
}

/// Every room's floating-origin frame, keyed by `RoomId`. Rooms absent from the
/// map are grounded (zero frame).
#[derive(Resource, Default)]
struct RoomFrames {
    by_room: HashMap<RoomId, RoomFrame>,
}

impl RoomFrames {
    fn get(&self, room: RoomId) -> RoomFrame {
        self.by_room.get(&room).copied().unwrap_or_default()
    }
}

/// A room entity's collision layers for its frame state: membership on the room's
/// bit; filters the room's bit plus — only while grounded — the ground's bit 0
/// (rebased rooms must not collide with the ground collider left at the local
/// origin: it serves every room at once and cannot move per-room, so while the
/// frame is active it's a phantom — the true ground is at `-offset`). The single
/// construction point for parts (`tag_room_part`), avatars (`assign_rooms`), and
/// the rebase transitions. `CollisionLayers` is immutable (avian), so transitions
/// re-insert it.
fn room_layers(bit: u32, grounded: bool) -> CollisionLayers {
    CollisionLayers::from_bits(bit, bit | if grounded { GROUND_LAYER } else { 0 })
}

/// Integrate each room's frame and rebase rooms whose assembly drifted from the
/// local origin (see the module-section comment above). Runs in `FixedUpdate`
/// *before* every system that computes world-space force targets for the same tick
/// (`server_grab`/`server_hold` chain, `apply_room_rocket_thrust`) — a thrust
/// application point computed in the pre-shift frame but applied post-shift would
/// be a km-scale lever arm. Movement/contact systems are frame-invariant and need
/// no ordering.
fn rebase_room_frames(
    time: Res<Time>,
    mut commands: Commands,
    mut frames: ResMut<RoomFrames>,
    // `Has<LockJoint>`: a lock weld's `body1` is an avatar, not a part, so it looks
    // like a "stray ground joint" to the index test below — but the avatar rides the
    // frame (it's shifted with everything else), so the weld is fine and must NOT be
    // cut. Without this exemption every locked rider is unlocked at the first rebase
    // (~2 km).
    joints: Query<(Entity, &SphericalJoint, Option<&RoomMember>, Has<LockJoint>)>,
    mut orbs: Query<(&RoomStateOf, &mut NetRoomFrame)>,
    mut parts: Query<(Entity, &NetPart, &PartRoom, &mut Position, &mut LinearVelocity)>,
    mut avatars: Query<
        (Entity, &RoomMember, &mut Position, &mut LinearVelocity),
        (With<ServerAvatar>, Without<NetPart>),
    >,
) {
    // 1. Integrate every moving frame's origin along its velocity (exact, in f64).
    let dt = time.delta_secs_f64();
    for frame in frames.by_room.values_mut() {
        if frame.velocity != Vec3::ZERO {
            frame.offset += frame.velocity.as_dvec3() * dt;
        }
    }

    // Fast path for the common case — no frame active and nothing anywhere near
    // the rebase band: skip the per-room anchor work (index + union-find) entirely.
    // Publishing still runs (a fresh orb needs its zero frame written once).
    const TRIGGER_SQ: f32 = REBASE_TRIGGER_M * REBASE_TRIGGER_M;
    if frames.by_room.values().all(|f| !f.is_active())
        && parts.iter().all(|(.., position, _)| position.0.length_squared() < TRIGGER_SQ)
    {
        for (_, mut net_frame) in &mut orbs {
            net_frame.set_if_neq(NetRoomFrame::default());
        }
        return;
    }

    // 2. Anchor each room on its largest assembly — the thing whose solver
    //    precision matters (and whose deck the riders stand on).
    let mut index: HashMap<Entity, usize> = HashMap::new();
    let mut items: Vec<(Vec3, f32, RoomId)> = Vec::new();
    let mut velocities: Vec<Vec3> = Vec::new();
    for (entity, part, room, position, linear) in &parts {
        index.insert(entity, items.len());
        items.push((position.0, part_volume(part.shape), room.id));
        velocities.push(linear.0);
    }
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for (_, joint, _, _) in &joints {
        if let (Some(&a), Some(&b)) = (index.get(&joint.body1), index.get(&joint.body2)) {
            edges.push((a, b));
        }
    }
    let assemblies = largest_assembly_per_room(&items, &edges);
    // Mass-weighted position + velocity of a room's anchor: its largest assembly,
    // or (no assembly — e.g. the ride broke up entirely) all of its parts, so the
    // frame keeps tracking whatever is left.
    let anchor = |room: RoomId| -> Option<(Vec3, Vec3)> {
        let com = |member_indices: &mut dyn Iterator<Item = usize>| {
            let (mut weighted_pos, mut weighted_vel, mut mass) = (Vec3::ZERO, Vec3::ZERO, 0.0);
            for i in member_indices {
                let (position, weight, _) = items[i];
                weighted_pos += position * weight;
                weighted_vel += velocities[i] * weight;
                mass += weight;
            }
            (mass > 0.0).then(|| (weighted_pos / mass, weighted_vel / mass))
        };
        match assemblies.get(&room) {
            Some(a) => com(&mut a.members.iter().copied()),
            None => com(&mut (0..items.len()).filter(|&i| items[i].2 == room)),
        }
    };

    // 3. Per room: rebase on drift, reset near the true origin, publish the frame.
    for (orb_room, mut net_frame) in &mut orbs {
        let room = orb_room.0;
        let frame = frames.by_room.entry(room).or_default();
        if let Some((anchor_pos, anchor_vel)) = anchor(room) {
            let true_anchor = frame.offset + anchor_pos.as_dvec3();
            // (shift, boost) to subtract from every room entity's position/velocity.
            let shift = if frame.is_active() && true_anchor.length() < REBASE_RESET_M {
                // Back near the true origin: land the frame exactly on zero so the
                // ground is real again (assign directly — accumulating the inverse
                // shift in f32 would leave a residue).
                let shift = (-frame.offset.as_vec3(), -frame.velocity);
                (frame.offset, frame.velocity) = (DVec3::ZERO, Vec3::ZERO);
                Some(shift)
            } else if anchor_pos.length() > REBASE_TRIGGER_M {
                let dpos = anchor_pos - Vec3::Y * REBASE_REST_Y;
                frame.offset += dpos.as_dvec3();
                frame.velocity += anchor_vel;
                Some((dpos, anchor_vel))
            } else {
                None
            };
            if let Some((dpos, dvel)) = shift {
                let grounded = !frame.is_active();
                // A rebased room must have no part↔ground joints: the shared
                // ground body does not ride the frame, so such a joint would pin
                // the assembly to an anchor the shift just moved km away — a
                // constraint violation violent enough to explode the assembly.
                // Real flights cut them at blastoff; cut any stragglers here.
                if !grounded {
                    for (joint_entity, joint, member, is_lock) in &joints {
                        // Lock welds have a non-part (avatar) endpoint but legitimately
                        // ride the frame — skip them, or the rebase unlocks every rider.
                        if !is_lock
                            && member.is_some_and(|m| m.0 == room)
                            && (!index.contains_key(&joint.body1)
                                || !index.contains_key(&joint.body2))
                        {
                            println!("[rebase] room {room:?}: cutting ground joint");
                            commands.entity(joint_entity).despawn();
                        }
                    }
                }
                // Avatars don't carry the room's collision bit; pick it up from the
                // room's parts (a room always has parts).
                let mut bit = None;
                for (entity, _, part_room, mut position, mut linear) in &mut parts {
                    if part_room.id == room {
                        position.0 -= dpos;
                        linear.0 -= dvel;
                        bit = Some(part_room.bit);
                        commands.entity(entity).insert(room_layers(part_room.bit, grounded));
                    }
                }
                for (entity, member, mut position, mut linear) in &mut avatars {
                    if member.0 == room {
                        position.0 -= dpos;
                        linear.0 -= dvel;
                        if let Some(bit) = bit {
                            commands.entity(entity).insert(room_layers(bit, grounded));
                        }
                    }
                }
                println!(
                    "[rebase] room {room:?}: shift {dpos:?} boost {dvel:?} -> offset {:?} vel {:?}",
                    frame.offset, frame.velocity
                );
            }
        }
        net_frame.set_if_neq(frame.net());
    }
}

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
    mut frames: ResMut<RoomFrames>,
    mut initial: ResMut<InitialWorlds>,
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
    lock_joints: Query<(Entity, &SphericalJoint), With<LockJoint>>,
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
                    teleport_avatar(
                        &mut commands,
                        &lock_joints,
                        entity,
                        spawn_position(),
                        &mut position,
                        &mut linear,
                        &mut angular,
                    );
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
            // A loaded save doubles as the room's *initial* world for `ResetRoom`;
            // a fresh random room's is snapshotted by `capture_initial_worlds`
            // once this system's commands apply.
            match save::take_pending(&save::code_string(state.0.room)) {
                Some(world) => {
                    spawn_room_world_from_save(
                        &mut commands,
                        room,
                        &world,
                        grounds.iter().next(),
                        &mut launches,
                        &mut frames,
                    );
                    initial.by_room.insert(room.id, world);
                }
                None => spawn_room_world(&mut commands, room, &frames),
            }
        }
        // Scope this avatar and this client to the room (`Rooms` is immutable, so
        // `insert` replaces any prior membership). The avatar is a real dynamic body
        // in the one shared Avian world, so isolate it to the room's collision layer
        // too (membership = room bit, filter = room bit + ground's default bit 0) —
        // otherwise it would shove *every* room's blocks. Matches `tag_room_part`,
        // so same-room avatars/parts/ground interact and cross-room ones don't —
        // including the frame-aware ground bit (a mid-flight room has no ground).
        let grounded = !frames.get(room.id).is_active();
        commands.entity(entity).insert((
            Rooms::single(room.id),
            RoomMember(room.id),
            room_layers(room.bit, grounded),
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
/// reset lands on the same valid on-platform disc a fresh join uses — except in a room
/// whose floating-origin frame is active, where the pad disc is mid-air: there a reset
/// puts the player back aboard the assembly deck (the only place to stand).
fn apply_position_resets(
    mut commands: Commands,
    frames: Res<RoomFrames>,
    deck_parts: DeckParts,
    lock_joints: Query<(Entity, &SphericalJoint), With<LockJoint>>,
    mut links: Query<
        (Entity, &mut MessageReceiver<ResetPosition>),
        (With<ClientOf>, With<Connected>),
    >,
    mut avatars: Query<
        (
            Entity,
            &ControlledBy,
            Option<&RoomMember>,
            &mut Position,
            &mut LinearVelocity,
            &mut AngularVelocity,
        ),
        (With<ServerAvatar>, Without<NetPart>),
    >,
) {
    let mut decks: Option<HashMap<RoomId, Vec3>> = None;
    for (link, mut receiver) in &mut links {
        // Drain the window; act once if any reset was requested (a single teleport is
        // idempotent, so coalescing repeats is correct).
        if receiver.receive().count() == 0 {
            continue;
        }
        for (avatar, controlled, member, mut position, mut linear, mut angular) in &mut avatars {
            if controlled.owner == link {
                let deck = member
                    .filter(|member| frames.get(member.0).is_active())
                    .and_then(|member| {
                        decks
                            .get_or_insert_with(|| {
                                deck_respawn_points(deck_parts.iter().map(|(p, part, room)| {
                                    (p.0, part_volume(part.shape), room.id)
                                }))
                            })
                            .get(&member.0)
                    });
                teleport_avatar(
                    &mut commands,
                    &lock_joints,
                    avatar,
                    deck.copied().unwrap_or_else(spawn_position),
                    &mut position,
                    &mut linear,
                    &mut angular,
                );
            }
        }
    }
}

/// Marks an avatar the demo deck-spawn has already placed, so [`spawn_demo_players_on_deck`]
/// runs once per join (and the player can then walk freely off the deck).
#[derive(Component)]
struct DemoOnDeck;

/// Demo hook: when `BS_SPAWN_ON_DECK` is set, teleport each freshly-joined avatar onto
/// the top of its room's largest assembly (the same mass-weighted deck point the
/// mid-flight reset/fall path uses via [`deck_respawn_points`]) instead of leaving it on
/// the pad. Lets a "rocket built + character onboard" save (a launch-ready stack loaded
/// into the room) put the player standing on the deck at spawn — they only Lock + Launch.
///
/// Acts once per avatar (gated by the [`DemoOnDeck`] marker), and only once the room's
/// parts exist and are tagged into the largest assembly (so `deck_respawn_points` has a
/// deck to find) — on the first join that creates the room, that's a frame or two after
/// the parts spawn. No-op (early return) unless the env var is set, so ordinary rooms are
/// untouched.
fn spawn_demo_players_on_deck(
    mut commands: Commands,
    deck_parts: DeckParts,
    lock_joints: Query<(Entity, &SphericalJoint), With<LockJoint>>,
    mut avatars: Query<
        (
            Entity,
            Option<&RoomMember>,
            &mut Position,
            &mut LinearVelocity,
            &mut AngularVelocity,
        ),
        (With<ServerAvatar>, Without<NetPart>, Without<DemoOnDeck>),
    >,
) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var("BS_SPAWN_ON_DECK").is_ok()) {
        return;
    }
    if avatars.iter().next().is_none() {
        return;
    }
    let decks = deck_respawn_points(
        deck_parts.iter().map(|(p, part, room)| (p.0, part_volume(part.shape), room.id)),
    );
    for (avatar, member, mut position, mut linear, mut angular) in &mut avatars {
        let Some(deck) = member.and_then(|m| decks.get(&m.0)) else {
            continue; // not roomed yet, or the room's assembly hasn't spawned/marked yet
        };
        teleport_avatar(
            &mut commands,
            &lock_joints,
            avatar,
            *deck,
            &mut position,
            &mut linear,
            &mut angular,
        );
        commands.entity(avatar).insert(DemoOnDeck);
    }
}

/// Each room's world exactly as it was created, kept so a [`ResetRoom`] request can
/// restore it: a room loaded from a save keeps that save's world (stored by
/// `assign_rooms`); a fresh random room is snapshotted by
/// [`capture_initial_worlds`]. Entries live for the server's lifetime — rooms are
/// few and a world is a couple of KB.
#[derive(Resource, Default)]
struct InitialWorlds {
    by_room: HashMap<RoomId, SaveWorld>,
}

/// Minimum spacing between two resets of the same room (see [`apply_room_resets`]).
const RESET_DEBOUNCE_SECS: f32 = 1.0;

/// Rooms queued for a reset by the crash detector ([`detect_assembly_crash`]),
/// drained by [`apply_room_resets`] alongside client `ResetRoom` requests. A set,
/// so several fixed steps flagging the same crashing room before the next `Update`
/// coalesce to one reset.
#[derive(Resource, Default)]
struct PendingRoomResets(HashSet<RoomId>);

/// A grounded, pre-blastoff assembly whose parts fall below this have toppled off
/// the platform toward the planet — a crash. Below the platform (parts rest at
/// `y > 0`) but above `PART_FALL_Y` (`-10`), so the room resets before its parts
/// individually recycle out from under the crash.
const ASSEMBLY_CRASH_Y: f32 = -5.0;

/// Flag a room for reset when its largest assembly crashes into the planet: any
/// member part dropping below [`ASSEMBLY_CRASH_Y`] while the room is grounded and
/// pre-blastoff. `InLargestAssembly` scopes this to the real rocket stack (≥ 2
/// jointed parts) — a lone part rolling off the edge just recycles. Skipped once a
/// room is launched or in an active floating-origin frame (in flight the assembly
/// legitimately sits anywhere; there's no planet to hit).
fn detect_assembly_crash(
    launches: Res<LaunchRegistry>,
    frames: Res<RoomFrames>,
    mut pending: ResMut<PendingRoomResets>,
    assembly: Query<(&Position, &PartRoom), With<InLargestAssembly>>,
) {
    for (position, part_room) in &assembly {
        let room = part_room.id;
        if pending.0.contains(&room) {
            continue;
        }
        if launches.is_launched(room) || frames.get(room).is_active() {
            // In flight the crash surface is radial: the planet has no collider away
            // from the pad bowl, so an assembly whose TRUE position sinks below the
            // planet sphere has flown into the terrain. Without this, a flight whose
            // realized attitude sagged flatter than planned (e.g. a side-hung rider's
            // standing torque) could circle *inside* the visual planet burning forever —
            // escape energy is unreachable that deep in the well. Small margin so a low
            // skim that visibly grazes the surface reads as the crash it looks like.
            let true_pos = position.0 + frames.get(room).offset.as_vec3();
            if (true_pos - PLANET_CENTER).length() < PLANET_RADIUS - 50.0 {
                pending.0.insert(room);
            }
            continue;
        }
        if position.0.y < ASSEMBLY_CRASH_Y {
            pending.0.insert(room);
        }
    }
}

/// Snapshot each freshly-created random room's world into [`InitialWorlds`]. A
/// room needing a snapshot is derived, not tracked: it's in `RoomRegistry` (which
/// `assign_rooms` mutates synchronously) but has no initial world yet. Runs in
/// `PostUpdate` — after `assign_rooms`' commands flushed at the end of `Update`
/// (so the room's parts are queryable at their exact spawn poses) and before the
/// next frame's fixed-schedule physics moves them — without the mid-`Update` sync
/// point an `.after(assign_rooms)` ordering edge would insert on every frame.
fn capture_initial_worlds(
    registry: Res<RoomRegistry>,
    mut initial: ResMut<InitialWorlds>,
    launches: Res<LaunchRegistry>,
    frames: Res<RoomFrames>,
    avatars: SnapshotAvatars,
    parts: SnapshotParts,
    joints: SnapshotJoints,
) {
    // Steady state: every known room already has its initial world (save-loaded
    // rooms get theirs the moment they're registered).
    if initial.by_room.len() == registry.by_code.len() {
        return;
    }
    for room in registry.by_code.values() {
        if initial.by_room.contains_key(&room.id) {
            continue;
        }
        let world = snapshot_room(
            room.id,
            launches.is_launched(room.id),
            frames.get(room.id).save(),
            &avatars,
            &parts,
            &joints,
        );
        initial.by_room.insert(room.id, world);
    }
}

/// Reset a room to its initial conditions on a client's [`ResetRoom`] request (the
/// menu's confirmed "Reset Room" action). Room-wide by design, like a launch: tear
/// down the live world (every part, joint, and the room state orb — all
/// server-owned, so the despawns replicate), clear the launch/countdown and
/// attitude-integral state, respawn the world the room was created with
/// ([`InitialWorlds`] — `spawn_room_world_from_save` also restores the initial
/// floating-origin frame and launched flag), and teleport every player in the room
/// to a fresh spawn, empty-handed (their held parts no longer exist). The avatars'
/// collision layers are re-derived from the restored frame, since a mid-flight
/// room had dropped its ground bit.
///
/// Requests are debounced per room ([`RESET_DEBOUNCE_SECS`]): unlike every other
/// control message, a reset is not idempotent (each one tears down and respawns
/// the world), and the reliable channel can deliver one send several times in
/// quick succession (observed on loopback, where the ~0 RTT makes the resend
/// timer fire before the first ack lands). The debounce also coalesces two
/// players confirming the dialog near-simultaneously.
fn apply_room_resets(
    time: Res<Time>,
    mut recent: Local<HashMap<RoomId, f32>>,
    mut pending: ResMut<PendingRoomResets>,
    mut commands: Commands,
    mut links: Query<(Entity, &mut MessageReceiver<ResetRoom>), (With<ClientOf>, With<Connected>)>,
    members: Query<(&ControlledBy, &RoomMember)>,
    initial: Res<InitialWorlds>,
    registry: Res<RoomRegistry>,
    grounds: Query<Entity, With<Grass>>,
    mut launches: ResMut<LaunchRegistry>,
    mut integrals: ResMut<RoomAttitudeIntegrals>,
    mut frames: ResMut<RoomFrames>,
    parts: Query<(Entity, &PartRoom)>,
    // Every joint in the room — part joints, ground clamps, AND player-lock welds
    // (the reset teleports every avatar to spawn, and a surviving weld would yank
    // the freshly-restored parts across the room). Positive "any joint" phrasing so
    // a future joint class is torn down by default rather than silently surviving.
    joints: Query<(Entity, &RoomMember), With<SphericalJoint>>,
    orbs: Query<(Entity, &RoomStateOf)>,
    mut avatars: Query<
        (
            Entity,
            &RoomMember,
            &mut Position,
            &mut LinearVelocity,
            &mut AngularVelocity,
            &mut HeldPart,
        ),
        (With<ServerAvatar>, Without<NetPart>),
    >,
) {
    // Rooms to reset this frame: client `ResetRoom` requests (menu button) plus
    // crash-flagged rooms from `detect_assembly_crash`.
    let mut rooms: Vec<RoomId> = Vec::new();
    for (link, mut receiver) in &mut links {
        // Drain the window; act at most once per link (repeats coalesce).
        if receiver.receive().count() == 0 {
            continue;
        }
        if let Some((_, member)) = members.iter().find(|(c, _)| c.owner == link) {
            rooms.push(member.0);
        }
    }
    rooms.extend(pending.0.drain());
    if rooms.is_empty() {
        return;
    }
    let now = time.elapsed_secs();
    recent.retain(|_, at| now - *at < RESET_DEBOUNCE_SECS);
    for room_id in rooms {
        // Debounce (see the doc comment): coalesces reliable-channel resends, two
        // players confirming near-simultaneously, and a crash racing a manual reset.
        if recent.get(&room_id).is_some_and(|at| now - at < RESET_DEBOUNCE_SECS) {
            continue;
        }
        recent.insert(room_id, now);
        let Some(world) = initial.by_room.get(&room_id) else {
            // Unreachable in practice (the snapshot lands the frame the room is
            // created, a reset arrives much later) — refuse rather than guess.
            println!("[reset] no initial world recorded for room {room_id:?} — ignoring");
            continue;
        };
        let Some(room) = registry.by_code.values().find(|r| r.id == room_id).copied() else {
            continue;
        };
        for (entity, part_room) in &parts {
            if part_room.id == room_id {
                commands.entity(entity).despawn();
            }
        }
        for (entity, member) in &joints {
            if member.0 == room_id {
                commands.entity(entity).despawn();
            }
        }
        for (entity, orb) in &orbs {
            if orb.0 == room_id {
                commands.entity(entity).despawn();
            }
        }
        launches.by_room.remove(&room_id);
        integrals.0.remove(&room_id);
        spawn_room_world_from_save(
            &mut commands,
            room,
            world,
            grounds.iter().next(),
            &mut launches,
            &mut frames,
        );
        // `spawn_room_world_from_save` restored the initial frame, so the layers
        // read the post-reset grounded state (almost always: grounded again).
        let grounded = !frames.get(room_id).is_active();
        for (entity, member, mut position, mut linear, mut angular, mut held) in &mut avatars {
            if member.0 != room_id {
                continue;
            }
            position.0 = spawn_position();
            linear.0 = Vec3::ZERO;
            angular.0 = Vec3::ZERO;
            held.0 = None;
            commands.entity(entity).insert(room_layers(room.bit, grounded));
        }
        println!("[reset] room {room_id:?} reset to initial conditions");
    }
}

/// How far below its room's deck a rider can fall (in flight, co-moving frame)
/// before being put back aboard. Generous enough that a jump off the edge reads
/// as a real fall first.
const FLIGHT_FALL_MARGIN: f32 = 50.0;

/// Per-room "back aboard" point for a room whose frame is active: over the
/// largest assembly's mass-weighted XZ (the deck's balance point) and just above
/// its highest part, so a respawned rider drops onto the deck. `members` yields
/// each assembly member as `(local position, mass weight, room)`.
fn deck_respawn_points<I: Iterator<Item = (Vec3, f32, RoomId)>>(
    members: I,
) -> HashMap<RoomId, Vec3> {
    const DECK_CLEARANCE: f32 = 2.0;
    let mut acc: HashMap<RoomId, (Vec3, f32, f32)> = HashMap::new();
    for (position, weight, room) in members {
        let (weighted, mass, top) = acc.entry(room).or_insert((Vec3::ZERO, 0.0, f32::MIN));
        *weighted += position * weight;
        *mass += weight;
        *top = top.max(position.y);
    }
    acc.into_iter()
        .filter(|(_, (_, mass, _))| *mass > 0.0)
        .map(|(room, (weighted, mass, top))| {
            let com = weighted / mass;
            (room, Vec3::new(com.x, top + DECK_CLEARANCE, com.z))
        })
        .collect()
}

/// The query behind [`deck_respawn_points`]: every largest-assembly member's local
/// pose + mass weight + room. Reading `&NetPart` makes it provably disjoint from
/// the avatar queries the callers mutate `Position` through (avatars never carry
/// `NetPart` — they filter `Without<NetPart>`).
type DeckParts<'w, 's> = Query<
    'w,
    's,
    (&'static Position, &'static NetPart, &'static PartRoom),
    With<InLargestAssembly>,
>;

/// The avatar equivalent of single-player's fall→respawn cycle (`player::despawn`
/// + `spawn`, both suppressed on the server): a fallen or diverged avatar is
/// *teleported* back, never despawned. Despawning would replicate as a recursive
/// despawn of the client's predicted avatar — taking the camera rig mounted under
/// it with it — and nothing would respawn it. Gated on `RoomMember` because a
/// not-yet-roomed avatar is deliberately parked at y = -1000 (the bootstrap
/// hiding spot). Runs in `FixedUpdate` for the same NaN-broadphase reason as
/// `replace_fallen_room_parts`.
///
/// Grounded rooms: fall below -30 (matching single-player, deeper than
/// `PART_FALL_Y` so a rider falls visibly past the part cull line) → a fresh pad
/// spawn. Rooms with an active floating-origin frame have no ground and their
/// deck can sit anywhere within the rebase band, so the check is deck-relative:
/// fall `FLIGHT_FALL_MARGIN` below the assembly deck → back aboard it. This is
/// also what catches a mid-flight joiner (spawned at the pad disc, which in an
/// active frame is mid-air near the assembly): they free-fall briefly, then land
/// on the deck.
fn respawn_fallen_avatars(
    mut commands: Commands,
    frames: Res<RoomFrames>,
    deck_parts: DeckParts,
    lock_joints: Query<(Entity, &SphericalJoint), With<LockJoint>>,
    mut avatars: Query<
        (Entity, &RoomMember, &mut Position, &mut LinearVelocity, &mut AngularVelocity, Option<&RiderLock>),
        (With<ServerAvatar>, Without<NetPart>),
    >,
) {
    // Grounded rooms: respawn as a player reaches the planet surface below the
    // cliffs. This is the "touch the ground other than the grass platform → respawn"
    // rule — the planet has no collider, so a fall off the cliff is caught here by
    // height ([`PLANET_RESPAWN_Y`], well below `PART_FALL_Y` so a rider falls past the
    // part cull line first).
    const AVATAR_FALL_Y: f32 = PLANET_RESPAWN_Y;
    // Deck points are only needed for avatars in active-frame rooms — computed
    // lazily so the (common) all-grounded case never builds them.
    let mut decks: Option<HashMap<RoomId, Vec3>> = None;
    for (avatar, member, mut position, mut linear, mut angular, locked) in &mut avatars {
        let deck = frames
            .get(member.0)
            .is_active()
            .then(|| {
                decks
                    .get_or_insert_with(|| {
                        deck_respawn_points(deck_parts.iter().map(|(p, part, room)| {
                            (p.0, part_volume(part.shape), room.id)
                        }))
                    })
                    .get(&member.0)
            })
            .flatten();
        let fallen = match deck {
            Some(deck) => position.0.y < deck.y - FLIGHT_FALL_MARGIN,
            None => position.0.y < AVATAR_FALL_Y,
        };
        // A rider who INTENDS to be locked (`RiderLock`) but whose weld broke and let it
        // fall is left to the tether (`keep_riders_aboard`), which snaps it back onto its
        // lock point and re-welds — respawning it here would unlock it (teleport clears
        // `RiderLock`) and strand it standing free. Divergence (NaN / runaway) still
        // respawns unconditionally: the broadphase would otherwise panic.
        if part_state_diverged(position.0, linear.0, angular.0) || (fallen && locked.is_none()) {
            teleport_avatar(
                &mut commands,
                &lock_joints,
                avatar,
                deck.copied().unwrap_or_else(spawn_position),
                &mut position,
                &mut linear,
                &mut angular,
            );
        }
    }
}

/// How long a reconnecting rider keeps trying to re-weld to its remembered deck point
/// before giving up (the deck may be gone — landed, reset — by the time it returns).
const RELOCK_GRACE_SECS: f32 = 8.0;

/// Distance past which a rider that has come loose from its lock (a solver break flung
/// it clear, or a reconnect left it near-but-unwelded) is snapped back onto its
/// [`RiderLock`] deck point and re-welded — the "locked riders can't be left behind"
/// safety net. A live weld pins the feet ~0 m from the anchor, so a still-welded rider
/// never trips it. Frame-invariant (both poses room-local).
const RIDER_TETHER_M: f32 = 50.0;

/// A reconnecting rider that was LOCKED when its session dropped, carrying the anchor to
/// restore the weld with. `spawn_player_for_client` restores position but not the weld
/// (it lived on the despawned `SessionBased` avatar); `relock_resumed_riders` re-welds
/// once the body + room are ready.
#[derive(Component)]
struct RelockOnResume {
    anchor: LockAnchor,
    timer: f32,
}

/// Re-weld a resumed locked rider ([`RelockOnResume`]) to its remembered deck point.
/// Once the reconnected body + `RoomMember` exist and the referenced part is present,
/// snap the feet exactly onto the lock point (so the weld can't miss the gap) and
/// re-weld, recording the fresh [`RiderLock`]. Gives up after [`RELOCK_GRACE_SECS`] if
/// the part never reappears (the room was reset or the rider rejoined a different one).
/// Runs after `respawn_fallen_avatars` so the body has already settled near the deck.
fn relock_resumed_riders(
    mut commands: Commands,
    time: Res<Time>,
    parts: Query<(Entity, &NetPart, &Collider, &Position, &Rotation, &PartRoom)>,
    lock_joints: Query<(Entity, &SphericalJoint), With<LockJoint>>,
    mut riders: Query<
        (
            Entity,
            &Collider,
            &mut Position,
            &Rotation,
            &mut LinearVelocity,
            &mut AngularVelocity,
            &HeldPart,
            &RoomMember,
            &NetPlayer,
            &mut RelockOnResume,
        ),
        (With<ServerAvatar>, Without<NetPart>),
    >,
) {
    let dt = time.delta_secs();
    for (avatar, collider, mut position, rotation, mut linear, mut angular, held, member, player, mut relock) in
        &mut riders
    {
        let anchor = relock.anchor;
        if let Some(target) = lock_target(&parts, member.0, anchor, rotation.0) {
            teleport_avatar(
                &mut commands,
                &lock_joints,
                avatar,
                target,
                &mut position,
                &mut linear,
                &mut angular,
            );
            let (welds, primary) = weld_avatar_to_room_parts(
                &mut commands,
                &parts,
                avatar,
                collider,
                target,
                rotation.0,
                held.0,
                member.0,
                player.client_id,
            );
            commands.entity(avatar).insert(RiderLock(primary.unwrap_or(anchor)));
            commands.entity(avatar).remove::<RelockOnResume>();
            println!("[lock] client_id={} re-locked on resume ({} welds)", player.client_id, welds);
            continue;
        }
        relock.timer -= dt;
        if relock.timer <= 0.0 {
            commands.entity(avatar).remove::<RelockOnResume>();
            println!("[lock] client_id={} relock-on-resume gave up (deck gone)", player.client_id);
        }
    }
}

/// Tether: snap a rider that has drifted more than [`RIDER_TETHER_M`] from its
/// [`RiderLock`] deck point back onto it and re-weld. A held weld keeps the feet at the
/// anchor, so this only fires once the weld is genuinely gone AND the rider has fallen
/// clear — the safety net for "a rider falls away from the assembly." If the reference
/// part has vanished (recycled/reset), the lock is meaningless, so drop `RiderLock` and
/// let the normal fall-respawn take over next tick.
fn keep_riders_aboard(
    mut commands: Commands,
    parts: Query<(Entity, &NetPart, &Collider, &Position, &Rotation, &PartRoom)>,
    lock_joints: Query<(Entity, &SphericalJoint), With<LockJoint>>,
    mut riders: Query<
        (
            Entity,
            &Collider,
            &mut Position,
            &Rotation,
            &mut LinearVelocity,
            &mut AngularVelocity,
            &HeldPart,
            &RoomMember,
            &NetPlayer,
            &RiderLock,
        ),
        (With<ServerAvatar>, Without<NetPart>),
    >,
) {
    for (avatar, collider, mut position, rotation, mut linear, mut angular, held, member, player, lock) in
        &mut riders
    {
        let anchor = lock.0;
        let Some(target) = lock_target(&parts, member.0, anchor, rotation.0) else {
            commands.entity(avatar).remove::<RiderLock>();
            continue;
        };
        // `target` places the feet on the anchor; the feet's current offset from the
        // body (`rotation·foot_local`) is common to both, so feet-to-anchor distance is
        // just how far the body is from `target`.
        if position.0.distance(target) <= RIDER_TETHER_M {
            continue;
        }
        teleport_avatar(
            &mut commands,
            &lock_joints,
            avatar,
            target,
            &mut position,
            &mut linear,
            &mut angular,
        );
        let (welds, primary) = weld_avatar_to_room_parts(
            &mut commands,
            &parts,
            avatar,
            collider,
            target,
            rotation.0,
            held.0,
            member.0,
            player.client_id,
        );
        commands.entity(avatar).insert(RiderLock(primary.unwrap_or(anchor)));
        println!("[lock] client_id={} tether snap-back ({} welds)", player.client_id, welds);
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
    avatars: Query<
        (&ActionState<NetInput>, &Position, Option<&RiderLock>, Option<&RelockOnResume>),
        With<RoomMember>,
    >,
) {
    *throttle -= time.delta_secs();
    if *throttle > 0.0 {
        return;
    }
    *throttle = 0.25;
    let now = SystemTime::now();
    for (state, position, lock, relock) in &avatars {
        if state.0.resume_id != 0 {
            // Lock intent = welded (`RiderLock`) OR still re-welding after a reconnect
            // (`RelockOnResume`). Folding in the pending case keeps a fast second
            // reconnect from recording the transient un-welded avatar as *unlocked* and
            // blanking the anchor before the relock completes.
            let anchor = lock.map(|l| l.0).or_else(|| relock.map(|r| r.anchor));
            resume.by_id.insert(state.0.resume_id, (position.0, state.0.room, anchor, now));
        }
    }
    // Drop records for players who didn't return within the grace window.
    resume.by_id.retain(|_, (_, _, _, at)| {
        at.elapsed().map(|e| e.as_secs() < RESUME_GRACE_SECS).unwrap_or(false)
    });
}

/// Spawn a fresh room's world: its own set of parts (replicated + predicted +
/// collision-isolated to the room). Parts replicate immediately — a client that
/// joins mid-fall (or mid-shove) now receives their velocity too, so its predicted
/// copy falls in sync rather than drifting.
fn spawn_room_world(commands: &mut Commands, room: Room, frames: &RoomFrames) {
    for _ in 0..NUM_PARTS {
        let (entity, half_extents, seed) = spawn_random_part(commands);
        tag_room_part(
            commands,
            entity,
            PartShape::Cuboid { half_extents: half_extents.to_array() },
            seed,
            room,
            frames,
        );
    }
    // Rocket engines join the loose-parts pool (see `spawn_random_part` above): same
    // room-scoped replication + prediction, distinguished only by `PartShape::RocketEngine`
    // so each client rebuilds the cylinder+cone body instead of a cuboid. Rockets carry no
    // appearance seed (their striped body material is fixed) — pass 0.
    for _ in 0..NUM_ROCKET_ENGINES {
        let entity = spawn_random_rocket(commands);
        tag_room_part(commands, entity, PartShape::RocketEngine, 0, room, frames);
    }
    spawn_room_state(commands, room, NetRoomFrame::default());
}

/// One replicated per-room state entity: a server-owned, physics-less data holder that
/// carries the room's replicated state — its launch/countdown (`NetLaunch`) and its
/// floating-origin frame (`NetRoomFrame`). Scoped to the room so only that room's
/// clients receive it, so a single replicated entity per room tells clients where the
/// launch sequence is and where the room is in true coordinates. (The assembly's
/// centre-of-mass orb is no longer streamed from here — clients derive it locally from
/// the replicated `InLargestAssembly` membership; see `mark_largest_assembly`.)
fn spawn_room_state(commands: &mut Commands, room: Room, frame: NetRoomFrame) {
    commands.spawn((
        NetLaunch::default(),
        frame,
        Replicate::to_clients(NetworkTarget::All),
        Rooms::single(room.id),
        RoomStateOf(room.id),
    ));
}

/// Spawn a fresh room's world from a saved snapshot instead of the random pool
/// (the load half of the save-game feature — see `crate::save`): respawn every
/// saved part at its saved pose/velocity, rebuild the joints (remapping saved
/// part indices to the new entities; ground endpoints to the shared `Grass`
/// entity), restore the launched flag and floating-origin frame (saved poses are
/// room-local, so a mid-flight save resumes its flight — same local world, same
/// frame), and spawn the room orb.
fn spawn_room_world_from_save(
    commands: &mut Commands,
    room: Room,
    world: &SaveWorld,
    ground: Option<Entity>,
    launches: &mut LaunchRegistry,
    frames: &mut RoomFrames,
) {
    let frame = RoomFrame {
        offset: DVec3::from_array(world.frame.offset),
        velocity: Vec3::from_array(world.frame.velocity),
    };
    frames.by_room.insert(room.id, frame);
    let grounded = !frame.is_active();
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
            tag_room_part(commands, entity, shape, p.seed, room, frames);
            entity
        })
        .collect();

    for joint in &world.joints {
        // A ground joint is meaningless (and dangerous) in a room whose frame is
        // active: the shared ground body does NOT ride the frame, so the joint
        // would pin the assembly to the phantom ground at the local origin — and
        // the next rebase would shift the bodies km away from the unmoved anchor,
        // exploding the constraint. Real saves never combine the two (blastoff
        // cuts ground joints long before the first rebase); refuse it anyway.
        if !grounded && (joint.body1 == SaveBody::Ground || joint.body2 == SaveBody::Ground) {
            println!("[save] skipping ground joint (room frame is active: {joint:?})");
            continue;
        }
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
    spawn_room_state(commands, room, frame.net());
}

/// Tag a freshly-spawned part for room-scoped replication: its shape + stable id
/// via `NetPart`, its pose via the predicted Avian `Position`/`Rotation`,
/// replicated + predicted, scoped to the room's `Rooms`, and isolated to the
/// room's collision layer. It reads the room's floating-origin frame itself, so
/// no caller can tag a part with the wrong ground bit: grounded rooms collide
/// with the ground, rebased rooms don't.
fn tag_room_part(
    commands: &mut Commands,
    entity: Entity,
    shape: PartShape,
    seed: u32,
    room: Room,
    frames: &RoomFrames,
) {
    let grounded = !frames.get(room.id).is_active();
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
        room_layers(room.bit, grounded),
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

/// On the attach intent, joint the held part to any nearby part — or the ground —
/// within `JOINT_GAP`, at the (thinned) manifold anchors, then release it (it's now
/// part of the assembly). Cross-room parts are filtered out and the ground is shared
/// but the joint itself is room-tagged, so the join is room-scoped automatically. The
/// same shared `part_gap_contacts` the single-player path uses. Joints are server
/// physics, so the joined parts move together and their replicated poses tell the
/// story (no joint replication needed).
fn server_attach(
    mut commands: Commands,
    // Every part's collider + authoritative pose + room, for the gap weld query
    // (`part_gap_contacts`). Includes the held part. Poses come from Avian `Position`/
    // `Rotation`, not `Transform` (`lightyear_avian` owns the Position→Transform sync,
    // so `Transform` can lag).
    parts_q: Query<(Entity, &Collider, &Position, &Rotation, &PartRoom), With<NetPart>>,
    // The shared ground bowl (`RigidBody::Static` at the world origin) is a weld
    // candidate too — `part_gap_contacts` thins its faceted manifold to a spread rigid
    // set, so a rocket clamps to the ground with no anchor triangle.
    ground_q: Query<(Entity, &Collider), With<Grass>>,
    // Existing joints (to tell which parts are joining for the FIRST time) and each
    // part's room (to spawn the replacement in the same room).
    joints: Query<&SphericalJoint>,
    part_rooms: Query<&PartRoom>,
    frames: Res<RoomFrames>,
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
        // Replenish the loose-parts pool when a part joins its FIRST joint (it's been
        // consumed into a structure). `commands.spawn` is deferred, so `had_joint`
        // reflects the pre-attach state. Ground/characters never reach here.
        let mut replenish = |commands: &mut Commands, endpoint: Entity| {
            if !had_joint.contains(&endpoint) && !replaced.contains(&endpoint) {
                replaced.push(endpoint);
                if let Some(room) = held_room {
                    let (new_entity, half_extents, seed) = spawn_random_part(commands);
                    tag_room_part(
                        commands,
                        new_entity,
                        PartShape::Cuboid { half_extents: half_extents.to_array() },
                        seed,
                        room,
                        &frames,
                    );
                }
            }
        };

        // Weld the held part to any same-room part OR the shared ground within
        // `JOINT_GAP`, from the thinned contact manifold (flush faces → a spread rigid
        // set of welds; the faceted bowl is thinned to a handful, so no special ground
        // path). The held part is body1 so the client anchors the gizmo to it (a
        // `NetPart`); the ground endpoint is named by the `GROUND_JOINT_ID` sentinel.
        // Each weld freezes the pair at its current relative pose (zero rest error).
        if let Ok((_, held_collider, held_pos, held_rot, held_pr)) = parts_q.get(held_entity) {
            let parts_iter = parts_q
                .iter()
                .filter(|(o, _, _, _, pr)| *o != held_entity && pr.id == held_pr.id)
                .map(|(o, c, p, r, _)| (o, c, p.0, r.0, o.to_bits()));
            let ground_iter = ground_q
                .iter()
                .map(|(o, c)| (o, c, Vec3::ZERO, Quat::IDENTITY, GROUND_JOINT_ID));
            let mut contacts = Vec::new();
            for (other, other_collider, other_pos, other_rot, other_net_id) in
                parts_iter.chain(ground_iter)
            {
                contacts.clear();
                part_gap_contacts(
                    held_collider,
                    held_pos.0,
                    held_rot.0,
                    other_collider,
                    other_pos,
                    other_rot,
                    &mut contacts,
                );
                for (held_local, other_local) in contacts.iter().copied() {
                    spawn_room_joint(
                        &mut commands,
                        member.0,
                        (held_entity, held_local, held_entity.to_bits()),
                        (other, other_local, other_net_id),
                    );
                    attached = true;
                }
                if !contacts.is_empty() {
                    replenish(&mut commands, held_entity);
                    // The ground (its endpoint is the `GROUND_JOINT_ID` sentinel) isn't a
                    // loose part — never replace it.
                    if other_net_id != GROUND_JOINT_ID {
                        replenish(&mut commands, other);
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
    // `With<NetJoint>` — the player-built part joints, positively. Player-lock welds
    // (dissolved by "Unlock", never the delete gesture) don't carry it, and neither
    // would any future non-part joint class, so they're exempt by default.
    joints: Query<(Entity, &SphericalJoint, &RoomMember), With<NetJoint>>,
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

/// Teleport an avatar: dissolve its lock welds first (a teleport while welded would
/// drag the welded parts along — the deferred despawns apply before this tick's
/// physics step, so the weld never solves across the jump), then set the pose and
/// zero the velocities. THE way to move an avatar server-side; every teleport site
/// (reset-position, fall respawn, room reset, resume revoke) goes through it so the
/// "teleport implies unlock" invariant can't be forgotten at a future site. Also drops
/// the persistent [`RiderLock`] anchor, so a reset/respawn genuinely unlocks the rider
/// and the tether ([`keep_riders_aboard`]) won't drag them back to the old spot; the
/// two re-lock sites re-insert it right after their own teleport.
fn teleport_avatar(
    commands: &mut Commands,
    lock_joints: &Query<(Entity, &SphericalJoint), With<LockJoint>>,
    avatar: Entity,
    to: Vec3,
    position: &mut Position,
    linear: &mut LinearVelocity,
    angular: &mut AngularVelocity,
) {
    despawn_player_lock_welds(commands, lock_joints, avatar);
    commands.entity(avatar).try_remove::<RiderLock>();
    position.0 = to;
    linear.0 = Vec3::ZERO;
    angular.0 = Vec3::ZERO;
}

/// A rider's lock reference point: the primary weld's part (by stable replicated id)
/// plus the two anchors, so the server can restore the weld after the live joint is
/// gone — on reconnect (the `SessionBased` avatar and its welds despawned with the
/// session) or after a solver break flings the rider clear ([`keep_riders_aboard`]).
/// Deliberately the server-side, non-replicated twin of `NetLockJoint` (which rides the
/// per-weld replicated entity and dies with the avatar's session): clients derive
/// lockedness from the `NetLockJoint` set; this is the server's private memory of
/// *where* to re-attach.
#[derive(Clone, Copy)]
struct LockAnchor {
    /// Stable `NetPart::id` of the part the primary weld pins to (survives entity churn).
    part_net_id: u64,
    /// Rider-frame foot anchor (`body1` local anchor) — rotation-invariant.
    foot_local: Vec3,
    /// Part-frame anchor (`body2` local anchor).
    part_local: Vec3,
}

/// Marks an avatar as *intending* to be locked, carrying the anchor to restore the weld
/// with. Set on lock ([`apply_lock_changes`]) and after a reconnect/tether re-weld;
/// cleared on unlock and on any teleport ([`teleport_avatar`]).
#[derive(Component, Clone, Copy)]
struct RiderLock(LockAnchor);

/// Weld an avatar to every same-room part within the lock gap (skipping its own held
/// part), spawning each `SphericalJoint` + replicated `NetLockJoint`. Returns the weld
/// count and the PRIMARY anchor (the first weld) for the caller to store as the rider's
/// [`RiderLock`]. The weld geometry is the shared `avatar_lock_contacts`, so the server
/// lock can't drift from single-player.
fn weld_avatar_to_room_parts(
    commands: &mut Commands,
    parts: &Query<(Entity, &NetPart, &Collider, &Position, &Rotation, &PartRoom)>,
    avatar: Entity,
    collider: &Collider,
    position: Vec3,
    rotation: Quat,
    held: Option<Entity>,
    member: RoomId,
    client_id: u64,
) -> (usize, Option<LockAnchor>) {
    let mut welds = 0usize;
    let mut primary = None;
    let candidates = parts
        .iter()
        .filter(|(part, _, _, _, _, part_room)| part_room.id == member && held != Some(*part))
        .map(|(part, _, c, p, r, _)| (part, c, p.0, r.0));
    avatar_lock_contacts(
        (collider, position, rotation),
        candidates,
        |part, avatar_local, part_local| {
            // Shared-borrow re-read of the same query the candidates iterate — just to
            // name the part's stable replicated id on the weld (and the anchor record).
            let net_id = parts.get(part).map(|(_, net_part, ..)| net_part.id).unwrap_or(0);
            commands.spawn((
                SphericalJoint::new(avatar, part)
                    .with_local_anchor1(avatar_local)
                    .with_local_anchor2(part_local),
                LockJoint,
                NetLockJoint {
                    player: client_id,
                    part: net_id,
                    anchor_player: avatar_local.to_array(),
                    anchor_part: part_local.to_array(),
                },
                Replicate::to_clients(NetworkTarget::All),
                Rooms::single(member),
                RoomMember(member),
            ));
            if primary.is_none() {
                primary =
                    Some(LockAnchor { part_net_id: net_id, foot_local: avatar_local, part_local });
            }
            welds += 1;
        },
    );
    (welds, primary)
}

/// The avatar position that lands its feet exactly on a [`LockAnchor`]'s deck point,
/// given the referenced part's current pose (looked up by stable id within the rider's
/// room). `None` if that part is no longer present (recycled/reset). Frame-invariant:
/// both sides are room-local, so a floating-origin rebase can't spoof it.
fn lock_target(
    parts: &Query<(Entity, &NetPart, &Collider, &Position, &Rotation, &PartRoom)>,
    member: RoomId,
    anchor: LockAnchor,
    rotation: Quat,
) -> Option<Vec3> {
    let (_, _, _, part_pos, part_rot, _) =
        parts.iter().find(|(_, np, _, _, _, pr)| pr.id == member && np.id == anchor.part_net_id)?;
    let anchor_world = part_pos.0 + part_rot.0 * anchor.part_local;
    Some(anchor_world - rotation * anchor.foot_local)
}

/// Apply a client's "Lock"/"Unlock" request ([`SetLocked`]). Locking welds the
/// sender's avatar to every same-room part currently within the weld gap — the same
/// gap-tolerant, freeze-in-place contact manifold `server_attach` welds parts with
/// (`part_gap_contacts`), so the rider is pinned exactly where they stand with zero
/// rest error. Each weld is a `SphericalJoint` (avatar = `body1`) plus its replicated
/// [`NetLockJoint`] mirror, so every client rebuilds it between its *predicted*
/// avatar/part copies. Never welds to the ground (an avatar↔ground weld would pin the
/// rider to the pad at blastoff) or to the sender's own held part. Unlocking despawns
/// all of the sender's welds; the despawn replicates and every client drops them.
///
/// Idempotent (unlike `ResetRoom`, no debounce needed): locking while already locked
/// or unlocking while free is a no-op, so the reliable channel's duplicate delivery
/// is harmless. Rapid toggles in one drain window coalesce to the last value.
fn apply_lock_changes(
    mut commands: Commands,
    mut links: Query<(Entity, &mut MessageReceiver<SetLocked>), (With<ClientOf>, With<Connected>)>,
    avatars: Query<
        (
            Entity,
            &ControlledBy,
            &RoomMember,
            &NetPlayer,
            &Collider,
            &Position,
            &Rotation,
            &HeldPart,
        ),
        With<ServerAvatar>,
    >,
    parts: Query<(Entity, &NetPart, &Collider, &Position, &Rotation, &PartRoom)>,
    lock_joints: Query<(Entity, &SphericalJoint), With<LockJoint>>,
) {
    for (link, mut receiver) in &mut links {
        let Some(want) = receiver.receive().last() else {
            continue;
        };
        let Some((avatar, _, member, player, collider, position, rotation, held)) =
            avatars.iter().find(|(_, controlled, ..)| controlled.owner == link)
        else {
            continue;
        };
        let already_locked = lock_joints.iter().any(|(_, joint)| joint.body1 == avatar);
        if !want.0 {
            despawn_player_lock_welds(&mut commands, &lock_joints, avatar);
            commands.entity(avatar).remove::<RiderLock>();
            println!("[lock] client_id={} unlocked", player.client_id);
            continue;
        }
        if already_locked {
            continue; // A duplicate delivery or a stale press.
        }
        // Weld to every same-room part within the gap and remember the primary anchor so
        // a reconnect or a >50 m break can restore the weld (`RiderLock`).
        let (welds, anchor) = weld_avatar_to_room_parts(
            &mut commands,
            &parts,
            avatar,
            collider,
            position.0,
            rotation.0,
            held.0,
            member.0,
            player.client_id,
        );
        if let Some(anchor) = anchor {
            commands.entity(avatar).insert(RiderLock(anchor));
        }
        println!("[lock] client_id={} locked with {} welds", player.client_id, welds);
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

/// Mirror whether each avatar's player is pressing a move/jump input into its
/// replicated [`NetMoving`], so remote clients animate it honestly (walk only on
/// real input — never from world-frame motion, which made riders on a drifting
/// rocket look like they were running in place). Only writes on change, so an idle
/// or steadily-walking avatar generates no traffic.
fn sync_avatar_moving(mut avatars: Query<(&ActionState<NetInput>, &mut NetMoving)>) {
    for (state, mut moving) in &mut avatars {
        let now = state.0.move_xz != [0.0, 0.0] || state.0.jump;
        if moving.0 != now {
            moving.0 = now;
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
                // `try_remove` for the same reason as the `try_insert` below — and
                // this arm is reachable in one move: `apply_room_resets` (same
                // unordered Update tuple) releases the holder AND despawns the part.
                commands.entity(entity).try_remove::<NetHold>();
            }
        }
    }
    // Insert tags for parts newly held this tick (those still left in the map).
    // `try_insert`, not `insert`: a held part can't fall, but if one is ever despawned
    // the same frame (e.g. `replace_fallen_room_parts` or a room reset) the deferred
    // insert would hit a missing entity — `try_insert` no-ops instead of erroring.
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
    frames: Res<RoomFrames>,
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
        // The fall check only means something in true coordinates: while the room's
        // floating-origin frame is active there is no ground, and the parts left
        // behind at the pad sit far below the local origin *on purpose* — recycling
        // them mid-flight would respawn them mid-air next to the assembly and they'd
        // fall (and recycle) forever. They keep falling out of the world instead,
        // and the moment the frame resets to zero this check sees true coordinates
        // again and restocks the pad. Divergence still recycles anywhere.
        let grounded = !frames.get(part_room.id).is_active();
        if (grounded && position.0.y < PART_FALL_Y) || diverged {
            commands.entity(entity).despawn();
            let room = part_room.room();
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
                        &frames,
                    );
                }
                PartShape::RocketEngine => {
                    let new_entity = spawn_random_rocket(&mut commands);
                    tag_room_part(
                        &mut commands,
                        new_entity,
                        PartShape::RocketEngine,
                        0,
                        room,
                        &frames,
                    );
                }
            }
        }
    }
}

/// Keep at least one **unused** rocket engine available in every grounded room: once a
/// builder has jointed all of a room's rockets into assemblies, drop a fresh one from the
/// sky (the same spawn-zone free-fall as the initial pool) so they're never stranded
/// without a spare to add to the stack.
///
/// "Unused" = not referenced by any joint — a rocket welded into a stack (or pinned to the
/// ground) counts as used. Only **grounded** rooms restock: a room mid-flight has no
/// ground for a fresh rocket to land on, and its assembly's rockets are all attached by
/// design, so a sky-drop there would just fall away below the ascending stack. When the
/// stack lands (the floating-origin frame resets to zero) the room is grounded again and a
/// spare drops if all its rockets are still attached.
///
/// Runs on a 1 s timer (`run_if` at registration): the shortfall it repairs only
/// arises on joint/rocket churn, so scanning every joint at the 60 Hz tick rate was
/// pure waste, and a restock appearing within a second is indistinguishable from
/// immediate. The spawn is deferred, so the fresh rocket first appears (as an unused
/// rocket) next run — one restock per shortfall, no double-spawn.
fn ensure_spare_rocket(
    mut commands: Commands,
    frames: Res<RoomFrames>,
    rockets: Query<(Entity, &PartRoom), With<RocketEngine>>,
    joints: Query<&SphericalJoint>,
) {
    // Every rocket entity that participates in a joint is "in use".
    let jointed: HashSet<Entity> = joints.iter().flat_map(|j| [j.body1, j.body2]).collect();
    // Per grounded room: its descriptor + whether it already has an unused rocket.
    let mut rooms: HashMap<RoomId, (Room, bool)> = HashMap::new();
    for (entity, part_room) in &rockets {
        if frames.get(part_room.id).is_active() {
            continue; // in flight — no ground to restock onto
        }
        let entry = rooms.entry(part_room.id).or_insert((part_room.room(), false));
        entry.1 |= !jointed.contains(&entity);
    }
    for (room, has_spare) in rooms.into_values() {
        if !has_spare {
            let entity = spawn_random_rocket(&mut commands);
            tag_room_part(&mut commands, entity, PartShape::RocketEngine, 0, room, &frames);
        }
    }
}

/// Recompute each room's **largest assembly** — the biggest connected component of
/// parts joined together through joints — and publish which parts belong to it by
/// marking them with a replicated [`InLargestAssembly`]. Clients derive the
/// centre-of-mass orb and the combined thrust arrow from this membership locally, over
/// their *predicted* parts, so those visuals are predicted client-side (no streamed COM
/// position).
///
/// Runs every frame, which covers "whenever a joint is created or deleted" (the only
/// time membership can change). The per-part marker only re-replicates when membership
/// actually flips (guarded by `Has<InLargestAssembly>`), so a settled world generates
/// no traffic.
///
/// Parts never joint to the ground (`server_attach` attaches only to other `NetPart`s)
/// and cross-room parts can't collide (collision layers), so the graph is purely
/// part-to-part within one room — "blocks connected through the ground" simply can't
/// arise here. A lone part is not an assembly, so only components of ≥ 2 parts count.
fn mark_largest_assembly(
    mut commands: Commands,
    parts: Query<(Entity, &Position, &NetPart, &PartRoom, Has<InLargestAssembly>)>,
    joints: Query<&SphericalJoint>,
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
    // `try_insert`/`try_remove`: a part queried this frame can be despawned before
    // this system's commands apply (`apply_room_resets` tears down a whole room's
    // parts mid-`Update`), and the plain commands panic on a dead entity.
    for (entity, _, _, _, is_marked) in &parts {
        let is_member = index.get(&entity).is_some_and(|i| member_indices.contains(i));
        if is_member && !is_marked {
            commands.entity(entity).try_insert(InLargestAssembly);
        } else if !is_member && is_marked {
            commands.entity(entity).try_remove::<InLargestAssembly>();
        }
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

/// Per-room attitude-integral state for the launch autopilot's PID (the `integral`
/// argument of `assembly_burn`) — one `Vec3` per launched room, persisted across
/// ticks so a standing external torque (a rider off-centre) is held with zero
/// standing attitude error.
#[derive(Resource, Default)]
struct RoomAttitudeIntegrals(HashMap<RoomId, Vec3>);

/// Per-room cumulative launch fuel, as thrust **impulse** (N·s = ∫ Σ|engine force| dt) —
/// the authoritative propellant tally the flight recorder logs so a bot's fuel-to-escape
/// can be measured exactly. Accumulated by [`apply_room_rocket_thrust`], cleared when the
/// room resets so a re-launch starts from zero. (The client keeps its own approximate
/// copy for the HUD; this is the exact one.)
#[derive(Resource, Default)]
struct RoomFuel(HashMap<RoomId, f32>);

/// Per-room fuel-optimal ascent plan: the [`PitchProgram`] built on the first launched
/// tick from the assembly's real thrust-to-weight (see `PitchProgram::plan` — what the
/// autopilot actually flies, and why not closed-loop prograde). Its pitchover angle
/// replicates via [`NetLaunch::pitchover`] so the predicted twin rebuilds the identical
/// program. Cleared when a fresh launch arms so a rebuilt/reloaded assembly gets
/// re-planned (see [`handle_launch_requests`]).
#[derive(Resource, Default)]
struct RoomPolicy(HashMap<RoomId, PitchProgram>);

/// Per-room apparent-up for riders aboard a launched assembly (see the client's
/// `ApparentUp` for the full rationale): `normalize(thrust_accel − gravity_at(true_com))`
/// — bounded tilt near the planet, radial-up in coast, pure thrust axis in deep space.
/// Written by [`apply_room_rocket_thrust`] each tick; rooms without an entry (not
/// launched) fall back to world-up in [`sample_felt_up`].
#[derive(Resource, Default)]
struct RoomApparentUp(HashMap<RoomId, Vec3>);

/// Per-room escape-cutoff hysteresis state (see [`escape_cutoff`]): whether a room's
/// assembly currently has its throttle cut for reaching escape. Held across ticks so the
/// cut can't chatter at the boundary, yet re-fires if the ship falls back below escape.
/// Cleared for rooms that are no longer launched.
#[derive(Resource, Default)]
struct RoomEscaped(HashMap<RoomId, bool>);

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
///
/// A launch is only granted when **every player in the room is locked to the
/// assembly** (has at least one lock weld into an `InLargestAssembly` part) — the
/// server-side twin of the client's launch-button gate, so a mid-flight-unattached
/// rider can't be stranded by a peer's stale/hacked request.
fn handle_launch_requests(
    mut links: Query<
        (Entity, &mut MessageReceiver<RequestLaunch>),
        (With<ClientOf>, With<Connected>),
    >,
    avatars: Query<(&ControlledBy, &RoomMember)>,
    room_avatars: Query<(Entity, &RoomMember), With<ServerAvatar>>,
    lock_joints: Query<&SphericalJoint, With<LockJoint>>,
    assembly: Query<(), With<InLargestAssembly>>,
    mut registry: ResMut<LaunchRegistry>,
    mut fuel: ResMut<RoomFuel>,
    mut policies: ResMut<RoomPolicy>,
) {
    for (link, mut receiver) in &mut links {
        if receiver.receive().count() == 0 {
            continue;
        }
        for (controlled, member) in &avatars {
            if controlled.owner == link {
                let room = member.0;
                let locked_to_assembly = |avatar: Entity| {
                    lock_joints
                        .iter()
                        .any(|joint| joint.body1 == avatar && assembly.get(joint.body2).is_ok())
                };
                // Test hook: BS_ALLOW_UNMANNED waives the everyone-locked launch gate so
                // headless fuel/guidance A/B flights can fly WITHOUT a rider aboard — a
                // rider's weld lands somewhere slightly different every boarding, and that
                // standing trim torque swings a flight's fuel by ±5-10%, drowning the
                // effects under measurement. Unmanned same-save flights are deterministic.
                static ALLOW_UNMANNED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                let all_aboard = *ALLOW_UNMANNED
                    .get_or_init(|| std::env::var("BS_ALLOW_UNMANNED").is_ok())
                    || room_avatars
                        .iter()
                        .filter(|(_, m)| m.0 == room)
                        .all(|(avatar, _)| locked_to_assembly(avatar));
                if !all_aboard {
                    println!(
                        "[launch] room {room:?} request refused — not every player is locked to the assembly"
                    );
                    continue;
                }
                if !registry.by_room.contains_key(&room) {
                    // Fresh countdown armed → start this flight's fuel tally from zero
                    // (a room can be reset and re-launched; the old tally must not carry)
                    // and drop any stale ascent plan so the optimizer re-plans for
                    // whatever the assembly is now.
                    fuel.0.insert(room, 0.0);
                    policies.0.remove(&room);
                }
                registry
                    .by_room
                    .entry(room)
                    .or_insert(RoomLaunch::Counting { remaining: LAUNCH_COUNTDOWN_SECS });
            }
        }
    }
}

/// Advance each room's countdown; at blastoff flip it to `Launched` and cut every joint
/// pinning that room's assembly to the ground. Ground joints are identified
/// **positively** by the `GROUND_JOINT_ID` sentinel their replicated `NetJoint`
/// carries (every server joint goes through `spawn_room_joint`) — not by "endpoint
/// isn't a part", which would silently sever every future non-part joint class at
/// blastoff (it already would have cut player-lock welds, severing riders at the
/// exact moment of liftoff). Part-to-part joints and lock welds stay intact.
fn tick_room_launches(
    time: Res<Time>,
    mut commands: Commands,
    mut registry: ResMut<LaunchRegistry>,
    joints: Query<(Entity, &NetJoint, &RoomMember)>,
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
                && (joint.body1 == GROUND_JOINT_ID || joint.body2 == GROUND_JOINT_ID)
            {
                commands.entity(entity).despawn();
            }
        }
    }
}

/// Mirror each room's launch state onto its orb `NetLaunch` so it replicates to every
/// client in the room (countdown banner + predicted thrust). Rooms with no launch entry
/// report the idle default. `set_if_neq` keeps a settled/idle room quiet.
fn publish_room_launch(
    registry: Res<LaunchRegistry>,
    policies: Res<RoomPolicy>,
    mut orbs: Query<(&RoomStateOf, &mut NetLaunch)>,
) {
    for (orb_room, mut launch) in &mut orbs {
        // The optimizer's chosen ascent angle rides along (0 until the first launched
        // tick computes it) so the predicted client rebuilds the same pitch program.
        let pitchover =
            policies.0.get(&orb_room.0).map(|plan| plan.pitchover).unwrap_or(DEFAULT_PITCHOVER);
        let next = match registry.by_room.get(&orb_room.0) {
            Some(RoomLaunch::Counting { remaining }) => {
                NetLaunch { remaining: remaining.max(0.0), launched: false, pitchover }
            }
            Some(RoomLaunch::Launched) => {
                NetLaunch { remaining: 0.0, launched: true, pitchover }
            }
            None => NetLaunch::default(),
        };
        launch.set_if_neq(next);
    }
}

/// Planet gravity for the authoritative server sim: a per-tick radial correction on
/// every dynamic body (parts + avatars) — gravity points at the planet centre and
/// weakens with altitude (see [`gravity_at`]). The correction rides on top of Avian's
/// unchanged uniform `Gravity`, so it is ~zero at the pad and only bites as an assembly
/// climbs. Each body's TRUE position folds in its room's floating-origin offset
/// ([`RoomFrames`]) so `r` is the real distance from the centre while the co-moving
/// frame keeps the body near the local origin. The client predicts the identical field
/// (`apply_mp_gravity`), so prediction converges.
fn apply_server_gravity(
    frames: Res<RoomFrames>,
    gravity: Res<Gravity>,
    mut bodies: Query<
        (&Position, Option<&PartRoom>, Option<&RoomMember>, Forces),
        Or<(With<NetPart>, With<ServerAvatar>)>,
    >,
) {
    for (position, part_room, member, mut forces) in &mut bodies {
        let room = part_room.map(|r| r.id).or_else(|| member.map(|m| m.0));
        let offset = room.map_or(DVec3::ZERO, |r| frames.get(r).offset);
        apply_gravity_correction(&mut forces, position.0 + offset.as_vec3(), gravity.0);
    }
}

/// Apply balanced rocket thrust to every launched room's assembly rockets each physics
/// tick. Reuses the replicated `InLargestAssembly` membership + `PartRoom` grouping the COM
/// system maintains: computes each launched room's mass-weighted COM + rotational state
/// from its members and the balanced per-rocket forces via the shared
/// `balanced_assembly_thrust` (whose PD stability assist needs the spin measurement).
fn apply_room_rocket_thrust(
    time: Res<Time>,
    registry: Res<LaunchRegistry>,
    mut integrals: ResMut<RoomAttitudeIntegrals>,
    mut fuel: ResMut<RoomFuel>,
    mut policies: ResMut<RoomPolicy>,
    mut escaped: ResMut<RoomEscaped>,
    mut apparent: ResMut<RoomApparentUp>,
    frames: Res<RoomFrames>,
    gravity: Res<Gravity>,
    // Riders' masses, for the ascent plan: every avatar is locked to the assembly at
    // launch (the launch gate guarantees it), so their weight flies with the stack — a
    // plan built from parts-only mass overestimates thrust-to-weight and the real arc
    // diverges from the planned one (recorder-verified: same program, 886 m vs 16 km
    // escape altitude with/without a rider in the mass model).
    riders: Query<
        (&RoomMember, &Position, &LinearVelocity, &ComputedMass),
        (With<ServerAvatar>, Without<RocketEngine>),
    >,
    // `Forces` takes `AngularVelocity` mutably inside (and writes each rocket's
    // `Gimbal` the geometry pass reads), so the member/geometry reads and the force
    // write cannot coexist as sibling queries (B0001) — sequence them.
    mut set: ParamSet<(
        Query<
            (Entity, &Position, &Rotation, &PartRoom, &Gimbal),
            (With<InLargestAssembly>, With<RocketEngine>),
        >,
        Query<
            (&Position, &LinearVelocity, &AngularVelocity, &ComputedMass, &PartRoom),
            With<InLargestAssembly>,
        >,
        Query<(Entity, Forces, &mut Gimbal), With<RocketEngine>>,
    )>,
) {
    // Group launched rooms' member rockets by room.
    let mut per_room: HashMap<RoomId, Vec<(Entity, Vec3, Quat, Vec2)>> = HashMap::new();
    for (entity, position, rotation, room, gimbal) in &set.p0() {
        if registry.is_launched(room.id) {
            per_room
                .entry(room.id)
                .or_default()
                .push((entity, position.0, rotation.0, gimbal.0));
        }
    }
    if per_room.is_empty() {
        apparent.0.clear();
        return;
    }
    // Drop apparent-up entries of rooms that are no longer launched (reset/landed).
    let launched: std::collections::HashSet<RoomId> = per_room.keys().copied().collect();
    apparent.0.retain(|room, _| launched.contains(room));
    escaped.0.retain(|room, _| launched.contains(room));

    // Resolve each room's burn (shared `assembly_burn`, so the client's predicted twin
    // computes the identical trims + gimbal slews). The COM + rotational state come
    // from the shared `measure_assembly_spin`, over the same `ComputedMass`.
    let dt = time.delta_secs();
    let mut burns = Vec::new();
    // Aerodynamic drag on each launched assembly (see the shared
    // `map::apply_assembly_drag` for the physics): `(first rocket, local COM, true COM,
    // true velocity)`, collected here (the member query is borrowed) and applied in the
    // `Forces` pass below, after the thrust so it can't clobber a slewed gimbal.
    let mut drags: Vec<(Entity, Vec3, Vec3, Vec3)> = Vec::new();
    {
        let members = set.p1();
        for (room, rockets) in &per_room {
            let samples = || {
                members
                    .iter()
                    .filter(|(.., r)| r.id == *room)
                    .map(|(position, linear, angular, mass, _)| {
                        (position.0, linear.0, angular.0, mass.value())
                    })
                    // Locked riders fly with the assembly (welded at launch), so their
                    // weight belongs in the COM + inertia the attitude controller balances
                    // about — otherwise thrust is trimmed about the parts-only COM and the
                    // rider's off-centre mass is a standing disturbance the integral must
                    // fight (and, on a small stack with an off-centre rider, can't). The
                    // avatar is rotation-locked, so it contributes mass + linear motion but
                    // no body spin (Vec3::ZERO angular).
                    .chain(riders.iter().filter(|(rm, ..)| rm.0 == *room).map(
                        |(_, position, linear, mass)| {
                            (position.0, linear.0, Vec3::ZERO, mass.value())
                        },
                    ))
            };
            let Some((com, spin)) = measure_assembly_spin(samples) else {
                continue;
            };
            // True (planet-frame) state = local + the room's floating-origin frame, so the
            // guidance reasons about real altitude/velocity even under a rebase.
            let frame = frames.get(*room);
            let true_com = com + frame.offset.as_vec3();
            let true_vel = spin.linear_velocity + frame.velocity;
            // Fuel-optimal ascent plan: built once per launch, on the first launched
            // tick, from this assembly's real thrust-to-weight (`PitchProgram::plan`,
            // the shared constructor all three thrust sites use — a heavy hauler gets a
            // gentle lean, an engine-dense stack flies straight up). The chosen angle
            // replicates via `NetLaunch::pitchover`, so the predicted client rebuilds
            // the identical program — including under `BS_FORCE_PITCHOVER_DEG`, the
            // headless A/B hook that forces the angle here at the seam where it's chosen.
            let plan = policies.0.entry(*room).or_insert_with(|| {
                let total_mass: f32 = members
                    .iter()
                    .filter(|(.., r)| r.id == *room)
                    .map(|(_, _, _, m, _)| m.value())
                    .sum::<f32>()
                    + riders
                        .iter()
                        .filter(|(m, ..)| m.0 == *room)
                        .map(|(.., mass)| mass.value())
                        .sum::<f32>();
                static FORCE: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
                let forced = FORCE
                    .get_or_init(|| {
                        std::env::var("BS_FORCE_PITCHOVER_DEG")
                            .ok()
                            .and_then(|s| s.parse::<f32>().ok())
                    })
                    .map(|deg| deg.to_radians());
                PitchProgram::plan(true_com, true_vel, rockets.len(), gravity.0, total_mass, forced)
            });
            let pitchover = plan.pitchover;
            let guidance =
                program_guidance(true_com, true_vel, plan, escaped.0.entry(*room).or_default());
            static DEBUG_GUIDANCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *DEBUG_GUIDANCE.get_or_init(|| std::env::var("BS_DEBUG_GUIDANCE").is_ok()) {
                use std::sync::atomic::{AtomicU32, Ordering};
                static GT: AtomicU32 = AtomicU32::new(0);
                if GT.fetch_add(1, Ordering::Relaxed) % 30 == 0 {
                    let alt = radial_altitude(true_com);
                    let e = bad_spaceship_shared::guidance::specific_energy(true_com, true_vel);
                    let f = fuel.0.get(room).copied().unwrap_or(0.0);
                    println!(
                        "[guid] alt={:.0}m tvel={:.0} e={:.0} thr={:.1} fuel={:.0} pitch={:.0}deg comY={:.0} offY={:.0} frameV={:.0}",
                        alt, true_vel.length(), e, guidance.throttle, f,
                        pitchover.to_degrees(), com.y, frame.offset.y, frame.velocity.length()
                    );
                }
            }
            let integral = integrals.0.entry(*room).or_default();
            let burn = assembly_burn(com, gravity.0, dt, rockets, &spin, integral, guidance);
            // Tally propellant burned this tick (see `RoomFuel` and `burn_impulse`).
            *fuel.0.entry(*room).or_default() += burn_impulse(&burn, dt);
            // Publish the riders' apparent up (see `RoomApparentUp`); mass matches the
            // client's (parts + riders) so the predicted movement basis agrees.
            let total_mass: f32 = members
                .iter()
                .filter(|(.., r)| r.id == *room)
                .map(|(_, _, _, m, _)| m.value())
                .sum::<f32>()
                + riders
                    .iter()
                    .filter(|(m, ..)| m.0 == *room)
                    .map(|(.., mass)| mass.value())
                    .sum::<f32>();
            if total_mass > 0.0 {
                let net_force: Vec3 = burn.iter().map(|b| b.force).sum();
                apparent.0.insert(*room, apparent_up(net_force, total_mass, true_com));
            }
            // Diagnostics (BS_DEBUG_GIMBAL): once a second, the controller state the
            // flight recorder can't see - body axis vs velocity vs nozzle deflections.
            static DEBUG_GIMBAL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *DEBUG_GIMBAL.get_or_init(|| std::env::var("BS_DEBUG_GIMBAL").is_ok()) {
                use std::sync::atomic::{AtomicU32, Ordering};
                static TICKS: AtomicU32 = AtomicU32::new(0);
                if TICKS.fetch_add(1, Ordering::Relaxed) % 60 == 0 {
                    let axis = rockets[0].2 * Vec3::Y;
                    let v = spin.linear_velocity;
                    let gims: Vec<String> =
                        burn.iter().map(|b| format!("({:+.3},{:+.3})", b.gimbal.x, b.gimbal.y)).collect();
                    println!(
                        "[gimbal] axis=({:+.3},{:+.3}) vlat=({:+.1},{:+.1}) w=({:+.2},{:+.2},{:+.2}) gims={}",
                        axis.x, axis.z, v.x, v.z,
                        spin.angular_velocity.x, spin.angular_velocity.y, spin.angular_velocity.z,
                        gims.join(" ")
                    );
                }
            }
            // Diagnostics (BS_DEBUG_ATTITUDE): the attitude-hold state the recorder can't
            // see — how many riders are in the mass model, the body tilt, the torque the
            // burn actually produces about the (rider-inclusive) COM, and how hard the
            // integral is working. A large standing |net_tau|/|I| = the controller fighting
            // an unmodelled disturbance.
            static DEBUG_ATT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *DEBUG_ATT.get_or_init(|| std::env::var("BS_DEBUG_ATTITUDE").is_ok()) {
                use std::sync::atomic::{AtomicU32, Ordering};
                static AT: AtomicU32 = AtomicU32::new(0);
                if AT.fetch_add(1, Ordering::Relaxed) % 30 == 0 {
                    let n_riders = riders.iter().filter(|(rm, ..)| rm.0 == *room).count();
                    let net_torque: Vec3 = burn.iter().map(|b| (b.point - com).cross(b.force)).sum();
                    let tilt = (rockets[0].2 * Vec3::Y).angle_between(Vec3::Y).to_degrees();
                    println!(
                        "[att] nparts={} nrider={} tilt={:.1} |w|={:.3} |net_tau|={:.2} |I|={:.2}",
                        rockets.len(), n_riders, tilt,
                        spin.angular_velocity.length(),
                        net_torque.length(),
                        integral.length(),
                    );
                }
            }
            burns.extend(burn);
            // Drag on the whole stack, at its COM, charged to the first member rocket.
            if let Some(first) = rockets.first() {
                drags.push((first.0, com, true_com, true_vel));
            }
        }
    }
    let mut rockets = set.p2();
    for burn in burns {
        if let Ok((_, mut forces, mut gimbal)) = rockets.get_mut(burn.entity) {
            gimbal.0 = burn.gimbal;
            forces.apply_force_at_point(burn.force, burn.point);
        }
    }
    for (entity, com, true_com, true_vel) in drags {
        if let Ok((_, mut forces, _)) = rockets.get_mut(entity) {
            apply_assembly_drag(&mut forces, com, true_com, true_vel);
        }
    }
}

/// Feed every avatar's [`FeltUp`] window one sample of this tick's apparent-up
/// direction: its room's launched-assembly plumb line (see [`RoomApparentUp`]), world
/// +Y otherwise. The server half of the felt-up basis — the client sampler consumes the
/// same formula's output from its own predicted burn, so the bases agree without
/// replicating anything.
fn sample_felt_up(
    mut commands: Commands,
    apparent: Res<RoomApparentUp>,
    mut avatars: Query<
        (Entity, &RoomMember, Option<&mut FeltUp>, &mut Rotation, &mut Position, &Collider),
        With<ServerAvatar>,
    >,
) {
    for (entity, member, felt, mut rotation, mut position, collider) in &mut avatars {
        let target = apparent.0.get(&member.0).copied().unwrap_or(Vec3::Y);
        let pivot = capsule_bottom_center(collider);
        drive_felt_up(&mut commands, entity, felt, &mut rotation, &mut position, pivot, target);
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
    frame: SaveFrame,
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

    SaveWorld { parts: save_parts, joints: save_joints, avatars: save_avatars, launched, frame }
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
    frames: Res<RoomFrames>,
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
        let world = snapshot_room(
            room.id,
            launches.is_launched(room.id),
            frames.get(room.id).save(),
            &avatars,
            &parts,
            &joints,
        );
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
    frames: Res<RoomFrames>,
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
        let world = snapshot_room(
            room_id,
            launches.is_launched(room_id),
            frames.get(room_id).save(),
            &snapshot_avatars,
            &parts,
            &joints,
        );
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
    /// Cumulative launch fuel for this room, thrust impulse in N·s (see [`RoomFuel`]) —
    /// so a flight's fuel-to-escape is read straight off the recording.
    fuel_impulse: f32,
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
    fuel: Res<RoomFuel>,
    frames: Res<RoomFrames>,
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

        let world = snapshot_room(
            room.id,
            launches.is_launched(room.id),
            frames.get(room.id).save(),
            &avatars,
            &parts,
            &joints,
        );
        let frame = RecordedFrame {
            tick: *tick,
            unix_ms: save::now_unix_ms(),
            fuel_impulse: fuel.0.get(&room.id).copied().unwrap_or(0.0),
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
    resume: Res<ResumeRegistry>,
) {
    let client = trigger.entity;
    let client_id = client_identity(client, &remote);
    // The persistent resume id rides in the connect token's `user_data` (see the client's
    // `build_netcode_client`). Resolve the remembered position NOW, at connect — before
    // the avatar's body assembles and its first `Position` replicates — so a reconnecting
    // avatar is built directly at its saved spot (`InitialPose`), with no origin→saved
    // ease. READ, don't consume: mobile clients often reconnect twice in quick succession
    // (a `reconnect_dropped` attempt that flaps, then the real one), and removing the
    // record on the first attempt left the second — the one that sticks — a fresh,
    // unlocked avatar. The record stays until `record_resume_positions` overwrites it with
    // the live avatar's pose or the grace window expires.
    let rid = tokens
        .get(client)
        .ok()
        .map(|t| bad_spaceship_shared::net::resume_id_from_user_data(&t.0))
        .unwrap_or(0);
    let resume_pos = (rid != 0)
        .then(|| {
            resume.by_id.get(&rid).copied().and_then(|(pos, room, lock, at)| {
                at.elapsed()
                    .map(|e| e.as_secs() < RESUME_GRACE_SECS)
                    .unwrap_or(false)
                    .then_some((pos, room, lock))
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
        // Predict every avatar on EVERY client (not just the owner). Other players'
        // avatars used to be interpolated, which renders them a fixed delay behind the
        // server; the deck they ride is `PredictionTarget::All` (rendered at the
        // predicted tick, ahead). During a rocket ride the assembly's local vertical
        // velocity climbs to ~100 m/s between rebases, so that interpolation-vs-prediction
        // time gap put a remote rider several metres below the deck — they looked like
        // they were constantly falling through the platform. Predicting all avatars puts
        // every rider on the *same* timeline as the deck (each client simulates the
        // remote body locally as a dynamic capsule that rests on the predicted deck via
        // contact, reconciled by rollback against the server), so they ride together.
        // Replicated `LinearVelocity` makes the input-less remote body coast at its real
        // velocity between snapshots, so walking stays smooth too. No `InterpolationTarget`
        // — with no interpolated avatars, the leaked-`Interpolated`-on-own-avatar bug
        // class (the old jump-lag/twin-avatar hazards) simply can't arise.
        PredictionTarget::to_clients(NetworkTarget::All),
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
        // Runaway guard: consecutive-stale-input counter for `stop_stale_avatars`.
        StaleInputRun::default(),
        // Replicated facing (mirrored from the avatar's `Yaw` by
        // `sync_avatar_facing`) so remote clients can draw it facing its look.
        NetFacing::default(),
        // Replicated move/jump-input flag (mirrored by `sync_avatar_moving`) so
        // remote clients animate it from real input, not world-frame motion.
        NetMoving::default(),
        // Display name — replicated from spawn (empty until `assign_rooms` picks a
        // unique per-room default), so the client never queries a nameless avatar.
        NetName::default(),
    ));
    if let Some((pos, room, lock)) = resume_pos {
        // Optimistically build at the remembered spot (the common case — an iOS
        // reload rejoining the same room — must not slide in from the origin).
        // `assign_rooms` revokes it if the first input reveals a DIFFERENT room:
        // a remembered position means nothing in another room's world.
        avatar.insert((InitialPose(pos), ResumeRoom(room)));
        // A rider who was LOCKED when the session dropped: re-weld it to that deck
        // point once the body + room are ready (`relock_resumed_riders`), so a
        // tab-background mid-flight doesn't leave it standing free on a moving deck.
        if let Some(anchor) = lock {
            avatar.insert(RelockOnResume { anchor, timer: RELOCK_GRACE_SECS });
        }
        println!(
            "[resume] client_id={client_id} reconnect -> spawn at {pos:?} (locked={})",
            lock.is_some()
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A fallen (or diverged) roomed avatar is teleported to a fresh spawn with
    /// zeroed velocity — never despawned (a replicated despawn would recursively
    /// take the client's camera rig down with the predicted avatar). An avatar
    /// still parked at the y = -1000 bootstrap spot (no `RoomMember`) is left alone.
    #[test]
    fn fallen_avatars_respawn_in_place() {
        let mut app = App::new();
        app.init_resource::<RoomFrames>();
        app.add_systems(Update, respawn_fallen_avatars);

        let room = RoomAllocator::default().allocate();
        let fallen = app
            .world_mut()
            .spawn((
                ServerAvatar,
                RoomMember(room),
                Position(Vec3::new(5.0, -31.0, 2.0)),
                LinearVelocity(Vec3::new(0.0, -40.0, 0.0)),
                AngularVelocity(Vec3::ZERO),
            ))
            .id();
        let diverged = app
            .world_mut()
            .spawn((
                ServerAvatar,
                RoomMember(room),
                Position(Vec3::new(f32::NAN, 100.0, 0.0)),
                LinearVelocity(Vec3::ZERO),
                AngularVelocity(Vec3::ZERO),
            ))
            .id();
        let parked = app
            .world_mut()
            .spawn((
                ServerAvatar,
                Position(Vec3::new(0.0, -1000.0, 0.0)),
                LinearVelocity(Vec3::ZERO),
                AngularVelocity(Vec3::ZERO),
            ))
            .id();

        app.update();

        for entity in [fallen, diverged] {
            let pos = app.world().get::<Position>(entity).unwrap().0;
            assert_eq!(pos.y, 0.0, "respawned on the ground plane");
            assert!(pos.length() < 100.0, "respawned near the spawn disc");
            assert_eq!(app.world().get::<LinearVelocity>(entity).unwrap().0, Vec3::ZERO);
        }
        let parked_pos = app.world().get::<Position>(parked).unwrap().0;
        assert_eq!(parked_pos.y, -1000.0, "un-roomed bootstrap avatar untouched");
    }

    /// A part bundle for the rebase tests: a unit-volume cuboid in `room`.
    fn test_part(
        room: RoomId,
        bit: u32,
        grounded: bool,
        pos: Vec3,
        vel: Vec3,
    ) -> impl Bundle {
        (
            NetPart { shape: PartShape::Cuboid { half_extents: [0.5; 3] }, id: 0, seed: 0 },
            PartRoom { id: room, bit },
            Position(pos),
            LinearVelocity(vel),
            CollisionLayers::from_bits(bit, bit | if grounded { GROUND_LAYER } else { 0 }),
        )
    }

    /// When a room's assembly drifts past `REBASE_TRIGGER_M`, the whole room —
    /// assembly, stray pad parts, avatars — is shifted into the assembly's
    /// co-moving frame (positions AND velocities), the frame accumulates the
    /// shift, and the ground bit is dropped (the ground collider at the local
    /// origin is a phantom now).
    #[test]
    fn rebase_shifts_room_into_comoving_frame() {
        let mut app = App::new();
        app.init_resource::<Time>();
        app.init_resource::<RoomFrames>();
        app.add_systems(Update, rebase_room_frames);

        let room = RoomAllocator::default().allocate();
        let bit = 1u32 << 1;
        let climb = Vec3::new(0.0, 120.0, 0.0);
        let a = app
            .world_mut()
            .spawn(test_part(room, bit, true, Vec3::new(0.0, 2499.0, 0.0), climb))
            .id();
        let b = app
            .world_mut()
            .spawn(test_part(room, bit, true, Vec3::new(0.0, 2501.0, 0.0), climb))
            .id();
        app.world_mut().spawn(SphericalJoint::new(a, b));
        // A loose part left behind at the pad — shifted like everything else.
        let stray = app
            .world_mut()
            .spawn(test_part(room, bit, true, Vec3::new(3.0, 1.0, 0.0), Vec3::ZERO))
            .id();
        let rider = app
            .world_mut()
            .spawn((
                ServerAvatar,
                RoomMember(room),
                Position(Vec3::new(0.0, 2502.0, 0.0)),
                LinearVelocity(climb),
                CollisionLayers::from_bits(bit, bit | GROUND_LAYER),
            ))
            .id();
        let orb = app.world_mut().spawn((RoomStateOf(room), NetRoomFrame::default())).id();

        app.update();

        // Assembly COM was (0, 2500, 0) at 120 m/s up → parked at REBASE_REST_Y,
        // co-moving (local velocity zero).
        assert_eq!(app.world().get::<Position>(a).unwrap().0.y, REBASE_REST_Y - 1.0);
        assert_eq!(app.world().get::<Position>(b).unwrap().0.y, REBASE_REST_Y + 1.0);
        assert_eq!(app.world().get::<LinearVelocity>(a).unwrap().0, Vec3::ZERO);
        assert_eq!(app.world().get::<Position>(rider).unwrap().0.y, REBASE_REST_Y + 2.0);
        assert_eq!(app.world().get::<LinearVelocity>(rider).unwrap().0, Vec3::ZERO);
        // The stray pad part rides the same frame: now far below, falling behind.
        assert_eq!(app.world().get::<Position>(stray).unwrap().0.y, 1.0 - 2400.0);
        assert_eq!(app.world().get::<LinearVelocity>(stray).unwrap().0, -climb);
        // Frame bookkeeping: local + frame = true.
        let frame = app.world().get::<NetRoomFrame>(orb).unwrap();
        assert_eq!(frame.offset, [0.0, 2400.0, 0.0]);
        assert_eq!(frame.velocity, climb.to_array());
        // Ground bit dropped while the frame is active.
        let layers = app.world().get::<CollisionLayers>(a).unwrap();
        assert_eq!(*layers, room_layers(bit, false));
    }

    /// A room descending back under `REBASE_RESET_M` true altitude snaps its frame
    /// to exactly zero: coordinates are true again, velocities get their frame
    /// boost back, and the ground bit is restored for the landing.
    #[test]
    fn rebase_resets_to_true_coordinates_near_ground() {
        let mut app = App::new();
        app.init_resource::<Time>();
        app.init_resource::<RoomFrames>();
        app.add_systems(Update, rebase_room_frames);

        let room = RoomAllocator::default().allocate();
        let bit = 1u32 << 1;
        app.world_mut().resource_mut::<RoomFrames>().by_room.insert(
            room,
            RoomFrame {
                offset: DVec3::new(0.0, 500.0, 0.0),
                velocity: Vec3::new(0.0, -50.0, 0.0),
            },
        );
        let sink = Vec3::new(0.0, -60.0, 0.0);
        let a = app
            .world_mut()
            .spawn(test_part(room, bit, false, Vec3::new(0.0, 99.0, 0.0), sink))
            .id();
        let b = app
            .world_mut()
            .spawn(test_part(room, bit, false, Vec3::new(0.0, 101.0, 0.0), sink))
            .id();
        app.world_mut().spawn(SphericalJoint::new(a, b));
        let orb = app.world_mut().spawn((RoomStateOf(room), NetRoomFrame::default())).id();

        app.update();

        // True COM was 600 m up (< reset threshold) → back to true coordinates.
        assert_eq!(app.world().get::<Position>(a).unwrap().0.y, 599.0);
        // True velocity = frame velocity + local = -50 + -60 = -110.
        assert_eq!(app.world().get::<LinearVelocity>(a).unwrap().0.y, -110.0);
        let frame = app.world().get::<NetRoomFrame>(orb).unwrap();
        assert!(!frame.is_active(), "frame landed on exactly zero");
        let layers = app.world().get::<CollisionLayers>(a).unwrap();
        assert_eq!(*layers, room_layers(bit, true));
    }

    /// In a room with an active frame there is no ground and the deck can sit
    /// anywhere in the rebase band, so the avatar fall check is deck-relative and
    /// the respawn point is back aboard the assembly.
    #[test]
    fn fallen_flight_avatars_respawn_on_deck() {
        let mut app = App::new();
        app.init_resource::<RoomFrames>();
        app.add_systems(Update, respawn_fallen_avatars);

        let room = RoomAllocator::default().allocate();
        let bit = 1u32 << 1;
        app.world_mut().resource_mut::<RoomFrames>().by_room.insert(
            room,
            RoomFrame { offset: DVec3::new(0.0, 10_000.0, 0.0), velocity: Vec3::ZERO },
        );
        // Two equal-mass assembly members: COM XZ (2, 0), top y 20 → deck (2, 22, 0).
        for pos in [Vec3::new(0.0, 10.0, 0.0), Vec3::new(4.0, 20.0, 0.0)] {
            app.world_mut().spawn((test_part(room, bit, false, pos, Vec3::ZERO), InLargestAssembly));
        }
        let overboard = app
            .world_mut()
            .spawn((
                ServerAvatar,
                RoomMember(room),
                Position(Vec3::new(9.0, -30.0, 0.0)),
                LinearVelocity(Vec3::new(0.0, -80.0, 0.0)),
                AngularVelocity(Vec3::ZERO),
            ))
            .id();
        // Below the grounded -30 threshold but within the deck margin: in flight
        // that is a rider mid-jump/fall, not a loss — untouched.
        let falling = app
            .world_mut()
            .spawn((
                ServerAvatar,
                RoomMember(room),
                Position(Vec3::new(0.0, -20.0, 0.0)),
                LinearVelocity(Vec3::ZERO),
                AngularVelocity(Vec3::ZERO),
            ))
            .id();

        app.update();

        assert_eq!(
            app.world().get::<Position>(overboard).unwrap().0,
            Vec3::new(2.0, 22.0, 0.0),
            "back aboard the deck"
        );
        assert_eq!(app.world().get::<LinearVelocity>(overboard).unwrap().0, Vec3::ZERO);
        assert_eq!(app.world().get::<Position>(falling).unwrap().0.y, -20.0, "still falling");
    }
}
