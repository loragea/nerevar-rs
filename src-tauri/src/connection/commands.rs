use std::path::Path;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, State};

use crate::config::update_synced_instance;
use crate::data::NewConnectionConfig;
use crate::instance_data::ensure_instance_data_layout;
use crate::instance_data::find_instance_by_id;
use crate::instance_data::{load_manifest, resolve_package_data_dir};
use crate::instance_setup::{
    create_instance_data_dir, ensure_instance_path_available, instance_paths, instance_tes3mp_dir,
    write_owned_client_connection, write_tes3mp_client_connection,
};
use crate::process_manager::{
    launch_tes3mp_client, launch_tes3mp_server, stop_tes3mp_process, GlobalProcessStatus,
    ProcessManager, ProcessRole,
};
use crate::reporter::{emit_event, EventSink, TauriEventSink};
use crate::sync_client::{
    fetch_manifest_summary, game_host, ping_nerevar_server, run_instance_sync, sync_if_needed,
    touch_last_synced, write_synced_client_connection, RemoteManifestSummary, SyncCoordinator,
    SyncOutcome,
};
use crate::AppState;
use nerevar_core::config::nerevar_config::{
    build_synced_instance_config, persist_synced_instance_to_config,
};
use nerevar_core::runtime::{self, TargetPlatform, TrustedRuntimeRepos};

#[tauri::command]
pub async fn ping_remote_nerevar_server(
    remote_host: String,
    remote_sync_port: u16,
) -> Result<(), String> {
    ping_nerevar_server(&remote_host, remote_sync_port).await
}

#[tauri::command]
pub async fn fetch_remote_manifest_summary(
    remote_host: String,
    remote_sync_port: u16,
    sync_password: Option<String>,
) -> Result<RemoteManifestSummary, String> {
    fetch_manifest_summary(&remote_host, remote_sync_port, sync_password.as_deref()).await
}

/// The instance directory a connection with this name would get, for the
/// page to show before the user submits.
///
/// The same `instance_paths` call the create itself makes, so the preview
/// cannot drift from the path that ends up on disk.
#[tauri::command]
pub fn preview_connection_instance_path(
    root_path: String,
    connection_name: String,
) -> Result<String, String> {
    Ok(instance_paths(Path::new(&root_path), &connection_name)?
        .root
        .display()
        .to_string())
}

/// Creates a synced instance for a remote host: instance tree, TES3MP
/// runtime, client cfg pointed at the host, then the config entry.
///
/// `operation_id` is the frontend's background-operation id when the create
/// was started from the app, so the runtime install's progress lands on the
/// banner the user is already looking at; `None` lets core mint one.
#[tauri::command]
pub async fn add_synced_connection(
    state: State<'_, Mutex<AppState>>,
    new_connection: NewConnectionConfig,
    operation_id: Option<String>,
) -> Result<String, String> {
    let paths = instance_paths(
        Path::new(&new_connection.root_path),
        &new_connection.connection_name,
    )?;
    let instance_root = paths.root.as_path();
    ensure_instance_path_available(instance_root)?;

    ping_nerevar_server(&new_connection.remote_host, new_connection.remote_sync_port).await?;
    let summary = fetch_manifest_summary(
        &new_connection.remote_host,
        new_connection.remote_sync_port,
        Some(new_connection.sync_password.as_str()),
    )
    .await?;

    // Resolved up front rather than after the download: the runtime install
    // reports its progress through it, and the trust list decides whether the
    // download is allowed at all. The check happens here as well as inside
    // `runtime::acquire` so a repository the player has not trusted is refused
    // before the instance tree is created, not after.
    let (runtime_sink, trusted) = {
        let guard = state
            .lock()
            .map_err(|_| "App state lock poisoned".to_string())?;
        let sink = guard
            .event_sink
            .clone()
            .ok_or_else(|| "Event sink not initialized".to_string())?;
        (
            sink,
            TrustedRuntimeRepos::from_config(&guard.nerevar_config),
        )
    };
    trusted.require_trusted_source(&new_connection.runtime)?;

    if let Err(err) = (async {
        std::fs::create_dir_all(instance_root).map_err(|e| e.to_string())?;
        let instance_data_dir = paths.data_dir.as_path();
        create_instance_data_dir(instance_data_dir)?;
        ensure_instance_data_layout(instance_data_dir)?;

        let tes3mp_dir = instance_tes3mp_dir(instance_root);
        std::fs::create_dir_all(&tes3mp_dir).map_err(|e| e.to_string())?;

        let installed = runtime::acquire(
            &new_connection.runtime,
            &tes3mp_dir,
            TargetPlatform::current(),
            runtime_sink.clone(),
            operation_id.clone(),
            &trusted,
        )
        .await?;
        installed.require_complete()?;

        // The game connection is UDP straight to TES3MP: a URL host names the
        // operator's HTTPS proxy, which only the sync traffic goes through, so
        // the client cfg gets its hostname alone.
        write_tes3mp_client_connection(
            &tes3mp_dir,
            &game_host(&new_connection.remote_host)?,
            summary.tes3mp_server_port,
            &new_connection.sync_password,
        )?;

        Ok::<(), String>(())
    })
    .await
    {
        if instance_root.exists() {
            let _ = std::fs::remove_dir_all(instance_root);
        }
        return Err(err);
    }

    let mut instance = build_synced_instance_config(&new_connection, &paths);
    instance.tes3mp_server_port = Some(summary.tes3mp_server_port);
    let instance_id = instance.id.clone();
    let (config, sink) = persist_synced_instance_to_config(state.inner(), instance)?;

    emit_event(&*sink, "on_config_added_connection", &config);
    emit_event(&*sink, "on_config_change", &config);

    Ok(instance_id)
}

