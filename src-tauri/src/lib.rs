// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod app_state;
mod app_update;
mod config;
mod connection;
mod file_actions;
mod instance_data;
mod instance_settings;
mod mo2_plugin;
mod port_conflict;
mod reporter;
mod supervisor;
mod sync_client;
mod sync_host;

// nerevar-core module shims (leaf-layer split, step 5): these modules now
// live in nerevar-core; re-exporting them here as `crate::x` keeps every
// existing `crate::data::...`, `crate::instance_setup::...`, etc. path in
// untouched files resolving unchanged.
pub(crate) use nerevar_core::data;
pub(crate) use nerevar_core::github_getters;
pub(crate) use nerevar_core::instance_setup;
pub(crate) use nerevar_core::openmw_ini_importer;
// `sync_auth`'s only remaining app-side consumer is `sync_roundtrip_test.rs`
// (`#[cfg(test)]`): its production call sites (`nerevar_server`, `sync_client`) moved into
// core in this same step (mid-layer split, step 6), so this shim is now test-only — gated
// to avoid an unused-import warning on non-test builds. `sync_paths` has no remaining
// app-side consumer at all (same reason) and its step-5 shim is dropped outright.
#[cfg(test)]
pub(crate) use nerevar_core::sync_auth;

// nerevar-core module shims (mid-layer split, step 6): `process_manager` and
// `nerevar_server` moved wholesale (no app-side command residue — process launching has
// no #[tauri::command] fns of its own, and the embedded server has no Tauri surface at
// all), so they're pure re-exports here like the step-5 leaf modules above.
// `instance_data`/`instance_settings`/`sync_host`/`sync_client`/`port_conflict` are NOT
// shimmed this way: each kept a real app-side file (its `commands.rs`, or `sync.rs` /
// `port_conflict.rs`) that itself re-exports the rest of its core-side module.
pub(crate) use nerevar_core::nerevar_server;
pub(crate) use nerevar_core::process_manager;

#[cfg(test)]
mod sync_roundtrip_test;

pub(crate) use crate::app_state::AppState;
use crate::data::GithubReleaseResponse;
use crate::data::NerevarConfig;
use crate::data::NewInstanceConfig;
use crate::process_manager::ProcessManager;
use crate::reporter::{EventSink, TauriEventSink};
use crate::sync_client::SyncCoordinator;
use crate::sync_host::{new_shared_hosting_manifest_cache, new_shared_sync_host};
use std::sync::{Arc, Mutex};
use tauri::{Manager, RunEvent};
use tauri::State;
use tokio::sync::watch;

