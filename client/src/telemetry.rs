//! Lightweight wasm telemetry for live mobile/browser debugging.
//!
//! iOS browsers give no practical console access, and on wasm a Rust panic just
//! freezes the canvas with no on-page error — so a touch-triggered panic is
//! invisible on the device. This module installs a panic hook and exposes a
//! `tlog!` breadcrumb macro; both `navigator.sendBeacon()` the message to an
//! external HTTPS sink that the developer polls. `sendBeacon` is fire-and-forget
//! and still flushes while the runtime is aborting, so panic messages get out.
//!
//! No-op on native (the whole thing is `#[cfg(target_arch = "wasm32")]`). The sink
//! is a disposable bucket used only for this debugging pass — remove the call
//! sites (and this module) once the mobile issues are resolved.

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::sync::atomic::{AtomicU32, Ordering};
    use wasm_bindgen::JsValue;

    // Disposable webhook.site bucket. Safe to ship in the public wasm: it only
    // accepts logs (no secrets, no auth, write-only from the client's view).
    const SINK: &str = "https://webhook.site/aee19aeb-71ba-421f-bee8-2b8690325f7e";

    // Per-load monotonic sequence so out-of-order beacon delivery can be reordered.
    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn beacon(line: &str) {
        if let Some(win) = web_sys::window() {
            // text/plain body → CORS-safelisted, no preflight; opaque response is fine.
            let _ = win.navigator().send_beacon_with_opt_str(SINK, Some(line));
        }
    }

    pub fn log(msg: &str) {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let line = format!("#{seq} [{}] {msg}", env!("SHORT_GIT_HASH"));
        web_sys::console::log_1(&JsValue::from_str(&line));
        beacon(&line);
    }

    pub fn init() {
        std::panic::set_hook(Box::new(|info| {
            // `PanicHookInfo`'s Display is "panicked at 'msg', src/file.rs:line:col".
            let line = format!("PANIC [{}] {info}", env!("SHORT_GIT_HASH"));
            web_sys::console::error_1(&JsValue::from_str(&line));
            beacon(&line);
        }));
        log("boot");
    }
}

#[cfg(target_arch = "wasm32")]
pub use imp::{init, log};

#[cfg(not(target_arch = "wasm32"))]
pub fn init() {}

#[cfg(not(target_arch = "wasm32"))]
pub fn log(_msg: &str) {}

/// `tlog!("fmt {}", x)` — a `format!`-style breadcrumb to the telemetry sink
/// (no-op on native).
#[macro_export]
macro_rules! tlog {
    ($($arg:tt)*) => {
        $crate::telemetry::log(&format!($($arg)*))
    };
}
