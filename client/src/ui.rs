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
use once_cell::sync::Lazy;
use shadow_rs::shadow;

shadow!(build);
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        // bevy_egui 0.35 deprecated single-pass mode (the old
        // `enable_multipass_for_primary_context` flag) and made multipass the only
        // supported path, so add the plugin with `default()`. Multipass requires
        // systems that touch an egui context to run in the dedicated
        // `EguiPrimaryContextPass` schedule rather than `Update`, and
        // `EguiContexts::ctx_mut()` returns a `Result` (those systems are fallible
        // and use `?`). `update_ui_scale_factor` now drives the egui zoom factor
        // (bevy_egui 0.40 removed `EguiContextSettings::scale_factor`), so it moved
        // into the egui pass too. Systems that only read input (not the context),
        // like `capture_mouse_on_click`, stay in `Update`.
        app.add_plugins((EguiPlugin::default(), FrameTimeDiagnosticsPlugin::default()))
            .add_systems(
                Update,
                capture_mouse_on_click.run_if(in_state(AppState::Initial)),
            )
            .add_systems(
                EguiPrimaryContextPass,
                (
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
    // bevy_egui 0.40 / egui 0.34 removed `EguiContextSettings::scale_factor`; UI
    // scaling is now egui's per-context *zoom factor*. Drive it from our own
    // Ctrl +/- handler and turn off egui's built-in keyboard zoom so the two don't
    // both react to the same keypress.
    let ctx = contexts.ctx_mut()?;
    ctx.options_mut(|options| options.zoom_with_keyboard = false);
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

fn show_bottom_panel(
    mut contexts: EguiContexts,
    diagnostics: Res<DiagnosticsStore>,
) -> Result {
    let mut fps = 0.0;
    // Bevy 0.13 replaced `DiagnosticId` with `DiagnosticPath`; `get` takes `&path`.
    if let Some(fps_diagnostic) = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS) {
        if let Some(fps_avg) = fps_diagnostic.average() {
            fps = fps_avg;
        }
    }
    egui::TopBottomPanel::bottom("bottom_panel")
        .frame(Frame::default().multiply_with_opacity(0.0))
        // Drop the hairline divider egui draws at the panel's edge.
        .show_separator_line(false)
        .show(contexts.ctx_mut()?, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(
                    Color32::from_rgb(255, 0, 0),
                    format!(
                        "Commit: {}, Built: {} ({} ago)",
                        env!("SHORT_GIT_HASH"),
                        build::BUILD_TIME_2822,
                        commit_age(),
                    ),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.colored_label(Color32::from_rgb(255, 0, 0), format!("{:.0} FPS", fps,));
                });
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
Hold block to touch another to display potential joints (in yellow).
Shift-click to make potential joints real (displayed in blue).
While not holding block, hold shift to display deletion zone.
Hold deletion zone over existing joint to highlight it in red.
Click while joint is highlighted red to delete it.
";

const TOUCH_INSTRUCTIONS: &str = "Instructions:
Left stick to move, right stick to look (drag from where you touch).
Aim a block within range and tap GRAB to pick it up; tap again to drop.
Carry a block to touch another to show potential joints (yellow),
then tap Join Parts to make them real (blue).
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
