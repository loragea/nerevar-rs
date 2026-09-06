use nerevar_core::reporter::EventSink;

/// Host-local `EventSink`: turns core events into log lines instead of
/// forwarding them to a UI (there isn't one). `process-output` — the
/// TES3MP server's own stdout/stderr — goes to the `tes3mp` log target at
/// `info` so an admin tailing the journal sees the server talk; the other
/// named lifecycle events go to the `event` target at `info`; everything
/// else (progress spam — sync/scan progress events not relevant headless)
/// is downgraded to `debug`.
///
/// `admin-request` and `admin-apply` are in the `info` set on purpose: they
/// are the co-admin audit trail (who did what, what they got back, and what
/// an apply changed), and they are only useful if they reach journald by
/// default.
pub struct LogEventSink;

impl EventSink for LogEventSink {
    fn emit(&self, event: &'static str, payload: serde_json::Value) {
        match event {
            "process-output" => {
                let line = payload.get("line").and_then(|v| v.as_str()).unwrap_or("");
                log::info!(target: "tes3mp", "{line}");
            }
            "process-status"
            | "hosting-changed"
            | "port-conflicts-detected"
            | nerevar_core::admin::ADMIN_REQUEST_EVENT
            | nerevar_core::admin::ADMIN_APPLY_EVENT => {
                log::info!(target: "event", "{event} {payload}");
            }
            _ => {
                log::debug!(target: "event", "{event} {payload}");
            }
        }
    }
}
