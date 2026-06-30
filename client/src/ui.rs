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
use chrono::{DateTime, FixedOffset, Utc};
use lightyear::prelude::client::Connected;
use lightyear::prelude::{PingManager, PredictionMetrics};
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
        app.add_plugins((EguiPlugin::default(), FrameTimeDiagnosticsPlugin::default()))
            .add_systems(
                Update,
                capture_mouse_on_click.run_if(in_state(AppState::Initial)),
            )
            .add_systems(
                EguiPrimaryContextPass,
                (
                    // Touches an egui context (zoom factor), so it must run in the
                    // egui pass alongside the panel-drawing systems, not in `Update`.
                    update_ui_scale_factor,
                    show_menu.run_if(in_state(AppState::InGameMenu)),
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
then tap Join Parts to make them real (green).
Drag the open area while carrying to rotate the held block.
Empty-handed, aim at a joint until it highlights red,
then tap Delete Joints to remove it. Tap pause (top-right) for the menu.
";

fn show_instructions(
    mut contexts: EguiContexts,
    mobile: Res<crate::mobile::MobileActive>,
) -> Result {
    let text = if mobile.0 {
        TOUCH_INSTRUCTIONS
    } else {
        INSTRUCTIONS
    };
    egui::TopBottomPanel::top("top_panel")
        .frame(Frame::default().multiply_with_opacity(0.0))
        // Drop the hairline divider egui draws at the panel's edge.
        .show_separator_line(false)
        .show(contexts.ctx_mut()?, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(Color32::from_rgb(255, 0, 0), text);
            });
        });
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
