use crate::utils::html;
use bevy::prelude::*;
use bevy_webgl2::renderer::JsCast;
use gloo::events::EventListener;
use std::sync::{
    atomic::{AtomicBool, AtomicI32, Ordering::SeqCst},
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

pub fn process_mouse_clicks(mouse_button_input: Res<WasmMouseClickTracker>) -> Option<MouseButton> {
    if mouse_button_input.just_pressed() {
        Some(MouseButton::Left)
    } else {
        None
    }
}

pub struct WasmKeyboardTracker {
    w: Arc<AtomicBool>,
    s: Arc<AtomicBool>,
    d: Arc<AtomicBool>,
    a: Arc<AtomicBool>,
    space: Arc<AtomicBool>,
}

impl WasmKeyboardTracker {
    pub fn new() -> Self {
        let w = Arc::new(AtomicBool::new(false));
        let s = Arc::new(AtomicBool::new(false));
        let d = Arc::new(AtomicBool::new(false));
        let a = Arc::new(AtomicBool::new(false));
        let space = Arc::new(AtomicBool::new(false));

        let w1 = Arc::clone(&w);
        let s1 = Arc::clone(&s);
        let d1 = Arc::clone(&d);
        let a1 = Arc::clone(&a);
        let space1 = Arc::clone(&space);
        let on_press = EventListener::new(&html::get_document(), "keydown", move |_event| {
            let ke = _event.clone().dyn_into::<KeyboardEvent>().unwrap();
            match ke.key().as_ref() {
                "w" => {
                    w1.store(true, SeqCst);
                }
                "s" => {
                    s1.store(true, SeqCst);
                }
                "d" => {
                    d1.store(true, SeqCst);
                }
                "a" => {
                    a1.store(true, SeqCst);
                }
                " " => {
                    space1.store(true, SeqCst);
                }
                _ => {}
            }
        });
        on_press.forget();

        let w2 = Arc::clone(&w);
        let s2 = Arc::clone(&s);
        let d2 = Arc::clone(&d);
        let a2 = Arc::clone(&a);
        let space2 = Arc::clone(&space);
        let on_press = EventListener::new(&html::get_document(), "keyup", move |_event| {
            let ke = _event.clone().dyn_into::<KeyboardEvent>().unwrap();
            match ke.key().as_ref() {
                "w" => {
                    w2.store(false, SeqCst);
                }
                "s" => {
                    s2.store(false, SeqCst);
                }
                "d" => {
                    d2.store(false, SeqCst);
                }
                "a" => {
                    a2.store(false, SeqCst);
                }
                " " => {
                    space2.store(false, SeqCst);
                }
                _ => {}
            }
        });
        on_press.forget();

        Self { w, s, d, a, space }
    }

    pub fn get(&self) -> super::Vec3 {
        let mut direction = Vec3::ZERO;

        if self.w.load(SeqCst) {
            direction.z += 1.;
        }

        if self.s.load(SeqCst) {
            direction.z -= 1.;
        }

        if self.d.load(SeqCst) {
            direction.x += 1.;
        }

        if self.a.load(SeqCst) {
            direction.x -= 1.;
        }

        if self.space.load(SeqCst) {
            direction.y += 1.;
        }

        direction.normalize_or_zero()
    }
}

pub fn process_keyboard_events(keyboard_input: Res<WasmKeyboardTracker>) -> Vec3 {
    keyboard_input.get()
}