#[tauri::command]
async fn get_all_releases() -> Result<Vec<GithubReleaseResponse>, String> {
    github_getters::get_all_releases()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_app_version() -> String {
    app_update::current_app_version()
}

#[tauri::command]
async fn check_for_app_update() -> Result<app_update::AppUpdateStatus, String> {
    app_update::check_for_update().await
}

#[tauri::command]
async fn download_and_run_nerevar_update(
    app: tauri::AppHandle,
    release_id: u64,
) -> Result<(), String> {
    app_update::download_and_run_installer(release_id).await?;
    app.exit(0);
    Ok(())
}

#[tauri::command]
fn load_or_create_nerevar_config(
    state: State<'_, Mutex<AppState>>,
) -> Result<NerevarConfig, String> {
    config::load_or_create_nerevar_config(state.inner())
}

#[tauri::command]
async fn complete_onboarding(state: State<'_, Mutex<AppState>>) -> Result<(), String> {
    config::complete_onboarding(state.inner())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_directory_picker() -> Result<String, String> {
    file_actions::open_directory_picker().map_err(|e| e.to_string())
}

#[tauri::command]
fn open_csv_file_picker() -> Result<String, String> {
    file_actions::open_csv_file_picker().map_err(|e| e.to_string())
}

#[tauri::command]
fn open_esm_file_picker() -> Result<String, String> {
    file_actions::open_esm_file_picker().map_err(|e| e.to_string())
}

#[tauri::command]
fn open_directory(path: String) -> Result<(), String> {
    file_actions::open_directory(path.to_string()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_root_path(state: State<'_, Mutex<AppState>>, path: String) -> Result<(), String> {
    config::set_root_path(state.inner(), path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_sync_port(state: State<'_, Mutex<AppState>>, port: i32) -> Result<(), String> {
    config::set_sync_port(state.inner(), port)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_instance(
    state: State<'_, Mutex<AppState>>,
    new_instance: NewInstanceConfig,
) -> Result<(), String> {
    config::add_instance(state.inner(), new_instance).await
}

// #[tauri::command]
// async fn download_and_run_openmw_wizard(state: State<'_, Mutex<AppState>>) -> Result<(), String> {
//     config::download_and_run_openmw_wizard(state)
//         .await
//         .map_err(|e| e.to_string())
// }

#[tauri::command]
async fn validate_global_openmw_config() -> Result<bool, String> {
    config::validate_global_openmw_config()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn generate_default_global_openmw_config(
    morrowind_installation_path: String,
) -> Result<(), String> {
    config::generate_default_global_openmw_config(morrowind_installation_path)
        .await
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .build(),
        )
        .setup(|app| {
            app.manage(Mutex::new(AppState::default()));

            // Set the config path
            app.state::<Mutex<AppState>>()
                .lock()
                .unwrap()
                .nerevar_config_path = config::nerevar_config_file_path()
                .expect("Failed to resolve config path")
                .to_string_lossy()
                .to_string();

            let config =
                config::load_or_create_nerevar_config(app.state::<Mutex<AppState>>().inner())
                    .unwrap();

            // Set the config
            app.state::<Mutex<AppState>>()
                .lock()
                .unwrap()
                .nerevar_config = config;

            // Build the sink once and reuse it everywhere below (AppState,
            // the startup port probe, and the server supervisor) instead of
            // constructing a fresh TauriEventSink per use.
            let event_sink: Arc<dyn EventSink> = Arc::new(TauriEventSink::new(app.handle().clone()));
            app.state::<Mutex<AppState>>().lock().unwrap().event_sink = Some(event_sink.clone());

            // DISABLED CONFIG WATCHER FOR NOW AS EVEN INTERNAL CHANGES TRIGGER IT AND WILL
            // CAUSE UNECESSARY RE-RENDERS IN REACT

            // config::spawn_config_file_watcher(app.handle().clone());

            // Start web server supervisor: it will restart when sync_port changes.
            let initial_port = app
                .state::<Mutex<AppState>>()
                .lock()
                .unwrap()
                .nerevar_config
                .sync_port;

            let onboarding_complete = app
                .state::<Mutex<AppState>>()
                .lock()
                .unwrap()
                .nerevar_config
                .onboarding_complete;

            let (tx, rx) = watch::channel(initial_port);
            let (retry_tx, retry_rx) = watch::channel(0u64);
            let (enabled_tx, enabled_rx) = watch::channel(onboarding_complete);
            app.state::<Mutex<AppState>>()
                .lock()
                .unwrap()
                .server_port_tx = Some(tx);
            app.state::<Mutex<AppState>>()
                .lock()
                .unwrap()
                .server_retry_tx = Some(retry_tx);
            app.state::<Mutex<AppState>>()
                .lock()
                .unwrap()
                .server_enabled_tx = Some(enabled_tx);

            let sync_host = new_shared_sync_host();
            let manifest_cache = new_shared_hosting_manifest_cache();
            app.manage(sync_host.clone());
            app.manage(manifest_cache.clone());
            app.manage(Arc::new(SyncCoordinator::new()));
            app.manage(Arc::new(ProcessManager::new()));

            let server_ctx =
                nerevar_server::state::ServerContext::new(sync_host, manifest_cache);
            let startup_config = app
                .state::<Mutex<AppState>>()
                .lock()
                .unwrap()
                .nerevar_config
                .clone();

            if startup_config.onboarding_complete {
                let sink = event_sink.clone();
                tauri::async_runtime::spawn(supervisor::probe_startup_port_conflicts(
                    startup_config,
                    sink,
                ));
            }

            let sink = event_sink.clone();

            tauri::async_runtime::spawn(supervisor::run_server_supervisor(
                rx, retry_rx, enabled_rx, server_ctx, sink,
            ));
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        // REGISTER COMMANDS HERE
        .invoke_handler(tauri::generate_handler![
            get_all_releases,
            get_app_version,
            check_for_app_update,
            download_and_run_nerevar_update,
            load_or_create_nerevar_config,
            complete_onboarding,
            open_directory_picker,
            open_csv_file_picker,
            open_esm_file_picker,
            open_directory,
            set_root_path,
            set_sync_port,
            add_instance,
            validate_global_openmw_config,
            generate_default_global_openmw_config,
            instance_data::commands::scan_instance_data,
            instance_data::commands::get_instance_load_order,
            instance_data::commands::save_instance_load_order,
            instance_data::commands::delete_instance_package,
            instance_data::commands::import_mo2_modlist_csv,
            instance_data::commands::resolve_instance_openmw,
            instance_data::commands::write_instance_launch_cfg,
            instance_data::commands::save_and_host_instance,
            instance_data::commands::build_instance_manifest,
            instance_data::commands::validate_instance_manifest,
            instance_data::commands::set_hosting_instance,
            sync_host::commands::activate_hosting_instance,
            sync_host::commands::clear_hosting_instance,
            sync_host::commands::get_sync_host_status,
            connection::commands::ping_remote_nerevar_server,
            connection::commands::fetch_remote_manifest_summary,
            connection::commands::add_synced_connection,
            connection::commands::sync_instance_from_remote,
            sync_client::sync::get_instance_sync_status,
            connection::commands::cancel_instance_sync,
            connection::commands::launch_instance_client,
            connection::commands::launch_instance_server,
            connection::commands::stop_instance_process,
            connection::commands::is_instance_process_running,
            connection::commands::get_global_process_status,
            connection::instance_edit::get_instance_connection_settings,
            connection::instance_edit::update_instance,
            connection::instance_delete::delete_instance,
            port_conflict::check_port_conflicts,
            port_conflict::kill_port_process,
            port_conflict::retry_sync_server,
            mo2_plugin::install_mo2_export_plugin,
            instance_settings::commands::get_instance_setting_definitions,
            instance_settings::commands::get_instance_settings,
            instance_settings::commands::save_instance_settings_command,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app_handle, event| {
            if matches!(event, RunEvent::Exit | RunEvent::ExitRequested { .. }) {
                if let Some(manager) = app_handle.try_state::<Arc<ProcessManager>>() {
                    manager.restore_global_openmw_session_if_any();
                }
            }
        });
}
