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

use core::time::Duration;

use bad_spaceship_shared::net::{NetPlayer, NetTransform, PlayerInput, ProtocolPlugin};
use bad_spaceship_shared::{Character, Yaw};
use bevy::prelude::*;
use lightyear::prelude::client::input::InputSystems as ClientInputSystems;
use lightyear::prelude::client::*;
use lightyear::prelude::input::native::{ActionState, InputMarker};
use lightyear::prelude::Controlled;

/// 60 Hz, matching the server tick.
const TICK: Duration = Duration::from_millis(1000 / 60);

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
        // in sync with the replicated pose, and tag the player we control.
        app.add_systems(
            Update,
            (draw_replicated_players, apply_net_transform, mark_controlled_player),
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
    // The character ball is rotation-locked (its physics rotation is identity);
    // the player's facing is the look `Yaw`. Match the movement basis, which
    // yaws look directions by `-yaw` (see `move_character` in shared), so the
    // avatar's +Z "nose" points where the player looks.
    let translation = global.translation();
    let rotation = Quat::from_rotation_y(-yaw.0);
    for mut state in &mut controlled {
        state.0.translation = translation.to_array();
        state.0.rotation = rotation.to_array();
    }
}

/// Attach a mesh to any replicated player that doesn't have one yet.
fn draw_replicated_players(
    mut commands: Commands,
    new_players: Query<Entity, (With<NetPlayer>, Without<Mesh3d>)>,
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

/// Apply the replicated pose to the rendered transform.
fn apply_net_transform(mut q: Query<(&NetTransform, &mut Transform), Changed<NetTransform>>) {
    for (net, mut transform) in &mut q {
        *transform = net.to_transform();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn connect(mut commands: Commands) {
    use lightyear::prelude::Authentication;
    use std::net::SocketAddr;

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

    // Dev auth: a fixed protocol id + zero key, matching the server's
    // `NetcodeConfig::default()`. Production issues a real ConnectToken from the
    // matchmaker instead of `Manual`.
    let auth = Authentication::Manual {
        server_addr,
        client_id: rand::random::<u64>(),
        // The netcode private key (`Key = [u8; 32]`). Dev/loopback uses the
        // all-zero key, matching the server's `NetcodeConfig::default()`.
        private_key: [0u8; 32],
        protocol_id: 0,
    };
    let netcode = match NetcodeClient::new(auth, NetcodeConfig::default()) {
        Ok(n) => n,
        Err(e) => {
            error!("failed to build netcode client: {e:?}");
            return;
        }
    };

    let url = format!("ws://{server_addr}");
    let io = WebSocketClientIo::from_url(ClientConfig::builder().with_no_encryption(), url.clone());
    let client = commands.spawn((netcode, io)).id();
    commands.trigger(Connect { entity: client });
    info!("connecting to multiplayer server at {url}");
}

#[cfg(target_arch = "wasm32")]
fn connect(mut commands: Commands) {
    use lightyear::prelude::Authentication;
    use std::net::SocketAddr;

    let Some(url) = multiplayer_target() else {
        return;
    };

    // A `wss://` URL points at a hostname, not a SocketAddr; the browser
    // connects via the explicit URL (`from_url`), so the netcode token's
    // `server_addr` is only a logical field — a placeholder is fine.
    let server_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
    let auth = Authentication::Manual {
        server_addr,
        client_id: rand::random::<u64>(),
        private_key: [0u8; 32],
        protocol_id: 0,
    };
    let netcode = match NetcodeClient::new(auth, NetcodeConfig::default()) {
        Ok(n) => n,
        Err(e) => {
            error!("failed to build netcode client: {e:?}");
            return;
        }
    };

    // On wasm aeronet's `ClientConfig` is a unit struct (the browser owns TLS).
    let io = WebSocketClientIo::from_url(ClientConfig::default(), url.clone());
    let client = commands.spawn((netcode, io)).id();
    commands.trigger(Connect { entity: client });
    info!("connecting to multiplayer server at {url}");
}
