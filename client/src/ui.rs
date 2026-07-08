use crate::AppState;
use bevy::{
    diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin},
    input::mouse::MouseButtonInput,
    prelude::*,
};
use bevy_egui::{
    egui::{self, Align, Align2, Color32, Frame, Layout},
    EguiContexts, EguiPlugin, EguiPrimaryContextPass, EguiTextureHandle,
};
use avian3d::prelude::{LinearVelocity, Position};
use bad_spaceship_shared::character::{MovementModel, MovementTuning};
use bad_spaceship_shared::net::{
    sanitize_name, ControlChannel, NetName, NetPlayer, ResetPosition, SaveGame, SetAvatar,
    SetName, MAX_NAME_LEN, MONSTER_COUNT,
};
use bad_spaceship_shared::Character;
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
            .add_systems(Startup, load_avatar_thumbnails)
            .add_systems(
                Update,
                (
                    capture_mouse_on_click.run_if(in_state(AppState::Initial)),
                    restore_persisted_name,
                    restore_persisted_avatar,
                ),
            )
            .add_systems(
                EguiPrimaryContextPass,
                // The subset fonts must be installed before the first text layout
                // anywhere (there are no other fonts — `default_fonts` is off), so
                // every egui-drawing system sits in `EguiDrawSystems` and the
                // one-shot install is ordered before the whole set.
                install_fonts.run_if(run_once).before(EguiDrawSystems),
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
                    show_avatar_picker,
                    show_name_labels,
                    show_instructions,
                    show_bottom_panel,
                    show_flight_hud,
                    // Live movement-feel tuner. Disabled by default (the movement feel
                    // is locked in — see `character::MovementTuning`'s Default); flip
                    // `SHOW_MOVEMENT_PANEL` to bring the panel back for experimentation.
                    // When on, it's shown whenever we're past the initial click-to-start
                    // screen (usable both in-game and in the desktop pause menu, where
                    // the cursor is free).
                    show_movement_panel.run_if(|s: Res<State<AppState>>| {
                        SHOW_MOVEMENT_PANEL && *s.get() != AppState::Initial
                    }),
                )
                    .in_set(EguiDrawSystems),
            );
    }
}

/// Master switch for the in-game Movement panel below. `false` hides it (the
/// movement feel is locked in via `character::MovementTuning`'s Default); set it to
/// `true` to bring the live tuner back for experimentation — the panel code is kept
/// intact for exactly that.
const SHOW_MOVEMENT_PANEL: bool = false;

/// The in-game Movement panel: a live A/B tuner for how the character accelerates.
/// Pick a model from the combo box and the sliders below reveal exactly that model's
/// knobs (plus the shared max-speed and jump controls); everything applies immediately
/// via the shared `MovementTuning` resource that the `FixedUpdate` movement systems
/// read. "Copy settings" hands the current selection to the clipboard (a DOM overlay on
/// web, stdout on native) so the feel that plays best can be reported back verbatim.
fn show_movement_panel(
    mut contexts: EguiContexts,
    mut tuning: ResMut<MovementTuning>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    egui::Window::new("Movement")
        .default_pos(egui::pos2(12.0, 96.0))
        .collapsible(true)
        .resizable(false)
        .show(ctx, |ui| {
            egui::ComboBox::from_id_salt("movement_model")
                .selected_text(tuning.model.label())
                .show_ui(ui, |ui| {
                    for model in MovementModel::ALL {
                        ui.selectable_value(&mut tuning.model, model, model.label());
                    }
                });
            ui.separator();

            ui.add(egui::Slider::new(&mut tuning.max_speed, 1.0..=40.0).text("max speed"));
            match tuning.model {
                MovementModel::Smooth => {
                    ui.add(
                        egui::Slider::new(&mut tuning.smooth_rate, 1.0..=40.0)
                            .text("snappiness (rate)"),
                    );
                    ui.add(egui::Slider::new(&mut tuning.air_control, 0.0..=1.0).text("air control"));
                }
                MovementModel::Instant => {
                    ui.add(egui::Slider::new(&mut tuning.air_control, 0.0..=1.0).text("air control"));
                }
                MovementModel::Accel => {
                    ui.add(egui::Slider::new(&mut tuning.accel, 10.0..=400.0).text("acceleration"));
                    ui.add(egui::Slider::new(&mut tuning.decel, 10.0..=400.0).text("deceleration"));
                    ui.add(egui::Slider::new(&mut tuning.air_control, 0.0..=1.0).text("air control"));
                }
                MovementModel::Source => {
                    ui.add(egui::Slider::new(&mut tuning.friction, 0.0..=20.0).text("friction"));
                    ui.add(
                        egui::Slider::new(&mut tuning.ground_accel, 1.0..=30.0).text("ground accel"),
                    );
                    ui.add(egui::Slider::new(&mut tuning.air_accel, 0.0..=20.0).text("air accel"));
                    ui.add(egui::Slider::new(&mut tuning.stop_speed, 0.0..=10.0).text("stop speed"));
                }
            }

            ui.separator();
            ui.add(egui::Slider::new(&mut tuning.jump_force, 1.0..=30.0).text("jump force"));
            ui.add(
                egui::Slider::new(&mut tuning.fall_multiplier, 0.0..=60.0).text("fall boost"),
            );

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Copy settings").clicked() {
                    crate::platform::copy_to_clipboard(&tuning.settings_string());
                }
                if ui.button("Reset").clicked() {
                    // Full reset; `seed_movement_tuning` then re-applies the RON
                    // max-speed / jump-force next frame.
                    *tuning = MovementTuning::default();
                }
            });
            // Manual-copy fallback: a selectable read-only view of the same string
            // (edits to this scratch copy are discarded — it's regenerated each frame).
            let mut settings = tuning.settings_string();
            ui.add(
                egui::TextEdit::multiline(&mut settings)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
        });
    Ok(())
}

