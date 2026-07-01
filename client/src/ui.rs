use crate::AppState;
use bevy::{
    diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin},
    input::mouse::MouseButtonInput,
    prelude::*,
};
use bevy_egui::{
    egui::{self, Align, Align2, Color32, Frame, Layout},
    EguiContexts, EguiPlugin, EguiPrimaryContextPass,
};
use bad_spaceship_shared::net::{
    sanitize_name, ControlChannel, NetName, NetPlayer, ResetPosition, SetName, MAX_NAME_LEN,
};
use chrono::{DateTime, FixedOffset, Utc};
use lightyear::prelude::client::Connected;
use lightyear::prelude::{
    Interpolated, LocalId, MessageSender, PingManager, Predicted, PredictionMetrics,
};
use std::collections::BTreeMap;
use once_cell::sync::Lazy;
use shadow_rs::shadow;

shadow!(build);
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        // bevy_egui 0.35 deprecated single-pass mode (the old
        // `enable_multipass_for_primary_context` flag) and made multipass the only
        // supported path, so add the plugin with `default()`. Multipass requires
        // egui-drawing systems to run in the dedicated `EguiPrimaryContextPass`
        // schedule rather than `Update`, and `EguiContexts::ctx_mut()` now returns
        // a `Result` (the systems below are fallible and use `?`). Systems that
        // only read input or egui settings (not the context) stay in `Update`.
        app.init_resource::<HudState>()
            .add_plugins((EguiPlugin::default(), FrameTimeDiagnosticsPlugin::default()))
            .add_systems(
                Update,
                (
                    capture_mouse_on_click.run_if(in_state(AppState::Initial)),
                    restore_persisted_name,
                ),
            )
            .add_systems(
                EguiPrimaryContextPass,
                (
                    // Touches an egui context (zoom factor), so it must run in the
                    // egui pass alongside the panel-drawing systems, not in `Update`.
                    update_ui_scale_factor,
                    show_menu.run_if(in_state(AppState::InGameMenu)),
                    // Top-left controls (rename + help toggle), the rename modal, and
                    // the top-right player roster; billboard names over each avatar.
                    show_name_hud,
                    show_name_labels,
                    show_instructions,
                    show_bottom_panel,
                ),
            );
    }
}

struct CustomScaleFactor(f64);

impl Default for CustomScaleFactor {
    fn default() -> Self {
        Self(1.0)
    }
}

const MIN_SCALE_FACTOR: f64 = 0.5;
const MAX_SCALE_FACTOR: f64 = 10.0;

