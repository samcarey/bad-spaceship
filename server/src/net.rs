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

use std::collections::HashMap;
use std::net::SocketAddr;

use avian3d::prelude::{
    CollisionLayers, Collisions, ComputedCenterOfMass, Forces, Gravity, Position, Rotation,
    SphericalJoint,
};
use bad_spaceship_shared::character::{CharacterMovement, ServerAvatar};
use bad_spaceship_shared::net::{
    apply_hold_spring, apply_net_input, focused_part, NetFacing, NetInput, NetJoint, NetPart,
    NetPlayer, ProtocolPlugin, TICK,
};
use bad_spaceship_shared::part::{
    local_contact_anchor, spawn_random_part, SuppressLocalParts, NUM_PARTS,
};
use bad_spaceship_shared::{SuppressLocalPlayer, Yaw};
use bevy::prelude::*;
use lightyear::prelude::input::native::ActionState;
use lightyear::prelude::server::*;
use lightyear::prelude::*;

/// Telemetry: log every connected client's RTT/jitter every ~2s so latency can be
/// tracked from the server log without the player reporting it. lightyear keeps a
/// `PingManager` per client (`ClientOf`); `MinimalPlugins` has no `LogPlugin`, so
/// use `println!` (captured by launchd into the version's `server.log`).
fn log_client_rtt(
    time: Res<Time>,
    mut acc: Local<f32>,
    clients: Query<(Entity, &PingManager), (With<ClientOf>, With<Connected>)>,
) {
    *acc += time.delta_secs();
    if *acc < 2.0 {
        return;
    }
    *acc = 0.0;
    for (entity, ping) in &clients {
        if ping.latency_samples_recv() == 0 {
            continue;
        }
        println!(
            "[rtt] client={} rtt={:.1}ms jitter={:.1}ms samples={}",
            entity.to_bits(),
            ping.rtt().as_secs_f64() * 1000.0,
            ping.jitter().as_secs_f64() * 1000.0,
            ping.latency_samples_recv(),
        );
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
        // The server owns the part world per room, so suppress the shared
        // single-set spawner (`spawn_initial_parts`/`spawn_part`/
        // `replace_fallen_parts` in `PartPlugin`); the per-room spawner below
        // replaces it.
        app.insert_resource(SuppressLocalParts);
        // The server simulates one `ServerAvatar` body per connected client, so
        // suppress the stray local single-player character `CommonPlugins` would spawn.
        app.insert_resource(SuppressLocalPlayer);
        app.add_systems(Startup, start_server);
        // One server-owned, replicated player per client that connects.
        app.add_observer(spawn_player_for_client);
        // Latency telemetry: log each client's RTT/jitter to stdout (-> server.log).
        app.add_systems(Update, log_client_rtt);
        // Refill a room's parts that fall off its platform, and mirror each avatar's
        // look yaw into its replicated facing. Parts and joints replicate their state
        // directly (predicted Avian `Position`/`Rotation`; `NetJoint` data) — nothing
        // to stream per-frame.
        app.add_systems(Update, (replace_fallen_room_parts, sync_avatar_facing));
        // Assign each client (and its avatar) to its reported room on the first
        // input, lazily creating the room's world.
        app.add_systems(Update, assign_rooms);
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
                (server_grab, server_hold, server_attach).chain(),
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
    players: Query<(Entity, &ActionState<NetInput>, &ControlledBy), Without<RoomMember>>,
) {
    for (entity, state, controlled) in &players {
        // Wait for the first real input — the all-zero seed carries no room (a
        // real input always has a unit-quaternion rotation, never `[0,0,0,0]`).
        if state.0 == NetInput::default() {
            continue;
        }
        let (room, is_new) = registry.get_or_create(state.0.room, &mut allocator);
        if is_new {
            spawn_room_world(&mut commands, room);
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
            CollisionLayers::from_bits(room.bit, room.bit | 1),
        ));
        commands.entity(controlled.owner).insert(Rooms::single(room.id));
        info!("client {:?} joined room {:?}", controlled.owner, room.id);
    }
}

/// Spawn a fresh room's world: its own set of parts (replicated + predicted +
/// collision-isolated to the room). Parts replicate immediately — a client that
/// joins mid-fall (or mid-shove) now receives their velocity too, so its predicted
/// copy falls in sync rather than drifting.
fn spawn_room_world(commands: &mut Commands, room: Room) {
    for _ in 0..NUM_PARTS {
        let (entity, half_extents) = spawn_random_part(commands);
        tag_room_part(commands, entity, half_extents, room);
    }
}

