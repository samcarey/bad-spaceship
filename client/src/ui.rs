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
    fn build(&self, app: &mut AppBuilder) {
        app.add_plugin(EguiPlugin)
            .add_plugin(FrameTimeDiagnosticsPlugin)
            .add_system_set(
                SystemSet::on_update(AppState::InGameMenu).with_system(show_menu.system()),
            )
            .add_system_set(
                SystemSet::on_update(AppState::Initial)
                    .with_system(capture_mouse_on_click.system()),
            )
            .add_system(update_ui_scale_factor.system())
            .add_system(show_instructions.system())
            .add_system(show_bottom_panel.system());
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

fn show_menu(egui_ctx: ResMut<EguiContext>, mut state: ResMut<State<AppState>>) {
    egui::Window::new("Bad Spaceship")
        .anchor(Align2::CENTER_CENTER, [0., 0.])
        .collapsible(false)
        .resizable(false)
        .show(egui_ctx.ctx(), |ui| {
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

fn show_bottom_panel(egui_ctx: ResMut<EguiContext>, diagnostics: Res<Diagnostics>) {
    let mut fps = 0.0;
    if let Some(fps_diagnostic) = diagnostics.get(FrameTimeDiagnosticsPlugin::FPS) {
        if let Some(fps_avg) = fps_diagnostic.average() {
            fps = fps_avg;
        }
    }
    egui::TopBottomPanel::bottom("bottom_panel")
        .frame(Frame::default().multiply_with_opacity(0.0))
        .show(egui_ctx.ctx(), |ui| {
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
Press WSDA keys to move.
Move mouse to look.
Click while aiming at block within range to  it pick up.
Click again to drop it.
Hold shift and move mouse or scroll to rotate a held block.
Hold shift while held block is touching another to display potential joints (in yellow).
Shift click to make potential joints real (displayed in blue).
While not holding block, hold control to display deletion zone.
Hold deletion zone over existing joint to highlight it in red.
Click while joint is highlighted red to delete it.
";

fn show_instructions(egui_ctx: ResMut<EguiContext>) {
    egui::TopBottomPanel::top("top_panel")
        .frame(Frame::default().multiply_with_opacity(0.0))
        .show(egui_ctx.ctx(), |ui| {
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
