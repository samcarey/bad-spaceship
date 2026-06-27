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

use avian3d::prelude::{Collider, Position, RigidBody};
use bad_spaceship_shared::character::{
    insert_character_body, CharacterMovement, Config as CharacterConfig,
};
use bad_spaceship_shared::net::{
    apply_net_input, focused_part, NetInput, NetJoint, NetPart, NetPlayer, NetTransform,
    ProtocolPlugin, TICK,
};
use bad_spaceship_shared::part::{Holdable, SuppressLocalParts};
use bad_spaceship_shared::player::make_local_player;
use crate::render_secondary_pass::JointAppearance;
use bad_spaceship_shared::{
    CameraOrbitCenter, Character, DirectionalInput, FocusedInteractable, HoldPoint, Holding,
    InputEvents, LookPitch, Modifying, PartRotation, Player, PlayerClick, SuppressLocalPlayer, Yaw,
};
use bevy::prelude::*;
use lightyear::prelude::client::input::InputSystems as ClientInputSystems;
use lightyear::prelude::client::*;
use lightyear::prelude::input::native::{ActionState, InputMarker};
use lightyear::prelude::{Authentication, Interpolated, Predicted, PredictionManager};
use std::net::SocketAddr;

/// The lobby room this client is in, forwarded to the server (which scopes our
/// replicated world to it). Constant for the session.
#[derive(Resource)]
struct MyRoom([u8; 6]);

/// Pack a lobby code into the fixed 6-byte field the server keys rooms by: the
/// matchmaker hands out 6 uppercase chars, so uppercase, take up to 6 bytes,
/// zero-pad. An empty code (no room / native loopback) maps to all-zero — the
/// shared default room, so a roomless connect still works.
fn room_code_bytes(code: &str) -> [u8; 6] {
    let mut out = [0u8; 6];
    for (slot, byte) in out.iter_mut().zip(code.to_ascii_uppercase().bytes()) {
        *slot = byte;
    }
    out
}

/// The room code to report. Native reads `BS_ROOM`; wasm reads
/// `window.__BS_NET__.room` (set by `play.html` from the `?room=` query param).
#[cfg(not(target_arch = "wasm32"))]
fn multiplayer_room() -> [u8; 6] {
    room_code_bytes(std::env::var("BS_ROOM").ok().as_deref().unwrap_or(""))
}