fn update_ui_scale_factor(
    key_input: Res<ButtonInput<KeyCode>>,
    mut contexts: EguiContexts,
    mut custom_scale_factor: Local<CustomScaleFactor>,
) -> Result {
    if key_input.pressed(KeyCode::ControlLeft) || key_input.pressed(KeyCode::ControlRight) {
        if let Some(adjustment) = if key_input.just_pressed(KeyCode::Equal) {
            Some(1.1)
        } else if key_input.just_pressed(KeyCode::Minus) {
            Some(1. / 1.1)
        } else {
            None
        } {
            custom_scale_factor.0 = (custom_scale_factor.0 * adjustment)
                .max(MIN_SCALE_FACTOR)
                .min(MAX_SCALE_FACTOR);
        }
    }
    // bevy_egui 0.40 removed `EguiContextSettings::scale_factor`; UI scaling moved
    // to egui 0.34's per-context **zoom factor**. Drive it from our own Ctrl +/-
    // handler above, and turn egui's *built-in* keyboard zoom off so a single
    // keypress isn't applied twice (ours + egui's).
    let ctx = contexts.ctx_mut()?;
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    ctx.set_zoom_factor(custom_scale_factor.0 as f32);
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn align_menu(window: egui::Window) -> egui::Window {
    window.anchor(Align2::CENTER_TOP, [0., 150.])
}

#[cfg(target_arch = "wasm32")]
fn align_menu(window: egui::Window) -> egui::Window {
    window.anchor(Align2::CENTER_CENTER, [0., -70.])
}

fn show_menu(
    mut contexts: EguiContexts,
    mut next_state: ResMut<NextState<AppState>>,
) -> Result {
    align_menu(egui::Window::new("Bad Spaceship"))
        .collapsible(false)
        .resizable(false)
        .show(contexts.ctx_mut()?, |ui| {
            ui.with_layout(Layout::top_down_justified(Align::Center), |ui| {
                if ui.button("Options").clicked() {
                    bevy::log::info!("Options selected");
                }
                if ui.button("Multiplayer").clicked() {
                    bevy::log::info!("Multiplayer selected");
                }
                if ui.button("Resume").clicked() {
                    bevy::log::info!("Resume selected");
                    next_state.set(AppState::InGame);
                }
            });
        });
    Ok(())
}

/// The build timestamp, parsed once from the RFC 2822 string baked in at
/// compile time (`None` if it somehow fails to parse).
static BUILD_TIME: Lazy<Option<DateTime<FixedOffset>>> =
    Lazy::new(|| DateTime::parse_from_rfc2822(build::BUILD_TIME_2822).ok());

/// Age of the build, derived in real time by comparing now against the UTC
/// build timestamp shown in the bottom panel. Rendered as the single largest
/// whole unit (e.g. `0-59s`, `1-59m`, `1-23h`, `1-6d`, ...).
fn commit_age() -> String {
    let Some(built) = *BUILD_TIME else {
        return "?".to_string();
    };
    let secs = Utc::now()
        .signed_duration_since(built)
        .num_seconds()
        .max(0);
    const UNITS: &[(i64, &str)] = &[
        (365 * 24 * 60 * 60, "y"),
        (7 * 24 * 60 * 60, "w"),
        (24 * 60 * 60, "d"),
        (60 * 60, "h"),
        (60, "m"),
        (1, "s"),
    ];
    for &(threshold, unit) in UNITS {
        if secs >= threshold {
            return format!("{}{}", secs / threshold, unit);
        }
    }
    "0s".to_string()
}

/// Smoothed rollback-per-second estimate for the on-screen readout. `PredictionMetrics`
/// only exposes cumulative counters, so we sample the delta over a ~0.5 s wall-clock
/// window (kept in a `Local`) to turn the running total into a live rate.
#[derive(Default)]
struct RollbackRate {
    last_count: u32,
    last_secs: f64,
    rate: f64,
}

fn show_bottom_panel(
    mut contexts: EguiContexts,
    diagnostics: Res<DiagnosticsStore>,
    pings: Query<&PingManager, With<Connected>>,
    // Only present in multiplayer (added by lightyear's `PredictionPlugin`); `None`
    // in single-player, where the readout shows "RB —".
    metrics: Option<Res<PredictionMetrics>>,
    time: Res<Time<Real>>,
    mut rb_rate: Local<RollbackRate>,
) -> Result {
    let mut fps = 0.0;
    // Bevy 0.13 replaced `DiagnosticId` with `DiagnosticPath`; `get` takes `&path`.
    if let Some(fps_diagnostic) = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS) {
        if let Some(fps_avg) = fps_diagnostic.average() {
            fps = fps_avg;
        }
    }
    // Live round-trip time from lightyear's PingManager (multiplayer only; "—"
    // until the first ping samples land, or in single-player).
    let rtt_label = pings
        .iter()
        .next()
        .filter(|p| p.latency_samples_recv() > 0)
        .map(|p| {
            format!(
                "RTT {:.0}ms (±{:.0})",
                p.rtt().as_secs_f64() * 1000.0,
                p.jitter().as_secs_f64() * 1000.0,
            )
        })
        .unwrap_or_else(|| "RTT —".to_string());
    // Client-prediction correction load: cumulative rollbacks, a smoothed rate, and
    // the average rollback depth (ticks resimulated per rollback). This is the
    // baseline the determinism pass is measured against — fewer/smaller corrections
    // is the whole goal, so it must be visible on-device. "RB —" in single-player.
    let rb_label = if let Some(m) = metrics.as_ref() {
        let now = time.elapsed_secs_f64();
        let dt = now - rb_rate.last_secs;
        if dt >= 0.5 {
            rb_rate.rate = m.rollbacks.saturating_sub(rb_rate.last_count) as f64 / dt;
            rb_rate.last_count = m.rollbacks;
            rb_rate.last_secs = now;
        }
        let depth = if m.rollbacks == 0 {
            0.0
        } else {
            m.rollback_ticks as f64 / m.rollbacks as f64
        };
        format!("RB {} · {:.1}/s · d{:.1}", m.rollbacks, rb_rate.rate, depth)
    } else {
        "RB —".to_string()
    };
    egui::TopBottomPanel::bottom("bottom_panel")
        .frame(Frame::default().multiply_with_opacity(0.0))
        // Drop the hairline divider egui draws at the panel's edge.
        .show_separator_line(false)
        .show(contexts.ctx_mut()?, |ui| {
            // Stack each stat on its own short line (a single horizontal row crowds
            // and overlaps on a narrow phone screen).
            let red = Color32::from_rgb(255, 0, 0);
            ui.vertical(|ui| {
                ui.colored_label(red, rb_label);
                ui.colored_label(red, rtt_label);
                ui.colored_label(red, format!("{:.0} FPS", fps));
                ui.colored_label(
                    red,
                    format!("Commit {}", bad_spaceship_shared::net::BS_VERSION),
                );
                ui.colored_label(red, format!("Built {} ({} ago)", build::BUILD_TIME_2822, commit_age()));
            });
        });
    Ok(())
}

