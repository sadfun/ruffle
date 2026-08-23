//! Browser side of the Shararam profiler (`shararam_profiler` feature).
//!
//! Installs `performance.now()` as the profiler clock so that Ruffle events
//! share the browser's performance timeline, and exposes the event buffer to
//! the page as `window.__ruffleProfiler`:
//!
//! ```js
//! window.__ruffleProfiler.drain()   // -> JSON array string of events, clears the buffer
//! window.__ruffleProfiler.pending() // -> number of buffered events
//! window.__ruffleProfiler.memory()  // -> wasm linear memory size in bytes
//! ```
//!
//! Keeping the hand-off as one JSON string per drain avoids per-event
//! JS/wasm boundary crossings.

use js_sys::{Object, Reflect};
use wasm_bindgen::prelude::*;

thread_local! {
    static PERFORMANCE: Option<web_sys::Performance> =
        web_sys::window().and_then(|window| window.performance());
}

fn performance_now() -> f64 {
    PERFORMANCE.with(|performance| performance.as_ref().map_or(0.0, |p| p.now()))
}

fn wasm_memory_bytes() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        (core::arch::wasm32::memory_size(0) as f64) * 65_536.0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0.0
    }
}

pub fn install() {
    ruffle_core::profiler::set_clock(performance_now);

    let Some(window) = web_sys::window() else {
        return;
    };
    let api = Object::new();
    let drain = Closure::<dyn Fn() -> JsValue>::new(|| {
        JsValue::from_str(&ruffle_core::profiler::drain_json())
    });
    let pending = Closure::<dyn Fn() -> f64>::new(|| ruffle_core::profiler::pending() as f64);
    let memory = Closure::<dyn Fn() -> f64>::new(wasm_memory_bytes);
    let _ = Reflect::set(&api, &"drain".into(), drain.as_ref());
    let _ = Reflect::set(&api, &"pending".into(), pending.as_ref());
    let _ = Reflect::set(&api, &"memory".into(), memory.as_ref());
    // The closures live for the lifetime of the page.
    drain.forget();
    pending.forget();
    memory.forget();
    let _ = Reflect::set(&window, &"__ruffleProfiler".into(), &api);
    ruffle_core::profiler::mark("profiler", "installed");
}
