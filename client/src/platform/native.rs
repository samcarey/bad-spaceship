use crate::AppState;
use bad_spaceship_shared::{MouseMotionDelta, PlayerClick};
use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(show_cursor.system())
            .add_system_set(
                SystemSet::on_enter(AppState::InGameMenu).with_system(show_cursor.system()),
            )
            .add_system_set(SystemSet::on_enter(AppState::InGame).with_system(hide_cursor.system()))
            .add_system(toggle_menu_on_key.system())
            .add_event::<PlayerClick>()
            .add_system(get_look.system())
            .add_system_set(
                SystemSet::on_update(AppState::InGame).with_system(process_mouse_clicks.system()),
            );
    }
}

pub fn get_look(
    mut mouse_motion_events: EventReader<MouseMotion>,
    mut mouse_deltas: Query<&mut MouseMotionDelta>,
    state: Res<bevy::prelude::State<AppState>>,
) {
    let motion = match state.current() {
        AppState::InGame => match mouse_motion_events.iter().last() {
            Some(event) => event.delta,
            None => Vec2::ZERO,
        },
        _ => Vec2::ZERO,
    };
    for mut mouse_delta in mouse_deltas.iter_mut() {
        *mouse_delta = MouseMotionDelta(motion);
    }
}

pub fn process_mouse_clicks(
    mouse_button_input: Res<Input<MouseButton>>,
    mut clicks: EventWriter<PlayerClick>,
) {
    if mouse_button_input.just_pressed(MouseButton::Left) {
        clicks.send(PlayerClick);
    }
}

fn show_cursor(mut windows: ResMut<Windows>) {
    let window = windows.get_primary_mut().unwrap();
    window.set_cursor_lock_mode(false);
    window.set_cursor_visibility(true);
}

fn hide_cursor(mut windows: ResMut<Windows>) {
    let window = windows.get_primary_mut().unwrap();
    window.set_cursor_lock_mode(true);
    window.set_cursor_visibility(false);
}

fn toggle_menu_on_key(
    input: Res<Input<KeyCode>>,
    mut state: ResMut<bevy::prelude::State<AppState>>,
) {
    if input.just_pressed(KeyCode::Escape) {
        match state.current() {
            AppState::InGame => state.set(AppState::InGameMenu).unwrap(),
            AppState::InGameMenu => state.set(AppState::InGame).unwrap(),
            _ => {}
        }
    }
}
