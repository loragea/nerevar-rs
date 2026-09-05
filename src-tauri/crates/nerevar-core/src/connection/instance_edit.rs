use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::config::nerevar_config::{update_owned_instance, update_synced_instance};
use crate::data::{InstanceConnectionSettings, InstanceEditPayload};
use crate::instance_data::find_instance_by_id;
use crate::instance_setup::{
    instance_tes3mp_dir, read_tes3mp_client_settings, read_tes3mp_server_settings,
    update_server_connection_settings, write_owned_client_connection,
    write_tes3mp_client_connection,
};
use crate::reporter::{emit_event, EventSink};
use crate::runtime::{normalize_runtime_hint, RuntimeSource};
use crate::sync_host::{set_hosting_runtime_hint, SharedSyncHost};
use crate::AppState;

/// Moved here by the Tauri/core split (see AGENTS.md, "Architecture"):
/// this fn was already pure logic behind its `#[tauri::command]` signature — it only
/// reads `State<Mutex<AppState>>` to resolve the instance, same shape as the
/// mid-layer's `sync_client::sync::get_instance_sync_status`. The app-side
/// residue (`src-tauri/src/connection/instance_edit.rs`) unwraps its `State`
/// into `state.inner()` and calls straight through. Also called internally
/// by `update_instance` below (noted as a step-2 open question, resolved
/// here: it's fine to call directly since both now live in the same crate).
pub fn get_instance_connection_settings(
    state: &Mutex<AppState>,
    instance_id: String,
) -> Result<InstanceConnectionSettings, String> {
    let instance = {
        let guard = state.lock().map_err(|_| "App state lock poisoned".to_string())?;
        find_instance_by_id(&guard.nerevar_config, &instance_id)
            .ok_or_else(|| format!("Instance not found: {instance_id}"))?
            .clone()
    };

    let tes3mp_dir = instance_tes3mp_dir(Path::new(&instance.path));
    let is_synced = instance.remote_host.is_some();

    if is_synced {
        let client = read_tes3mp_client_settings(&tes3mp_dir)?;
        Ok(InstanceConnectionSettings {
            name: instance.name,
            description: instance.description,
            host: instance
                .remote_host
                .clone()
                .unwrap_or(client.destination_address),
            port: instance.remote_sync_port.unwrap_or(25567),
            password: instance
                .sync_password
                .clone()
                .unwrap_or(client.password),
            is_synced: true,
            sync_port: instance.remote_sync_port,
        })
    } else {
        let server = read_tes3mp_server_settings(&tes3mp_dir)?;
        Ok(InstanceConnectionSettings {
            name: instance.name,
            description: instance.description,
            host: server.hostname,
            port: server.port,
            password: server.password,
            is_synced: false,
            sync_port: None,
        })
    }
}

/// Top-layer split (step 7): everything except the `#[tauri::command]`
/// signature moved here — the app-side residue builds the `Arc<dyn
/// EventSink>` from its `AppHandle` and unwraps `State` into `state.inner()`
/// before delegating.
pub fn update_instance(
    state: &Mutex<AppState>,
    sink: Arc<dyn EventSink>,
    edit: InstanceEditPayload,
) -> Result<InstanceConnectionSettings, String> {
    let mut instance = {
        let guard = state.lock().map_err(|_| "App state lock poisoned".to_string())?;
        find_instance_by_id(&guard.nerevar_config, &edit.id)
            .ok_or_else(|| format!("Instance not found: {}", edit.id))?
            .clone()
    };

    let tes3mp_dir = instance_tes3mp_dir(Path::new(&instance.path));
    let is_synced = instance.remote_host.is_some();

    instance.name = edit.name.trim().to_string();
    instance.description = edit.description;

    if is_synced {
        instance.remote_host = Some(edit.host.trim().to_string());
        instance.remote_sync_port = Some(edit.port);
        instance.sync_password = Some(edit.password.clone());

        let game_port = instance.tes3mp_server_port.or_else(|| {
            read_tes3mp_client_settings(&tes3mp_dir)
                .ok()
                .map(|client| client.port)
        });
        if let Some(game_port) = game_port {
            write_tes3mp_client_connection(
                &tes3mp_dir,
                edit.host.trim(),
                game_port,
                &edit.password,
            )?;
            instance.tes3mp_server_port = Some(game_port);
        }
        let config = update_synced_instance(state, instance)?;
        emit_event(&*sink, "on_config_change", &config);
    } else {
        update_server_connection_settings(
            &tes3mp_dir,
            edit.host.trim(),
            edit.port,
            &edit.password,
        )?;
        write_owned_client_connection(&tes3mp_dir)?;
        instance.tes3mp_server_port = Some(edit.port);
        let config = update_owned_instance(state, instance)?;
        emit_event(&*sink, "on_config_change", &config);
    }

    get_instance_connection_settings(state, edit.id)
}

/// Sets (or clears) the TES3MP runtime an owned instance's host advertises
/// to the players who connect to it.
///
/// Only a `githubRelease` may be advertised — see
/// `runtime::normalize_runtime_hint`, which also normalises a pasted
/// repository URL into `owner/name`. `None` removes the suggestion.
///
/// Advertising is all this does: the hint is served in the manifest summary
/// and preselected in the connecting client's runtime picker, and the player
/// stays free to choose something else. When the instance happens to be
/// hosting right now, the live sync-host state is refreshed too, so the next
/// summary served already carries the change.
pub fn set_instance_runtime_hint(
    state: &Mutex<AppState>,
    sync_host: &SharedSyncHost,
    sink: Arc<dyn EventSink>,
    instance_id: String,
    hint: Option<RuntimeSource>,
) -> Result<Option<RuntimeSource>, String> {
    let hint = hint.map(normalize_runtime_hint).transpose()?;

    let mut instance = {
        let guard = state
            .lock()
            .map_err(|_| "App state lock poisoned".to_string())?;
        find_instance_by_id(&guard.nerevar_config, &instance_id)
            .ok_or_else(|| format!("Instance not found: {instance_id}"))?
            .clone()
    };

    if instance.remote_host.is_some() {
        return Err(
            "Only an instance you host can suggest a runtime; a synced instance follows its host."
                .to_string(),
        );
    }

    instance.runtime_hint = hint.clone();
    let config = update_owned_instance(state, instance)?;
    set_hosting_runtime_hint(sync_host, &instance_id, hint.clone())?;
    emit_event(&*sink, "on_config_change", &config);

    Ok(hint)
}
