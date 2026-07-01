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
//! For every player the server replicates, draw a body at its predicted/
//! interpolated Avian pose.

use avian3d::prelude::{Forces, Gravity, Position, RigidBody, Rotation, SphericalJoint};
use bad_spaceship_shared::character::{
    insert_character_body, CharacterMovement, Config as CharacterConfig,
};
use bad_spaceship_shared::net::{
    apply_hold_spring, apply_net_input, focused_part, DeleteZoneReport, NetFacing, NetHold,
    NetInput, NetJoint, NetPart, take_rollback_diag, NetPlayer, ProtocolPlugin, RollbackReport,
    TelemetryChannel, TICK,
};
use bad_spaceship_shared::part::{insert_part_physics, Holdable, SuppressLocalParts};
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
use lightyear::netcode::ConnectToken;
use lightyear::prelude::{
    Authentication, Connected, Interpolated, LocalId, MessageSender, PeerId, Predicted,
    PredictionManager, PredictionMetrics,
};
use lightyear::frame_interpolation::{FrameInterpolate, FrameInterpolationPlugin};
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

/// A stable per-player id persisted in `localStorage`, re-sent each session via
/// `NetInput::resume_id` so the server can restore this player's position after an
/// iOS reload (server-authoritative session resume). `0` on native (no reload case).
#[derive(Resource)]
struct ResumeId(u64);

#[cfg(not(target_arch = "wasm32"))]
fn resume_id() -> u64 {
    0
}

/// Reuse a stable id across reloads so the server recognises a reconnecting player;
/// mint + persist one on first run. An app-level token (NOT the netcode `client_id`)
/// so a quick reconnect isn't rejected as a duplicate connection.
#[cfg(target_arch = "wasm32")]
fn resume_id() -> u64 {
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        return 0;
    };
    if let Ok(Some(s)) = storage.get_item("bs-rid") {
        if let Ok(id) = s.parse::<u64>() {
            if id != 0 {
                return id;
            }
        }
    }
    let id = rand::random::<u64>() | 1; // never 0 (0 means "no resume")
    let _ = storage.set_item("bs-rid", &id.to_string());
    id
}

/// Camera look (yaw, pitch) restored from `sessionStorage` after a reload, so the
/// view resumes facing where it was. Position is restored by the server (it's
/// authoritative); the camera rig is client-owned, so its angle is restored here.
#[derive(Resource, Default)]
struct ResumeLook {
    yaw: f32,
    pitch: f32,
    has: bool,
}

#[cfg(not(target_arch = "wasm32"))]
fn read_resume_look() -> ResumeLook {
    ResumeLook::default()
}

