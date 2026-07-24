pub mod commands;

// Mid-layer split (step 6): `SyncHostState`/`activate_hosting`/`deactivate_hosting`
// (mod.rs), `manifest_cache.rs`, and `status.rs` moved into nerevar-core. Only
// `commands.rs` (the three `#[tauri::command]` wrappers, all `State<Mutex<AppState>>`
// readers) stays app-side.
pub use nerevar_core::sync_host::*;