/// Every system that draws egui (here and in `mobile.rs`) belongs to this set,
/// so `install_fonts` can be ordered before all of them.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct EguiDrawSystems;

/// The game's egui fonts: one subset of Ubuntu-Light (egui's default face)
/// replacing bevy_egui's `default_fonts` — 1.4 MB of embedded fonts, the wasm
/// binary's largest data item, cut to ~110 KB. The subset keeps full Latin
/// (incl. Extended Additional for Vietnamese), Greek, Cyrillic, and general
/// punctuation so typed player names render; other scripts and emoji fall back
/// to the font's notdef box (CJK already did with egui's defaults). Both egui
/// families map to it — the game only uses proportional text.
fn subset_fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::empty();
    fonts.font_data.insert(
        "ubuntu-subset".to_owned(),
        egui::FontData::from_static(include_bytes!("../fonts/ubuntu-light-game-subset.ttf"))
            .into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.insert(family, vec!["ubuntu-subset".to_owned()]);
    }
    fonts
}

/// One-shot (`run_once`): install the subset fonts on the egui context.
fn install_fonts(mut contexts: EguiContexts) -> Result {
    contexts.ctx_mut()?.set_fonts(subset_fonts());
    Ok(())
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

/// Altimeter + speedometer, shown once the player is meaningfully off the ground
/// (a rocket ride, or just standing on a tall build). **True** values, not local
/// coordinates: rooms flying under a floating-origin rebase keep their local
/// coordinates near the origin on purpose. Reads the client's *visual* frame
/// mirror ([`ClientRoomFrame`]) rather than the replicated `NetRoomFrame` — the
/// mirror absorbs the few-frame gap between the world snapping to a rebase and
/// the frame replicating, so the readout can't blip by a rebase chunk.
/// Single-player has no netcode (the resource is absent ⇒ zero frame), so it
/// reads plain local coordinates.
fn show_flight_hud(
    mut contexts: EguiContexts,
    character: Query<(&Position, &LinearVelocity), With<Character>>,
    frame: Option<Res<crate::net::ClientRoomFrame>>,
    mut smoothed_speed: Local<f32>,
) -> Result {
    /// Below this true altitude the readout is clutter (the pad, block towers,
    /// jumping), not flight.
    const SHOW_ABOVE_M: f64 = 30.0;
    let Ok((position, velocity)) = character.single() else {
        return Ok(());
    };
    // The reconciled frame offset keeps `offset + local` continuous through a
    // rebase; see `sync_visual_room_frame`.
    let (frame_offset_y, frame_velocity) =
        frame.map(|f| (f.offset.y, f.velocity)).unwrap_or((0.0, Vec3::ZERO));
    let altitude = frame_offset_y + position.0.y as f64;
    if altitude < SHOW_ABOVE_M {
        return Ok(());
    }
    // The two velocity halves swap magnitude at a rebase (the boost moves from
    // local to frame) 1-2 frames apart on the wire — smooth the *displayed*
    // number over ~0.2 s so that window can't flicker the readout.
    let raw_speed = (frame_velocity + velocity.0).length();
    *smoothed_speed += (raw_speed - *smoothed_speed) * 0.08;
    if (raw_speed - *smoothed_speed).abs() < 2.0 {
        *smoothed_speed = raw_speed;
    }
    let speed = *smoothed_speed;
    let altitude_text = if altitude < 10_000.0 {
        format!("{altitude:.0} m")
    } else {
        format!("{:.1} km", altitude / 1000.0)
    };
    let speed_text = if speed < 3000.0 {
        format!("{speed:.0} m/s")
    } else {
        format!("{:.2} km/s", speed / 1000.0)
    };
    let ctx = contexts.ctx_mut()?;
    egui::Area::new(egui::Id::new("bs_flight_hud"))
        .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 44.0))
        .show(ctx, |ui| {
            // Translucent panel, no drop shadow — matches the roster/instructions.
            Frame::default()
                .fill(Color32::from_black_alpha(160))
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    // ASCII only (the subset fonts carry no ▲/↑ glyphs) and
                    // never wrapped (the Area would fold "m/s" onto a second line).
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("Alt {altitude_text} · {speed_text}"))
                                .size(18.0)
                                .color(Color32::WHITE),
                        )
                        .wrap_mode(egui::TextWrapMode::Extend),
                    );
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
    /// The name-this-save modal is open (native; web uses the DOM overlay).
    show_save_modal: bool,
    /// The avatar-picker modal is open.
    show_avatar_modal: bool,
    /// The instructions overlay is revealed (toggled by the "?" button).
    show_help: bool,
    /// Live contents of the rename text field.
    editing: String,
    /// Live contents of the save-name text field.
    save_editing: String,
    /// Seconds left on the "Game saved" confirmation toast.
    save_flash: f32,
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

