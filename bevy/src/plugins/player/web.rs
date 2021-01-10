use crate::utils::html;
use bevy::prelude::*;
use bevy_webgl2::renderer::JsCast;
use gloo::events::EventListener;
use std::sync::{
    atomic::{AtomicI32, Ordering::SeqCst},
    Arc,
};
use web_sys::MouseEvent;

pub struct PlatformPlugin;

impl Plugin for PlatformPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_resource(WasmMouseTracker::new());
    }
}

pub struct WasmMouseTracker {
    delta_x: Arc<AtomicI32>,
    delta_y: Arc<AtomicI32>,
}

impl WasmMouseTracker {
    pub fn new() -> Self {
        let delta_x = Arc::new(AtomicI32::new(0));
        let delta_y = Arc::new(AtomicI32::new(0));

        let dx = Arc::clone(&delta_x);
        let dy = Arc::clone(&delta_y);
        let on_move = EventListener::new(&html::get_document(), "mousemove", move |_event| {
            let me = _event.clone().dyn_into::<MouseEvent>().unwrap();
            // info!("Moved! {:?}, {:?}", me.movement_x(), me.movement_y());
            dx.store(me.movement_x(), SeqCst);
            dy.store(me.movement_y(), SeqCst);
        });
        on_move.forget();

        // let on_move = EventListener::new(&html::get_body(), "mousemove", move |_event| {
        //     let me = _event.clone().dyn_into::<MouseEvent>().unwrap();
        //     // info!("Moved! {:?}, {:?}", me.movement_x(), me.movement_y());
        //     // dx.store(me.movement_x(), SeqCst);
        //     // dy.store(me.movement_y(), SeqCst);
        // });
        // on_move.forget();

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

pub fn get_look(wasm_mouse_tracker: Res<WasmMouseTracker>) -> Vec2 {
    wasm_mouse_tracker.get_delta_and_reset()
}