const INSTRUCTIONS: &str = "Instructions:
For best experience, open this in Google Chrome and use a mouse and keyboard.
Press WSDA keys to move.
Move mouse to look around.
Look at block within range to highlight it.
Click while block is highlighted to pick it up.
Click again to drop it.
Hold shift and move mouse or scroll to rotate a held block.
Hold block to touch another to display potential joints (in blue).
Shift-click to make potential joints real (displayed in green).
While not holding block, hold shift to display deletion zone.
Hold deletion zone over existing joint to highlight it in red.
Click while joint is highlighted red to delete it.
";

const TOUCH_INSTRUCTIONS: &str = "Instructions:
Left stick to move, right stick to look (drag from where you touch).
Aim a block within range and tap GRAB to pick it up; tap again to drop.
Carry a block to touch another to show potential joints (blue),
then tap Create Joints to make them real (green).
Drag the open area while carrying to rotate the held block.
Empty-handed, aim at a joint until it highlights red,
then tap Delete Joints to remove it. Use the top-left menu to change your name or reset your position.
";

/// The instructions overlay, now **hidden by default** and revealed by the top-left
/// "?" help button (`HudState::show_help`). Drawn as a boxed panel just under the
/// button row (an `Area` offset below the top-left controls) rather than the old
/// full-width top panel, so it doesn't collide with the controls or the roster.
fn show_instructions(
    mut contexts: EguiContexts,
    hud: Res<HudState>,
    mobile: Res<crate::mobile::MobileActive>,
) -> Result {
    if !hud.show_help {
        return Ok(());
    }
    let text = if mobile.0 {
        TOUCH_INSTRUCTIONS
    } else {
        INSTRUCTIONS
    };
    let ctx = contexts.ctx_mut()?;
    egui::Area::new(egui::Id::new("bs_instructions"))
        .anchor(Align2::LEFT_TOP, egui::vec2(8.0, 48.0))
        .show(ctx, |ui| {
            // Translucent panel, no drop shadow — matches the roster.
            Frame::default()
                .fill(Color32::from_black_alpha(160))
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    ui.colored_label(Color32::from_rgb(255, 0, 0), text);
                });
        });
    Ok(())
}

/// Transient HUD state for the top-left controls: whether the rename modal and the
/// instructions overlay are open, and the in-progress text of the rename field.
#[derive(Resource, Default)]
struct HudState {
    /// The top-left hamburger menu is expanded.
    show_menu: bool,
    /// The rename modal is open.
    show_change_modal: bool,
    /// The instructions overlay is revealed (toggled by the "?" button).
    show_help: bool,
    /// Live contents of the rename text field.
    editing: String,
}

/// How high above an avatar's origin its name billboard floats (metres). The avatar
/// body is ~1.2 m tall centred on the origin, so this sits the label just overhead.
const NAME_LABEL_HEIGHT: f32 = 1.6;

/// Side length (points) of the top-left icon buttons (hamburger + "?"). Both are drawn
/// as equal squares at ~double egui's default control height for an easy touch target.
const HUD_ICON_SIZE: f32 = 32.0;

/// Roster player-name colour: a light grey so names read clearly over the translucent
/// panel (egui's default body text is a dimmer grey).
const ROSTER_NAME_COLOR: Color32 = Color32::from_rgb(225, 230, 240);

