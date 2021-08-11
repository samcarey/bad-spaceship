use crate::utils::{html, listen, AtomicBoolExt, AtomicI32Ext};
use bevy::prelude::*;
use bevy_webgl2::renderer::JsCast;
use gloo::events::EventListener;
use std::sync::{
    atomic::{AtomicBool, AtomicI32},
    Arc,
};
use web_sys::{KeyboardEvent, MouseEvent};

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.insert_resource(WasmMouseMovementTracker::new())
            .insert_resource(WasmMouseClickTracker::new())
            .insert_resource(WasmKeyboardTracker::new());
    }
}

#[derive(Clone, Default)]
pub struct WasmMouseMovementTracker {
    delta_x: Arc<AtomicI32>,
    delta_y: Arc<AtomicI32>,
}

impl WasmMouseMovementTracker {
    pub fn new() -> Self {
        let new = Self::default();

        let clone = new.clone();
        let on_move = EventListener::new(&html::get_document(), "mousemove", move |_event| {
            let me = _event.clone().dyn_into::<MouseEvent>().unwrap();
            clone.delta_x.set(me.movement_x());
            clone.delta_y.set(me.movement_y());
        });
        on_move.forget();
        new
    }

    pub fn get_delta_and_reset(&self) -> super::Vec2 {
        let delta = super::Vec2::new(self.delta_x.get() as f32, self.delta_y.get() as f32);
        self.delta_x.set(0);
        self.delta_y.set(0);
        delta
    }
}

pub fn get_look(wasm_mouse_tracker: Res<WasmMouseMovementTracker>) -> Vec2 {
    wasm_mouse_tracker.get_delta_and_reset() * 0.15
}

#[derive(Clone, Default)]
pub struct WasmMouseClickTracker {
    just_pressed: Arc<AtomicBool>,
    x: Arc<AtomicI32>,
    y: Arc<AtomicI32>,
}

impl WasmMouseClickTracker {
    pub fn new() -> Self {
        let new = Self::default();
        let clone = new.clone();
        listen("pointerdown", move |_event| {
            let me = _event.clone().dyn_into::<MouseEvent>().unwrap();
            // Wait for left click specifically
            if me.button() == 0 {
                clone.just_pressed.set(true);
                clone.x.set(me.client_x());
                clone.y.set(me.client_y());
            }
        });
        new
    }

    pub fn just_pressed(&self) -> bool {
        let just_pressed = self.just_pressed.get();
        if just_pressed {
            self.just_pressed.set(false);
        }
        just_pressed
    }
}

pub fn process_mouse_clicks(mouse_button_input: Res<WasmMouseClickTracker>) -> Option<MouseButton> {
    if mouse_button_input.just_pressed() {
        Some(MouseButton::Left)
    } else {
        None
    }
}

#[derive(Clone, Default)]
pub struct WasmKeyboardTracker {
    w: Arc<AtomicBool>,
    s: Arc<AtomicBool>,
    d: Arc<AtomicBool>,
    a: Arc<AtomicBool>,
    space: Arc<AtomicBool>,
}

impl WasmKeyboardTracker {
    pub fn listen(self, event_type: &'static str, set_value: bool) {
        listen(&event_type, move |_event| {
            let cast_event = _event.clone().dyn_into::<KeyboardEvent>().unwrap();
            if let Some(arc) = self.key_to_arc(&cast_event.key()) {
                arc.set(set_value);
            }
        });
    }

    fn key_to_arc(&self, key: &str) -> Option<&Arc<AtomicBool>> {
        match key {
            "w" => Some(&self.w),
            "s" => Some(&self.s),
            "d" => Some(&self.d),
            "a" => Some(&self.a),
            " " => Some(&self.space),
            _ => None,
        }
    }

    pub fn new() -> Self {
        let new = Self::default();
        new.clone().listen("keydown", true);
        new.clone().listen("keyup", false);
        new
    }

    pub fn get(&self) -> super::Vec3 {
        let mut direction = Vec3::ZERO;
        if self.w.get() {
            direction.z += 1.;
        }
        if self.s.get() {
            direction.z -= 1.;
        }
        if self.d.get() {
            direction.x += 1.;
        }
        if self.a.get() {
            direction.x -= 1.;
        }
        if self.space.get() {
            direction.y += 1.;
        }
        direction.normalize_or_zero()
    }
}

pub fn get_keyboard_input(keyboard_input: Res<WasmKeyboardTracker>) -> Vec3 {
    keyboard_input.get()
}
