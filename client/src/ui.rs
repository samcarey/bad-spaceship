use crate::AppState;
use bevy::{
    diagnostic::{Diagnostics, FrameTimeDiagnosticsPlugin},
    input::mouse::MouseButtonInput,
    prelude::*,
};
use bevy_egui::{
    egui::{self, Align, Align2, Color32, Frame, Layout},
    EguiContext, EguiPlugin, EguiSettings,
};
use shadow_rs::shadow;

shadow!(build);
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugin(EguiPlugin)
            .add_plugin(FrameTimeDiagnosticsPlugin)
            .add_system_set(SystemSet::on_update(AppState::InGameMenu).with_system(show_menu))
            .add_system_set(
                SystemSet::on_update(AppState::Initial).with_system(capture_mouse_on_click),
            )
            .add_system(update_ui_scale_factor)
            .add_system(show_instructions)
            .add_system(show_bottom_panel);
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
    key_input: Res<Input<KeyCode>>,
    mut egui_settings: ResMut<EguiSettings>,
    mut custom_scale_factor: Local<CustomScaleFactor>,
) {
    if key_input.pressed(KeyCode::LControl) || key_input.pressed(KeyCode::RControl) {
        if let Some(adjustment) = if key_input.just_pressed(KeyCode::Equals) {
            Some(1.1)
        } else if key_input.just_pressed(KeyCode::Minus) {
            Some(1. / 1.1)
        } else {
            None
        } {
            custom_scale_factor.0 = (custom_scale_factor.0 * adjustment)
                .max(MIN_SCALE_FACTOR)
                .min(MAX_SCALE_FACTOR);
            println!("Custom scale factor set to {}", custom_scale_factor.0);
        }
    }
    egui_settings.scale_factor = 1.0 * custom_scale_factor.0;
}

#[cfg(not(target_arch = "wasm32"))]
fn align_menu(window: egui::Window) -> egui::Window {
    window.anchor(Align2::CENTER_TOP, [0., 150.])
}

#[cfg(target_arch = "wasm32")]
fn align_menu(window: egui::Window) -> egui::Window {
    window.anchor(Align2::CENTER_CENTER, [0., -70.])
}

fn show_menu(mut egui_ctx: ResMut<EguiContext>, mut state: ResMut<State<AppState>>) {
    align_menu(egui::Window::new("Bad Spaceship"))
        .collapsible(false)
        .resizable(false)
        .show(egui_ctx.ctx_mut(), |ui| {
            ui.with_layout(Layout::top_down_justified(Align::Center), |ui| {
                if ui.button("Options").clicked() {
                    bevy::log::info!("Options selected");
                }
                if ui.button("Multiplayer").clicked() {
                    bevy::log::info!("Multiplayer selected");
                }
                if ui.button("Resume").clicked() {
                    bevy::log::info!("Resume selected");
                    state.set(AppState::InGame).unwrap();
                }
            });
        });
}

fn show_bottom_panel(mut egui_ctx: ResMut<EguiContext>, diagnostics: Res<Diagnostics>) {
    let mut fps = 0.0;
    if let Some(fps_diagnostic) = diagnostics.get(FrameTimeDiagnosticsPlugin::FPS) {
        if let Some(fps_avg) = fps_diagnostic.average() {
            fps = fps_avg;
        }
    }
    egui::TopBottomPanel::bottom("bottom_panel")
        .frame(Frame::default().multiply_with_opacity(0.0))
        .show(egui_ctx.ctx_mut(), |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(
                    Color32::from_rgb(255, 0, 0),
                    format!(
                        "Commit: {}, Built: {}",
                        env!("SHORT_GIT_HASH"),
                        build::BUILD_TIME_2822
                    ),
                );
                ui.with_layout(Layout::right_to_left(), |ui| {
                    ui.colored_label(Color32::from_rgb(255, 0, 0), format!("{:.0} FPS", fps,));
                });
            });
        });
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

fn show_instructions(mut egui_ctx: ResMut<EguiContext>) {
    egui::TopBottomPanel::top("top_panel")
        .frame(Frame::default().multiply_with_opacity(0.0))
        .show(egui_ctx.ctx_mut(), |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(Color32::from_rgb(255, 0, 0), INSTRUCTIONS);
            });
        });
}

fn capture_mouse_on_click(
    mut mouse_button_input_events: EventReader<MouseButtonInput>,
    mut state: ResMut<State<AppState>>,
) {
    for _ev in mouse_button_input_events.iter() {
        if *state.current() != AppState::InGame {
            state.overwrite_set(AppState::InGame).unwrap();
        }
    }
}
