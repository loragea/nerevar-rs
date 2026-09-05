//! Headless client-side sync driver: sync one *synced* Nerevar instance from
//! its host and, optionally, launch the TES3MP client for it — without the
//! desktop app. Uses only `nerevar-core`'s public API, the same functions the
//! Tauri commands `sync_instance_from_remote` / `launch_instance_client` call,
//! so what it proves about sync and launch holds for the app too.
//!
//! It is a developer/rig tool that also serves a Linux user without a desktop:
//! point it at the same `config.json` the app writes and the synced instance's
//! id, and it fetches the host manifest, downloads or resumes the packages,
//! writes the instance's launch cfg and patches its `tes3mp-client-default.cfg`
//! with the host's game port and password.
//!
//! ```text
//! cargo run -p nerevar-core --example sync_client -- \
//!     --config <nerevar config.json> --instance <id or name> \
//!     [--force] [--launch] [--onboard <Morrowind Data Files dir>]
//!
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
//! (`sync-progress`, `process-output`, `process-status`, ...), plus two of its
//! own: `sync-result` (the `ManifestValidationResult`) and `sync-error`.
//! Diagnostics go to stderr (`RUST_LOG=debug` for more).
//!
//! `--launch` does the global `openmw.cfg` swap exactly like the app: it
//! composes the profile's `openmw.nerevar.cfg` with the instance launch cfg
//! into the per-user OpenMW config dir (`$XDG_CONFIG_HOME/openmw` on Linux,
//! `Documents/My Games/OpenMW` on Windows, `~/Library/Preferences/openmw` on
//! macOS) for the client's lifetime and restores it afterwards. Run it with the
//! environment you want the client to see, and never two at once per profile.
//!
//! Exit codes: 0 sync ok (and, with --launch, the client exited with 0 or was
//! stopped cleanly on a signal); 1 validation failed; 2 error.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nerevar_core::config::{generate_default_global_openmw_config, load_nerevar_config_at};
use nerevar_core::data::{InstanceConfig, NerevarConfig};
use nerevar_core::instance_data::{load_manifest, resolve_package_data_dir};
use nerevar_core::process_manager::{launch_tes3mp_client, ProcessManager, ProcessRole};
use nerevar_core::reporter::EventSink;
use nerevar_core::sync_client::{
    sync_if_needed, touch_last_synced, write_synced_client_connection, SyncCoordinator,
};

const USAGE: &str = "usage: sync_client --config <config.json> --instance <id-or-name> \
[--force] [--launch] [--onboard <Morrowind Data Files dir>]";

/// How long to wait for the exit watcher's `process-status` after the client
/// is gone before giving up on its exit code.
const EXIT_STATUS_GRACE: Duration = Duration::from_secs(3);

struct Args {
    config: PathBuf,
    instance: String,
    force: bool,
    launch: bool,
    onboard: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut config = None;
    let mut instance = None;
    let mut force = false;
    let mut launch = false;
    let mut onboard = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config = Some(PathBuf::from(value_of(&mut args, "--config")?)),
            "--instance" => instance = Some(value_of(&mut args, "--instance")?),
            "--onboard" => onboard = Some(PathBuf::from(value_of(&mut args, "--onboard")?)),
            "--force" => force = true,
            "--launch" => launch = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}\n{USAGE}")),
        }
    }

    Ok(Args {
        config: config.ok_or_else(|| format!("--config is required\n{USAGE}"))?,
        instance: instance.ok_or_else(|| format!("--instance is required\n{USAGE}"))?,
        force,
        launch,
        onboard,
    })
}

fn value_of(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))
}

/// Prints every core event as one JSON line on stdout and forwards the
/// client's exit (its `process-status` with `running: false`) to `main`.
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

/// Minimal stderr logger so core's `log::` diagnostics (what it launched, what
/// it restored) are visible; level from `RUST_LOG` (error|warn|info|debug|
/// trace), default info. No `env_logger` here — it is not a core dependency.
struct StderrLogger;

static LOGGER: StderrLogger = StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{} {}] {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

fn install_logger() {
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|v| v.parse::<log::LevelFilter>().ok())
        .unwrap_or(log::LevelFilter::Info);
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level);
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

async fn run(args: Args) -> Result<i32, String> {
    if let Some(data_files) = &args.onboard {
        generate_default_global_openmw_config(data_files.to_string_lossy().into_owned()).await?;
        log::info!(
            "Wrote the global OpenMW scaffold from {}",
            data_files.display()
        );
    }

    let mut config = load_nerevar_config_at(&args.config)?;
    let instance = find_synced_instance(&config, &args.instance)
        .ok_or_else(|| format!("Synced instance not found: {}", args.instance))?;
    let instance_id = instance.id.clone();

    let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel::<Option<i32>>();
    let sink: Arc<dyn EventSink> = Arc::new(JsonLinesSink::new(exit_tx));
    let coordinator = Arc::new(SyncCoordinator::new());

    let validation = sync_if_needed(sink.clone(), coordinator, &instance, args.force).await?;
    print_event(
        "sync-result",
        serde_json::to_value(&validation).map_err(|e| e.to_string())?,
    );
    if !validation.valid {
        return Ok(1);
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

#[tokio::main]
async fn main() {
    install_logger();
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("sync_client: {err}");
            std::process::exit(2);
        }
    };

    let code = match run(args).await {
        Ok(code) => code,
        Err(err) => {
            print_event("sync-error", serde_json::Value::String(err.clone()));
            eprintln!("sync_client: {err}");
            2
        }
    };
    std::process::exit(code);
}