/// Allocate a square, translucent icon-button background; return its rect, click
/// `Response`, and the interaction-tinted foreground colour (brightens on hover/press).
/// Shared by `hamburger_button` and `glyph_button` so the two are identical squares.
fn icon_button(ui: &mut egui::Ui, size: f32) -> (egui::Rect, egui::Response, Color32) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    let color = ui.style().interact(&response).fg_stroke.color;
    if ui.is_rect_visible(rect) {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(4), Color32::from_black_alpha(120));
    }
    (rect, response, color)
}

/// A hamburger (three horizontal lines) icon button — egui's default font can't render
/// the ☰ glyph, so paint it. Square + translucent (see `icon_button`).
fn hamburger_button(ui: &mut egui::Ui, size: f32) -> egui::Response {
    let (rect, response, color) = icon_button(ui, size);
    if ui.is_rect_visible(rect) {
        let stroke = egui::Stroke::new((size * 0.09).max(2.0), color);
        let x0 = rect.left() + size * 0.22;
        let x1 = rect.right() - size * 0.22;
        for i in 0..3 {
            let y = rect.top() + size * (0.30 + 0.20 * i as f32);
            ui.painter()
                .line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], stroke);
        }
    }
    response
}

/// A square, translucent icon button showing a single centred glyph (e.g. "?"), the
/// same size/style as `hamburger_button` so the two sit as equal squares.
fn glyph_button(ui: &mut egui::Ui, size: f32, glyph: &str) -> egui::Response {
    let (rect, response, color) = icon_button(ui, size);
    if ui.is_rect_visible(rect) {
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            egui::FontId::proportional(size * 0.62),
            color,
        );
    }
    response
}

/// The one representative entity per player carrying its name: the owner's own
/// avatar is `Predicted`, every remote avatar is `Interpolated`. This excludes the
/// invisible `Confirmed` copies so each player appears exactly once (in the roster
/// and as a single billboard).
type RenderedAvatar = Or<(With<Predicted>, With<Interpolated>)>;

