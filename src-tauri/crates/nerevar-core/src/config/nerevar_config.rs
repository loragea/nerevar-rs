use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::data::{InstanceConfig, NerevarConfig, NewConnectionConfig, NewInstanceConfig};
use crate::port_conflict;
use crate::instance_data::ensure_instance_data_layout;
use crate::instance_setup::{apply_server_defaults, create_instance_data_dir, instance_tes3mp_dir};
use crate::reporter::{emit_event, EventSink};
use crate::runtime::{self, RuntimeSource, TargetPlatform};
use crate::AppState;
use log::info;
use uuid::Uuid;

const CONFIG_FILE_NAME: &str = "config.json";
// const TES3MP_081_RELEASE_ID: &str = "65767406";

/// Same path as `app.path().app_data_dir()` / `config.json` (see Tauri
/// `PathResolver::app_data_dir`). Top-layer split (step 7, see
/// notes/core-split-plan.md): this used to call `tauri::generate_context!()`
/// itself to get the app identifier; now it takes the identifier as a plain
/// parameter so core stays Tauri-free. The app-side wrapper
/// (`src-tauri/src/config/mod.rs`) resolves `context.config().identifier` via
/// `tauri::generate_context!()` and passes it in here — no duplicated
/// identifier constant on either side.
pub fn nerevar_config_file_path(identifier: &str) -> PathBuf {
    let app_data_dir = dirs::data_dir()
        .expect("Failed to resolve app data directory")
        .join(identifier);
    app_data_dir.join(CONFIG_FILE_NAME)
}

/// Non-panicking counterpart to `nerevar_config_file_path`: `None` when
/// `dirs::data_dir()` doesn't resolve (e.g. a service account with no home),
/// instead of panicking. The GUI treats a resolvable user data dir as a
/// startup invariant and keeps using the panicking form; the headless
/// `nerevar-host` daemon's config-resolution fallback (see
/// notes/nerevar-host-design.md) needs to handle the "doesn't resolve" case
/// gracefully, so it uses this instead.
pub fn nerevar_config_file_path_opt(identifier: &str) -> Option<PathBuf> {
    Some(dirs::data_dir()?.join(identifier).join(CONFIG_FILE_NAME))
}

