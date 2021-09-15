use crate::AppState;
use bad_spaceship_shared::{InputEvents, WebKeyCode, WebMouseButton};
use bevy::{
    input::mouse::{MouseMotion, MouseScrollUnit, MouseWheel},
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
        app.insert_resource(PointerLockTracker::new())
            .add_system_set(SystemSet::on_enter(AppState::InGame).with_system(hide_cursor.system()))
            .add_system(toggle_menu_on_pointer_lock.system())
            .insert_resource(MouseMovementTracker::new())
            .insert_resource(MouseClickTracker::new())
            .insert_resource(KeyboardTracker::new())
            .insert_resource(WheelTracker::new())
            .add_system_set(
                SystemSet::new()
                    .before(InputEvents)
                    .with_system(get_wheel.system())
                    .with_system(get_keyboard_input.system())
                    .with_system(process_mouse_clicks.system())
                    .with_system(get_mouse_motion.system()),
            );
    }
}

#[derive(Clone, Default)]
struct PointerLockTracker {
    lock: Arc<AtomicBool>,
}

impl PointerLockTracker {
    pub fn new() -> Self {
        let new = Self::default();
        let clone = new.clone();
        listen("pointerlockchange", move |_event| {
            match get_document().pointer_lock_element() {
                Some(element) => {
                    if element == get_body().dyn_into::<Element>().unwrap() {
                        clone.lock.set(true);
                    }
                }
                None => {
                    clone.lock.set(false);
                }
            }
        });
        new
    }

    fn get(&self) -> bool {
        self.lock.get()
    }
}

fn hide_cursor() {
    get_body().request_pointer_lock();
}

fn toggle_menu_on_pointer_lock(
    lock_state: Res<PointerLockTracker>,
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
pub struct MouseMovementTracker {
    delta_x: Arc<AtomicI32>,
    delta_y: Arc<AtomicI32>,
}

impl MouseMovementTracker {
    pub fn new() -> Self {
        let new = Self::default();
        let clone = new.clone();
        listen("mousemove", move |_event| {
            let me = _event.clone().dyn_into::<MouseEvent>().unwrap();
            clone.delta_x.set(me.movement_x());
            clone.delta_y.set(me.movement_y());
        });
        new
    }

    pub fn get_and_reset(&self) -> Option<MouseMotion> {
        let delta = Vec2::new(self.delta_x.get() as f32, self.delta_y.get() as f32);

        if delta != Vec2::ZERO {
            self.delta_x.set(0);
            self.delta_y.set(0);
            bevy::log::info!("reset {:?}", delta);
            Some(MouseMotion {
                delta: delta * 0.15,
            })
        } else {
            None
        }
    }
}

fn get_mouse_motion(
    mouse_motion_tracker: Res<MouseMovementTracker>,
    mut mouse_motion_events: EventWriter<MouseMotion>,
) {
    if let Some(mouse_motion) = mouse_motion_tracker.get_and_reset() {
        bevy::log::info!("get {:?}", mouse_motion);
        mouse_motion_events.send(mouse_motion);
    }
}

#[derive(Clone, Default)]
pub struct MouseClickTracker {
    just_pressed: Arc<AtomicBool>,
    x: Arc<AtomicI32>,
    y: Arc<AtomicI32>,
}

impl MouseClickTracker {
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
}

trait SetWebMouseButton {
    fn set(&mut self, atomic_bool: &Arc<AtomicBool>, key: MouseButton);
}

impl SetWebMouseButton for Input<WebMouseButton> {
    fn set(&mut self, atomic_key: &Arc<AtomicBool>, button: MouseButton) {
        if atomic_key.get() {
            self.press(WebMouseButton(button));
            atomic_key.set(false);
        } else {
            self.release(WebMouseButton(button));
        }
    }
}

pub fn process_mouse_clicks(
    mouse_button_tracker: Res<MouseClickTracker>,
    mut web_mouse_button_input: ResMut<Input<WebMouseButton>>,
) {
    web_mouse_button_input.set(&mouse_button_tracker.just_pressed, MouseButton::Left);
}

#[derive(Clone, Default)]
pub struct KeyboardTracker {
    w: Arc<AtomicBool>,
    s: Arc<AtomicBool>,
    d: Arc<AtomicBool>,
    a: Arc<AtomicBool>,
    space: Arc<AtomicBool>,
    shift: Arc<AtomicBool>,
}

impl KeyboardTracker {
    fn listen(self, event_type: &'static str, set_value: bool) {
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
            "Shift" => Some(&self.shift),
            _ => None,
        }
    }

    pub fn new() -> Self {
        let new = Self::default();
        new.clone().listen("keydown", true);
        new.clone().listen("keyup", false);
        new
    }
}

trait SetWebKey {
    fn set(&mut self, atomic_key: &Arc<AtomicBool>, key: KeyCode);
}

impl SetWebKey for Input<WebKeyCode> {
    fn set(&mut self, atomic_key: &Arc<AtomicBool>, key: KeyCode) {
        if atomic_key.get() {
            self.press(WebKeyCode(key));
        } else {
            self.release(WebKeyCode(key));
        }
    }
}

pub fn get_keyboard_input(
    keyboard_tracker: Res<KeyboardTracker>,
    mut keyboard_input: ResMut<Input<WebKeyCode>>,
) {
    keyboard_input.set(&keyboard_tracker.w, KeyCode::W);
    keyboard_input.set(&keyboard_tracker.s, KeyCode::S);
    keyboard_input.set(&keyboard_tracker.d, KeyCode::D);
    keyboard_input.set(&keyboard_tracker.a, KeyCode::A);
    keyboard_input.set(&keyboard_tracker.space, KeyCode::Space);
    keyboard_input.set(&keyboard_tracker.shift, KeyCode::LShift);
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
pub struct WheelTracker {
    delta_y: Arc<AtomicI32>,
    delta_mode: Arc<AtomicI32>,
}

impl WheelTracker {
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

fn get_wheel(wheel_tracker: ResMut<WheelTracker>, mut mouse_wheel_events: EventWriter<MouseWheel>) {
    if let Some(event) = wheel_tracker.get() {
        mouse_wheel_events.send(event);
    }
}
