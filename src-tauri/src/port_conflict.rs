use std::sync::Mutex;

use tauri::State;

use crate::AppState;

// Mid-layer split (step 6): detection (`find_listening_process`, `conflict_for_port`,
// `check_startup_conflicts`, `kill_process`, `is_addr_in_use_error`), the `PortConflict`/
// `PortRole` ts-rs types, and `emit_port_conflicts` all moved into nerevar-core. Only the
// three `#[tauri::command]` wrappers below, which read `State<Mutex<AppState>>`, stay
// app-side.
pub use nerevar_core::port_conflict::*;

#[tauri::command]
pub fn check_port_conflicts(
    state: State<'_, Mutex<AppState>>,
) -> Result<Vec<PortConflict>, String> {
    let guard = state.lock().map_err(|_| "App state lock poisoned".to_string())?;
    check_startup_conflicts(&guard.nerevar_config)
}

#[tauri::command]
pub fn kill_port_process(pid: u32) -> Result<(), String> {
    kill_process(pid)
}

#[tauri::command]
pub fn retry_sync_server(state: State<'_, Mutex<AppState>>) -> Result<(), String> {
    let mut guard = state
        .lock()
        .map_err(|_| "App state lock poisoned".to_string())?;

    if !guard.nerevar_config.onboarding_complete {
        return Ok(());
    }

    let port = guard.nerevar_config.sync_port;
    let next_retry = guard.server_retry_generation.wrapping_add(1);
    guard.server_retry_generation = next_retry;

    if let Some(tx) = guard.server_retry_tx.clone() {
        let _ = tx.send(next_retry);
    }
    if let Some(tx) = guard.server_port_tx.clone() {
        let _ = tx.send(port);
    }

    Ok(())
}
