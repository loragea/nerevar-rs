use std::sync::{Arc, Mutex};

use tauri::{AppHandle, State};

use crate::data::NerevarConfig;
use crate::process_manager::ProcessManager;
use crate::reporter::{EventSink, TauriEventSink};
use crate::sync_client::SyncCoordinator;
use crate::sync_host::{SharedHostingManifestCache, SharedSyncHost};
use crate::AppState;

/// Command residue after the Tauri/core split (see AGENTS.md,
/// "Architecture"): all logic moved into `nerevar_core::connection::instance_delete::delete_instance`. This
/// residue just resolves Tauri's `State`/`AppHandle` wrappers into the plain
/// references and `Arc<dyn EventSink>` the core fn takes.
#[tauri::command]
pub fn delete_instance(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    sync_host: State<'_, SharedSyncHost>,
    manifest_cache: State<'_, SharedHostingManifestCache>,
    process_manager: State<'_, Arc<ProcessManager>>,
    coordinator: State<'_, Arc<SyncCoordinator>>,
    instance_id: String,
    delete_data_directory: bool,
) -> Result<NerevarConfig, String> {
    let sink: Arc<dyn EventSink> = Arc::new(TauriEventSink::new(app));
    nerevar_core::connection::instance_delete::delete_instance(
        state.inner(),
        sync_host.inner(),
        manifest_cache.inner(),
        process_manager.inner(),
        coordinator.inner(),
        sink,
        instance_id,
        delete_data_directory,
    )
}
