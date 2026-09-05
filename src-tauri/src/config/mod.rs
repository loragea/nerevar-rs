//! App-side config residue after the Tauri/core split (see AGENTS.md,
//! "Architecture"): the plain config logic (load/persist,
//! add/update instance, `build_instance_config`/`build_synced_instance_config`,
//! `start_sync_server_supervisor`, ...) moved into
//! `nerevar_core::config::nerevar_config`, re-exported below so every
//! existing `crate::config::x` path in untouched files keeps resolving.
//!
//! What stays here: `nerevar_config_file_path`, since only the app crate can
//! call `tauri::generate_context!()` to get the app identifier (core takes
//! it as a plain `&str` parameter instead); and the dead/commented
//! config-file-watcher block, left behind rather than dragged into core with
//! the rest of the file (it's Tauri-shaped — `AppHandle`, `State<Mutex<AppState>>`
//! — and disabled, so there's nothing to gain by moving it).

// `update_owned_instance` isn't re-exported: its only remaining app-side call site
// (`connection/instance_edit.rs`) moved into core in this same step (top-layer split,
// step 7), so nothing in this crate reaches it through this path anymore — same reasoning
// as the dropped `sync_paths`/`openmw_ini_importer` shims in `lib.rs`.
pub use nerevar_core::config::nerevar_config::{
    add_instance, complete_onboarding, generate_default_global_openmw_config,
    load_or_create_nerevar_config, set_root_path, set_sync_port, update_synced_instance,
    validate_global_openmw_config,
};

/// Same path as `app.path().app_data_dir()` / `config.json` (see Tauri
/// `PathResolver::app_data_dir`). Resolves the app identifier via
/// `tauri::generate_context!()` (only possible app-side) and delegates the
/// actual path-building to core.
pub fn nerevar_config_file_path() -> std::path::PathBuf {
    let context: tauri::Context<tauri::Wry> = tauri::generate_context!();
    nerevar_core::config::nerevar_config_file_path(&context.config().identifier)
}

// DISABLED CONFIG WATCHER FOR NOW AS EVEN INTERNAL CHANGES TRIGGER IT AND WILL
// CAUSE UNECESSARY RE-RENDERS IN REACT
//
// use std::path::PathBuf;
// use std::sync::Mutex;
// use std::time::Duration;
// use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
// use tauri::{AppHandle, Emitter, Manager};
// use crate::AppState;
//
// pub fn spawn_config_file_watcher(app: AppHandle) {
//     tauri::async_runtime::spawn(async move {
//         let config_path = app
//             .state::<Mutex<AppState>>()
//             .lock()
//             .expect("config state poisoned")
//             .nerevar_config_path
//             .clone();
//
//         let _ = tauri::async_runtime::spawn_blocking(move || {
//             let path = PathBuf::from(&config_path);
//             let watch_path = path.clone();
//
//             let mut watcher = RecommendedWatcher::new(
//                 move |result: Result<notify::Event, notify::Error>| {
//                     let Ok(event) = result else { return };
//                     if !matches!(event.kind, EventKind::Modify(_)) {
//                         return;
//                     }
//
//                     let Ok(config) = nerevar_core::config::nerevar_config::load_or_create_nerevar_config_at(&watch_path) else {
//                         return;
//                     };
//
//                     let state = app.state::<Mutex<AppState>>();
//                     if let Ok(mut app_state) = state.lock() {
//                         app_state.nerevar_config = config.clone();
//                     }
//
//                     let _ = app.emit("on_config_change", config);
//                     log::info!("Config file changed externally, emitting event and updating app state");
//                 },
//                 notify::Config::default(),
//             )
//             .expect("failed to create config watcher");
//
//             watcher
//                 .watch(&path, RecursiveMode::NonRecursive)
//                 .expect("failed to watch config file");
//
//             log::info!("Watching config file at {}", path.display());
//
//             loop {
//                 std::thread::sleep(Duration::from_secs(3600));
//             }
//         })
//         .await;
//     });
// }
