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

use avian3d::prelude::{
    Collider, Collisions, ComputedCenterOfMass, Forces, Gravity, ReadRigidBodyForces, SphericalJoint,
    WriteRigidBodyForces,
};
use bad_spaceship_shared::net::{
    focused_part, hold_acceleration, orient_acceleration, NetJoint, NetPart, NetPlayer,
    NetTransform, PlayerInput, ProtocolPlugin, TICK,
};
use bad_spaceship_shared::part::Holdable;
use bad_spaceship_shared::utils::QuatExt;
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
        // Replicate the authoritative shared part world: tag new parts for
        // replication, then stream their (and the joints') poses each frame.
        app.add_systems(
            Update,
            (replicate_parts, sync_part_transforms, sync_joint_transforms),
        );
        // Apply each client's replicated input to its player (FixedUpdate, the
        // sim tick). Grab/hold run in Update where Avian's `Forces` helper
        // accumulates (matching the single-player `position_held_part`).
        app.add_systems(FixedUpdate, apply_player_input);
        app.add_systems(Update, (server_grab, server_hold, server_attach).chain());
    }
}

/// The part a networked player is currently holding (server-authoritative).
#[derive(Component, Default)]
struct HeldPart(Option<Entity>);

/// Resolve each player's grab intent: on grab, latch the part the player is most
/// directly looking at (within focus range/angle) — the same selection as
/// single-player, cast from the forwarded orbit-center origin along the ray to
/// the hold target. On release, let go. The part stays dynamic throughout.
fn server_grab(
    mut players: Query<(&ActionState<PlayerInput>, &mut HeldPart)>,
    parts: Query<(Entity, &Transform), With<NetPart>>,
) {
    for (state, mut held) in &mut players {
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
            parts.iter().map(|(entity, t)| (entity, t.translation)),
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

/// On the attach intent, joint the held part to whatever (other) replicated part
/// it's touching, at the contact anchors — then release it (it's now part of the
/// assembly). Ports single-player's `update_active_joints`/`attach`: Avian's
/// contact anchors are world-space, COM-relative, so recover each body-local
/// anchor with `rot⁻¹ · anchor + com`. Joints are server physics, so the joined
/// parts move together and their replicated poses tell the story (no joint
/// replication needed).
fn server_attach(
    mut commands: Commands,
    collisions: Collisions,
    transforms: Query<&Transform>,
    coms: Query<&ComputedCenterOfMass>,
    net_parts: Query<(), With<NetPart>>,
    mut players: Query<(&ActionState<PlayerInput>, &mut HeldPart)>,
) {
    for (state, mut held) in &mut players {
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
                        // Replicate a marker at the joint so clients can draw it.
                        NetJoint,
                        NetTransform::default(),
                        Replicate::to_clients(NetworkTarget::All),
                        InterpolationTarget::to_clients(NetworkTarget::All),
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

/// Tag each newly-spawned part (a `Holdable` body with a cuboid collider) for
/// replication: its shape via `NetPart`, its pose via `NetTransform`, replicated
/// and interpolated to all clients.
fn replicate_parts(
    mut commands: Commands,
    new_parts: Query<(Entity, &Collider), (With<Holdable>, Without<NetPart>)>,
) {
    for (entity, collider) in &new_parts {
        let Some(cuboid) = collider.shape().as_cuboid() else {
            continue;
        };
        let half = cuboid.half_extents;
        commands.entity(entity).insert((
            NetPart { half_extents: [half[0], half[1], half[2]] },
            NetTransform::default(),
            Replicate::to_clients(NetworkTarget::All),
            InterpolationTarget::to_clients(NetworkTarget::All),
        ));
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
        HeldPart::default(),
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
