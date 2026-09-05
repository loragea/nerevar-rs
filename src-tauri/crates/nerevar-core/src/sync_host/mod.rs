mod manifest_cache;
mod status;

pub use manifest_cache::{
    get_package_file_path, new_shared_hosting_manifest_cache, SharedHostingManifestCache,
};
pub use status::SyncHostStatus;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::reporter::{emit_event, EventSink};
use crate::runtime::RuntimeSource;

#[derive(Default, Clone)]
pub struct SyncHostState {
    pub hosting_instance_id: Option<String>,
    pub hosting_data_dir: Option<PathBuf>,
    pub hosting_instance_root: Option<PathBuf>,
    /// Cached TES3MP sync password so file requests do not re-read server cfg every time.
    pub hosting_sync_password: Option<String>,
    /// The hosted instance's `runtime_hint`, served in the manifest summary
    /// so a connecting client's runtime picker can preselect it. Snapshotted
    /// at activation (the config is not readable from the request path) and
    /// refreshed by `set_hosting_runtime_hint` when the operator changes it.
    pub hosting_runtime_hint: Option<RuntimeSource>,
}

pub type SharedSyncHost = Arc<Mutex<SyncHostState>>;

pub fn new_shared_sync_host() -> SharedSyncHost {
    Arc::new(Mutex::new(SyncHostState::default()))
}

/// Points the shared sync host at `instance_id`'s data, clears the hosting
/// manifest cache, and emits `hosting-changed`. Shared by any command that
/// starts or updates hosting for an instance (Tauri-free below the command
/// boundary; the caller constructs `sink` from an `AppHandle`).
// Eight parameters, one per thing "what is being hosted" consists of;
// bundling them into a struct would only move the same list one level out.
#[allow(clippy::too_many_arguments)]
pub fn activate_hosting(
    sync_host: &SharedSyncHost,
    manifest_cache: &SharedHostingManifestCache,
    instance_id: String,
    data_dir: PathBuf,
    instance_root: PathBuf,
    sync_password: String,
    runtime_hint: Option<RuntimeSource>,
    sink: Arc<dyn EventSink>,
) -> Result<(), String> {
    {
        let mut host = sync_host
            .lock()
            .map_err(|_| "Sync host lock poisoned".to_string())?;
        host.hosting_instance_id = Some(instance_id);
        host.hosting_data_dir = Some(data_dir);
        host.hosting_instance_root = Some(instance_root);
        host.hosting_sync_password = Some(sync_password);
        host.hosting_runtime_hint = runtime_hint;
    }

    if let Ok(mut cache) = manifest_cache.write() {
        cache.clear();
    }

    emit_event(&*sink, "hosting-changed", &());
    Ok(())
}

/// Clears hosting state, clears the hosting manifest cache, and emits
/// `hosting-changed`. Mirror of `activate_hosting` for the stop-hosting path.
pub fn deactivate_hosting(
    sync_host: &SharedSyncHost,
    manifest_cache: &SharedHostingManifestCache,
    sink: Arc<dyn EventSink>,
) -> Result<(), String> {
    {
        let mut host = sync_host
            .lock()
            .map_err(|_| "Sync host lock poisoned".to_string())?;
        host.hosting_instance_id = None;
        host.hosting_data_dir = None;
        host.hosting_instance_root = None;
        host.hosting_sync_password = None;
        host.hosting_runtime_hint = None;
    }

    if let Ok(mut cache) = manifest_cache.write() {
        cache.clear();
    }

    emit_event(&*sink, "hosting-changed", &());
    Ok(())
}

/// Refreshes the advertised runtime hint if `instance_id` is the instance
/// currently hosting.
///
/// The hint is snapshotted into the hosting state at activation, so an
/// operator who changes it while hosting would otherwise keep serving the
/// old one until the next activation. Returns whether the live state was
/// touched; a change to an instance that is not hosting is not an error.
pub fn set_hosting_runtime_hint(
    sync_host: &SharedSyncHost,
    instance_id: &str,
    runtime_hint: Option<RuntimeSource>,
) -> Result<bool, String> {
    let mut host = sync_host
        .lock()
        .map_err(|_| "Sync host lock poisoned".to_string())?;

    if host.hosting_instance_id.as_deref() != Some(instance_id) {
        return Ok(false);
    }
    host.hosting_runtime_hint = runtime_hint;
    Ok(true)
}