/// See the native counterpart. Absent/empty ⇒ all-zero (default room).
#[cfg(target_arch = "wasm32")]
fn multiplayer_room() -> [u8; 6] {
    use wasm_bindgen::JsValue;
    let code = (|| {
        let window = web_sys::window()?;
        let bs_net = js_sys::Reflect::get(&window, &JsValue::from_str("__BS_NET__")).ok()?;
        if bs_net.is_undefined() || bs_net.is_null() {
            return None;
        }
        let room = js_sys::Reflect::get(&bs_net, &JsValue::from_str("room")).ok()?;
        room.as_string()
    })()
    .unwrap_or_default();
    room_code_bytes(&code)
}

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
        // Owns the Avian `Position`↔`Transform` sync (its sub-plugins are disabled
        // in multiplayer by `add_physics`), frame interpolation, and client-side
        // prediction rollback for replicated Avian bodies. State replication, so no
        // `rollback_resources`.
        app.add_plugins(lightyear_avian3d::prelude::LightyearAvianPlugin {
            replication_mode: lightyear_avian3d::plugin::AvianReplicationMode::Position,
            update_syncs_manually: false,
            rollback_resources: false,
        });
        // In multiplayer the parts are server-authoritative: suppress the local
        // part sim and render the server's replicated parts instead.
        app.insert_resource(SuppressLocalParts);
        // The local character is the *predicted networked avatar* (assembled by
        // `setup_predicted_avatar` on the lightyear `Predicted` entity), not a
        // separate single-player character — suppress the latter so there's exactly
        // one character on the client.
        app.insert_resource(SuppressLocalPlayer);
        // The lobby room this client is in, forwarded to the server so it scopes
        // our world. Read once at plugin build (after `play.html` has populated
        // `window.__BS_NET__`); constant for the session.
        app.insert_resource(MyRoom(multiplayer_room()));
        app.init_resource::<WantHold>();
        app.init_resource::<WantAttach>();
        app.init_resource::<HeldRotation>();
        app.add_systems(Startup, connect);
        // Recover from a dropped link (e.g. a suspended/backgrounded tab) by
        // reconnecting when the tab returns to the foreground.
        app.add_systems(Update, reconnect_dropped);
        // Toggle the grab intent on each (non-modifier) click; sent in NetInput.
        // After `InputEvents` (mobile `apply_pointer` / desktop `get_modifying`)
        // so `Modifying` is current when classifying a click as grab vs attach.
        app.add_systems(Update, read_grab_intent.after(InputEvents));
        // Assemble our predicted avatar into the controllable character, give every
        // *other* replicated player a visible body, keep parts/joints in sync with
        // their replicated pose.
        app.add_systems(
            Update,
            (
                setup_predicted_avatar,
                draw_replicated_players,
                draw_replicated_parts,
                draw_replicated_joints,
                apply_net_transform,
                // Mirror the networked grab into local `Holding`/`FocusedInteractable`
                // (after the intent is read), then track the held part's target
                // orientation, then highlight it — in that order so each reads the
                // previous one's freshly-written state this frame.
                (mirror_grab_state, track_hold_rotation, highlight_grabbable)
                    .chain()
                    .after(read_grab_intent),
            ),
        );
        // Forward our input intent each tick, in lightyear's input-writing set.
        app.add_systems(
            FixedPreUpdate,
            write_input.in_set(ClientInputSystems::WriteClientInputs),
        );
        // Drive the *predicted* avatar from the buffered input intent each sim tick —
        // the same bridge the server runs — so local prediction and rollback replay
        // use exactly the inputs the server will. Before `CharacterMovement` reads them.
        app.add_systems(FixedUpdate, apply_net_input.before(CharacterMovement));
    }
}

/// Turn our *predicted* networked avatar into the controllable local character.
/// lightyear creates a `Predicted` entity for the avatar the server marks
/// `PredictionTarget` to us (only our own — predicting a remote player is
/// impossible without its input), and rolls back its Avian `Position`/`Rotation`.
/// We give that entity the real character body (`insert_character_body`) so Avian
/// simulates it locally with zero input delay, plus the player/input state
/// (`make_local_player`) and the networked-input marker so `write_input` fills its
/// `ActionState` and lightyear sends it. From there it's an ordinary `Character`:
/// `assign_characters` renders it and `attach_camera_orbit` mounts the camera —
/// the same path single-player uses.
///
/// Gated on `Position` so we assemble the body only once the avatar's real spawn
/// pose has arrived (rather than briefly at the origin). In this phase the avatar
/// is the only predicted entity, so `With<Predicted>` identifies it; predicted
/// loose blocks (a later phase) will need a marker to disambiguate.
fn setup_predicted_avatar(
    mut commands: Commands,
    new: Query<Entity, (With<Predicted>, With<Position>, Without<Character>)>,
    configs: Res<Assets<CharacterConfig>>,
) {
    let Some((_, config)) = configs.iter().next() else {
        return;
    };
    for entity in &new {
        let mut e = commands.entity(entity);
        insert_character_body(&mut e, config.size());
        make_local_player(&mut e);
        e.insert((
            InputMarker::<NetInput>::default(),
            ActionState::<NetInput>::default(),
        ));
    }
}

