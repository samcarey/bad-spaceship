//! Client-side netcode (lightyear) — connects to the dedicated server and
//! renders the players it replicates.
//!
//! Added only when a connect target is configured; otherwise the client is the
//! unchanged single-player game. The target comes from:
//! - **native**: `BS_CONNECT=host:port` → connects over plain `ws://` (no TLS),
//!   for the local loopback slice.
//! - **web/wasm**: `window.__BS_NET__.server` (set by `play.html` from the
//!   `?server=` query param) → a full `wss://host[:port]` URL. The browser owns
//!   TLS, so no certs are configured client-side.
//!
//! For every player the server replicates, draw a cube at its `NetTransform`.

use bad_spaceship_shared::net::{NetPlayer, NetTransform, PlayerInput, ProtocolPlugin, TICK};
use bad_spaceship_shared::{Character, Yaw};
use bevy::prelude::*;
use lightyear::prelude::client::input::InputSystems as ClientInputSystems;
use lightyear::prelude::client::*;
use lightyear::prelude::input::native::{ActionState, InputMarker};
use lightyear::prelude::{Authentication, Controlled, Interpolated};
use std::net::SocketAddr;

/// The server to connect to, or `None` for single-player.
/// Native reads `BS_CONNECT` (e.g. `127.0.0.1:5001`).
#[cfg(not(target_arch = "wasm32"))]
pub fn multiplayer_target() -> Option<String> {
    std::env::var("BS_CONNECT").ok()
}

/// Web reads `window.__BS_NET__.server` (a `wss://…` URL set by play.html from
/// the `?server=` query param). Absent/empty ⇒ single-player.
#[cfg(target_arch = "wasm32")]
pub fn multiplayer_target() -> Option<String> {
    use wasm_bindgen::JsValue;
    let window = web_sys::window()?;
    let bs_net = js_sys::Reflect::get(&window, &JsValue::from_str("__BS_NET__")).ok()?;
    if bs_net.is_undefined() || bs_net.is_null() {
        return None;
    }
    let server = js_sys::Reflect::get(&bs_net, &JsValue::from_str("server")).ok()?;
    let url = server.as_string()?;
    (!url.is_empty()).then_some(url)
}

pub struct NetClientPlugin;

impl Plugin for NetClientPlugin {
    fn build(&self, app: &mut App) {
        // Order matters: plugin group → protocol → spawn the client entity.
        app.add_plugins(ClientPlugins { tick_duration: TICK });
        app.add_plugins(ProtocolPlugin);
        app.add_systems(Startup, connect);
        // Give every replicated player a visible body, then keep its transform
        // in sync with the replicated pose, tag the player we control, and tag
        // our own avatar so we can render it predicted (from the local pose).
        app.add_systems(
            Update,
            (
                draw_replicated_players,
                apply_net_transform,
                mark_controlled_player,
                mark_own_avatar,
                predict_own_avatar,
            ),
        );
        // Forward our character pose each tick, in lightyear's input-writing set.
        app.add_systems(
            FixedPreUpdate,
            write_player_pose.in_set(ClientInputSystems::WriteClientInputs),
        );
    }
}

/// The server binds a player to us via `ControlledBy`; on the client that entity
/// arrives carrying the `Controlled` marker. Tag it with `InputMarker` (and seed
/// its `ActionState`) so our input is written to and sent for that entity.
fn mark_controlled_player(
    mut commands: Commands,
    new: Query<Entity, (With<Controlled>, Without<InputMarker<PlayerInput>>)>,
) {
    for entity in &new {
        commands.entity(entity).insert((
            InputMarker::<PlayerInput>::default(),
            ActionState::<PlayerInput>::default(),
        ));
    }
}

/// Build an avatar render pose from a world translation plus a yaw-derived
/// rotation. The character ball is rotation-locked (its physics rotation is
/// identity); the player's facing is the look `Yaw`. Match the movement basis,
/// which yaws look directions by `-yaw` (see `move_character` in shared), so the
/// avatar's +Z "nose" points where the player looks.
fn avatar_pose(translation: Vec3, yaw: &Yaw) -> Transform {
    Transform {
        translation,
        rotation: Quat::from_rotation_y(-yaw.0),
        ..default()
    }
}

/// Forward our local character's authoritative world pose into the controlled
/// player's `ActionState` (lightyear sends it to the server, which mirrors it
/// into the replicated `NetTransform`). The local character is a single body
/// (the `Character` ball — `Player` and `Character` are the same entity), so its
/// `GlobalTransform` is the player's true position/orientation on every platform.
fn write_player_pose(
    character: Query<(&GlobalTransform, &Yaw), With<Character>>,
    mut controlled: Query<&mut ActionState<PlayerInput>, With<InputMarker<PlayerInput>>>,
) {
    let Some((global, yaw)) = character.iter().next() else {
        return;
    };
    let pose = avatar_pose(global.translation(), yaw);
    for mut state in &mut controlled {
        state.0.translation = pose.translation.to_array();
        state.0.rotation = pose.rotation.to_array();
    }
}

/// Marks our *own* avatar's `Interpolated` entity, so it can be rendered from
/// the local character pose (prediction) rather than the network echo.
#[derive(Component)]
struct OwnAvatar;