/// One centred native text-prompt modal — the shared body of the "Change name"
/// and "Save game" forms: a single-line field (capped at [`MAX_NAME_LEN`]) where
/// Enter submits, plus Save/Cancel buttons. Returns `(submitted, closed)`;
/// `closed` is true on either outcome so the caller clears its open flag.
fn text_prompt_modal(
    ctx: &egui::Context,
    title: &str,
    label: &str,
    editing: &mut String,
) -> (bool, bool) {
    let mut save = false;
    let mut close = false;
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(label);
            let field = ui.add(egui::TextEdit::singleline(editing).char_limit(MAX_NAME_LEN));
            // Enter in the field submits, like clicking Save.
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                save = true;
            }
            ui.horizontal(|ui| {
                save |= ui.button("Save").clicked();
                close |= ui.button("Cancel").clicked();
            });
        });
    (save, save || close)
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
    time: Res<Time>,
    local: Query<&LocalId, With<Connected>>,
    players: Query<(&NetPlayer, &NetName), RenderedAvatar>,
    mut name_sender: Query<&mut MessageSender<SetName>, With<Connected>>,
    mut reset_sender: Query<&mut MessageSender<ResetPosition>, With<Connected>>,
    mut save_sender: Query<&mut MessageSender<SaveGame>, With<Connected>>,
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
    let mut save_as: Option<String> = None;
    // On web, a submitted name arrives (a later frame) from the non-blocking DOM
    // text overlay opened below; collect it here to send like any other rename/save.
    #[cfg(target_arch = "wasm32")]
    {
        use crate::platform::TextPrompt;
        if let Some(name) = crate::platform::take_text_edit(TextPrompt::Name) {
            rename_to = Some(name);
        }
        if let Some(name) = crate::platform::take_text_edit(TextPrompt::SaveGame) {
            save_as = Some(name);
        }
    }
    let mut toggle_menu = false;
    let mut close_menu = false;
    let mut toggle_help = false;
    let mut open_rename = false;
    let mut open_save = false;
    let mut open_avatar = false;
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
                        if ui.button("Change Avatar").clicked() {
                            open_avatar = true;
                        }
                        if ui.button("Save Game").clicked() {
                            open_save = true;
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
            crate::platform::begin_text_edit(crate::platform::TextPrompt::Name, "Enter your name", &current);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            hud.editing = current;
            hud.show_change_modal = true;
        }
    }
    if open_save {
        hud.show_menu = false;
        // Text entry: same web/native split as rename (the DOM overlay raises the
        // mobile keyboard without freezing the loop).
        #[cfg(target_arch = "wasm32")]
        {
            crate::platform::begin_text_edit(crate::platform::TextPrompt::SaveGame, "Name this save", "");
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            hud.save_editing.clear();
            hud.show_save_modal = true;
        }
    }
    if open_avatar {
        hud.show_menu = false;
        // The picker is a plain thumbnail grid (no text entry), so the same egui modal
        // works on web and native — unlike rename, no DOM overlay is needed.
        hud.show_avatar_modal = true;
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

    // The native rename + save-name modals (never opened on web — the DOM overlay
    // handles both there). One shared modal body (`text_prompt_modal`); save names
    // share the rename's length cap and sanitize rules, so the forms can't drift.
    if hud.show_change_modal {
        let (submit, close) = text_prompt_modal(ctx, "Change name", "Enter a new name:", &mut hud.editing);
        if submit {
            rename_to = Some(hud.editing.clone());
        }
        if close {
            hud.show_change_modal = false;
        }
    }
    if hud.show_save_modal {
        let (submit, close) =
            text_prompt_modal(ctx, "Save game", "Name this save:", &mut hud.save_editing);
        if submit {
            save_as = Some(hud.save_editing.clone());
        }
        if close {
            hud.show_save_modal = false;
        }
    }

    // Send the committed manual save once (same sanitize rules as the server; a
    // blank name is a no-op), and flash a brief confirmation toast.
    if let Some(name) = save_as {
        let cleaned = sanitize_name(&name);
        if !cleaned.is_empty() {
            if let Ok(mut sender) = save_sender.single_mut() {
                sender.send::<ControlChannel>(SaveGame(cleaned));
                hud.save_flash = 2.0;
            }
        }
    }

    // The "Game saved" confirmation toast (top-centre, fades out by timer).
    if hud.save_flash > 0.0 {
        hud.save_flash -= time.delta_secs();
        egui::Area::new(egui::Id::new("bs_save_toast"))
            .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 48.0))
            .show(ctx, |ui| {
                Frame::default()
                    .fill(Color32::from_black_alpha(160))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                        ui.label(egui::RichText::new("Game saved").size(18.0));
                    });
            });
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