/// Forward our per-tick input *intent* (move/jump/look) into the controlled
/// avatar's `ActionState`; lightyear sends it and the server simulates our
/// character authoritatively from it. We read the local character's combined
/// `DirectionalInput` — the same intent that drives our local (predicted)
/// character — plus `Yaw`/`LookPitch`. The grab-ray fields are still forwarded
/// from the local camera entities for now (the server's grab uses them directly);
/// a later phase reconstructs the ray from the simulated character + look angles.
fn write_input(
    character: Query<(&DirectionalInput, &Yaw, &LookPitch), With<Character>>,
    orbit: Query<&GlobalTransform, With<CameraOrbitCenter>>,
    hold: Query<&GlobalTransform, With<HoldPoint>>,
    want_hold: Res<WantHold>,
    mut want_attach: ResMut<WantAttach>,
    held_rotation: Res<HeldRotation>,
    my_room: Res<MyRoom>,
    mut controlled: Query<&mut ActionState<NetInput>, With<InputMarker<NetInput>>>,
) {
    let Some((dir, yaw, pitch)) = character.iter().next() else {
        return;
    };
    let grab_origin = orbit.iter().next().map(|g| g.translation());
    let hold_pos = hold.iter().next().map(|g| g.translation());
    let attach = want_attach.0;
    for mut state in &mut controlled {
        // DirectionalInput: x = strafe, y = jump (non-zero), z = forward.
        state.0.move_xz = [dir.0.x, dir.0.z];
        state.0.jump = dir.0.y != 0.0;
        state.0.yaw = yaw.0;
        state.0.pitch = pitch.0;
        state.0.attach = attach;
        // The room is constant for the session; the server keys our world on it.
        state.0.room = my_room.0;
        match (grab_origin, hold_pos) {
            (Some(origin), Some(hold_pos)) => {
                state.0.grab_origin = origin.to_array();
                state.0.hold_target = hold_pos.to_array();
                state.0.hold_rotation = held_rotation.0.to_array();
                state.0.grab = want_hold.0;
            }
            // No hold point yet (camera orbit not attached) — can't grab.
            _ => state.0.grab = false,
        }
    }
    // One-shot attach intent: consumed after forwarding.
    want_attach.0 = false;
}

/// Highlight the part the player is interacting with in single-player's yellow
/// focus colour. While holding, the *held* part stays highlighted (the latched
/// `FocusedInteractable`), so the glow doesn't jump to whatever you look at next.
/// While empty-handed, highlight the grab preview — the part the look-ray is
/// most directly aimed at (same rule the server grabs by).
fn highlight_grabbable(
    want_hold: Res<WantHold>,
    player: Query<&FocusedInteractable, With<Player>>,
    orbit: Query<&GlobalTransform, With<CameraOrbitCenter>>,
    hold: Query<&GlobalTransform, With<HoldPoint>>,
    parts: Query<(Entity, &Transform, &MeshMaterial3d<StandardMaterial>), With<NetPart>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    // The previously-highlighted part, so we only re-colour on change. Mutating a
    // material flags it for GPU re-upload, so recolouring every part every frame
    // (when nothing moved) would needlessly re-upload all of them.
    mut lit: Local<Option<Entity>>,
) {
    let highlighted = if want_hold.0 {
        // Keep the grabbed part lit (mirror_grab_state latched it).
        player.iter().next().and_then(|f| f.0)
    } else {
        let (Some(orbit), Some(hold)) = (orbit.iter().next(), hold.iter().next()) else {
            return;
        };
        let origin = orbit.translation();
        let look = (hold.translation() - origin).normalize_or_zero();
        focused_part(
            origin,
            look,
            parts.iter().map(|(entity, t, _)| (entity, t.translation)),
        )
    };
    if *lit == highlighted {
        return;
    }
    let recolour = |entity, materials: &mut Assets<StandardMaterial>, lit: bool| {
        if let Ok((_, _, material)) = parts.get(entity) {
            if let Some(mut mat) = materials.get_mut(&material.0) {
                (mat.base_color, mat.emissive) = if lit {
                    (Color::srgb(1.0, 1.0, 0.0), LinearRgba::rgb(0.6, 0.6, 0.0))
                } else {
                    (Color::srgb(0.55, 0.6, 0.72), LinearRgba::BLACK)
                };
            }
        }
    };
    // Reset the part that just lost focus, light the one that gained it. (Newly
    // replicated parts already spawn with the base colour, so they need no reset.)
    if let Some(prev) = *lit {
        recolour(prev, &mut materials, false);
    }
    if let Some(now) = highlighted {
        recolour(now, &mut materials, true);
    }
    *lit = highlighted;
}

