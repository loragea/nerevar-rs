//! `nerevar-cli sync` — headless client-side sync driver.
//!
//! Syncs one *synced* Nerevar instance from its host and, optionally, launches
//! the TES3MP client for it, without the desktop app. Uses only
//! `nerevar-core`'s public API, the same functions the Tauri commands
//! `sync_instance_from_remote` / `launch_instance_client` call, so what it
//! proves about sync and launch holds for the app too.
//!
//! It is a developer/rig tool that also serves a Linux user without a desktop:
//! point it at the same `config.json` the app writes and the synced instance's
//! id, and it fetches the host manifest, downloads or resumes the packages,
//! writes the instance's launch cfg and patches its
//! `tes3mp-client-default.cfg` with the host's game port and password.
//!
//! ```text
//! nerevar-cli sync --config <nerevar config.json> --instance <id or name> \
//!     [--install-runtime] [--force] [--launch] \
//!     [--onboard <Morrowind Data Files dir>]
//!
//!   --install-runtime
//!               install the instance's configured TES3MP runtime into
//!               <instance>/tes3mp/ before syncing, if that directory is not
//!               already populated. This is what the desktop's create flow
//!               does; the driver does it separately because it is handed an
//!               instance that already exists in a config.json
//!   --force     re-download every file instead of resuming from sync-state
//!   --launch    after a valid sync, launch the instance's TES3MP client and
//!               block until it exits or this process gets SIGTERM/SIGINT
//!   --onboard   first write the global OpenMW scaffold (openmw.nerevar.cfg)
//!               from that Data Files dir, exactly like the app's onboarding —
//!               needed once per user profile before --launch can work
//! ```
//!
//! Output: one JSON object per line on stdout, `{"event": <name>, "payload":
//! <value>}`, carrying every core event exactly as the app would receive it
//! (`sync-progress`, `runtime-mismatch`, `process-output`, `process-status`,
//! ...), plus two of its own: `sync-result` (the `SyncOutcome`) and
//! `sync-error`. Diagnostics go to stderr (`RUST_LOG=debug` for more).
//!
//! A host may require a particular TES3MP version. When the installed runtime
//! is not it, core emits `runtime-mismatch` and the outcome carries it:
//! `--install-runtime` then updates the runtime from the instance's own
//! repository before continuing, and without it a requested `--launch` is
//! refused (exit 1) rather than starting a client the server will not have.
//!
//! `--launch` does the global `openmw.cfg` swap exactly like the app: it
//! composes the profile's `openmw.nerevar.cfg` with the instance launch cfg
//! into the per-user OpenMW config dir (`$XDG_CONFIG_HOME/openmw` on Linux,
//! `Documents/My Games/OpenMW` on Windows, `~/Library/Preferences/openmw` on
//! macOS) for the client's lifetime and restores it afterwards. Run it with
//! the environment you want the client to see, and never two at once per
//! profile.
//!
//! Exit codes: 0 sync ok (and, with --launch, the client exited with 0 or was
//! stopped cleanly on a signal); 1 validation failed, or --launch was asked
//! for while the host requires a TES3MP version this instance does not have;
//! 2 error.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nerevar_core::config::{generate_default_global_openmw_config, load_nerevar_config_at};
use nerevar_core::data::{InstanceConfig, NerevarConfig};
use nerevar_core::instance_data::{load_manifest, resolve_package_data_dir};
use nerevar_core::instance_setup::instance_tes3mp_dir;
use nerevar_core::process_manager::{launch_tes3mp_client, ProcessManager, ProcessRole};
use nerevar_core::reporter::EventSink;
use nerevar_core::runtime::{self, RuntimeMismatch, TargetPlatform, TrustedRuntimeRepos};
use nerevar_core::sync_client::{
    sync_if_needed, touch_last_synced, write_synced_client_connection, SyncCoordinator,
};

use crate::cli::SyncArgs;

/// How long to wait for the exit watcher's `process-status` after the client
/// is gone before giving up on its exit code.
const EXIT_STATUS_GRACE: Duration = Duration::from_secs(3);

/// Prints every core event as one JSON line on stdout and forwards the
/// client's exit (its `process-status` with `running: false`) to [`run`].
struct JsonLinesSink {
    client_exit: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Option<i32>>>>,
}

impl JsonLinesSink {
    fn new(client_exit: tokio::sync::mpsc::UnboundedSender<Option<i32>>) -> Self {
        Self {
            client_exit: Mutex::new(Some(client_exit)),
        }
    }
}

