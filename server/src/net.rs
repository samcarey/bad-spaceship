//! Server-side netcode (lightyear) — the dedicated authoritative host.
//!
//! Added only in multiplayer mode (the `BS_MULTIPLAYER` env var); single-player
//! headless runs are byte-identical to before.
//!
//! **Per-room world isolation.** One server process hosts many lobby rooms, kept
//! separate via lightyear's room visibility (`Rooms`): an entity is only
//! replicated to clients sharing a room with it. Each client reports its lobby
//! code (carried on `PlayerInput`); the server maps the code to a `RoomId` and a
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
    CollisionLayers, Collisions, ComputedCenterOfMass, Forces, Gravity, ReadRigidBodyForces,
    SphericalJoint, WriteRigidBodyForces,
};
use bad_spaceship_shared::net::{
    focused_part, hold_acceleration, orient_acceleration, NetJoint, NetPart, NetPlayer,
    NetTransform, PlayerInput, ProtocolPlugin, TICK,
};
use bad_spaceship_shared::part::{spawn_random_part, SuppressLocalParts, NUM_PARTS};
use bad_spaceship_shared::utils::QuatExt;
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
        // Room-based interest management: scope replication so a client only sees
        // entities sharing one of its rooms.
        app.add_plugins(RoomPlugin);
        app.init_resource::<RoomRegistry>();
        // The server owns the part world per room, so suppress the shared
        // single-set spawner (`spawn_initial_parts`/`spawn_part`/
        // `replace_fallen_parts` in `PartPlugin`); the per-room spawner below
        // replaces it.
        app.insert_resource(SuppressLocalParts);
        app.add_systems(Startup, start_server);
        // One server-owned, replicated player per client that connects.
        app.add_observer(spawn_player_for_client);
        // Latency telemetry: log each client's RTT/jitter to stdout (-> server.log).
        app.add_systems(Update, log_client_rtt);
        // Stream the authoritative part/joint poses each frame, and refill a
        // room's parts that fall off its platform.
        app.add_systems(
            Update,
            (sync_part_transforms, sync_joint_transforms, replace_fallen_room_parts),
        );
        // Assign each client (and its avatar) to its reported room on the first
        // input, lazily creating the room's world.
        app.add_systems(Update, assign_rooms);
        // Apply each client's replicated input to its player (FixedUpdate, the
        // sim tick). Grab/hold run in Update where Avian's `Forces` helper
        // accumulates (matching the single-player `position_held_part`).
        app.add_systems(FixedUpdate, apply_player_input);
        app.add_systems(Update, (server_grab, server_hold, server_attach).chain());
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

/// Assign each connected client (and its avatar) to the room it reported, the
/// first time a real input arrives. Lazily creates the room's world (parts) on
/// first sighting of a code. Until assigned, the avatar carries no
/// `Rooms` filter, so it's visible to its own client (which bootstraps the
/// input/control loop) — the assignment then scopes it.
fn assign_rooms(
    mut commands: Commands,
    mut registry: ResMut<RoomRegistry>,
    mut allocator: ResMut<RoomAllocator>,
    players: Query<(Entity, &ActionState<PlayerInput>, &ControlledBy), Without<RoomMember>>,
) {
    for (entity, state, controlled) in &players {
        // Wait for the first real input — the all-zero seed carries no room (a
        // real input always has a unit-quaternion rotation, never `[0,0,0,0]`).
        if state.0 == PlayerInput::default() {
            continue;
        }
        let (room, is_new) = registry.get_or_create(state.0.room, &mut allocator);
        if is_new {
            spawn_room_world(&mut commands, room);
        }
        // Scope this avatar and this client to the room (`Rooms` is immutable, so
        // `insert` replaces any prior membership).
        commands
            .entity(entity)
            .insert((Rooms::single(room.id), RoomMember(room.id)));
        commands.entity(controlled.owner).insert(Rooms::single(room.id));
        info!("client {:?} joined room {:?}", controlled.owner, room.id);
    }
}

/// Spawn a fresh room's world: its own set of parts (replicated + interpolated +
/// collision-isolated to the room).
fn spawn_room_world(commands: &mut Commands, room: Room) {
    for _ in 0..NUM_PARTS {
        let (entity, half_extents) = spawn_random_part(commands);
        tag_room_part(commands, entity, half_extents, room);
    }
}

/// Tag a freshly-spawned part for room-scoped replication: its shape via
/// `NetPart`, its pose via `NetTransform`, replicated + interpolated, scoped to
/// the room's `Rooms`, and isolated to the room's collision layer (it collides
/// only with same-room parts and the ground — default bit 0).
fn tag_room_part(commands: &mut Commands, entity: Entity, half_extents: Vec3, room: Room) {
    commands.entity(entity).insert((
        NetPart { half_extents: half_extents.to_array() },
        NetTransform::default(),
        Replicate::to_clients(NetworkTarget::All),
        InterpolationTarget::to_clients(NetworkTarget::All),
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
    mut players: Query<(&ActionState<PlayerInput>, &mut HeldPart, &RoomMember)>,
    parts: Query<(Entity, &Transform, &PartRoom), With<NetPart>>,
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
                .map(|(entity, t, _)| (entity, t.translation)),
        );
    }
}