/// The client's grab intent: toggled on each non-modifier click. The local
/// hold mechanic (`toggle_holding`) is inert in multiplayer (no local parts to
/// focus), so the grab toggle is tracked here and sent to the server instead.
#[derive(Resource, Default)]
struct WantHold(bool);

/// One-shot attach intent, set on a modifier click (the join gesture), consumed
/// by `write_player_pose` after it's forwarded.
#[derive(Resource, Default)]
struct WantAttach(bool);

/// The held part's target orientation, tracked client-side and forwarded to the
/// server as `NetInput::hold_rotation`. `Quat::default()` is the identity, so
/// the derived `Default` seeds it correctly. Mirrors single-player's
/// `TargetOrientation`: it's seeded to the part's orientation at pickup and
/// accumulates the rotate gesture (`track_hold_rotation`). Public so the
/// secondary-pass gizmo can orient itself to it (indicating the target).
#[derive(Resource, Default)]
pub struct HeldRotation(pub Quat);

/// Track the target orientation of the held part the way single-player does:
/// seed it to the part's orientation the moment it's grabbed, then each frame
/// fold in the rotate gesture (`PartRotation`, computed locally by
/// `set_part_rotation` from the modifier + look delta — identity when not
/// rotating). The server drives the part toward this in `server_hold`.
fn track_hold_rotation(
    want_hold: Res<WantHold>,
    mut was_holding: Local<bool>,
    mut held_rotation: ResMut<HeldRotation>,
    player: Query<(&FocusedInteractable, &PartRotation), With<Player>>,
    parts: Query<&Transform, (With<NetPart>, With<Interpolated>)>,
) {
    let Ok((focused, part_rotation)) = player.single() else {
        return;
    };
    let just_grabbed = want_hold.0 && !*was_holding;
    *was_holding = want_hold.0;
    if !want_hold.0 {
        return;
    }
    if just_grabbed {
        // Seed to the pickup orientation (the part's current pose).
        if let Some(t) = focused.0.and_then(|e| parts.get(e).ok()) {
            held_rotation.0 = t.rotation;
        }
    } else {
        // Accumulate the rotate gesture, matching `apply_part_rotation`.
        held_rotation.0 = part_rotation.0 * held_rotation.0;
    }
}

/// A plain (non-`Modifying`) click toggles grab/drop; a modifier click (the
/// join/action gesture) requests attach. Same gestures as single-player, sourced
/// from desktop clicks and the mobile grab/action buttons (both emit
/// `PlayerClick`; `Modifying` distinguishes them).
fn read_grab_intent(
    mut clicks: MessageReader<PlayerClick>,
    modifying: Query<&Modifying, With<Player>>,
    mut want_hold: ResMut<WantHold>,
    mut want_attach: ResMut<WantAttach>,
) {
    let modding = modifying.iter().next().is_some_and(|m| m.0);
    for _ in clicks.read() {
        if modding {
            want_attach.0 = true;
        } else {
            want_hold.0 = !want_hold.0;
        }
    }
}

/// Attach a mesh to each *other* player's `Interpolated` copy (the smoothed visual
/// entity) that doesn't have one yet. Our own avatar is `Predicted`, not
/// `Interpolated`, and renders via the single-player character path
/// (`assign_characters`), so it's excluded here. The raw `Confirmed` entities stay
/// invisible.
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

