//! Backend→frontend event abstraction.
//!
//! Backend code used to take a `tauri::AppHandle` solely to emit UI events,
//! which welded otherwise-reusable logic (sync, process management, instance
//! operations) to the Tauri runtime. `EventSink` inverts that: core code emits
//! through the trait, and the Tauri command layer decides where events go —
//! the GUI (`TauriEventSink`), nowhere (`NullEventSink`, headless), or a test
//! buffer (`CollectingEventSink`).
//!
//! Event names and payload shapes are unchanged from the direct
//! `app.emit(name, payload)` calls they replace; the ts-rs bindings remain the
//! frontend contract.

use serde::Serialize;
use tauri::Emitter;
use log::warn;

/// Sink for backend events destined for a UI, log, or test.
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: &'static str, payload: serde_json::Value);
}

/// Serialize a typed payload and emit it. Call sites keep constructing the
/// same typed (ts-rs exported) structs as before; serialization failures are
/// logged and dropped, matching the old `let _ = app.emit(..)` behavior.
pub fn emit_event<T: Serialize>(sink: &dyn EventSink, event: &'static str, payload: &T) {
    match serde_json::to_value(payload) {
        Ok(value) => sink.emit(event, value),
        Err(error) => warn!("Failed to serialize {event} event payload: {error}"),
    }
}

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

/// Discards all events; for headless use where no UI is listening.
#[allow(dead_code)] // consumers arrive as call sites migrate module by module
pub struct NullEventSink;

impl EventSink for NullEventSink {
    fn emit(&self, _event: &'static str, _payload: serde_json::Value) {}
}

/// Buffers events for assertions in tests.
#[cfg(test)]
#[derive(Default)]
pub struct CollectingEventSink {
    events: std::sync::Mutex<Vec<(&'static str, serde_json::Value)>>,
}

#[cfg(test)]
impl CollectingEventSink {
    pub fn events(&self) -> Vec<(&'static str, serde_json::Value)> {
        self.events.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl EventSink for CollectingEventSink {
    fn emit(&self, event: &'static str, payload: serde_json::Value) {
        self.events.lock().unwrap().push((event, payload));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FakePayload {
        instance_id: String,
        step: u64,
    }

    #[test]
    fn emit_event_serializes_payload_to_sink() {
        let sink = CollectingEventSink::default();
        emit_event(
            &sink,
            "fake-event",
            &FakePayload {
                instance_id: "abc".to_string(),
                step: 3,
            },
        );

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "fake-event");
        assert_eq!(events[0].1["instanceId"], "abc");
        assert_eq!(events[0].1["step"], 3);
    }

    #[test]
    fn null_sink_discards() {
        // Just proves the impl exists and is callable as a trait object.
        let sink: std::sync::Arc<dyn EventSink> = std::sync::Arc::new(NullEventSink);
        sink.emit("ignored", serde_json::Value::Null);
    }
}