/// Tag our own avatar: the `Interpolated` entity whose `NetPlayer.client_id`
/// matches the player we control. (Both the `Controlled`/`Confirmed` entity and
/// its `Interpolated` copy replicate the same `NetPlayer`.)
fn mark_own_avatar(
    mut commands: Commands,
    controlled: Query<&NetPlayer, With<Controlled>>,
    candidates: Query<(Entity, &NetPlayer), (With<Interpolated>, Without<OwnAvatar>)>,
) {
    let Some(mine) = controlled.iter().next() else {
        return;
    };
    for (entity, player) in &candidates {
        if player.client_id == mine.client_id {
            commands.entity(entity).insert(OwnAvatar);
        }
    }
}

/// Render our own avatar from the live local character pose (zero round-trip),
/// overriding the interpolated network echo. Because the client is authoritative
/// over its own pose (it forwards it to the server), this "prediction" is exact —
/// no rollback needed, unlike server-authoritative movement prediction. Reads the
/// character's `Transform` (not `GlobalTransform`, which the engine only refreshes
/// in PostUpdate and would lag a frame): the character is a root entity, so its
/// `Transform` is the current world pose, and the avatar then propagates in lockstep
/// with the rendered character the same frame.
fn predict_own_avatar(
    character: Query<(&Transform, &Yaw), (With<Character>, Without<OwnAvatar>)>,
    mut own: Query<&mut Transform, (With<OwnAvatar>, Without<Character>)>,
) {
    let Some((transform, yaw)) = character.iter().next() else {
        return;
    };
    let pose = avatar_pose(transform.translation, yaw);
    for mut own_transform in &mut own {
        *own_transform = pose;
    }
}

/// Attach a mesh to each player's `Interpolated` copy (the smoothed visual
/// entity) that doesn't have one yet. The raw `Confirmed` entities stay
/// invisible; input/control still rides on them (they carry `Controlled`).
fn draw_replicated_players(
    mut commands: Commands,
    new_players: Query<Entity, (With<NetPlayer>, With<Interpolated>, Without<Mesh3d>)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for entity in &new_players {
        // A small contrasting "nose" on the front (+Z) so the avatar's facing is
        // visible — the body's footprint alone can't show a yaw rotation.
        let nose = commands
            .spawn((
                Mesh3d(meshes.add(Cuboid::new(0.3, 0.3, 0.6))),
                MeshMaterial3d(materials.add(Color::srgb(1.0, 0.85, 0.2))),
                Transform::from_xyz(0.0, 0.0, 0.9),
            ))
            .id();
        commands
            .entity(entity)
            .insert((
                Mesh3d(meshes.add(Cuboid::new(0.8, 1.2, 1.6))),
                MeshMaterial3d(materials.add(Color::srgb(0.9, 0.35, 0.35))),
            ))
            .add_children(&[nose]);
    }
}

/// Apply the (interpolated) replicated pose to the rendered transform. Lightyear
/// eases the `NetTransform` on `Interpolated` entities each frame; mirror it onto
/// the Bevy `Transform` we render.
fn apply_net_transform(
    mut q: Query<
        (&NetTransform, &mut Transform),
        (Changed<NetTransform>, With<Interpolated>, Without<OwnAvatar>),
    >,
) {
    for (net, mut transform) in &mut q {
        *transform = net.to_transform();
    }
}

/// Build the dev netcode client for `server_addr`. Dev auth uses a fixed
/// protocol id + the all-zero key, matching the server's `NetcodeConfig::
/// default()`; production would issue a real ConnectToken from the matchmaker
/// instead of `Manual`.
fn build_netcode_client(server_addr: SocketAddr) -> Option<NetcodeClient> {
    let auth = Authentication::Manual {
        server_addr,
        client_id: rand::random::<u64>(),
        private_key: [0u8; 32],
        protocol_id: 0,
    };
    match NetcodeClient::new(auth, NetcodeConfig::default()) {
        Ok(n) => Some(n),
        Err(e) => {
            error!("failed to build netcode client: {e:?}");
            None
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn connect(mut commands: Commands) {
    let Some(addr_str) = multiplayer_target() else {
        return;
    };
    let server_addr: SocketAddr = match addr_str.parse() {
        Ok(addr) => addr,
        Err(e) => {
            error!("BS_CONNECT ('{addr_str}') is not a host:port address: {e}");
            return;
        }
    };
    let Some(netcode) = build_netcode_client(server_addr) else {
        return;
    };

    let url = format!("ws://{server_addr}");
    let io = WebSocketClientIo::from_url(ClientConfig::builder().with_no_encryption(), url.clone());
    let client = commands.spawn((netcode, io)).id();
    commands.trigger(Connect { entity: client });
    info!("connecting to multiplayer server at {url}");
}

#[cfg(target_arch = "wasm32")]
fn connect(mut commands: Commands) {
    let Some(url) = multiplayer_target() else {
        return;
    };

    // A `wss://` URL points at a hostname, not a SocketAddr; the browser
    // connects via the explicit URL (`from_url`), so the netcode token's
    // `server_addr` is only a logical field — a placeholder is fine.
    let server_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
    let Some(netcode) = build_netcode_client(server_addr) else {
        return;
    };

    // On wasm aeronet's `ClientConfig` is a unit struct (the browser owns TLS).
    let io = WebSocketClientIo::from_url(ClientConfig::default(), url.clone());
    let client = commands.spawn((netcode, io)).id();
    commands.trigger(Connect { entity: client });
    info!("connecting to multiplayer server at {url}");
}
