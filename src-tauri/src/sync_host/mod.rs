pub mod commands;
mod manifest_cache;
mod status;

pub use commands::{activate_hosting_instance, clear_hosting_instance, get_sync_host_status};
pub use manifest_cache::{
    get_package_file_path, new_shared_hosting_manifest_cache, SharedHostingManifestCache,
};
pub use status::SyncHostStatus;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::reporter::{emit_event, EventSink};

#[derive(Default, Clone)]
pub struct SyncHostState {
    pub hosting_instance_id: Option<String>,
    pub hosting_data_dir: Option<PathBuf>,
    pub hosting_instance_root: Option<PathBuf>,
    /// Cached TES3MP sync password so file requests do not re-read server cfg every time.
    pub hosting_sync_password: Option<String>,
}

pub type SharedSyncHost = Arc<Mutex<SyncHostState>>;

pub fn new_shared_sync_host() -> SharedSyncHost {
    Arc::new(Mutex::new(SyncHostState::default()))
}

/// Points the shared sync host at `instance_id`'s data, clears the hosting
/// manifest cache, and emits `hosting-changed`. Shared by any command that
/// starts or updates hosting for an instance (Tauri-free below the command
/// boundary; the caller constructs `sink` from an `AppHandle`).
pub fn activate_hosting(
    sync_host: &SharedSyncHost,
    manifest_cache: &SharedHostingManifestCache,
    instance_id: String,
    data_dir: PathBuf,
    instance_root: PathBuf,
    sync_password: String,
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
    }

    if let Ok(mut cache) = manifest_cache.write() {
        cache.clear();
    }

    emit_event(&*sink, "hosting-changed", &());
    Ok(())
}