/// Give each replicated part's `Interpolated` copy a cuboid mesh + a kinematic
/// collider built from its `NetPart` shape. The pose is driven by the server via
/// the interpolated `NetTransform` (`apply_net_transform`); a `Kinematic` body
/// follows that pose and blocks the local dynamic character, so the player bumps
/// the shared world (the part is never pushed back — the server is authoritative).
fn draw_replicated_parts(
    mut commands: Commands,
    new_parts: Query<(Entity, &NetPart), (With<Interpolated>, Without<Mesh3d>)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, part) in &new_parts {
        let [hx, hy, hz] = part.half_extents;
        commands.entity(entity).insert((
            Mesh3d(meshes.add(Cuboid::new(hx * 2.0, hy * 2.0, hz * 2.0))),
            MeshMaterial3d(materials.add(Color::srgb(0.55, 0.6, 0.72))),
            RigidBody::Kinematic,
            // Avian's `Collider::cuboid` takes FULL extents (= 2 × half_extents).
            Collider::cuboid(hx * 2.0, hy * 2.0, hz * 2.0),
            // Marked Holdable so the real joint-display systems (which query
            // Holdable transforms) can render potential/existing joints on them.
            Holdable,
        ));
    }
}

/// Mirror the networked grab into the local `Holding`/`FocusedInteractable`
/// state, so the game's real systems light up in multiplayer: the join/delete
/// button label (keyed on `Holding`), and `update_active_joints` +
/// `display_potential_joints` (keyed on the focused held part). The local
/// `toggle_holding` is gated off in MP so it doesn't fight this.
fn mirror_grab_state(
    want_hold: Res<WantHold>,
    orbit: Query<&GlobalTransform, With<CameraOrbitCenter>>,
    hold: Query<&GlobalTransform, With<HoldPoint>>,
    // Only the `Interpolated` copies carry the collider/Holdable that Avian's
    // `Collisions` (read by `update_active_joints`) reports against, so focus must
    // latch one of those — not the invisible `Confirmed` originals.
    parts: Query<(Entity, &Transform), (With<NetPart>, With<Interpolated>)>,
    mut player: Query<(&mut Holding, &mut FocusedInteractable), With<Player>>,
) {
    let Ok((mut holding, mut focused)) = player.single_mut() else {
        return;
    };
    holding.0 = want_hold.0;
    if !want_hold.0 {
        focused.0 = None;
        return;
    }
    // Latch the focused part once (the server latches its grab the same way).
    if focused.0.is_none() {
        if let (Some(orbit), Some(hold)) = (orbit.iter().next(), hold.iter().next()) {
            let look = (hold.translation() - orbit.translation()).normalize_or_zero();
            focused.0 = focused_part(
                orbit.translation(),
                look,
                parts.iter().map(|(entity, t)| (entity, t.translation)),
            );
        }
    }
}

/// Draw each replicated joint marker using the game's *real* joint visuals — the
/// `JointAppearance` mesh + `GizmoMaterial` that single-player uses for existing
/// joints (so it looks identical and draws on top via the secondary pass) —
/// positioned via the interpolated `NetTransform`.
fn draw_replicated_joints(
    mut commands: Commands,
    new_joints: Query<Entity, (With<NetJoint>, With<Interpolated>, Without<Mesh3d>)>,
    appearance: Res<JointAppearance>,
) {
    let (Some(mesh), Some(material)) = (&appearance.mesh, &appearance.invalid_material) else {
        return;
    };
    for entity in &new_joints {
        commands
            .entity(entity)
            .insert((Mesh3d(mesh.clone()), MeshMaterial3d(material.clone())));
    }
}

/// Apply the (interpolated) replicated pose to the rendered transform. Lightyear
/// eases the `NetTransform` on `Interpolated` entities each frame; mirror it onto
/// the Bevy `Transform` we render.
fn apply_net_transform(
    mut q: Query<(&NetTransform, &mut Transform), (Changed<NetTransform>, With<Interpolated>)>,
) {
    for (net, mut transform) in &mut q {
        *transform = net.to_transform();
    }
}

