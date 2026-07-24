//! Shared application state: the config snapshot, the event sink, and the
//! watch-channel senders that drive the sync-server supervisor (see
//! `supervisor.rs`).
//!
//! Core-bound (see notes/core-split-plan.md): nothing here depends on
//! Tauri directly. `lib.rs` still `app.manage`s this behind
//! `Mutex<AppState>` so `State<'_, Mutex<AppState>>` in commands keeps
//! working; fields are `pub(crate)` (rather than private) because this
//! type moved out from under the modules that read/write its fields
//! directly (config, connection, sync_host, instance_data, ...), and
//! Rust's default field privacy only reaches descendant modules.

use std::sync::Arc;

use tokio::sync::watch;

use crate::data::NerevarConfig;
use crate::reporter::EventSink;

#[derive(Default)]
pub(crate) struct AppState {
    pub(crate) event_sink: Option<Arc<dyn EventSink>>,
    pub(crate) nerevar_config_path: String,
    pub(crate) nerevar_config: NerevarConfig,
    pub(crate) server_port_tx: Option<watch::Sender<i32>>,
    pub(crate) server_retry_tx: Option<watch::Sender<u64>>,
    pub(crate) server_enabled_tx: Option<watch::Sender<bool>>,
    pub(crate) server_retry_generation: u64,
}