pub fn load_or_create_nerevar_config_at(config_path: &Path) -> Result<NerevarConfig, String> {
    if !config_path.exists() {
        info!("Creating default config file at {}", config_path.display());
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let default_config = NerevarConfig {
            onboarding_complete: false,
            owned_instances: None,
            synced_instances: None,
            root_path: None,
            sync_port: 25567,
        };
        std::fs::write(
            config_path,
            serde_json::to_string_pretty(&default_config).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        info!("Default config file created at {}", config_path.display());
        return Ok(default_config);
    }

    let contents = std::fs::read_to_string(config_path).map_err(|e| e.to_string())?;
    info!("Loading config file from {}", config_path.display());
    let mut config: NerevarConfig = serde_json::from_str(&contents).map_err(|e| e.to_string())?;
    migrate_runtime_sources(&mut config);
    Ok(config)
}

/// Fills in `InstanceConfig::runtime` for instances written before the field
/// existed. Everything such an instance recorded is a `tes3mp/tes3mp`
/// release id — the tag and asset it was installed from were never stored,
/// so they migrate as empty. Nothing is written back: the migration is
/// applied on every load, and the field only lands on disk the next time
/// something saves the config.
fn migrate_runtime_sources(config: &mut NerevarConfig) {
    let instances = config
        .owned_instances
        .iter_mut()
        .chain(config.synced_instances.iter_mut())
        .flat_map(|list| list.iter_mut());

    for instance in instances {
        if instance.runtime.is_some() {
            continue;
        }
        let Some(release_id) = instance.release_id.as_deref() else {
            continue;
        };
        instance.runtime = Some(RuntimeSource::from_legacy_release_id(release_id));
    }
}

/// Read-only counterpart to `load_or_create_nerevar_config_at`: parses an
/// existing config file and errors if it is missing, instead of writing a
/// default one. For callers that must never create config as a side effect
/// of reading it (the headless `nerevar-host` daemon — see
/// notes/nerevar-host-design.md's Config section; the GUI's onboarding flow
/// is the only place auto-creation is desired).
pub fn load_nerevar_config_at(config_path: &Path) -> Result<NerevarConfig, String> {
    if !config_path.exists() {
        return Err(format!("Config file not found at {}", config_path.display()));
    }
    let contents = std::fs::read_to_string(config_path).map_err(|e| e.to_string())?;
    info!("Loading config file from {}", config_path.display());
    let mut config: NerevarConfig = serde_json::from_str(&contents).map_err(|e| e.to_string())?;
    migrate_runtime_sources(&mut config);
    Ok(config)
}

pub fn load_or_create_nerevar_config(
    state: &Mutex<AppState>,
) -> Result<NerevarConfig, String> {
    let config_path = state.lock().unwrap().nerevar_config_path.clone();
    load_or_create_nerevar_config_at(Path::new(&config_path))
}

pub async fn complete_onboarding(state: &Mutex<AppState>) -> Result<(), String> {
    let (sink, config, start_sync_server) = {
        let mut state = state.lock().unwrap();
        let was_complete = state.nerevar_config.onboarding_complete;
        state.nerevar_config.onboarding_complete = true;
        std::fs::write(
            Path::new(&state.nerevar_config_path),
            serde_json::to_string_pretty(&state.nerevar_config).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        log::info!("Onboarding marked as complete in config and app state");

        if !was_complete {
            start_sync_server_supervisor(&mut state);
        }

        (
            state
                .event_sink
                .clone()
                .ok_or_else(|| "Event sink not initialized".to_string())?,
            state.nerevar_config.clone(),
            !was_complete,
        )
    };

    emit_event(&*sink, "on_config_change", &config);

    if start_sync_server {
        let sink = sink.clone();
        // `tokio::spawn`, not `tauri::async_runtime::spawn` (step 7 signature
        // prep, see notes/core-split-plan.md): this fn now lives in core,
        // which has no Tauri runtime to reach for. Same underlying Tokio
        // runtime either way — the app's Tauri build already runs on one, and
        // this spawn only needs to outlive the calling command, not the
        // process, so the swap is behavior-preserving.
        tokio::spawn(async move {
            if let Ok(conflicts) = port_conflict::check_startup_conflicts(&config) {
                port_conflict::emit_port_conflicts(&*sink, conflicts);
            }
        });
    }

    Ok(())
}

fn start_sync_server_supervisor(state: &mut AppState) {
    if let Some(tx) = state.server_enabled_tx.clone() {
        let _ = tx.send(true);
    }

    let port = state.nerevar_config.sync_port;
    if let Some(tx) = state.server_port_tx.clone() {
        let _ = tx.send(port);
    }

    let next_retry = state.server_retry_generation.wrapping_add(1);
    state.server_retry_generation = next_retry;
    if let Some(tx) = state.server_retry_tx.clone() {
        let _ = tx.send(next_retry);
    }
}

pub async fn set_root_path(state: &Mutex<AppState>, path: String) -> Result<(), String> {
    let mut state = state.lock().unwrap();
    state.nerevar_config.root_path = Some(path);
    std::fs::write(
        Path::new(&state.nerevar_config_path),
        serde_json::to_string_pretty(&state.nerevar_config).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn set_sync_port(state: &Mutex<AppState>, port: i32) -> Result<(), String> {
    if !(1..=65535).contains(&port) {
        return Err(format!("Sync port must be between 1 and 65535, got {port}"));
    }

    let (config_path, config_snapshot, tx, sink, restart_server) = {
        let mut guard = state
            .lock()
            .map_err(|_| "App state lock poisoned".to_string())?;
        guard.nerevar_config.sync_port = port;
        (
            guard.nerevar_config_path.clone(),
            guard.nerevar_config.clone(),
            guard.server_port_tx.clone(),
            guard.event_sink.clone(),
            guard.nerevar_config.onboarding_complete,
        )
    };

    std::fs::write(
        Path::new(&config_path),
        serde_json::to_string_pretty(&config_snapshot).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    if restart_server {
        if let Some(tx) = tx {
            let _ = tx.send(port);
        }
    }

    if let Some(sink) = sink {
        emit_event(&*sink, "on_config_change", &config_snapshot);
    }

    Ok(())
}

fn new_instance_id() -> String {
    Uuid::new_v4().to_string()
}

fn build_instance_config(new_instance: &NewInstanceConfig) -> InstanceConfig {
    InstanceConfig {
        id: new_instance_id(),
        name: new_instance.instance_name.clone(),
        description: new_instance.instance_description.clone(),
        path: new_instance.instance_root_path.clone(),
        data_dir: new_instance.instance_data_dir.clone(),
        release_id: new_instance.runtime.legacy_release_id(),
        runtime: Some(new_instance.runtime.clone()),
        runtime_hint: None,
        remote_host: None,
        remote_sync_port: None,
        last_synced_at: None,
        tes3mp_server_port: None,
        sync_password: None,
    }
}

pub fn build_synced_instance_config(new_connection: &NewConnectionConfig) -> InstanceConfig {
    InstanceConfig {
        id: new_instance_id(),
        name: new_connection.connection_name.clone(),
        description: new_connection.connection_description.clone(),
        path: new_connection.instance_root_path.clone(),
        data_dir: new_connection.instance_data_dir.clone(),
        release_id: new_connection.runtime.legacy_release_id(),
        runtime: Some(new_connection.runtime.clone()),
        runtime_hint: None,
        remote_host: Some(new_connection.remote_host.clone()),
        remote_sync_port: Some(new_connection.remote_sync_port),
        last_synced_at: None,
        tes3mp_server_port: None,
        sync_password: Some(new_connection.sync_password.clone()),
    }
}

fn persist_owned_instance_to_config(
    state: &Mutex<AppState>,
    instance: InstanceConfig,
) -> Result<(NerevarConfig, Arc<dyn EventSink>), String> {
    let mut guard = state
        .lock()
        .map_err(|_| "App state lock poisoned".to_string())?;

    match guard.nerevar_config.owned_instances {
        Some(ref mut owned_instances) => owned_instances.push(instance),
        None => guard.nerevar_config.owned_instances = Some(vec![instance]),
    }

    std::fs::write(
        Path::new(&guard.nerevar_config_path),
        serde_json::to_string_pretty(&guard.nerevar_config).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    let config = guard.nerevar_config.clone();
    let sink = guard
        .event_sink
        .clone()
        .ok_or_else(|| "Event sink not initialized".to_string())?;

    Ok((config, sink))
}

pub fn persist_synced_instance_to_config(
    state: &Mutex<AppState>,
    instance: InstanceConfig,
) -> Result<(NerevarConfig, Arc<dyn EventSink>), String> {
    let mut guard = state
        .lock()
        .map_err(|_| "App state lock poisoned".to_string())?;

    match guard.nerevar_config.synced_instances {
        Some(ref mut synced_instances) => synced_instances.push(instance),
        None => guard.nerevar_config.synced_instances = Some(vec![instance]),
    }

    std::fs::write(
        Path::new(&guard.nerevar_config_path),
        serde_json::to_string_pretty(&guard.nerevar_config).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    let config = guard.nerevar_config.clone();
    let sink = guard
        .event_sink
        .clone()
        .ok_or_else(|| "Event sink not initialized".to_string())?;

    Ok((config, sink))
}

pub fn update_synced_instance(
    state: &Mutex<AppState>,
    instance: InstanceConfig,
) -> Result<NerevarConfig, String> {
    let mut guard = state
        .lock()
        .map_err(|_| "App state lock poisoned".to_string())?;

    let synced = guard
        .nerevar_config
        .synced_instances
        .as_mut()
        .ok_or_else(|| "No synced instances configured".to_string())?;

    let entry = synced
        .iter_mut()
        .find(|i| i.id == instance.id)
        .ok_or_else(|| format!("Synced instance not found: {}", instance.id))?;
    *entry = instance;

    std::fs::write(
        Path::new(&guard.nerevar_config_path),
        serde_json::to_string_pretty(&guard.nerevar_config).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    Ok(guard.nerevar_config.clone())
}

pub fn update_owned_instance(
    state: &Mutex<AppState>,
    instance: InstanceConfig,
) -> Result<NerevarConfig, String> {
    let mut guard = state
        .lock()
        .map_err(|_| "App state lock poisoned".to_string())?;

    let owned = guard
        .nerevar_config
        .owned_instances
        .as_mut()
        .ok_or_else(|| "No owned instances configured".to_string())?;

    let entry = owned
        .iter_mut()
        .find(|i| i.id == instance.id)
        .ok_or_else(|| format!("Owned instance not found: {}", instance.id))?;
    *entry = instance;

    std::fs::write(
        Path::new(&guard.nerevar_config_path),
        serde_json::to_string_pretty(&guard.nerevar_config).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    Ok(guard.nerevar_config.clone())
}

fn cleanup_failed_instance_root(path: &Path) {
    if path.exists() {
        if let Err(err) = std::fs::remove_dir_all(path) {
            info!(
                "Failed to clean up instance directory at {}: {err}",
                path.display()
            );
        }
    }
}

/// Creates an owned instance: instance tree, TES3MP runtime, server defaults,
/// then the config entry.
///
/// `operation_id` is the frontend's background-operation id when the create
/// was started from the app, so the runtime install's progress lands on the
/// banner the user is already looking at; `None` lets core mint one.
pub async fn add_instance(
    state: &Mutex<AppState>,
    new_instance: NewInstanceConfig,
    operation_id: Option<String>,
) -> Result<(), String> {
    let instance_root = Path::new(&new_instance.instance_root_path);
    if instance_root.exists() {
        return Err(format!(
            "Instance path already exists: {}",
            instance_root.display()
        ));
    }

    // Resolved up front rather than after the download: the runtime install
    // reports its progress through it.
    let sink = {
        let guard = state
            .lock()
            .map_err(|_| "App state lock poisoned".to_string())?;
        guard
            .event_sink
            .clone()
            .ok_or_else(|| "Event sink not initialized".to_string())?
    };

    // Filesystem setup first; only persist config after success.
    if let Err(err) = (async {
        std::fs::create_dir_all(instance_root).map_err(|e| e.to_string())?;
        let instance_data_dir = Path::new(&new_instance.instance_data_dir);
        create_instance_data_dir(instance_data_dir)?;
        ensure_instance_data_layout(instance_data_dir)?;

        let tes3mp_dir = instance_tes3mp_dir(instance_root);
        std::fs::create_dir_all(&tes3mp_dir).map_err(|e| e.to_string())?;

        let installed = runtime::acquire(
            &new_instance.runtime,
            &tes3mp_dir,
            TargetPlatform::current(),
            sink.clone(),
            operation_id.clone(),
        )
        .await?;
        installed.require_complete()?;

        apply_server_defaults(&tes3mp_dir, &new_instance)?;

        Ok::<(), String>(())
    })
    .await
    {
        cleanup_failed_instance_root(instance_root);
        return Err(err);
    }

    let instance = build_instance_config(&new_instance);
    let (config, sink) = persist_owned_instance_to_config(state, instance)?;

    emit_event(&*sink, "on_config_added_instance", &config);
    emit_event(&*sink, "on_config_change", &config);

    info!(
        "Instance '{}' created at {}",
        new_instance.instance_name, new_instance.instance_root_path
    );

    Ok(())
}

// pub async fn download_and_run_openmw_wizard(
//     state: State<'_, Mutex<AppState>>,
// ) -> Result<(), String> {
//     // Use const release id to call github_getters::download_and_extract_release_zip_by_id_to_path
//     // Use the root path + "Base TES3MP" to create the path for the extracted files
//     // once extracted run openmw-wizard.exe
//     // once openmw-wizard.exe is done, return Ok(())
//     // if any error occurs, return Err(String::from("Failed to download and run openmw-wizard"))
//     let root_path = state
//         .lock()
//         .unwrap()
//         .nerevar_config
//         .root_path
//         .clone()
//         .ok_or_else(|| "Root path not set".to_string())?;
//     let base_tes3mp_path = Path::new(&root_path).join("Base TES3MP");
//     std::fs::create_dir_all(&base_tes3mp_path).map_err(|e| e.to_string())?;
//     github_getters::download_and_extract_release_zip_by_id_to_path(
//         TES3MP_081_RELEASE_ID.to_string(),
//         base_tes3mp_path.to_string_lossy().into_owned(),
//     )
//     .await?;
//     let openmw_wizard_path = base_tes3mp_path.join("openmw-wizard.exe");
//     if !openmw_wizard_path.exists() {
//         return Err(String::from("Failed to download and run openmw-wizard"));
//     }
//     let mut result = std::process::Command::new(openmw_wizard_path)
//         .spawn()
//         .map_err(|e| e.to_string())?;
//     let status = result.wait().map_err(|e| e.to_string())?;
//     if !status.success() {
//         return Err(String::from("Failed to run openmw-wizard"));
//     }
//     Ok(())
// }

pub async fn validate_global_openmw_config() -> Result<bool, String> {
    crate::openmw_ini_importer::validate_nerevar_openmw_scaffold()
}

pub async fn generate_default_global_openmw_config(
    morrowind_installation_path: String,
) -> Result<(), String> {
    crate::openmw_ini_importer::setup_nerevar_openmw_scaffold(Path::new(
        &morrowind_installation_path,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::DEFAULT_TES3MP_REPO;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-config-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// A config.json exactly as builds before `runtime` wrote it.
    const LEGACY_CONFIG: &str = r#"{
      "onboardingComplete": true,
      "ownedInstances": [
        {
          "id": "owned-1",
          "name": "Owned",
          "description": "",
          "path": "/instances/owned",
          "dataDir": "/instances/owned/data",
          "releaseId": "65767406"
        }
      ],
      "syncedInstances": [
        {
          "id": "synced-1",
          "name": "Synced",
          "description": "",
          "path": "/instances/synced",
          "dataDir": "/instances/synced/data",
          "releaseId": "65767406",
          "remoteHost": "example.invalid",
          "remoteSyncPort": 25567
        }
      ],
      "rootPath": "/instances",
      "syncPort": 25567
    }"#;

    fn legacy_source() -> RuntimeSource {
        RuntimeSource::GithubRelease {
            repo: DEFAULT_TES3MP_REPO.to_string(),
            release_id: "65767406".to_string(),
            tag: String::new(),
            asset_name: String::new(),
        }
    }

    #[test]
    fn legacy_release_id_becomes_a_runtime_source_on_load() {
        let scratch = scratch("legacy");
        let path = scratch.0.join("config.json");
        std::fs::write(&path, LEGACY_CONFIG).unwrap();

        let config = load_nerevar_config_at(&path).expect("legacy config should load");

        let owned = &config.owned_instances.as_ref().unwrap()[0];
        assert_eq!(owned.release_id.as_deref(), Some("65767406"));
        assert_eq!(owned.runtime.as_ref(), Some(&legacy_source()));

        let synced = &config.synced_instances.as_ref().unwrap()[0];
        assert_eq!(synced.runtime.as_ref(), Some(&legacy_source()));

        // The same migration on the create-if-missing path.
        let created = load_or_create_nerevar_config_at(&path).expect("load");
        assert_eq!(
            created.owned_instances.as_ref().unwrap()[0].runtime.as_ref(),
            Some(&legacy_source())
        );
    }

    #[test]
    fn an_explicit_runtime_survives_a_load_untouched() {
        let scratch = scratch("explicit");
        let path = scratch.0.join("config.json");
        std::fs::write(
            &path,
            r#"{
              "onboardingComplete": true,
              "ownedInstances": [
                {
                  "id": "owned-1",
                  "name": "Owned",
                  "description": "",
                  "path": "/instances/owned",
                  "dataDir": "/instances/owned/data",
                  "releaseId": "1",
                  "runtime": {
                    "kind": "githubRelease",
                    "repo": "someone/tes3mp",
                    "releaseId": "999",
                    "tag": "v1.2.3",
                    "assetName": "tes3mp.Win64.zip"
                  }
                }
              ],
              "syncedInstances": null,
              "rootPath": "/instances",
              "syncPort": 25567
            }"#,
        )
        .unwrap();

        let config = load_nerevar_config_at(&path).expect("config should load");
        let owned = &config.owned_instances.as_ref().unwrap()[0];
        assert_eq!(
            owned.runtime,
            Some(RuntimeSource::GithubRelease {
                repo: "someone/tes3mp".to_string(),
                release_id: "999".to_string(),
                tag: "v1.2.3".to_string(),
                asset_name: "tes3mp.Win64.zip".to_string(),
            })
        );
    }

    /// An instance that never had a release id (host-side configs written by
    /// hand, see docs/headless-hosting.md) stays runtime-less rather than
    /// gaining an invented one.
    #[test]
    fn an_instance_with_no_release_id_gets_no_runtime() {
        let scratch = scratch("no-release-id");
        let path = scratch.0.join("config.json");
        std::fs::write(
            &path,
            r#"{
              "onboardingComplete": true,
              "ownedInstances": [
                {
                  "id": "owned-1",
                  "name": "Owned",
                  "description": "",
                  "path": "/instances/owned",
                  "dataDir": "/instances/owned/data"
                }
              ],
              "syncedInstances": null,
              "rootPath": "/instances",
              "syncPort": 25567
            }"#,
        )
        .unwrap();

        let config = load_nerevar_config_at(&path).expect("config should load");
        assert!(config.owned_instances.as_ref().unwrap()[0].runtime.is_none());
    }

    /// A host operator's suggestion survives a load, and an instance without
    /// one comes back with `None` — the shape of every config written before
    /// the field existed.
    #[test]
    fn a_runtime_hint_round_trips_through_the_config_file() {
        let scratch = scratch("hint");
        let path = scratch.0.join("config.json");
        std::fs::write(&path, LEGACY_CONFIG).unwrap();

        let mut config = load_nerevar_config_at(&path).expect("config should load");
        assert!(
            config.owned_instances.as_ref().unwrap()[0]
                .runtime_hint
                .is_none(),
            "a config written before the field must load as no suggestion"
        );

        let hint = RuntimeSource::GithubRelease {
            repo: "owner/name".to_string(),
            release_id: "999".to_string(),
            tag: "v1.2.3".to_string(),
            asset_name: String::new(),
        };
        config.owned_instances.as_mut().unwrap()[0].runtime_hint = Some(hint.clone());
        std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        // Written under the camelCase name the daemon's operator docs name.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("\"runtimeHint\""),
            "not written as runtimeHint: {raw}"
        );

        let reloaded = load_nerevar_config_at(&path).expect("config should reload");
        assert_eq!(
            reloaded.owned_instances.as_ref().unwrap()[0].runtime_hint,
            Some(hint)
        );
        // An instance with no suggestion writes no key at all, so an older
        // build reading this file sees exactly what it saw before.
        assert!(
            !serde_json::to_string(&reloaded.synced_instances.as_ref().unwrap()[0])
                .unwrap()
                .contains("runtimeHint")
        );
    }

    /// A config this build writes keeps the legacy `releaseId` alongside the
    /// new `runtime`, so an older Nerevar still finds what it looks for.
    #[test]
    fn a_new_instance_records_both_the_runtime_and_the_legacy_release_id() {
        let new_instance = NewInstanceConfig {
            runtime: legacy_source(),
            instance_name: "Owned".to_string(),
            instance_description: String::new(),
            instance_root_path: "/instances/owned".to_string(),
            instance_data_dir: "/instances/owned/data".to_string(),
            server_host_name: "A Nerevar Server".to_string(),
            max_players: 64,
            server_port: 25565,
            password: String::new(),
            master_server_enabled: true,
        };

        let instance = build_instance_config(&new_instance);
        assert_eq!(instance.release_id.as_deref(), Some("65767406"));
        assert_eq!(instance.runtime.as_ref(), Some(&legacy_source()));

        let json = serde_json::to_value(&instance).unwrap();
        assert_eq!(json["releaseId"], "65767406");
        assert_eq!(json["runtime"]["kind"], "githubRelease");
    }
}