fn print_event(event: &str, payload: serde_json::Value) {
    let line = serde_json::json!({ "event": event, "payload": payload });
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

impl EventSink for JsonLinesSink {
    fn emit(&self, event: &'static str, payload: serde_json::Value) {
        if event == "process-status"
            && payload.get("role").and_then(|v| v.as_str()) == Some("client")
            && payload.get("running").and_then(|v| v.as_bool()) == Some(false)
        {
            let code = payload
                .get("exitCode")
                .and_then(|v| v.as_i64())
                .map(|v| v as i32);
            if let Ok(guard) = self.client_exit.lock() {
                if let Some(tx) = guard.as_ref() {
                    let _ = tx.send(code);
                }
            }
        }
        print_event(event, payload);
    }
}

fn find_synced_instance(config: &NerevarConfig, key: &str) -> Option<InstanceConfig> {
    let synced = config.synced_instances.as_deref().unwrap_or(&[]);
    synced
        .iter()
        .find(|i| i.id == key)
        .or_else(|| synced.iter().find(|i| i.name == key))
        .cloned()
}

/// Same on-disk effect as the app's `update_synced_instance` after a sync:
/// the instance entry gains `lastSyncedAt` and the manifest's game port.
fn persist_synced_instance(
    config_path: &Path,
    config: &mut NerevarConfig,
    updated: InstanceConfig,
) -> Result<(), String> {
    let synced = config
        .synced_instances
        .as_mut()
        .ok_or_else(|| "No synced instances configured".to_string())?;
    let entry = synced
        .iter_mut()
        .find(|i| i.id == updated.id)
        .ok_or_else(|| format!("Synced instance not found: {}", updated.id))?;
    *entry = updated;
    std::fs::write(
        config_path,
        serde_json::to_string_pretty(config).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("Failed to write {}: {e}", config_path.display()))
}

/// Installs the instance's configured TES3MP runtime into `<instance>/tes3mp/`
/// through `runtime::acquire` — the same call the desktop's create flow makes.
///
/// Skipped when that directory already holds a complete runtime: this driver
/// is pointed at an instance that may already be installed, and re-installing
/// over a live tree would clobber the cfgs Nerevar has patched. "Complete"
/// rather than "non-empty" is the test because the wrapper's own profile
/// directory can exist beside a runtime that was never installed.
async fn install_runtime(
    instance: &InstanceConfig,
    trusted: &TrustedRuntimeRepos,
    sink: Arc<dyn EventSink>,
) -> Result<(), String> {
    let source = instance.runtime.as_ref().ok_or_else(|| {
        format!(
            "Instance {} records no runtime source, so there is nothing to install",
            instance.id
        )
    })?;

    let tes3mp_dir = instance_tes3mp_dir(Path::new(&instance.path));
    let installed = runtime::inspect(&tes3mp_dir)
        .map(|info| info.require_complete().is_ok())
        .unwrap_or(false);
    if installed {
        log::info!(
            "TES3MP runtime already installed at {} — skipping",
            tes3mp_dir.display()
        );
        return Ok(());
    }

    let info = runtime::acquire(
        source,
        &tes3mp_dir,
        TargetPlatform::current(),
        sink,
        None,
        trusted,
    )
    .await?;
    info.require_complete()?;
    Ok(())
}

/// What this run does about a host requiring a TES3MP version the instance
/// does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MismatchAction {
    /// Report it and carry on: either nothing is being launched, or the
    /// mismatch is one Nerevar cannot act on (a runtime installed from the
    /// user's own folder or archive, whose version it cannot read).
    Report,
    /// Install the required version first — what `--install-runtime` asks for.
    Update,
    /// Refuse: `--launch` was requested and nothing authorised an update, so
    /// starting the client would connect a wrong build to the server.
    RefuseLaunch,
}

/// Decides [`MismatchAction`] from the flags this run was given.
///
/// Split out from [`run`] so the decision is testable without a host, a
/// network, or a TES3MP install: it is the whole of the CLI's version-lock
/// policy.
pub fn mismatch_action(
    mismatch: &RuntimeMismatch,
    install_runtime: bool,
    launch: bool,
) -> MismatchAction {
    if !mismatch.enforced {
        return MismatchAction::Report;
    }
    if install_runtime {
        MismatchAction::Update
    } else if launch {
        MismatchAction::RefuseLaunch
    } else {
        MismatchAction::Report
    }
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => log::info!("Received SIGTERM"),
        _ = sigint.recv() => log::info!("Received SIGINT"),
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    log::info!("Received Ctrl-C");
}

