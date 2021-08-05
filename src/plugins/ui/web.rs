use crate::utils::{html, AtomicBoolExt};
use crate::AppState;
use bevy::prelude::*;
use bevy_webgl2::renderer::JsCast;
use gloo::events::EventListener;
use std::sync::{
    atomic::{AtomicBool, Ordering::SeqCst},
    Arc,
};
use web_sys::Element;

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.insert_resource(WasmPointerLockTracker::new())
            .add_system_set(SystemSet::on_enter(AppState::InGame).with_system(hide_cursor.system()))
            .add_system(toggle_menu_on_pointer_lock.system());
    }
}

struct WasmPointerLockTracker {
    lock: Arc<AtomicBool>,
}

impl WasmPointerLockTracker {
    pub fn new() -> Self {
        // Derived from https://developer.mozilla.org/en-US/docs/Web/API/Document/pointerlockchange_event
        // and https://rustwasm.github.io/wasm-bindgen/api/web_sys/struct.PointerEvent.html
        let lock = Arc::new(AtomicBool::new(false));
        let lock_clone = Arc::clone(&lock);
        let on_lock =
            EventListener::new(&html::get_document(), "pointerlockchange", move |_event| {
                match html::get_document().pointer_lock_element() {
                    Some(element) => {
                        if element == html::get_body().dyn_into::<Element>().unwrap() {
                            info!("Locked!");
                            lock_clone.set(true);
                        }
                    }
                    None => {
                        info!("Unlocked!");
                        lock_clone.set(false);
                    }
                }
            });
        on_lock.forget();

        let on_focus = EventListener::new(&html::get_document(), "onfocus", move |_event| {
            info!("Focus!")
        });
        on_focus.forget();

        Self { lock: lock }
    }

    pub fn get(&self) -> bool {
        self.lock.load(SeqCst)
    }
}

fn hide_cursor() {
    html::get_body().request_pointer_lock();
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