/// Draw the top-left controls (a hamburger menu with Change Name + Reset Position,
/// plus a "?" help toggle), the native rename modal, and the top-right player roster.
/// All name state is replicated (`NetPlayer` + `NetName`); the roster is deduped by
/// `client_id` (a player has a `Confirmed` copy plus a rendered one) and the local
/// player is found via `my_netcode_id` and flagged with an asterisk — the same
/// self-identification the netcode uses (`NetPlayer::client_id == LocalId`). The menu
/// only appears once connected; the "?" help toggle is always available. A committed
/// rename / reset is sent over the reliable `ControlChannel`.
///
/// Menu clicks set local flags that mutate `hud` and send messages *after* the egui
/// closures return, so `hud`/senders aren't reborrowed inside nested `ui` closures.
fn show_name_hud(
    mut contexts: EguiContexts,
    mut hud: ResMut<HudState>,
    local: Query<&LocalId, With<Connected>>,
    players: Query<(&NetPlayer, &NetName), RenderedAvatar>,
    mut name_sender: Query<&mut MessageSender<SetName>, With<Connected>>,
    mut reset_sender: Query<&mut MessageSender<ResetPosition>, With<Connected>>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    let my_id = crate::net::my_netcode_id(&local);
    let connected = my_id.is_some();
    // Dedup replicated copies into one row per player, ordered by id for stability.
    let mut roster: BTreeMap<u64, String> = BTreeMap::new();
    for (player, name) in &players {
        roster
            .entry(player.client_id)
            .or_insert_with(|| name.0.clone());
    }

    // Menu actions, applied after the egui closures (see the doc comment).
    let mut rename_to: Option<String> = None;
    // On web, a submitted name arrives (a later frame) from the non-blocking DOM
    // rename overlay opened below; collect it here to send like any other rename.
    #[cfg(target_arch = "wasm32")]
    if let Some(name) = crate::platform::take_name_edit() {
        rename_to = Some(name);
    }
    let mut toggle_menu = false;
    let mut close_menu = false;
    let mut toggle_help = false;
    let mut open_rename = false;
    let mut do_reset = false;

    // Top-left: hamburger menu (connected only) + help toggle, both drawn large.
    egui::Area::new(egui::Id::new("bs_top_left"))
        .anchor(Align2::LEFT_TOP, egui::vec2(8.0, 8.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if connected && hamburger_button(ui, HUD_ICON_SIZE).clicked() {
                    toggle_menu = true;
                }
                if glyph_button(ui, HUD_ICON_SIZE, "?").clicked() {
                    toggle_help = true;
                }
            });
            if connected && hud.show_menu {
                let menu = Frame::default()
                    .fill(Color32::from_black_alpha(160))
                    .inner_margin(egui::Margin::same(6))
                    .show(ui, |ui| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                        if ui.button("Change Name").clicked() {
                            open_rename = true;
                        }
                        if ui.button("Reset Position").clicked() {
                            do_reset = true;
                        }
                    });
                // A click anywhere outside the open menu collapses it.
                if menu.response.clicked_elsewhere() {
                    close_menu = true;
                }
            }
        });

    if toggle_menu {
        hud.show_menu = !hud.show_menu;
    }
    if close_menu {
        hud.show_menu = false;
    }
    if toggle_help {
        hud.show_help = !hud.show_help;
    }
    if open_rename {
        hud.show_menu = false;
        let current = my_id
            .and_then(|id| roster.get(&id).cloned())
            .unwrap_or_default();
        // On web, open the non-blocking DOM rename overlay (raises the mobile keyboard
        // without freezing the loop; the result is polled above next frame). On native,
        // open the egui modal (desktop text entry works fine).
        #[cfg(target_arch = "wasm32")]
        {
            crate::platform::begin_name_edit(&current);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            hud.editing = current;
            hud.show_change_modal = true;
        }
    }
    if do_reset {
        hud.show_menu = false;
        // Keep whatever name we currently have across the reset (native no-op).
        if let Some(name) = my_id.and_then(|id| roster.get(&id).cloned()) {
            crate::platform::store_name(&name);
        }
        // On web, reload fresh: this drops the resume id so the server spawns us at a
        // new spawn point instead of recalling our last position (the name is restored
        // on connect). On native, teleport server-side via `ResetPosition`.
        #[cfg(target_arch = "wasm32")]
        {
            let _ = &mut reset_sender; // web reloads rather than messaging the dropped link
            crate::platform::reset_position_reload();
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(mut sender) = reset_sender.single_mut() {
            sender.send::<ControlChannel>(ResetPosition);
        }
    }

    // The native rename modal (never opened on web — the prompt above handles it).
    if hud.show_change_modal {
        let mut save = false;
        let mut close = false;
        egui::Window::new("Change name")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Enter a new name:");
                let field =
                    ui.add(egui::TextEdit::singleline(&mut hud.editing).char_limit(MAX_NAME_LEN));
                // Enter in the field submits, like clicking Save.
                if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    save = true;
                }
                ui.horizontal(|ui| {
                    save |= ui.button("Save").clicked();
                    close |= ui.button("Cancel").clicked();
                });
            });
        if save {
            rename_to = Some(hud.editing.clone());
        }
        if save || close {
            hud.show_change_modal = false;
        }
    }

    // Send the committed rename once. `sanitize_name` + the empty check mirror the
    // server, so a blank rename is a no-op (keeps the current name).
    if let Some(name) = rename_to {
        let cleaned = sanitize_name(&name);
        if !cleaned.is_empty() {
            // Persist so it survives a reload / reconnect (native no-op).
            crate::platform::store_name(&cleaned);
            if let Ok(mut sender) = name_sender.single_mut() {
                sender.send::<ControlChannel>(SetName(cleaned));
            }
        }
    }

    // Top-right: the roster, self marked with an asterisk.
    if connected && !roster.is_empty() {
        egui::Area::new(egui::Id::new("bs_roster"))
            .anchor(Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0))
            .show(ctx, |ui| {
                // Translucent panel, no drop shadow (`Frame::popup` adds one).
                Frame::default()
                    .fill(Color32::from_black_alpha(160))
                    .inner_margin(egui::Margin::same(6))
                    .show(ui, |ui| {
                        // Extend to fit each name rather than wrapping it.
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                        ui.label(egui::RichText::new("Lobby").strong().underline().size(18.0));
                        for (id, name) in &roster {
                            let label = if Some(*id) == my_id {
                                format!("{name} *")
                            } else {
                                name.clone()
                            };
                            ui.label(egui::RichText::new(label).color(ROSTER_NAME_COLOR));
                        }
                    });
            });
    }
    Ok(())
}

