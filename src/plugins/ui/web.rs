use crate::utils::{html, listen, AtomicBoolExt};
use crate::AppState;
use bevy::prelude::*;
use bevy_webgl2::renderer::JsCast;
use std::sync::{atomic::AtomicBool, Arc};
use web_sys::Element;

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.insert_resource(WasmPointerLockTracker::new())
            .add_system_set(SystemSet::on_enter(AppState::InGame).with_system(hide_cursor.system()))
            .add_system(toggle_menu_on_pointer_lock.system());
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
        listen(
            "pointerlockchange",
            move |_event| match html::get_document().pointer_lock_element() {
                Some(element) => {
                    if element == html::get_body().dyn_into::<Element>().unwrap() {
                        lock_clone.lock.set(true);
                    }
                }
                None => {
                    lock_clone.lock.set(false);
                }
            },
        );
        lock
    }

    fn get(&self) -> bool {
        self.lock.get()
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
