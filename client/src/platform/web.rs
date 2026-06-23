use crate::AppState;
use bad_spaceship_shared::{Grass, InputEvents, WebKeyCode, WebMouseButton};
use bevy::{
    input::mouse::{MouseMotion, MouseScrollUnit, MouseWheel},
    prelude::*,
};
use gloo::events::EventListener;
use std::sync::{
    atomic::{AtomicBool, AtomicI32, Ordering::SeqCst},
    Arc,
};
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, Event, HtmlElement, KeyboardEvent, MouseEvent, WheelEvent};

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(PointerLockTracker::new())
            .add_systems(OnEnter(AppState::InGame), hide_cursor)
            .add_systems(
                Update,
                (
                    toggle_menu_on_pointer_lock,
                    signal_game_ready,
                    (
                        get_wheel,
                        get_keyboard_input,
                        process_mouse_clicks,
                        get_mouse_motion,
                    )
                        .before(InputEvents),
                ),
            )
            .insert_resource(MouseMovementTracker::new())
            .insert_resource(MouseClickTracker::new())
            .insert_resource(KeyboardTracker::new())
            .insert_resource(WheelTracker::new());
    }
}

#[derive(Clone, Default, Resource)]
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
                        clone.lock.store_val(true);
                    }
                }
                None => {
                    clone.lock.store_val(false);
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

/// Tells the HTML loading overlay (`client/index.html`) that the game is actually
/// on screen, so it can hide instead of cutting to a blank screen while Bevy 0.16
/// compiles its render pipelines on first use. Fires a few frames after the ground
/// mesh exists (i.e. the map has loaded and been drawn) by tagging `<body>`; the
/// loader polls for the attribute, with its own timeout as a fallback.
fn signal_game_ready(
    grass: Query<(), (With<Grass>, With<Mesh3d>)>,
    mut frames_drawn: Local<u32>,
    mut done: Local<bool>,
) {
    if *done || grass.is_empty() {
        return;
    }
    *frames_drawn += 1;
    if *frames_drawn >= 3 {
        *done = true;
        let _ = get_body().set_attribute("data-game-ready", "1");
    }
}

fn toggle_menu_on_pointer_lock(
    lock_state: Res<PointerLockTracker>,
    state: Res<State<AppState>>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    if lock_state.get() {
        if *state.get() == AppState::InGameMenu {
            next_state.set(AppState::InGame);
        }
    } else {
        if *state.get() == AppState::InGame {
            next_state.set(AppState::InGameMenu);
        }
    }
}

#[derive(Clone, Default, Resource)]
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
            clone.delta_x.store_val(me.movement_x());
            clone.delta_y.store_val(me.movement_y());
        });
        new
    }

    pub fn get_and_reset(&self) -> Option<MouseMotion> {
        let delta = Vec2::new(self.delta_x.get() as f32, self.delta_y.get() as f32);

        if delta != Vec2::ZERO {
            self.delta_x.store_val(0);
            self.delta_y.store_val(0);
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
    mut mouse_motion_events: ResMut<Events<MouseMotion>>,
) {
    // Bevy 0.13 bumped winit 0.28 → 0.29, which (unlike 0.28) emits its own
    // `MouseMotion` on the web canvas under pointer lock. That phantom stream
    // fights this crate's DOM-listener input and spins the camera on its own.
    // Drop winit's events each frame and drive look solely from our tracker.
    mouse_motion_events.clear();
    if let Some(mouse_motion) = mouse_motion_tracker.get_and_reset() {
        mouse_motion_events.send(mouse_motion);
    }
}

#[derive(Clone, Default, Resource)]
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
                clone.just_pressed.store_val(true);
                clone.x.store_val(me.client_x());
                clone.y.store_val(me.client_y());
            }
        });
        new
    }
}

trait SetWebMouseButton {
    fn set_state(&mut self, atomic_bool: &Arc<AtomicBool>, key: MouseButton);
}

impl SetWebMouseButton for ButtonInput<WebMouseButton> {
    fn set_state(&mut self, atomic_key: &Arc<AtomicBool>, button: MouseButton) {
        if atomic_key.get() {
            self.press(WebMouseButton(button));
            atomic_key.store_val(false);
        } else {
            self.release(WebMouseButton(button));
        }
    }
}