/// Tag a freshly-spawned part for room-scoped replication: its shape + stable id
/// via `NetPart`, its pose via the predicted Avian `Position`/`Rotation`,
/// replicated + predicted, scoped to the room's `Rooms`, and isolated to the
/// room's collision layer (it collides only with same-room parts and the ground —
/// default bit 0).
fn tag_room_part(commands: &mut Commands, entity: Entity, half_extents: Vec3, room: Room) {
    commands.entity(entity).insert((
        // `id` is the part's stable cross-network identity (this entity's bits), so
        // a replicated `NetJoint` can name its two endpoints and the client can find
        // the matching *predicted* parts to joint locally.
        NetPart { half_extents: half_extents.to_array(), id: entity.to_bits() },
        Replicate::to_clients(NetworkTarget::All),
        // Predict the loose blocks on every client in the room: each client
        // simulates them locally (so shoving one is instant) and rollback reconciles
        // against the server's authoritative Avian `Position`/`Rotation` (which ride
        // on the predicted components registered in `ProtocolPlugin`).
        PredictionTarget::to_clients(NetworkTarget::All),
        Rooms::single(room.id),
        PartRoom { id: room.id, bit: room.bit },
        CollisionLayers::from_bits(room.bit, room.bit | 1),
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

/// On the attach intent, joint the held part to whatever (other) part it's
/// touching, at the contact anchors — then release it (it's now part of the
/// assembly). Cross-room parts can't touch (collision layers isolate rooms), so
/// the join is room-scoped automatically. Ports single-player's
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
    mut players: Query<(&ActionState<NetInput>, &HeldPart, &RoomMember, &mut AttachState)>,
) {
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
        let mut attached = false;
        for pair in collisions.collisions_with(held_entity) {
            if !pair.is_touching() {
                continue;
            }
            let (c1, c2) = (pair.collider1, pair.collider2);
            // Only attach to another replicated part (not the ground/character).
            let other = if c1 == held_entity { c2 } else { c1 };
            if net_parts.get(other).is_err() {
                continue;
            }
            let rot = |e| rotations.get(e).map(|r| r.0).unwrap_or(Quat::IDENTITY);
            let com = |e| coms.get(e).map(|c| c.0).unwrap_or(Vec3::ZERO);
            for manifold in &pair.manifolds {
                for contact in &manifold.points {
                    let p1 = local_contact_anchor(rot(c1), com(c1), contact.anchor1);
                    let p2 = local_contact_anchor(rot(c2), com(c2), contact.anchor2);
                    commands.spawn((
                        // The server's authoritative joint (body1=c2, body2=c1).
                        SphericalJoint::new(c2, c1)
                            .with_local_anchor1(p2)
                            .with_local_anchor2(p1),
                        // Replicate the joint's data (endpoints by stable id +
                        // anchors, matching the SphericalJoint above) so each client
                        // can rebuild it as real predicted physics between its
                        // predicted parts — and draw it — scoped to the holder's room.
                        NetJoint {
                            body1: c2.to_bits(),
                            body2: c1.to_bits(),
                            anchor1: p2.to_array(),
                            anchor2: p1.to_array(),
                        },
                        Replicate::to_clients(NetworkTarget::All),
                        Rooms::single(member.0),
                    ));
                    attached = true;
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

/// Replace a room's parts that have fallen off its platform, keeping each room
/// stocked (the server's per-room equivalent of single-player's
/// `replace_fallen_parts`, which is suppressed here). The replacement re-joins
/// the same room and collision layer.
fn replace_fallen_room_parts(
    mut commands: Commands,
    parts: Query<(Entity, &Transform, &PartRoom), With<NetPart>>,
) {
    for (entity, transform, part_room) in &parts {
        if transform.translation.y < -10.0 {
            commands.entity(entity).despawn();
            let (new_entity, half_extents) = spawn_random_part(&mut commands);
            tag_room_part(
                &mut commands,
                new_entity,
                half_extents,
                Room { id: part_room.id, bit: part_room.bit },
            );
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
) {
    let client = trigger.entity;
    // The owning client's peer id: it predicts its own avatar; everyone else
    // interpolates it. (Predicting a remote player is impossible without its input.)
    let owner = remote.get(client).map(|r| r.0).unwrap_or(PeerId::Server);
    commands.spawn((
        NetPlayer { client_id: client_identity(client, &remote) },
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
        // Replicated facing (mirrored from the avatar's `Yaw` by
        // `sync_avatar_facing`) so remote clients can draw it facing its look.
        NetFacing::default(),
    ));
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


