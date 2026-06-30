use crate::gamepad::GamepadActive;
use crate::mobile::MobileActive;
use crate::AppState;
use bad_spaceship_shared::{Grass, LookPitch, Player, Yaw};
use bevy::prelude::*;
use gloo::events::EventListener;
use std::sync::{
    atomic::{AtomicBool, Ordering::SeqCst},
    Arc,
};
use wasm_bindgen::JsCast;
use web_sys::{Document, Event, HtmlElement};

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut App) {
        // Keyboard, mouse buttons, the scroll wheel, and mouse motion all come
        // straight from winit now (the game reads the standard `ButtonInput<KeyCode>`
        // / `ButtonInput<MouseButton>` / `MouseWheel` / `MouseMotion` on web exactly
        // like native — see `client/src/input.rs`). The old hand-rolled DOM-listener
        // input layer (a `KeyCode`/`MouseButton` newtype each, three trackers, and a
        // merge step) dated to the Bevy 0.12 era when winit's web input didn't work;
        // winit 0.30 delivers all of it natively, so it's gone. What remains here is
        // the genuinely browser-specific glue: requesting pointer lock and reacting
        // to pointer-lock changes (the browser owns the Esc-to-exit gesture), plus
        // the loader-overlay "game ready" signal.
        app.insert_resource(PointerLockTracker::new())
            .add_systems(OnEnter(AppState::InGame), hide_cursor)
            .add_systems(
                Update,
                (
                    // Touch devices never acquire pointer lock, so this toggle would
                    // immediately bounce the player back to the menu on every frame
                    // in mobile mode — gate it off once touch input is active. A
                    // controller-only session (e.g. iPhone web) is in the same boat:
                    // iOS WebKit has no Pointer Lock API, so gate it off there too and
                    // let the Start button drive the menu (see `gamepad.rs`).
                    toggle_menu_on_pointer_lock
                        .run_if(|m: Res<MobileActive>, g: Res<GamepadActive>| !m.0 && !g.0),
                    signal_game_ready,
                    heartbeat,
                    write_look_beacon,
                ),
            );
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
            // Locked whenever *any* element holds the lock — that element is the
            // canvas (see `hide_cursor`), not `<body>`.
            clone
                .lock
                .store_val(get_document().pointer_lock_element().is_some());
        });
        new
    }

    fn get(&self) -> bool {
        self.lock.get()
    }
}

fn hide_cursor(mobile: Res<MobileActive>, gamepad: Res<GamepadActive>) {
    // iOS / touch WebKit doesn't implement the Pointer Lock API, so
    // `request_pointer_lock()` throws a JS exception there — and because it's a JS
    // throw (not a Rust panic) it unwinds out of the winit rAF callback and stops
    // the loop, freezing the canvas with no panic logged. Touch devices never use
    // pointer lock anyway (the menu toggle above is gated off too), so skip it
    // entirely in mobile mode. This is what froze the game on the first tap. A
    // controller-only iPhone session never touches the screen (so `MobileActive`
    // stays false) but hits the same iOS pointer-lock throw — skip it there too.
    if mobile.0 || gamepad.0 {
        return;
    }
    // Lock the *canvas*, not `<body>`. winit's mouse-button and scroll-wheel
    // listeners live on the canvas, and under pointer lock the browser routes
    // mouse events only to the lock element — locking `<body>` meant winit never
    // saw in-game clicks or scroll. (Keyboard was unaffected: key events target
    // the focused canvas regardless of pointer lock, which is why WASD worked but
    // clicking to grab didn't.) Locking the canvas lets winit's native input see
    // clicks/scroll under lock. Mouse *motion* comes through either way — winit's
    // `MouseMotion` fires from a document-level raw-motion listener.
    if let Some(canvas) = get_document()
        .query_selector("canvas")
        .ok()
        .flatten()
        .and_then(|el| el.dyn_into::<HtmlElement>().ok())
    {
        canvas.request_pointer_lock();
    }
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

/// Per-frame liveness beacon driving the page-visibility watchdog in `play.html`.
/// Writes the `Update`-schedule frame counter to `<body data-bs-tick>` every frame so
/// JS can tell, frame-by-frame, whether the wasm loop advanced. This is load-bearing
/// for the recovery: on iOS, backgrounding the tab fires `pagehide(persisted=true)`
/// (bfcache) which suspends Bevy, and on return iOS fires only `visibilitychange` —
/// *not* the `pageshow(persisted=true)`/`Resumed` winit needs — so winit's own
/// `requestAnimationFrame` loop never re-arms and the game stays frozen (verified via
/// device telemetry). JS timers/rAF *do* keep firing, so `play.html` runs its own rAF
/// watchdog that re-pumps winit (a synthetic canvas event) on any frame this counter
/// didn't move — and stays dormant when it does, so healthy browsers never double-step.
/// The per-frame write is the signal that lets that watchdog distinguish the two
/// without misfiring (a throttled beacon would read as "stalled" between writes).
fn heartbeat(mut frames: Local<u32>) {
    *frames = frames.wrapping_add(1);
    let _ = get_body().set_attribute("data-bs-tick", &frames.to_string());
}

/// Save the player's camera look (yaw, pitch) into `sessionStorage` so it survives
/// the iOS reload — `read_resume_look` (`client/src/net.rs`) restores it on boot.
/// Throttled (~5 Hz): the look barely changes faster than that, and this bounds the
/// per-frame `sessionStorage` write cost. Position isn't saved here — the server
/// remembers it (it's authoritative); only the client-owned camera angle is.
fn write_look_beacon(
    time: Res<Time>,
    mut throttle: Local<f32>,
    player: Query<(&Yaw, &LookPitch), With<Player>>,
) {
    *throttle -= time.delta_secs();
    if *throttle > 0.0 {
        return;
    }
    *throttle = 0.2;
    let Ok((yaw, pitch)) = player.single() else {
        return;
    };
    if let Some(storage) = web_sys::window().and_then(|w| w.session_storage().ok().flatten()) {
        let _ = storage.set_item("bs-look", &format!("{},{}", yaw.0, pitch.0));
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

trait AtomicBoolExt {
    fn store_val(&self, value: bool);
    fn get(&self) -> bool;
}

impl AtomicBoolExt for AtomicBool {
    fn store_val(&self, value: bool) {
        self.store(value, SeqCst);
    }

    fn get(&self) -> bool {
        self.load(SeqCst)
    }
}