/// Re-apply a persisted avatar pick (`platform::stored_avatar`) once per connection, so
/// an avatar chosen before an iOS reload / Reset survives the reconnect. The mirror of
/// `restore_persisted_name`: sends `SetAvatar` as soon as we're connected and the sender
/// is ready; the `sent` latch resets on disconnect so a background-reconnect re-applies
/// it too. No-op on native (no stored avatar) and in single-player (never connected).
fn restore_persisted_avatar(
    local: Query<&LocalId, With<Connected>>,
    mut sender: Query<&mut MessageSender<SetAvatar>, With<Connected>>,
    mut sent: Local<bool>,
) {
    if crate::net::my_netcode_id(&local).is_none() {
        *sent = false;
        return;
    }
    if *sent {
        return;
    }
    let Some(monster) = crate::platform::stored_avatar() else {
        *sent = true; // nothing to restore; don't keep checking this connection
        return;
    };
    // Retry next frame if the sender component isn't on the link yet.
    let Ok(mut sender) = sender.single_mut() else {
        return;
    };
    sender.send::<ControlChannel>(SetAvatar(monster % MONSTER_COUNT));
    *sent = true;
}

/// Side length (points) of each avatar thumbnail in the picker grid, and how many sit
/// per row (8 avatars → two rows of four).
const AVATAR_THUMB_SIZE: f32 = 72.0;
const AVATAR_COLS: u8 = 4;

