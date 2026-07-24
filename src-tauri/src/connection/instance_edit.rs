use std::sync::{Arc, Mutex};

use tauri::{AppHandle, State};

use crate::data::{InstanceConnectionSettings, InstanceEditPayload};
use crate::reporter::{EventSink, TauriEventSink};
use crate::AppState;

/// Top-layer split (step 7, see notes/core-split-plan.md): logic moved into
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
