//! Shared application state: the config snapshot, the event sink, and the
//! watch-channel senders that drive the sync-server supervisor (see
//! `supervisor.rs`).
//!
//! Top-layer split (step 7, see notes/core-split-plan.md): moved into
//! nerevar-core wholesale. The app crate's `lib.rs` still `app.manage`s
//! this behind `Mutex<AppState>` so `State<'_, Mutex<AppState>>` in
//! commands keeps working (via a `pub(crate) use nerevar_core::AppState`
//! shim). Fields are `pub` (widened from `pub(crate)`): this type is now a
//! genuinely public core type read/written from a dependent crate (config,
//! connection, sync_host, instance_data, ... on the core side, and the
//! app's command residues on the other), so `pub(crate)` — which only
//! reaches descendant modules of *this* crate — no longer covers every
//! caller.

use std::sync::Arc;

use tokio::sync::watch;

use crate::data::NerevarConfig;
use crate::reporter::EventSink;

#[derive(Default)]
pub struct AppState {
    pub event_sink: Option<Arc<dyn EventSink>>,
    pub nerevar_config_path: String,
    pub nerevar_config: NerevarConfig,
    pub server_port_tx: Option<watch::Sender<i32>>,
    pub server_retry_tx: Option<watch::Sender<u64>>,
    pub server_enabled_tx: Option<watch::Sender<bool>>,
    pub server_retry_generation: u64,
}