/// The loaded thumbnail image handles, one per avatar in `monster::MONSTERS` order. Kept
/// alive here (dropping a handle would unload the asset); the picker registers each as an
/// egui texture on demand. Handles load lazily, so a thumbnail simply pops in once ready.
#[derive(Resource)]
struct AvatarThumbnails(Vec<Handle<Image>>);

/// Kick off loading the eight avatar face thumbnails at startup so they're ready (or
/// nearly) by the time the picker is first opened.
fn load_avatar_thumbnails(mut commands: Commands, asset_server: Res<AssetServer>) {
    let handles = (0..MONSTER_COUNT)
        .map(|i| asset_server.load::<Image>(crate::monster::avatar_thumbnail_path(i)))
        .collect();
    commands.insert_resource(AvatarThumbnails(handles));
}

/// Draw the avatar-picker modal (opened from the "Change Avatar" menu button): a grid of
/// the monster face thumbnails. Clicking one sends a `SetAvatar` to the server — which
/// re-replicates `NetPlayer::monster`, re-dressing the avatar for everyone — and persists
/// the pick (web `localStorage`). Avatars already worn by *other* players are greyed out
/// and unclickable; the local player's current avatar is shown selected. Runs on web and
/// native alike (no text entry, so no DOM overlay).
fn show_avatar_picker(
    mut contexts: EguiContexts,
    mut hud: ResMut<HudState>,
    thumbs: Res<AvatarThumbnails>,
    local: Query<&LocalId, With<Connected>>,
    players: Query<&NetPlayer, RenderedAvatar>,
    mut avatar_sender: Query<&mut MessageSender<SetAvatar>, With<Connected>>,
) -> Result {
    if !hud.show_avatar_modal {
        return Ok(());
    }
    let my_id = crate::net::my_netcode_id(&local);
    // Split the roster: which avatar is mine (shown selected) and which are worn by
    // everyone else (greyed out). Deduping isn't needed — a doubled entry sets the same
    // flags twice.
    let mut mine: Option<u8> = None;
    let mut in_use = [false; MONSTER_COUNT as usize];
    for player in &players {
        if Some(player.client_id) == my_id {
            mine = Some(player.monster);
        } else {
            in_use[player.monster as usize % in_use.len()] = true;
        }
    }
    // Register each thumbnail as an egui texture (idempotent — `add_image` returns the
    // existing id for a handle it's already seen) BEFORE borrowing the context, since
    // both `add_image` and `ctx_mut` take `&mut contexts`.
    let texture_ids: Vec<egui::TextureId> = thumbs
        .0
        .iter()
        .map(|h| contexts.add_image(EguiTextureHandle::Strong(h.clone())))
        .collect();
    let ctx = contexts.ctx_mut()?;

    let mut picked: Option<u8> = None;
    let mut close = false;
    egui::Window::new("Choose avatar")
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::Grid::new("avatar_grid")
                .spacing([10.0, 10.0])
                .show(ui, |ui| {
                    for i in 0..MONSTER_COUNT {
                        let taken = in_use[i as usize];
                        let size = egui::vec2(AVATAR_THUMB_SIZE, AVATAR_THUMB_SIZE);
                        let source = egui::load::SizedTexture::new(texture_ids[i as usize], size);
                        // Grey out avatars another player already wears.
                        let tint = if taken { Color32::from_gray(70) } else { Color32::WHITE };
                        let image = egui::Image::new(source).tint(tint);
                        let button = egui::Button::image(image).selected(mine == Some(i));
                        ui.vertical(|ui| {
                            if ui.add_enabled(!taken, button).clicked() {
                                picked = Some(i);
                            }
                            let name = egui::RichText::new(crate::monster::avatar_name(i)).size(12.0);
                            ui.label(if taken { name.weak() } else { name });
                        });
                        if (i + 1) % AVATAR_COLS == 0 {
                            ui.end_row();
                        }
                    }
                });
            ui.separator();
            if ui.button("Close").clicked() {
                close = true;
            }
        });

    if let Some(monster) = picked {
        // Persist so it survives a reload / reconnect (native no-op), then request it.
        crate::platform::store_avatar(monster);
        if let Ok(mut sender) = avatar_sender.single_mut() {
            sender.send::<ControlChannel>(SetAvatar(monster));
        }
        close = true;
    }
    if close {
        hud.show_avatar_modal = false;
    }
    Ok(())
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