/// Read the look saved by `write_look_beacon` (`client/src/platform/web.rs`) into
/// `sessionStorage` before the suspension; `sessionStorage` survives the reload.
#[cfg(target_arch = "wasm32")]
fn read_resume_look() -> ResumeLook {
    let value = web_sys::window()
        .and_then(|w| w.session_storage().ok().flatten())
        .and_then(|s| s.get_item("bs-look").ok().flatten());
    if let Some(value) = value {
        let mut parts = value.split(',').filter_map(|v| v.parse::<f32>().ok());
        if let (Some(yaw), Some(pitch)) = (parts.next(), parts.next()) {
            return ResumeLook { yaw, pitch, has: true };
        }
    }
    ResumeLook::default()
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

/// Our own netcode id — the value the server stamps onto our avatar's
/// `NetPlayer::client_id`. Read from `LocalId` (set the instant the connection
/// reaches `Connected`, before any avatar replicates). `None` until connected, or if
/// the peer isn't a netcode id. This is the client-side "who am I" used to adopt only
/// our own avatar and to flag our own roster row — see `setup_predicted_avatar`.
pub(crate) fn my_netcode_id(local: &Query<&LocalId, With<Connected>>) -> Option<u64> {
    match local.iter().next().map(|l| l.0) {
        Some(PeerId::Netcode(id)) => Some(id),
        _ => None,
    }
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
        // Render-interpolate predicted bodies between fixed (60 Hz) sim ticks.
        // Prediction/rollback advance Position/Rotation only in `FixedUpdate`; without
        // this the rendered pose is held constant between ticks and the camera (a
        // child of the predicted character) judders when the render rate isn't
        // phase-locked to 60 Hz — measured as ~22% frame-to-frame speed variance and
        // felt as a "low frame rate". We interpolate `Position`/`Rotation` (NOT
        // `Transform`): in `AvianReplicationMode::Position` those are the predicted
        // components, they already have interpolation fns registered (`ProtocolPlugin`),
        // and the Position→Transform sync then carries the interpolated value to the
        // rendered `Transform` (`FrameInterpolate`'s change-detection trigger makes the
        // sync pick it up). `lightyear_avian` orders its sync around the
        // `FrameInterpolationSystems` sets but does NOT add these plugins or the
        // per-entity components — we must. Each predicted entity opts in via
        // `FrameInterpolate<Position/Rotation>` (`setup_predicted_avatar`,
        // `draw_replicated_parts`). Interpolated remote avatars are already
        // frame-smooth via lightyear's interpolation, so they don't need it.
        app.add_plugins((
            FrameInterpolationPlugin::<Position>::default(),
            FrameInterpolationPlugin::<Rotation>::default(),
        ));
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
        // Session resume (see `ResumeId`/`ResumeLook`): a persistent id sent to the
        // server so it restores our position after an iOS reload, plus the camera
        // look saved before the suspension so the view resumes facing where it was.
        app.insert_resource(ResumeId(resume_id()));
        app.insert_resource(read_resume_look());
        app.init_resource::<WantAttach>();
        app.init_resource::<WantDelete>();
        app.init_resource::<HeldRotation>();
        app.add_systems(Startup, connect);
        // Recover from a dropped link (e.g. a suspended/backgrounded tab) by
        // reconnecting when the tab returns to the foreground.
        app.add_systems(Update, reconnect_dropped);
        // Report our prediction load to the server's log for measurement (see
        // `RollbackReport`); diagnostics only, off the gameplay path.
        app.add_systems(Update, (report_rollbacks, report_delete_zone));
        // Each frame: track the look-focused part (empty-handed) into
        // `FocusedInteractable`, then read the click → grab/attach intent gated on it.
        // After `InputEvents` (mobile `apply_pointer` / desktop `get_modifying`) so
        // `Modifying` is current when classifying a click as grab vs attach, and so the
        // grab press sees the focus computed from this frame's look.
        app.add_systems(
            Update,
            (update_focus, read_grab_intent).chain().after(InputEvents),
        );
        // Assemble our predicted avatar into the controllable character, give every
        // *other* replicated player a visible body, keep parts/joints in sync with
        // their replicated pose.
        app.add_systems(
            Update,
            (
                setup_predicted_avatar,
                draw_replicated_players,
                face_replicated_players,
                draw_replicated_parts,
                (bind_replicated_joints, position_replicated_joints).chain(),
                // After the click → grab intent writes `Holding`, track the held part's
                // target orientation, then highlight the focused part — in that order so
                // each reads the previous one's freshly-written state this frame.
                (track_hold_rotation, highlight_grabbable)
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
        // `predict_hold` applies the held-part spring locally each tick (the same
        // spring the server runs) so carrying a block is instant and rollback-replayed.
        app.add_systems(
            FixedUpdate,
            (
                apply_net_input.before(CharacterMovement),
                // Both spring held parts through `Forces`; order them so the shared
                // `Forces` accumulation isn't an ambiguous double-write. `predict_hold`
                // handles our own held part (local, zero-delay hold point);
                // `predict_remote_hold` springs every *other* player's held part toward
                // its replicated `NetHold` target so it floats for us instead of bobbing.
                (predict_hold, predict_remote_hold).chain(),
            ),
        );
    }
}

/// Every ~2 s, report our cumulative `PredictionMetrics` to the server so it lands in
/// the dev box's per-version `server.log` (`[rb] …`) — rollbacks are computed
/// client-side and on wasm the browser console is unreachable from the build box, so
/// this is how prediction load is measured. Diagnostics only; nothing gameplay reads
/// it. Mirrors the cadence of the server's `[rtt]` logger.
fn report_rollbacks(
    time: Res<Time>,
    mut acc: Local<f32>,
    metrics: Option<Res<PredictionMetrics>>,
    mut sender: Query<&mut MessageSender<RollbackReport>, With<Connected>>,
) {
    *acc += time.delta_secs();
    if *acc < 2.0 {
        return;
    }
    *acc = 0.0;
    let Some(metrics) = metrics else { return };
    let Ok(mut sender) = sender.single_mut() else { return };
    let (max_pos_err_mm, pos_triggers) = take_rollback_diag();
    sender.send::<TelemetryChannel>(RollbackReport {
        rollbacks: metrics.rollbacks,
        rollback_ticks: metrics.rollback_ticks,
        max_pos_err_mm,
        pos_triggers,
    });
}

/// TEMPORARY: forward the client-local delete-zone detector output to the server so
/// it lands in the version's `server.log` (`[dz] …`) — the highlight is computed
/// client-side and wasm has no reachable console. Sends only while delete mode is
/// active (empty-handed + modifier), a few times a second, so the log shows exactly
/// what the detector saw when a joint inside the sphere failed to highlight. Remove
/// with `DeleteZoneDebug`.
fn report_delete_zone(
    time: Res<Time>,
    mut acc: Local<f32>,
    debug: Res<bad_spaceship_shared::DeleteZoneDebug>,
    mut sender: Query<&mut MessageSender<DeleteZoneReport>, With<Connected>>,
) {
    *acc += time.delta_secs();
    if *acc < 0.4 || !debug.active {
        return;
    }
    *acc = 0.0;
    let Ok(mut sender) = sender.single_mut() else { return };
    let mm = |d: f32| if d.is_finite() { (d * 1000.0) as u32 } else { u32::MAX };
    sender.send::<TelemetryChannel>(DeleteZoneReport {
        joints: debug.joints,
        resolved: debug.resolved,
        in_zone: debug.in_zone,
        nearest_body2_mm: mm(debug.nearest_body2),
        nearest_body1_mm: mm(debug.nearest_body1),
    });
}

/// Turn **our own** predicted networked avatar into the controllable local
/// character. lightyear rolls back the Avian `Position`/`Rotation` of every
/// `Predicted` entity it gives us; we give *ours* the real character body
/// (`insert_character_body`) so Avian simulates it locally with zero input delay,
/// plus the player/input state (`make_local_player`) and the networked-input marker
/// so `write_input` fills its `ActionState` and lightyear sends it. From there it's
/// an ordinary `Character`: `assign_characters` renders it and `attach_camera_orbit`
/// mounts the camera — the same path single-player uses.
///
/// Identify our avatar by `NetPlayer::client_id == our LocalId`, NOT by the bare
/// `Predicted` marker: lightyear can hand an already-connected client a *predicted
/// copy of a late joiner's avatar* (verified at runtime), so adopting "any predicted
/// avatar" makes the first player build a second character and the camera follows the
/// joiner. `LocalId` is our own netcode `PeerId`, set on the connection the moment it
/// reaches `Connected` (strictly before any avatar replicates) — the client-side mirror
/// of the `RemoteId` the server reads in `client_identity` to stamp `NetPlayer`, so the
/// ids match across the wire and exactly one avatar — ours — is adopted.
///
/// Gated on `Position` so we assemble the body only once the avatar's real spawn
/// pose has arrived (rather than briefly at the origin). The loose blocks are also
/// `Predicted` now, so exclude `NetPart` — the avatar is the predicted entity that
/// is NOT a part (it carries no `NetPart`; `draw_replicated_parts` handles those).
fn setup_predicted_avatar(
    mut commands: Commands,
    new: Query<
        (Entity, &NetPlayer),
        (With<Predicted>, With<Position>, Without<Character>, Without<NetPart>),
    >,
    local: Query<&LocalId, With<Connected>>,
    configs: Res<Assets<CharacterConfig>>,
    resume_look: Res<ResumeLook>,
    mut look_applied: Local<bool>,
) {
    let Some((_, config)) = configs.iter().next() else {
        return;
    };
    // Our own netcode id (the value the server stamps onto our avatar's `NetPlayer`).
    let Some(my_id) = my_netcode_id(&local) else {
        return;
    };
    for (entity, net_player) in &new {
        // Only our own avatar — skip predicted copies of other players' avatars
        // (lightyear leaks them onto already-connected clients).
        if net_player.client_id != my_id {
            continue;
        }
        let mut e = commands.entity(entity);
        insert_character_body(&mut e, config.size());
        make_local_player(&mut e);
        e.insert((
            InputMarker::<NetInput>::default(),
            ActionState::<NetInput>::default(),
            // Render-interpolate this predicted body between fixed ticks, so the camera
            // mounted on it moves smoothly instead of stepping at 60 Hz.
            FrameInterpolate::<Position>::default(),
            FrameInterpolate::<Rotation>::default(),
        ));
        // Session resume: restore the camera look saved before an iOS reload, once,
        // on the first avatar after boot (overrides `make_local_player`'s defaults).
        // The avatar's *position* is restored by the server; the look is client-owned.
        if resume_look.has && !*look_applied {
            e.insert((Yaw(resume_look.yaw), LookPitch(resume_look.pitch)));
            *look_applied = true;
        }
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
    character: Query<(&DirectionalInput, &Yaw, &LookPitch, &Holding), With<Character>>,
    orbit: Query<&GlobalTransform, With<CameraOrbitCenter>>,
    hold: Query<&GlobalTransform, With<HoldPoint>>,
    mut want_attach: ResMut<WantAttach>,
    mut want_delete: ResMut<WantDelete>,
    held_rotation: Res<HeldRotation>,
    my_room: Res<MyRoom>,
    resume_id: Res<ResumeId>,
    mut controlled: Query<&mut ActionState<NetInput>, With<InputMarker<NetInput>>>,
) {
    let Some((dir, yaw, pitch, holding)) = character.iter().next() else {
        return;
    };
    let grab_origin = orbit.iter().next().map(|g| g.translation());
    let hold_pos = hold.iter().next().map(|g| g.translation());
    let attach = want_attach.0 > 0;
    let delete = want_delete.0 > 0;
    for mut state in &mut controlled {
        // DirectionalInput: x = strafe, y = jump (non-zero), z = forward.
        state.0.move_xz = [dir.0.x, dir.0.z];
        state.0.jump = dir.0.y != 0.0;
        state.0.yaw = yaw.0;
        state.0.pitch = pitch.0;
        state.0.attach = attach;
        state.0.delete = delete;
        // The room is constant for the session; the server keys our world on it.
        state.0.room = my_room.0;
        // Persistent resume id — the server keys our remembered position on it.
        state.0.resume_id = resume_id.0;
        match (grab_origin, hold_pos) {
            (Some(origin), Some(hold_pos)) => {
                state.0.grab_origin = origin.to_array();
                state.0.hold_target = hold_pos.to_array();
                state.0.hold_rotation = held_rotation.0.to_array();
                state.0.grab = holding.0;
            }
            // No hold point yet (camera orbit not attached) — can't grab.
            _ => state.0.grab = false,
        }
    }
    // Assert the attach/delete intents for a few ticks after a press, then lapse.
    want_attach.0 = want_attach.0.saturating_sub(1);
    want_delete.0 = want_delete.0.saturating_sub(1);
}

/// Highlight the focused part in single-player's yellow focus colour. Follows
/// `FocusedInteractable` (maintained by `update_focus`): the grab preview while
/// empty-handed, or the held part while holding — so the glow doesn't jump to whatever
/// you look at next once you've grabbed.
fn highlight_grabbable(
    player: Query<&FocusedInteractable, With<Player>>,
    parts: Query<&MeshMaterial3d<StandardMaterial>, With<NetPart>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    // The previously-highlighted part, so we only re-colour on change. Mutating a
    // material flags it for GPU re-upload, so recolouring every part every frame
    // (when nothing moved) would needlessly re-upload all of them.
    mut lit: Local<Option<Entity>>,
) {
    let highlighted = player.iter().next().and_then(|f| f.0);
    if *lit == highlighted {
        return;
    }
    let recolour = |entity, materials: &mut Assets<StandardMaterial>, lit: bool| {
        if let Ok(material) = parts.get(entity) {
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

/// Attach intent as a small countdown of ticks, set on a modifier click (the join
/// gesture) and decremented each tick by `write_input`. Sending the intent for a few
/// ticks (rather than one) survives a dropped packet; the server arms its retry
/// window on the rising edge, so the extra ticks don't cause a double-join.
#[derive(Resource, Default)]
struct WantAttach(u32);

/// Same one-shot latch as [`WantAttach`], for the empty-handed modifier-click
/// joint-delete gesture. Asserted for [`ATTACH_SEND_TICKS`] ticks so a dropped
/// packet doesn't lose the press; the server acts on the rising edge.
#[derive(Resource, Default)]
struct WantDelete(u32);

/// How many ticks the client asserts the attach intent after a join press. The
/// server arms its retry window on the rising edge (so extra ticks don't double-join),
/// but sending the intent for a few ticks means a single dropped packet doesn't lose
/// the press. ~0.1s at 60 Hz.
const ATTACH_SEND_TICKS: u32 = 6;

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
    mut was_holding: Local<bool>,
    mut held_rotation: ResMut<HeldRotation>,
    player: Query<(&Holding, &FocusedInteractable, &PartRotation), With<Player>>,
    parts: Query<&Transform, (With<NetPart>, With<Predicted>)>,
) {
    let Ok((holding, focused, part_rotation)) = player.single() else {
        return;
    };
    let just_grabbed = holding.0 && !*was_holding;
    *was_holding = holding.0;
    if !holding.0 {
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

/// Predict the held-part spring locally so carrying a block is instant (zero
/// input delay), reconciled by rollback against the server's authoritative pose.
/// Mirrors `server_hold`: the same critically-damped anti-gravity float to the
/// hold point + orient spring toward the tracked target, applied via Avian
/// `Forces` to the *predicted* held part — in `FixedUpdate`, so the spring is part
/// of the simulation lightyear replays on a rollback. The held part is the one
/// `update_focus` latched into the local Player's `FocusedInteractable` (the
/// same look-angle rule the server grabs by), and the hold target/orientation are
/// the same values `write_input` forwards to the server (`HoldPoint` world position
/// + `HeldRotation`), so both worlds spring toward an identical goal and diverge
/// only by round-trip. Reads `Position`/`Rotation` (current in the fixed schedule),
/// not `Transform`, for the same reason `server_hold` does.
fn predict_hold(
    player: Query<(&Holding, &FocusedInteractable), With<Player>>,
    hold: Query<&GlobalTransform, With<HoldPoint>>,
    held_rotation: Res<HeldRotation>,
    mut parts: Query<(&Position, &Rotation, Forces), With<NetPart>>,
    gravity: Res<Gravity>,
) {
    let Ok((holding, focused)) = player.single() else {
        return;
    };
    if !holding.0 {
        return;
    }
    let (Some(part_entity), Some(hold_target)) =
        (focused.0, hold.iter().next().map(|g| g.translation()))
    else {
        return;
    };
    let Ok((position, rotation, mut forces)) = parts.get_mut(part_entity) else {
        return;
    };
    apply_hold_spring(
        &mut forces,
        position.0,
        rotation.0,
        hold_target,
        held_rotation.0,
        gravity.0,
    );
}

/// Predict the hold spring for parts held by **other** players, so a block someone
/// else is carrying floats for us instead of free-falling. Every loose part is
/// predicted on every client (`PredictionTarget::All`), but only the holder runs
/// `predict_hold` — so a held part the holder forwards no force for would, on our
/// client, fall each tick and get yanked back up by replication (the visible sag +
/// bob). Here we spring every part carrying a replicated [`NetHold`] toward that hold
/// state with the same `apply_hold_spring` the server runs, EXCEPT our own held part
/// (`holder == our LocalId`) — `predict_hold` already drives that from our local,
/// zero-delay hold point, so springing it again would double the force. Chained after
/// `predict_hold` because both accumulate through the shared `Forces`.
fn predict_remote_hold(
    local: Query<&LocalId, With<Connected>>,
    mut parts: Query<(&Position, &Rotation, &NetHold, Forces), With<NetPart>>,
    gravity: Res<Gravity>,
) {
    let Some(my_id) = my_netcode_id(&local) else {
        return;
    };
    for (position, rotation, hold, mut forces) in &mut parts {
        // Our own held part — `predict_hold` owns it (local, zero-delay hold point).
        if hold.holder == my_id {
            continue;
        }
        apply_hold_spring(
            &mut forces,
            position.0,
            rotation.0,
            Vec3::from_array(hold.target),
            Quat::from_array(hold.rotation),
            gravity.0,
        );
    }
}

/// Track which part the empty-handed player is looking at — the same look-angle rule
/// the server grabs by (`focused_part`) — into the local `FocusedInteractable`, so the
/// grab press can be *gated* on it. While holding, the latched part is left untouched
/// (the held part stays focused), so a single piece of state both previews the grab
/// target and freezes onto the grabbed one — driving the highlight, the join preview
/// (`update_active_joints`), and the predicted hold. This mirrors single-player's
/// `update_focused`, which the replicated parts don't trigger (they carry `Holdable`
/// but not the `Interactable` marker that system queries).
fn update_focus(
    orbit: Query<&GlobalTransform, With<CameraOrbitCenter>>,
    hold: Query<&GlobalTransform, With<HoldPoint>>,
    // The `Predicted` copies carry the dynamic body/collider Avian's `Collisions`
    // (read by `update_active_joints`) reports against, so focus the same copy the
    // server grab + the join preview resolve over — not the invisible `Confirmed` ones.
    parts: Query<(Entity, &Transform), (With<NetPart>, With<Predicted>)>,
    mut player: Query<(&Holding, &mut FocusedInteractable), With<Player>>,
) {
    let Ok((holding, mut focused)) = player.single_mut() else {
        return;
    };
    // While holding, keep the part latched the frame the grab began — don't re-aim.
    if holding.0 {
        return;
    }
    let (Some(orbit), Some(hold)) = (orbit.iter().next(), hold.iter().next()) else {
        return;
    };
    let look = (hold.translation() - orbit.translation()).normalize_or_zero();
    focused.0 = focused_part(
        orbit.translation(),
        look,
        parts.iter().map(|(entity, t)| (entity, t.translation)),
    );
}

/// A plain (non-`Modifying`) click grabs the focused part / drops the held one; a
/// modifier click (the join/action gesture) requests attach. Same gestures as
/// single-player, sourced from desktop clicks and the mobile grab/action buttons (both
/// emit `PlayerClick`; `Modifying` distinguishes them).
///
/// The grab is **gated on a part being focused at the instant of the press**
/// (`FocusedInteractable`, maintained by `update_focus`) — exactly like single-player's
/// `toggle_holding`, which only grabs `if interactable.0` is `Some`. Without this gate
/// the toggle armed unconditionally and the server's per-tick `server_grab` latched the
/// first block that drifted into view *afterward*; now an empty-handed press with
/// nothing looked-at is a no-op, so the next block you glance at is not auto-grabbed.
fn read_grab_intent(
    mut clicks: MessageReader<PlayerClick>,
    modifying: Query<&Modifying, With<Player>>,
    mut player: Query<(&mut Holding, &FocusedInteractable), With<Player>>,
    mut want_attach: ResMut<WantAttach>,
    mut want_delete: ResMut<WantDelete>,
) {
    let Ok((mut holding, focused)) = player.single_mut() else {
        return;
    };
    let modding = modifying.iter().next().is_some_and(|m| m.0);
    let looking_at_part = focused.0.is_some();
    for _ in clicks.read() {
        if modding {
            // Modifier click: holding → attach (join), empty-handed → delete a
            // joint in the delete zone. Same split as single-player's two systems.
            if holding.0 {
                want_attach.0 = ATTACH_SEND_TICKS;
            } else {
                want_delete.0 = ATTACH_SEND_TICKS;
            }
        } else if holding.0 {
            // Holding → drop, regardless of what's under the look (matches single-player).
            holding.0 = false;
        } else if looking_at_part {
            // Empty-handed → grab only if a part is focused at THIS press.
            holding.0 = true;
        }
    }
}

/// Records a remote avatar's visual yaw-pivot child (the entity carrying its body
/// + nose mesh), so `face_replicated_players` can turn it to the avatar's
/// replicated look `Yaw` without writing the avatar entity's own `Transform` —
/// which `lightyear_avian` owns (it syncs `Position`→`Transform` and pins the
/// rotation to the body's `ROTATION_LOCKED` identity every frame, so a yaw written
/// there would be stomped).
#[derive(Component)]
struct AvatarVisual(Entity);

/// Give each *other* player's `Interpolated` copy a visible body, mounted on a yaw
/// pivot so `face_replicated_players` can turn it to the player's look direction.
/// Our own avatar is `Predicted`, not `Interpolated`, and renders via the
/// single-player character path (`assign_characters`), so it's excluded here. The
/// raw `Confirmed` entities stay invisible.
fn draw_replicated_players(
    mut commands: Commands,
    new_players: Query<Entity, (With<NetPlayer>, With<Interpolated>, Without<AvatarVisual>)>,
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
        // The pivot carries the body mesh and is rotated to the look yaw; the avatar
        // entity itself keeps the Avian-driven Transform (translation only).
        let pivot = commands
            .spawn((
                Mesh3d(meshes.add(Cuboid::new(0.8, 1.2, 1.6))),
                MeshMaterial3d(materials.add(Color::srgb(0.9, 0.35, 0.35))),
                Transform::default(),
            ))
            .add_children(&[nose])
            .id();
        commands
            .entity(entity)
            .insert(AvatarVisual(pivot))
            .add_children(&[pivot]);
    }
}

/// Turn each remote avatar's visual pivot to its replicated, interpolated facing
/// (`NetFacing`, the server's mirror of that avatar's look yaw), so other players
/// are drawn facing the way they're looking. Uses the same basis as the movement
/// code (`Quat::from_rotation_y(-yaw)`, see `walk_based_on_input`), so the +Z nose
/// points along the avatar's forward. Reads `NetFacing`, not the owner's local-input
/// `Yaw` (which isn't replicated — replicating it broke the owner's turning).
fn face_replicated_players(
    avatars: Query<(&NetFacing, &AvatarVisual), (With<Interpolated>, Changed<NetFacing>)>,
    mut pivots: Query<&mut Transform>,
) {
    for (facing, visual) in &avatars {
        if let Ok(mut transform) = pivots.get_mut(visual.0) {
            transform.rotation = Quat::from_rotation_y(-facing.0);
        }
    }
}

/// Turn each replicated part's `Predicted` copy into a real dynamic body: the same
/// physics (`insert_part_physics`) the server simulates, a cuboid mesh from its
/// `NetPart` shape, and `Holdable` for the joint-display systems. The pose rides on
/// the predicted Avian `Position`/`Rotation`, so the client simulates the block
/// locally (shoving it is instant) and rollback reconciles against the server.
/// Gated on `Position` + `Rotation` both present (lightyear_avian inserts them on
/// the predicted entity from the server's confirmed state) so the body isn't built —
/// and rendered — at a default pose for a frame.
fn draw_replicated_parts(
    mut commands: Commands,
    new_parts: Query<(Entity, &NetPart), (With<Predicted>, With<Position>, With<Rotation>, Without<Mesh3d>)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, part) in &new_parts {
        let [hx, hy, hz] = part.half_extents;
        let mut e = commands.entity(entity);
        insert_part_physics(&mut e, Vec3::new(hx, hy, hz));
        e.insert((
            Mesh3d(meshes.add(Cuboid::new(hx * 2.0, hy * 2.0, hz * 2.0))),
            MeshMaterial3d(materials.add(Color::srgb(0.55, 0.6, 0.72))),
            Holdable,
            // Render-interpolate the predicted block between fixed ticks (same reason
            // as the character) so loose/held blocks move smoothly.
            FrameInterpolate::<Position>::default(),
            FrameInterpolate::<Rotation>::default(),
        ));
    }
}

/// The *predicted* part entity a replicated joint anchors its gizmo to (its
/// `body1`), recorded so `position_replicated_joints` can track the gizmo to the
/// moving assembly without re-resolving the id each frame. Its presence also marks
/// the `NetJoint` as already bound (so `bind_replicated_joints` doesn't re-spawn
/// the constraint).
#[derive(Component)]
struct JointAnchorBody(Entity);

/// Reconstruct each replicated joint as **real predicted physics**: look up the
/// local *predicted* part entities matching the `NetJoint`'s endpoint ids and
/// insert a real Avian `SphericalJoint` (with the replicated body-local anchors)
/// between them, so the client's own simulation holds the assembly together and
/// rollback keeps it consistent — the server's joint is server-only, so without
/// this the predicted parts are unconstrained locally and a lifted assembly sags
/// apart. Also gives the joint entity the game's real `JointAppearance` mesh +
/// `GizmoMaterial` so it draws identically to single-player (positioned by
/// `position_replicated_joints`).
///
/// Retries (gated on `Without<JointAnchorBody>`) until both predicted parts exist and
/// have their physics body built (`With<RigidBody>`), since the `NetJoint` can
/// replicate before the parts it references finish spawning.
fn bind_replicated_joints(
    mut commands: Commands,
    new_joints: Query<(Entity, &NetJoint), Without<JointAnchorBody>>,
    parts: Query<(Entity, &NetPart), (With<Predicted>, With<RigidBody>)>,
    appearance: Res<JointAppearance>,
) {
    let (Some(mesh), Some(material)) = (&appearance.mesh, &appearance.invalid_material) else {
        return;
    };
    for (joint_entity, joint) in &new_joints {
        let find = |id: u64| {
            parts
                .iter()
                .find(|(_, part)| part.id == id)
                .map(|(entity, _)| entity)
        };
        let (Some(body1), Some(body2)) = (find(joint.body1), find(joint.body2)) else {
            continue;
        };
        commands.entity(joint_entity).insert((
            SphericalJoint::new(body1, body2)
                .with_local_anchor1(Vec3::from_array(joint.anchor1))
                .with_local_anchor2(Vec3::from_array(joint.anchor2)),
            JointAnchorBody(body1),
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material.clone()),
            Transform::default(),
        ));
    }
}

/// Track each bound joint's gizmo to its assembly: place it at body1's world-space
/// anchor (`body1.transform · anchor1`), the same point the constraint pins, so the
/// visual rides the *predicted* blocks rather than floating at a stale pose.
fn position_replicated_joints(
    mut joints: Query<(&NetJoint, &JointAnchorBody, &mut Transform)>,
    parts: Query<&Transform, (With<NetPart>, Without<NetJoint>)>,
) {
    for (joint, anchor_body, mut transform) in &mut joints {
        if let Ok(body) = parts.get(anchor_body.0) {
            let anchor = body.transform_point(Vec3::from_array(joint.anchor1));
            // Only write on change, so a settled assembly stops dirtying `Transform`
            // (and re-propagating `GlobalTransform`) every frame.
            if transform.translation != anchor {
                transform.translation = anchor;
            }
        }
    }
}

/// Build the dev netcode client for `server_addr`. Dev auth uses a fixed
/// protocol id + the all-zero key, matching the server's `NetcodeConfig::
/// default()`; production would issue a real ConnectToken from the matchmaker
/// instead of `Manual`. The random netcode `client_id` is the handshake identity (a
/// fresh one per connect avoids duplicate-connection rejection); lightyear exposes it
/// back to us as `LocalId` once connected, which `setup_predicted_avatar` matches
/// against each avatar's replicated `NetPlayer::client_id` to find our own.
fn build_netcode_client(server_addr: SocketAddr) -> Option<NetcodeClient> {
    // Carry the persistent resume id in the connect token's `user_data` so the server
    // resolves this player's remembered position AT CONNECT — before the avatar's first
    // pose replicates — placing a reconnecting avatar directly at its saved spot (no
    // origin→saved ease). Built manually (`Authentication::Token`) because
    // `Authentication::Manual` doesn't expose `user_data`; the timeout/expire match
    // Manual's defaults (3 s / 30 s). The random netcode `client_id` is just the
    // handshake identity (a fresh one per connect avoids duplicate-connection rejection);
    // the *resume* identity is the `user_data` id, persisted in localStorage. Version
    // gate (`BS_PROTOCOL_ID`): a client only connects to a server built from the same
    // commit — the matchmaker routes to the matching version; this is the backstop.
    let mut user_data = [0u8; 256];
    user_data[..8].copy_from_slice(&resume_id().to_le_bytes());
    let token = ConnectToken::build(
        server_addr,
        bad_spaceship_shared::net::BS_PROTOCOL_ID,
        rand::random::<u64>(),
        [0u8; 32],
    )
    .timeout_seconds(3)
    .expire_seconds(30)
    .user_data(user_data)
    .generate();
    let token = match token {
        Ok(t) => t,
        Err(e) => {
            error!("failed to build connect token: {e:?}");
            return None;
        }
    };
    match NetcodeClient::new(Authentication::Token(token), NetcodeConfig::default()) {
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