/// Build the dev netcode client for `server_addr`. Dev auth uses a fixed
/// protocol id + the all-zero key, matching the server's `NetcodeConfig::
/// default()`; production would issue a real ConnectToken from the matchmaker
/// instead of `Manual`. The random client id is the netcode handshake identity
/// only — we recognise our own avatar locally by lightyear's `Predicted` marker,
/// so it doesn't need to be retained.
fn build_netcode_client(server_addr: SocketAddr) -> Option<NetcodeClient> {
    let auth = Authentication::Manual {
        server_addr,
        client_id: rand::random::<u64>(),
        private_key: [0u8; 32],
        // Version gate: a client only connects to a server built from the same
        // commit. The matchmaker routes to the matching version; this is the
        // netcode-level backstop if a mismatched URL ever slips through.
        protocol_id: bad_spaceship_shared::net::BS_PROTOCOL_ID,
    };
    match NetcodeClient::new(auth, NetcodeConfig::default()) {
        Ok(n) => Some(n),
        Err(e) => {
            error!("failed to build netcode client: {e:?}");
            None
        }
    }
}

/// Startup: open the initial connection.
fn connect(mut commands: Commands) {
    spawn_client(&mut commands);
}

/// Auto-reconnect after the link drops. iOS (and any browser) suspends a
/// backgrounded tab, which kills the WebSocket; lightyear then marks the client
/// `Disconnected` and clears the replicated world, so the scene goes blank and
/// stays blank until a manual reload. The whole wasm app is *frozen* while the
/// tab is suspended, so this system next runs the instant the tab returns to the
/// foreground: if the client is `Disconnected` and nothing is mid-connect, it
/// despawns the dead client and spawns a fresh one, which re-replicates the room
/// cleanly. A short cooldown keeps a genuinely-unreachable server from being
/// hammered. (Reconnect must build a *fresh* `NetcodeClient` rather than re-
/// `Connect` the same entity, because the default connect token expires after
/// 30s — long gone by the time a real backgrounding ends.)
fn reconnect_dropped(
    mut commands: Commands,
    time: Res<Time>,
    mut cooldown: Local<f32>,
    dropped: Query<Entity, (With<NetcodeClient>, With<Disconnected>)>,
    pending: Query<(), (With<NetcodeClient>, Or<(With<Connecting>, With<Connected>)>)>,
) {
    *cooldown = (*cooldown - time.delta_secs()).max(0.0);
    // A connect is already established or in progress, or we're still cooling
    // down from the last attempt — nothing to do.
    if !pending.is_empty() || *cooldown > 0.0 {
        return;
    }
    if dropped.is_empty() {
        return;
    }
    for entity in &dropped {
        commands.entity(entity).despawn();
    }
    spawn_client(&mut commands);
    *cooldown = 2.0;
}

/// (Re)spawn the netcode client entity and start connecting. Called at startup
/// and again by `reconnect_dropped`; each call builds a fresh `NetcodeClient`
/// (new token + client id), so it survives connect-token expiry across a long
/// background.
#[cfg(not(target_arch = "wasm32"))]
fn spawn_client(commands: &mut Commands) {
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
    // `PredictionManager` enables client-side prediction on this connection: its
    // insert-hook creates the `PredictionResource` lightyear needs to process
    // predicted entities. It is NOT auto-added (unlike the interpolation config),
    // so without it receiving a predicted avatar panics in `receive_replication`.
    let client = commands.spawn((netcode, io, PredictionManager::default())).id();
    commands.trigger(Connect { entity: client });
    info!("connecting to multiplayer server at {url}");
}

/// See the native counterpart.
#[cfg(target_arch = "wasm32")]
fn spawn_client(commands: &mut Commands) {
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
    // See the native counterpart: `PredictionManager` enables client-side
    // prediction (creates `PredictionResource`); required or receiving a predicted
    // entity panics.
    let client = commands.spawn((netcode, io, PredictionManager::default())).id();
    commands.trigger(Connect { entity: client });
    info!("connecting to multiplayer server at {url}");
}