/// Float each held part to its holder's hold point (critically-damped
/// anti-gravity force, matching `position_held_part`) AND orient it toward the
/// hold rotation (critically-damped angular acceleration, matching
/// `orient_held_part`). The part stays dynamic, so it still collides. Both forces
/// go through the one `Forces` accessor to avoid an ambiguous double-write.
fn server_hold(
    players: Query<(&ActionState<PlayerInput>, &HeldPart)>,
    mut parts: Query<(&Transform, Forces), With<NetPart>>,
    gravity: Res<Gravity>,
) {
    for (state, held) in &players {
        let Some(part_entity) = held.0 else {
            continue;
        };
        let Ok((transform, mut forces)) = parts.get_mut(part_entity) else {
            continue;
        };
        // Position.
        let displacement = Vec3::from_array(state.0.hold_target) - transform.translation;
        let lin_vel = forces.linear_velocity();
        forces.apply_linear_acceleration(hold_acceleration(displacement, lin_vel) - gravity.0);
        // Orientation: drive toward the target rotation (the client tracks it,
        // starting at the part's pickup orientation and folding in the rotate
        // gesture). Skip the unsent seed (the all-zero default quat is never a
        // real rotation). `to_rotation_vector` takes the shortest-path error,
        // exactly as the single-player `orient_held_part`.
        let target = Quat::from_array(state.0.hold_rotation);
        if target.length_squared() > 0.5 {
            let error = (target * transform.rotation.conjugate()).to_rotation_vector();
            let ang_vel = forces.angular_velocity();
            forces.apply_angular_acceleration(orient_acceleration(error, ang_vel));
        }
    }
}

/// On the attach intent, joint the held part to whatever (other) part it's
/// touching, at the contact anchors — then release it (it's now part of the
/// assembly). Cross-room parts can't touch (collision layers isolate rooms), so
/// the join is room-scoped automatically. Ports single-player's
/// `update_active_joints`/`attach`: Avian's contact anchors are world-space,
/// COM-relative, so recover each body-local anchor with `rot⁻¹ · anchor + com`.
/// Joints are server physics, so the joined parts move together and their
/// replicated poses tell the story (no joint replication needed).
fn server_attach(
    mut commands: Commands,
    collisions: Collisions,
    transforms: Query<&Transform>,
    coms: Query<&ComputedCenterOfMass>,
    net_parts: Query<(), With<NetPart>>,
    mut players: Query<(&ActionState<PlayerInput>, &mut HeldPart, &RoomMember)>,
) {
    for (state, mut held, member) in &mut players {
        if !state.0.attach {
            continue;
        }
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
            let rot = |e| transforms.get(e).map(|t| t.rotation).unwrap_or(Quat::IDENTITY);
            let com = |e| coms.get(e).map(|c| c.0).unwrap_or(Vec3::ZERO);
            for manifold in &pair.manifolds {
                for contact in &manifold.points {
                    let p1 = rot(c1).inverse() * contact.anchor1 + com(c1);
                    let p2 = rot(c2).inverse() * contact.anchor2 + com(c2);
                    commands.spawn((
                        SphericalJoint::new(c2, c1)
                            .with_local_anchor1(p2)
                            .with_local_anchor2(p1),
                        // Replicate a marker at the joint so clients can draw it,
                        // scoped to the holder's room.
                        NetJoint,
                        NetTransform::default(),
                        Replicate::to_clients(NetworkTarget::All),
                        InterpolationTarget::to_clients(NetworkTarget::All),
                        Rooms::single(member.0),
                    ));
                    attached = true;
                }
            }
        }
        if attached {
            held.0 = None;
        }
    }
}

/// Mirror each replicated part's authoritative physics pose into its
/// `NetTransform` so the change replicates to clients. Only writes on an actual
/// change, so settled (motionless) parts stop generating replication traffic.
fn sync_part_transforms(mut parts: Query<(&Transform, &mut NetTransform), With<NetPart>>) {
    for (transform, mut net) in &mut parts {
        let updated = NetTransform::from_transform(transform);
        if *net != updated {
            *net = updated;
        }
    }
}

/// Stream each replicated joint's world anchor point into its `NetTransform`
/// (body1's transform applied to local anchor1), so the client's joint marker
/// tracks the moving assembly.
fn sync_joint_transforms(
    mut joints: Query<(&SphericalJoint, &mut NetTransform), With<NetJoint>>,
    bodies: Query<&Transform, Without<NetJoint>>,
) {
    for (joint, mut net) in &mut joints {
        let (Ok(body), Some(anchor)) = (bodies.get(joint.body1), joint.local_anchor1()) else {
            continue;
        };
        let world = body.translation + body.rotation * anchor;
        let updated = NetTransform::from_transform(&Transform::from_translation(world));
        if *net != updated {
            *net = updated;
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
/// pose is a placeholder — the client's first `PlayerInput` (its real character
/// pose) overwrites it within a tick. The avatar starts with no `Rooms` filter so
/// it's visible to its own client (bootstrapping the input/control loop);
/// `assign_rooms` scopes it once the first input reveals the room.
fn spawn_player_for_client(
    trigger: On<Add, Connected>,
    mut commands: Commands,
    remote: Query<&RemoteId>,
) {
    let client = trigger.entity;
    commands.spawn((
        NetPlayer { client_id: client_identity(client, &remote) },
        // Parked far underground until the first input: while unassigned the
        // avatar carries no `Rooms` filter, so it's briefly visible to every
        // client (replicon shows filter-less entities by default); keeping it
        // off-screen hides that ~1-RTT blip from other rooms. The owner never
        // sees the placeholder (prediction overrides its own avatar's pose).
        NetTransform::from_transform(&Transform::from_xyz(0.0, -1000.0, 0.0)),
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
        HeldPart::default(),
    ));
    info!("client {client:?} connected — spawned replicated player");
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
