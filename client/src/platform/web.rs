use crate::AppState;
use bad_spaceship_shared::{InputEvents, KeyboardDirectionalInput, MouseMotionDelta, PlayerClick};
use bevy::{
    input::mouse::{MouseScrollUnit, MouseWheel},
    prelude::*,
};
use bevy_webgl2::renderer::JsCast;
use gloo::events::EventListener;
use std::sync::{
    atomic::{AtomicBool, AtomicI32, Ordering::SeqCst},
    Arc,
};
use web_sys::{Document, Element, Event, HtmlElement, KeyboardEvent, MouseEvent, WheelEvent};

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.insert_resource(WasmPointerLockTracker::new())
            .add_system_set(SystemSet::on_enter(AppState::InGame).with_system(hide_cursor.system()))
            .add_system(toggle_menu_on_pointer_lock.system())
            .insert_resource(WasmMouseMovementTracker::new())
            .insert_resource(WasmMouseClickTracker::new())
            .insert_resource(WasmKeyboardTracker::new())
            .insert_resource(WasmWheelTracker::new())
            .add_system(get_look.system())
            .add_system(process_mouse_clicks.system().label(InputEvents))
            .add_system(get_keyboard_input.system())
            .add_system(get_wheel.system().label(InputEvents));
    }
}

#[derive(Clone, Default)]
struct WasmPointerLockTracker {
    lock: Arc<AtomicBool>,
}

impl WasmPointerLockTracker {
    pub fn new() -> Self {
        let lock = Self::default();
        let lock_clone = lock.clone();
        listen("pointerlockchange", move |_event| {
            match get_document().pointer_lock_element() {
                Some(element) => {
                    if element == get_body().dyn_into::<Element>().unwrap() {
                        lock_clone.lock.set(true);
                    }
                }
                None => {
                    lock_clone.lock.set(false);
                }
            }
        });
        lock
    }

    fn get(&self) -> bool {
        self.lock.get()
    }
}

fn hide_cursor() {
    get_body().request_pointer_lock();
}

fn toggle_menu_on_pointer_lock(
    lock_state: Res<WasmPointerLockTracker>,
    mut state: ResMut<State<AppState>>,
) {
    if lock_state.get() {
        if *state.current() == AppState::InGameMenu {
            state.set(AppState::InGame).unwrap();
        }
    } else {
        if *state.current() == AppState::InGame {
            state.set(AppState::InGameMenu).unwrap();
        }
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
        let on_move = EventListener::new(&get_document(), "mousemove", move |_event| {
            let me = _event.clone().dyn_into::<MouseEvent>().unwrap();
            clone.delta_x.set(me.movement_x());
            clone.delta_y.set(me.movement_y());
        });
        on_move.forget();
        new
    }

    pub fn get_delta_and_reset(&self) -> Vec2 {
        let delta = Vec2::new(self.delta_x.get() as f32, self.delta_y.get() as f32);
        self.delta_x.set(0);
        self.delta_y.set(0);
        delta
    }
}

pub fn get_look(
    wasm_mouse_tracker: Res<WasmMouseMovementTracker>,
    mut query: Query<&mut MouseMotionDelta>,
    state: Res<State<AppState>>,
) {
    let input = match state.current() {
        AppState::InGame => wasm_mouse_tracker.get_delta_and_reset() * 0.15,
        _ => Vec2::ZERO,
    };
    for mut mouse_motion in query.iter_mut() {
        mouse_motion.0 = input;
    }
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

pub fn process_mouse_clicks(
    mouse_button_input: Res<WasmMouseClickTracker>,
    mut clicks: EventWriter<PlayerClick>,
    state: Res<State<AppState>>,
) {
    if *state.current() == AppState::InGame {
        if mouse_button_input.just_pressed() {
            clicks.send(PlayerClick);
        }
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

    pub fn get(&self) -> Vec3 {
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

pub fn get_keyboard_input(
    keyboard_input: Res<WasmKeyboardTracker>,
    mut query: Query<&mut KeyboardDirectionalInput>,
    state: Res<State<AppState>>,
) {
    let input = match state.current() {
        AppState::InGame => keyboard_input.get(),
        _ => Vec3::ZERO,
    };
    for mut keyboard_directional_input in query.iter_mut() {
        // Sum with whatever other input is also being applied (e.g. web)
        keyboard_directional_input.0 += input;
    }
}

pub fn listen<F>(event_type: &'static str, callback: F)
where
    F: FnMut(&Event) + 'static,
{
    let listener = EventListener::new(&get_document(), event_type, callback);
    listener.forget();
}

pub fn get_document() -> Document {
    let window = web_sys::window().expect("no global `window` exists");
    let document = window.document().expect("should have a document on window");
    document
}

pub fn get_body() -> HtmlElement {
    get_document().body().expect("document should have a body")
}

pub trait AtomicBoolExt {
    fn toggle(&self);
    fn set(&self, value: bool);
    fn get(&self) -> bool;
}

impl AtomicBoolExt for AtomicBool {
    fn toggle(&self) {
        self.store(!self.load(SeqCst), SeqCst);
    }

    fn set(&self, value: bool) {
        self.store(value, SeqCst);
    }

    fn get(&self) -> bool {
        self.load(SeqCst)
    }
}

pub trait AtomicI32Ext {
    fn set(&self, value: i32);
    fn get(&self) -> i32;
}

impl AtomicI32Ext for AtomicI32 {
    fn set(&self, value: i32) {
        self.store(value, SeqCst);
    }

    fn get(&self) -> i32 {
        self.load(SeqCst)
    }
}

#[derive(Clone, Default)]
pub struct WasmWheelTracker {
    delta_y: Arc<AtomicI32>,
    delta_mode: Arc<AtomicI32>,
}

impl WasmWheelTracker {
    pub fn new() -> Self {
        let new = Self::default();
        let clone = new.clone();
        listen("wheel", move |_event| {
            let we = _event.clone().dyn_into::<WheelEvent>().unwrap();
            // bevy::log::info!("{:?}", we.delta_y());
            clone.delta_y.set((we.delta_y() * 1000.0) as i32);
            clone.delta_mode.set(we.delta_mode() as i32);
        });
        new
    }

    fn get(&self) -> Option<MouseWheel> {
        match self.delta_y.get() {
            0 => None,
            y => {
                self.delta_y.set(0);
                Some(MouseWheel {
                    unit: match self.delta_mode.get() {
                        0 => MouseScrollUnit::Pixel,
                        _ => MouseScrollUnit::Line,
                    },
                    y: (y as f32) / 1000.0,
                    x: 0.0,
                })
            }
        }
    }
}

fn get_wheel(
    wheel_tracker: ResMut<WasmWheelTracker>,
    mut mouse_wheel_events: EventWriter<MouseWheel>,
) {
    if let Some(event) = wheel_tracker.get() {
        mouse_wheel_events.send(event);
    }
}
