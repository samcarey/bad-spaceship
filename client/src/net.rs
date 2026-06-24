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

use bad_spaceship_shared::net::{NetPlayer, NetTransform, ProtocolPlugin};
use bevy::prelude::*;
use lightyear::prelude::client::*;

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
        // in sync with the replicated pose.
        app.add_systems(Update, (draw_replicated_players, apply_net_transform));
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
        commands.entity(entity).insert((
            Mesh3d(meshes.add(Cuboid::new(1.0, 2.0, 1.0))),
            MeshMaterial3d(materials.add(Color::srgb(0.9, 0.35, 0.35))),
        ));
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