#[tauri::command]
pub async fn sync_instance_from_remote(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    coordinator: State<'_, Arc<SyncCoordinator>>,
    instance_id: String,
) -> Result<SyncOutcome, String> {
    let (instance, trusted) = resolve_instance_and_trust(&state, &instance_id)?;

    let sink: Arc<dyn EventSink> = Arc::new(TauriEventSink::new(app));
    let outcome = run_instance_sync(
        sink.clone(),
        coordinator.inner().clone(),
        &instance,
        &trusted,
    )
    .await?;

    if outcome.validation.valid {
        let data_dir = resolve_package_data_dir(&instance);
        let manifest = load_manifest(&data_dir)?;
        let mut updated = instance;
        touch_last_synced(&mut updated, &manifest);
        let config = update_synced_instance(state.inner(), updated)?;
        emit_event(&*sink, "on_config_change", &config);
    }

    Ok(outcome)
}

/// The instance a command was pointed at, plus the trust list that decides
/// where its runtime may come from — both read under one lock.
fn resolve_instance_and_trust(
    state: &State<'_, Mutex<AppState>>,
    instance_id: &str,
) -> Result<(crate::data::InstanceConfig, TrustedRuntimeRepos), String> {
    let guard = state
        .lock()
        .map_err(|_| "App state lock poisoned".to_string())?;
    let instance = find_instance_by_id(&guard.nerevar_config, instance_id)
        .ok_or_else(|| format!("Instance not found: {instance_id}"))?
        .clone();
    Ok((
        instance,
        TrustedRuntimeRepos::from_config(&guard.nerevar_config),
    ))
}