pub fn process_mouse_clicks(
    mouse_button_tracker: Res<MouseClickTracker>,
    mut web_mouse_button_input: ResMut<ButtonInput<WebMouseButton>>,
) {
    web_mouse_button_input.set_state(&mouse_button_tracker.just_pressed, MouseButton::Left);
}

#[derive(Clone, Default, Resource)]
pub struct KeyboardTracker {
    w: Arc<AtomicBool>,
    s: Arc<AtomicBool>,
    d: Arc<AtomicBool>,
    a: Arc<AtomicBool>,
    space: Arc<AtomicBool>,
    shift: Arc<AtomicBool>,
    control: Arc<AtomicBool>,
}

impl KeyboardTracker {
    fn listen(self, event_type: &'static str, set_value: bool) {
        listen(&event_type, move |_event| {
            let cast_event = _event.clone().dyn_into::<KeyboardEvent>().unwrap();
            if let Some(arc) = self.key_to_arc(&cast_event.key()) {
                arc.store_val(set_value);
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
            "Control" => Some(&self.control),
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
    fn set_state(&mut self, atomic_key: &Arc<AtomicBool>, key: KeyCode);
}

impl SetWebKey for ButtonInput<WebKeyCode> {
    fn set_state(&mut self, atomic_key: &Arc<AtomicBool>, key: KeyCode) {
        if atomic_key.get() {
            self.press(WebKeyCode(key));
        } else {
            self.release(WebKeyCode(key));
        }
    }
}

pub fn get_keyboard_input(
    keyboard_tracker: Res<KeyboardTracker>,
    mut keyboard_input: ResMut<ButtonInput<WebKeyCode>>,
) {
    keyboard_input.set_state(&keyboard_tracker.w, KeyCode::KeyW);
    keyboard_input.set_state(&keyboard_tracker.s, KeyCode::KeyS);
    keyboard_input.set_state(&keyboard_tracker.d, KeyCode::KeyD);
    keyboard_input.set_state(&keyboard_tracker.a, KeyCode::KeyA);
    keyboard_input.set_state(&keyboard_tracker.space, KeyCode::Space);
    keyboard_input.set_state(&keyboard_tracker.shift, KeyCode::ShiftLeft);
    keyboard_input.set_state(&keyboard_tracker.control, KeyCode::ControlLeft);
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
    fn store_val(&self, value: bool);
    fn get(&self) -> bool;
}

impl AtomicBoolExt for AtomicBool {
    fn toggle(&self) {
        self.store(!self.load(SeqCst), SeqCst);
    }

    fn store_val(&self, value: bool) {
        self.store(value, SeqCst);
    }

    fn get(&self) -> bool {
        self.load(SeqCst)
    }
}

pub trait AtomicI32Ext {
    fn store_val(&self, value: i32);
    fn get(&self) -> i32;
}

impl AtomicI32Ext for AtomicI32 {
    fn store_val(&self, value: i32) {
        self.store(value, SeqCst);
    }

    fn get(&self) -> i32 {
        self.load(SeqCst)
    }
}

#[derive(Clone, Default, Resource)]
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
            clone.delta_y.store_val((we.delta_y() * 1000.0) as i32);
            clone.delta_mode.store_val(we.delta_mode() as i32);
        });
        new
    }

    fn get(&self) -> Option<MouseWheel> {
        match self.delta_y.get() {
            0 => None,
            y => {
                self.delta_y.store_val(0);
                Some(MouseWheel {
                    unit: match self.delta_mode.get() {
                        0 => MouseScrollUnit::Pixel,
                        _ => MouseScrollUnit::Line,
                    },
                    y: (y as f32) / 1000.0 / 50.0,
                    x: 0.0,
                    // Bevy 0.11 added a source-window field to mouse events; this
                    // synthetic web event isn't tied to a winit window.
                    window: Entity::PLACEHOLDER,
                })
            }
        }
    }
}

fn get_wheel(wheel_tracker: ResMut<WheelTracker>, mut mouse_wheel_events: EventWriter<MouseWheel>) {
    if let Some(event) = wheel_tracker.get() {
        mouse_wheel_events.write(event);
    }
}