/// Billboard each avatar's name in 2D over its head, always facing the viewer: the
/// world point above the avatar is projected to the screen (`world_to_ndc` → egui's
/// point-space `screen_rect`, which is resolution- and zoom-independent), and the
/// name is painted there. Skips avatars behind/outside the frustum (`ndc.z`) and
/// empty names (an avatar not yet assigned one). A drop shadow keeps it legible over
/// the bright scene. Restricted to the rendered copies (own `Predicted` + remote
/// `Interpolated`), so it uses each avatar's live rendered pose and draws once.
fn show_name_labels(
    mut contexts: EguiContexts,
    camera: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    local: Query<&LocalId, With<Connected>>,
    avatars: Query<(&NetPlayer, &NetName, &GlobalTransform), RenderedAvatar>,
) -> Result {
    let Ok((camera, cam_tf)) = camera.single() else {
        return Ok(());
    };
    // Our own avatar is the one we're looking out of — don't label it (we'd see our
    // own name floating in front of the camera). Everyone else gets a billboard.
    let my_id = crate::net::my_netcode_id(&local);
    let ctx = contexts.ctx_mut()?;
    // The full render surface (not the panel-shrunk content area): the 3D camera
    // renders to the whole viewport, so NDC maps onto this rect.
    let rect = ctx.viewport_rect();
    let painter =
        ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("bs_names")));
    let font = egui::FontId::proportional(16.0);
    for (net_player, name, xf) in &avatars {
        // Skip our own avatar and any not-yet-named one.
        if Some(net_player.client_id) == my_id || name.0.is_empty() {
            continue;
        }
        let world = xf.translation() + Vec3::Y * NAME_LABEL_HEIGHT;
        let Some(ndc) = camera.world_to_ndc(cam_tf, world) else {
            continue;
        };
        // NDC z outside [0,1] is behind the camera or past the far plane — don't draw
        // a label for something not on screen (a raw projection flips behind the eye).
        if !(0.0..=1.0).contains(&ndc.z) {
            continue;
        }
        let pos = egui::pos2(
            rect.min.x + (ndc.x * 0.5 + 0.5) * rect.width(),
            // NDC y is up; egui y is down.
            rect.min.y + (0.5 - ndc.y * 0.5) * rect.height(),
        );
        painter.text(
            pos + egui::vec2(1.0, 1.0),
            Align2::CENTER_BOTTOM,
            &name.0,
            font.clone(),
            Color32::from_black_alpha(190),
        );
        painter.text(
            pos,
            Align2::CENTER_BOTTOM,
            &name.0,
            font.clone(),
            Color32::WHITE,
        );
    }
    Ok(())
}

/// Re-apply a persisted display name (`platform::stored_name`) once per connection, so
/// a name chosen before an iOS reload / Reset survives the reconnect. Sends `SetName`
/// as soon as we're connected and the sender is ready; the `sent` latch resets on
/// disconnect so a background-reconnect re-applies it too. No-op on native (no stored
/// name) and in single-player (never connected).
fn restore_persisted_name(
    local: Query<&LocalId, With<Connected>>,
    mut sender: Query<&mut MessageSender<SetName>, With<Connected>>,
    mut sent: Local<bool>,
) {
    if crate::net::my_netcode_id(&local).is_none() {
        *sent = false;
        return;
    }
    if *sent {
        return;
    }
    let Some(name) = crate::platform::stored_name() else {
        *sent = true; // nothing to restore; don't keep checking this connection
        return;
    };
    let cleaned = sanitize_name(&name);
    if cleaned.is_empty() {
        *sent = true;
        return;
    }
    // Retry next frame if the sender component isn't on the link yet.
    let Ok(mut sender) = sender.single_mut() else {
        return;
    };
    sender.send::<ControlChannel>(SetName(cleaned));
    *sent = true;
}

fn capture_mouse_on_click(
    mut mouse_button_input_events: MessageReader<MouseButtonInput>,
    state: Res<State<AppState>>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    for _ev in mouse_button_input_events.read() {
        if *state.get() != AppState::InGame {
            next_state.set(AppState::InGame);
        }
    }
}
