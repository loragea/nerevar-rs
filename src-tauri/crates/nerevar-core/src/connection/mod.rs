pub mod instance_delete;
pub mod instance_edit;

pub use instance_delete::delete_instance;
pub use instance_edit::{
    get_instance_connection_settings, set_instance_runtime_hint, update_instance,
};

// `connection::commands` (ping/fetch/add_synced_connection/sync_instance_from_remote/
// launch/stop/... ) stays app-side in full: every fn in it is a `#[tauri::command]` with no
// non-command helpers of its own (the Tauri/core split; see AGENTS.md,
// "Architecture").
