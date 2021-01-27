use crate::utils::html;
use crate::{AppState, APP_STATE};
use bevy::prelude::*;
use bevy_webgl2::renderer::JsCast;
use gloo::events::EventListener;
use std::sync::{
    atomic::{AtomicBool, AtomicI32, Ordering::SeqCst},
    Arc,
};
use web_sys::MouseEvent;

use super::{FocusedInteractable, Holding, Player};

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_resource(WasmMouseMovementTracker::new())
            .add_resource(WasmMouseClickTracker::new())
            .on_state_update(APP_STATE, AppState::InGame, process_mouse_clicks.system());
    }
}

pub struct WasmMouseMovementTracker {
    delta_x: Arc<AtomicI32>,
    delta_y: Arc<AtomicI32>,
}

impl WasmMouseMovementTracker {
    pub fn new() -> Self {
        let delta_x = Arc::new(AtomicI32::new(0));
        let delta_y = Arc::new(AtomicI32::new(0));

        let dx = Arc::clone(&delta_x);
        let dy = Arc::clone(&delta_y);
        let on_move = EventListener::new(&html::get_document(), "mousemove", move |_event| {
            let me = _event.clone().dyn_into::<MouseEvent>().unwrap();
            dx.store(me.movement_x(), SeqCst);
            dy.store(me.movement_y(), SeqCst);
        });
        on_move.forget();

        Self { delta_x, delta_y }
    }

    pub fn get_delta_and_reset(&self) -> super::Vec2 {
        let delta = super::Vec2::new(
            self.delta_x.load(SeqCst) as f32,
            self.delta_y.load(SeqCst) as f32,
        );
        self.delta_x.store(0, SeqCst);
        self.delta_y.store(0, SeqCst);
        delta
    }
}

pub fn get_look(wasm_mouse_tracker: Res<WasmMouseMovementTracker>) -> Vec2 {
    wasm_mouse_tracker.get_delta_and_reset() * 0.15
}

pub struct WasmMouseClickTracker {
    just_pressed: Arc<AtomicBool>,
}

impl WasmMouseClickTracker {
    pub fn new() -> Self {
        let just_pressed = Arc::new(AtomicBool::new(false));

        let just_pressed_clone = Arc::clone(&just_pressed);
        let on_click = EventListener::new(&html::get_body(), "mousedown", move |_event| {
            let me = _event.clone().dyn_into::<MouseEvent>().unwrap();
            // Wait for left click specifically
            if me.button() == 0 {
                just_pressed_clone.store(true, SeqCst);
            }
        });
        on_click.forget();

        Self { just_pressed }
    }

    pub fn just_pressed(&self) -> bool {
        let just_pressed = self.just_pressed.load(SeqCst);
        if just_pressed {
            self.just_pressed.store(false, SeqCst);
        }
        just_pressed
    }
}

fn process_mouse_clicks(
    mouse_button_input: Res<WasmMouseClickTracker>,
    mut players: Query<(&mut Holding, &FocusedInteractable), With<Player>>,
) {
    if mouse_button_input.just_pressed() {
        if let Some((mut holding, interactable)) = players.iter_mut().next() {
            if let Some(_current_interactable) = interactable.current {
                holding.0 = !holding.0;
            }
        }
    }
}
