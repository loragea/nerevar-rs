//! App-side event sink.
//!
//! The `EventSink` trait, `emit_event`, `NullEventSink`, and the test-only
//! `CollectingEventSink` moved to `nerevar_core::reporter` in the leaf-layer
//! split (step 5) — `EventSink`/`emit_event` are re-exported below so every
//! existing `crate::reporter::*` path in this crate keeps resolving.
//! `TauriEventSink` stays here: it wraps a `tauri::AppHandle`, and
//! nerevar-core is Tauri-free by design.
//!
//! `NullEventSink` isn't re-exported: nothing in this (GUI) crate uses it
//! today — it's for headless callers (`nerevar-host`, later) that use
//! `nerevar_core::reporter::NullEventSink` directly. `CollectingEventSink`'s
//! shim was dropped the same way once its only app-side consumer,
//! `sync_roundtrip_test.rs`, relocated into `nerevar-core/tests/` (step 8) —
//! this crate no longer needs `nerevar-core`'s `test-util` feature at all.

pub use nerevar_core::reporter::{emit_event, EventSink};

use tauri::Emitter;

/// Forwards events to the Tauri frontend, exactly like the direct
/// `app.emit(..)` calls this replaces.
pub struct TauriEventSink {
    app: tauri::AppHandle,
}

impl TauriEventSink {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

impl EventSink for TauriEventSink {
    fn emit(&self, event: &'static str, payload: serde_json::Value) {
        let _ = self.app.emit(event, payload);
    }
}
