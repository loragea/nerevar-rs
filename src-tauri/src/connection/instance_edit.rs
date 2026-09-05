use std::sync::{Arc, Mutex};

use tauri::{AppHandle, State};

use crate::data::{InstanceConnectionSettings, InstanceEditPayload};
use crate::reporter::{EventSink, TauriEventSink};
use crate::sync_host::SharedSyncHost;
use crate::AppState;
use nerevar_core::runtime::RuntimeSource;

/// Command residue after the Tauri/core split (see AGENTS.md,
/// "Architecture"): logic moved into
/// `nerevar_core::connection::instance_edit`. This residue just unwraps
/// Tauri's `State` into `state.inner()` and delegates.
#[tauri::command]
pub fn get_instance_connection_settings(
    state: State<'_, Mutex<AppState>>,
    instance_id: String,
) -> Result<InstanceConnectionSettings, String> {
    nerevar_core::connection::instance_edit::get_instance_connection_settings(
        state.inner(),
        instance_id,
    )
}

/// See `get_instance_connection_settings` above — same split. The
/// `Arc<dyn EventSink>` core needs is built here from the `AppHandle`.
#[tauri::command]
pub fn update_instance(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    edit: InstanceEditPayload,
) -> Result<InstanceConnectionSettings, String> {
    let sink: Arc<dyn EventSink> = Arc::new(TauriEventSink::new(app));
    nerevar_core::connection::instance_edit::update_instance(state.inner(), sink, edit)
}

/// Same split again: the host operator's "suggest this runtime to players"
/// control writes through here.
#[tauri::command]
pub fn set_instance_runtime_hint(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    sync_host: State<'_, SharedSyncHost>,
    instance_id: String,
    hint: Option<RuntimeSource>,
) -> Result<Option<RuntimeSource>, String> {
    let sink: Arc<dyn EventSink> = Arc::new(TauriEventSink::new(app));
    nerevar_core::connection::instance_edit::set_instance_runtime_hint(
        state.inner(),
        sync_host.inner(),
        sink,
        instance_id,
        hint,
    )
}