/// Runs the sync, returning the process exit code: 0 ok, 1 the manifest
/// validation failed. Every failure is an `Err`, which the caller reports as
/// a `sync-error` line and exit 2.
pub async fn run(args: SyncArgs) -> Result<i32, String> {
    if let Some(data_files) = &args.onboard {
        generate_default_global_openmw_config(data_files.to_string_lossy().into_owned()).await?;
        log::info!(
            "Wrote the global OpenMW scaffold from {}",
            data_files.display()
        );
    }

    let mut config = load_nerevar_config_at(&args.config)?;
    let trusted = TrustedRuntimeRepos::from_config(&config);
    let mut instance = find_synced_instance(&config, &args.instance)
        .ok_or_else(|| format!("Synced instance not found: {}", args.instance))?;
    let instance_id = instance.id.clone();

    let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel::<Option<i32>>();
    let sink: Arc<dyn EventSink> = Arc::new(JsonLinesSink::new(exit_tx));
    let coordinator = Arc::new(SyncCoordinator::new());

    if args.install_runtime {
        install_runtime(&instance, &trusted, sink.clone()).await?;
    }

    let outcome =
        sync_if_needed(sink.clone(), coordinator, &instance, args.force, &trusted).await?;
    print_event(
        "sync-result",
        serde_json::to_value(&outcome).map_err(|e| e.to_string())?,
    );
    if !outcome.validation.valid {
        return Ok(1);
    }

    if let Some(mismatch) = &outcome.runtime_mismatch {
        match mismatch_action(mismatch, args.install_runtime, args.launch) {
            MismatchAction::Report => log::warn!("{}", mismatch.message),
            MismatchAction::RefuseLaunch => {
                eprintln!(
                    "nerevar-cli sync: {} Re-run with --install-runtime to install it.",
                    mismatch.message
                );
                return Ok(1);
            }
            MismatchAction::Update => {
                log::info!("{} Installing {}.", mismatch.message, mismatch.required_tag);
                let source = instance.runtime.clone().ok_or_else(|| {
                    format!("Instance {instance_id} records no runtime source to update")
                })?;
                let updated = runtime::update_instance_runtime(
                    &instance_tes3mp_dir(Path::new(&instance.path)),
                    &source,
                    &mismatch.required_tag,
                    &trusted,
                    TargetPlatform::current(),
                    sink.clone(),
                    None,
                )
                .await?;
                instance.release_id = updated.legacy_release_id();
                instance.runtime = Some(updated);
                persist_synced_instance(&args.config, &mut config, instance.clone())?;
            }
        }
    }

    // What `launch_instance_client` does for a remote instance once the sync
    // is valid: point the client cfg at the host and record the sync.
    let data_dir = resolve_package_data_dir(&instance);
    let manifest = load_manifest(&data_dir)?;
    write_synced_client_connection(&instance, &manifest)?;
    let mut updated = instance.clone();
    touch_last_synced(&mut updated, &manifest);
    persist_synced_instance(&args.config, &mut config, updated)?;

    if !args.launch {
        return Ok(0);
    }

    let manager = Arc::new(ProcessManager::new());
    manager.ensure_can_launch(&instance_id, ProcessRole::Client)?;
    launch_tes3mp_client(
        sink.clone(),
        manager.clone(),
        &instance_id,
        Path::new(&instance.path),
        &data_dir,
    )?;

    // The exit watcher (not `is_running`) is the authority on "the client is
    // gone": it is the path that restores the global openmw.cfg and emits the
    // final process-status, so waiting on its event never races it.
    let stopped_by_signal = tokio::select! {
        _ = wait_for_shutdown_signal() => true,
        code = exit_rx.recv() => {
            return Ok(match code.flatten() {
                Some(0) => 0,
                other => {
                    log::error!("TES3MP client exited with code {other:?}");
                    2
                }
            });
        }
    };

    if stopped_by_signal {
        log::info!("Stopping the TES3MP client");
        manager.stop_blocking(Some(sink.clone()), &instance_id, ProcessRole::Client)?;
        // Drain the status the stop emitted so the log line lands before exit.
        let _ = tokio::time::timeout(EXIT_STATUS_GRACE, exit_rx.recv()).await;
    }
    Ok(0)
}

/// [`run`], with the failure convention the rig depends on: a `sync-error`
/// event line on stdout, the message on stderr, exit 2.
pub async fn run_reporting_errors(args: SyncArgs) -> i32 {
    match run(args).await {
        Ok(code) => code,
        Err(err) => {
            print_event("sync-error", serde_json::Value::String(err.clone()));
            eprintln!("nerevar-cli sync: {err}");
            2
        }
    }
}