/// Installs the TES3MP version this instance's host requires, replacing the
/// one it has.
///
/// The version is the host's to name; the repository is not — the new build
/// comes from the repository this instance was installed from, and only if the
/// player still trusts it. The old runtime stays in place until the new one has
/// been inspected, and the instance's client cfg is pointed back at the host
/// afterwards because a fresh install carries the release's default one.
#[tauri::command]
pub async fn update_instance_runtime(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    instance_id: String,
    required_tag: String,
    operation_id: Option<String>,
) -> Result<(), String> {
    let (instance, trusted) = resolve_instance_and_trust(&state, &instance_id)?;
    let source = instance.runtime.clone().ok_or_else(|| {
        format!("Instance {instance_id} records no runtime source, so there is nothing to update")
    })?;

    let sink: Arc<dyn EventSink> = Arc::new(TauriEventSink::new(app));
    let updated_source = runtime::update_instance_runtime(
        &instance_tes3mp_dir(Path::new(&instance.path)),
        &source,
        &required_tag,
        &trusted,
        TargetPlatform::current(),
        sink.clone(),
        operation_id,
    )
    .await?;

    let mut updated = instance.clone();
    updated.release_id = updated_source.legacy_release_id();
    updated.runtime = Some(updated_source);
    let config = update_synced_instance(state.inner(), updated)?;
    emit_event(&*sink, "on_config_change", &config);

    // The new install's `tes3mp-client-default.cfg` is the release's own, so
    // the host's address and password have to be written into it again.
    let data_dir = resolve_package_data_dir(&instance);
    if let Ok(manifest) = load_manifest(&data_dir) {
        write_synced_client_connection(&instance, &manifest)?;
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_instance_sync(
    coordinator: State<'_, Arc<SyncCoordinator>>,
    instance_id: String,
) -> Result<bool, String> {
    Ok(coordinator.cancel(&instance_id))
}

/// Launches an instance's TES3MP client, syncing a synced instance first.
///
/// `allow_runtime_mismatch` is the advanced escape hatch: a synced instance
/// whose host requires a TES3MP version other than the installed one does not
/// launch, because the client would be turned away (or misbehave) against a
/// server on a different build. A player who knows better can pass it and go
/// anyway.
#[tauri::command]
pub async fn launch_instance_client(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    process_manager: State<'_, Arc<ProcessManager>>,
    coordinator: State<'_, Arc<SyncCoordinator>>,
    instance_id: String,
    allow_runtime_mismatch: Option<bool>,
) -> Result<(), String> {
    let (instance, trusted) = resolve_instance_and_trust(&state, &instance_id)?;

    let sink: Arc<dyn EventSink> = Arc::new(TauriEventSink::new(app));

    if instance.remote_host.is_some() {
        let outcome = sync_if_needed(
            sink.clone(),
            coordinator.inner().clone(),
            &instance,
            false,
            &trusted,
        )
        .await?;

        if !outcome.validation.valid {
            return Err(format!(
                "Cannot launch: file validation failed ({} issue(s))",
                outcome.validation.issues.len()
            ));
        }

        if outcome.blocks_launch() && !allow_runtime_mismatch.unwrap_or(false) {
            let mismatch = outcome
                .runtime_mismatch
                .as_ref()
                .expect("blocks_launch implies a mismatch");
            return Err(mismatch.message.clone());
        }

        let data_dir = resolve_package_data_dir(&instance);
        let manifest = load_manifest(&data_dir)?;
        write_synced_client_connection(&instance, &manifest)?;

        let mut updated = instance.clone();
        touch_last_synced(&mut updated, &manifest);
        if updated.tes3mp_server_port != instance.tes3mp_server_port
            || updated.last_synced_at != instance.last_synced_at
        {
            let config = update_synced_instance(state.inner(), updated)?;
            emit_event(&*sink, "on_config_change", &config);
        }
    } else {
        let tes3mp_dir = instance_tes3mp_dir(Path::new(&instance.path));
        write_owned_client_connection(&tes3mp_dir)?;
    }

    process_manager
        .inner()
        .ensure_can_launch(&instance_id, ProcessRole::Client)?;

    let data_dir = resolve_package_data_dir(&instance);
    launch_tes3mp_client(
        sink,
        process_manager.inner().clone(),
        &instance_id,
        Path::new(&instance.path),
        &data_dir,
    )
}

#[tauri::command]
pub fn launch_instance_server(
    app: AppHandle,
    state: State<'_, Mutex<AppState>>,
    process_manager: State<'_, Arc<ProcessManager>>,
    instance_id: String,
) -> Result<(), String> {
    let instance = {
        let guard = state
            .lock()
            .map_err(|_| "App state lock poisoned".to_string())?;
        find_instance_by_id(&guard.nerevar_config, &instance_id)
            .ok_or_else(|| format!("Instance not found: {instance_id}"))?
            .clone()
    };

    process_manager
        .inner()
        .ensure_can_launch(&instance_id, ProcessRole::Server)?;

    let sink: Arc<dyn EventSink> = Arc::new(TauriEventSink::new(app));
    let data_dir = resolve_package_data_dir(&instance);
    launch_tes3mp_server(
        sink,
        process_manager.inner().clone(),
        &instance_id,
        Path::new(&instance.path),
        &data_dir,
    )
}

#[tauri::command]
pub fn stop_instance_process(
    app: AppHandle,
    process_manager: State<'_, Arc<ProcessManager>>,
    instance_id: String,
    role: String,
) -> Result<bool, String> {
    let role =
        ProcessRole::from_str(&role).ok_or_else(|| format!("Invalid process role: {role}"))?;
    let sink: Arc<dyn EventSink> = Arc::new(TauriEventSink::new(app));
    stop_tes3mp_process(sink, process_manager.inner(), &instance_id, role)
}

#[tauri::command]
pub fn is_instance_process_running(
    process_manager: State<'_, Arc<ProcessManager>>,
    instance_id: String,
    role: String,
) -> Result<bool, String> {
    let role =
        ProcessRole::from_str(&role).ok_or_else(|| format!("Invalid process role: {role}"))?;
    process_manager.is_running(&instance_id, role)
}

#[tauri::command]
pub fn get_global_process_status(
    process_manager: State<'_, Arc<ProcessManager>>,
) -> Result<GlobalProcessStatus, String> {
    process_manager.global_status()
}
