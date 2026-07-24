use std::sync::Mutex;

use tauri::State;

use crate::instance_data::{find_instance_by_id, load_manifest, manifest_path, resolve_package_data_dir};
use crate::sync_client::sync_state::{instance_sync_status, instance_sync_status_absent};
use crate::sync_client::InstanceSyncStatus;
use crate::AppState;

/// Residue of the mid-layer split (step 6): the rest of `sync.rs` (`run_instance_sync`,
/// `sync_if_needed`, `touch_last_synced`, ...) moved into nerevar-core unchanged. This one
/// command reads `State<Mutex<AppState>>` just to resolve the instance, then delegates
/// entirely to core-side pure functions, so it stayed behind as a small app-side wrapper
/// rather than dragging `AppState` into core ahead of step 7.
#[tauri::command]
pub fn get_instance_sync_status(
    state: State<'_, Mutex<AppState>>,
    instance_id: String,
) -> Result<InstanceSyncStatus, String> {
    let instance = {
        let guard = state.lock().map_err(|_| "App state lock poisoned".to_string())?;
        find_instance_by_id(&guard.nerevar_config, &instance_id)
            .ok_or_else(|| format!("Instance not found: {instance_id}"))?
            .clone()
    };

    let data_dir = resolve_package_data_dir(&instance);
    if !manifest_path(&data_dir).exists() {
        return Ok(instance_sync_status_absent());
    }

    let manifest = load_manifest(&data_dir)?;
    instance_sync_status(&data_dir, &manifest)
}
