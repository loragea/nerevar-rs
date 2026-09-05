mod check;
mod cli;
mod config;
mod instance;
mod manifest;
mod signal;
mod sink;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use nerevar_core::instance_data::resolve_package_data_dir;
use nerevar_core::instance_setup::{instance_tes3mp_dir, read_tes3mp_server_settings};
use nerevar_core::nerevar_server::state::ServerContext;
use nerevar_core::process_manager::{launch_tes3mp_server, ProcessManager, ProcessRole};
use nerevar_core::reporter::EventSink;
use nerevar_core::runtime::normalize_runtime_hint;
use nerevar_core::supervisor::run_server_supervisor;
use nerevar_core::sync_host::{
    activate_hosting, deactivate_hosting, new_shared_hosting_manifest_cache, new_shared_sync_host,
};

use cli::Cli;
use sink::LogEventSink;

/// Exit code for "the TES3MP dedicated server exited on its own" — distinct
/// from 1 (config/startup failure) so `systemctl status` tells the two apart.
/// Either way a `Restart=on-failure` unit brings the whole thing back; see
/// docs/headless-hosting.md.
///
/// 69 is sysexits.h's `EX_UNAVAILABLE`, which is what this is: the service the
/// daemon exists to provide is gone. A small code would read worse — systemd
/// renders 1-8 with LSB names, so exiting 3 makes `systemctl status` claim
/// "status=3/NOTIMPLEMENTED".
const EXIT_SERVER_DIED: i32 = 69;

/// How often to check that the TES3MP child is still alive. Fast enough that
/// a crash means seconds of downtime, slow enough to be free.
const SERVER_WATCH_INTERVAL: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Default `info`; `RUST_LOG` overrides (per notes/nerevar-host-design.md
    // — log level via env, no --verbose/-q flag).
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    match run(cli).await {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            log::error!("{err}");
            std::process::exit(1);
        }
    }
}

/// Why the daemon stopped waiting.
enum Stop {
    /// SIGTERM/SIGINT — the service manager or an operator asked us to stop.
    Signal,
    /// The TES3MP dedicated server exited without being asked to.
    ServerDied,
}

async fn run(cli: Cli) -> Result<i32, String> {
    let resolved = config::resolve_and_load_config(cli.config.as_deref())?;
    let instance = instance::select_owned_instance(&resolved.config, cli.instance.as_deref())?;
    let sync_port = cli.port.unwrap_or(resolved.config.sync_port);

    let instance_id = instance.id.clone();
    let instance_name = instance.name.clone();
    let instance_root = Path::new(&instance.path).to_path_buf();
    let data_dir = resolve_package_data_dir(instance);

    // Before --check, so `--scan --check` previews the scanned result.
    if cli.scan {
        manifest::scan_data_dir(&data_dir)?;
    }

    if cli.check {
        let ok = check::run_check(&resolved, instance, sync_port);
        return Ok(if ok { 0 } else { 1 });
    }

    // Rebuild the manifest before hosting, exactly like the GUI's
    // set_hosting_instance: activating hosting without this served whatever
    // manifest happened to be on disk — and 404s when nothing ever built one,
    // which on a GUI-less host is the normal case.
    let hosted = manifest::prepare(
        &instance_id,
        &instance_name,
        &instance_root,
        &data_dir,
        !cli.no_manifest_rebuild,
    )?;

    // Same pattern as the GUI's `save_and_host_instance`: best-effort read,
    // empty password if the server cfg can't be parsed rather than a hard
    // failure (sync hosting can still come up; only the TES3MP launch below
    // needs the cfg to actually be there).
    let sync_password = read_tes3mp_server_settings(&instance_tes3mp_dir(&instance_root))
        .map(|settings| settings.password)
        .unwrap_or_default();

    // Clients dial the game port the manifest carries (read from the
    // instance's tes3mp-server-default.cfg). config.json's copy is the GUI's
    // cache of it; when they disagree the cfg wins, so say so rather than let
    // an admin trust the wrong number.
    if let Some(configured) = instance.tes3mp_server_port {
        if configured != hosted.tes3mp_server_port {
            log::warn!(
                "config.json records TES3MP game port {configured} for this instance, but its \
                 tes3mp-server-default.cfg says {port} — clients follow the cfg ({port})",
                port = hosted.tes3mp_server_port
            );
        }
    }

    let sync_host = new_shared_sync_host();
    let manifest_cache = new_shared_hosting_manifest_cache();

    // The supervisor loop runs until one of these three channels' senders
    // drops; hold them for the daemon's lifetime even though this first cut
    // never sends on them again after startup (no runtime port/enable
    // changes — that's config hot-reload, explicitly deferred).
    let (_port_tx, port_rx) = tokio::sync::watch::channel(sync_port);
    let (_retry_tx, retry_rx) = tokio::sync::watch::channel(0u64);
    let (_enabled_tx, enabled_rx) = tokio::sync::watch::channel(true);

    let server_ctx = ServerContext::new(sync_host.clone(), manifest_cache.clone());
    let sink: Arc<dyn EventSink> = Arc::new(LogEventSink);

    // The hint is advertised, not applied: a malformed one costs players a
    // preselected runtime, which is no reason to refuse to host. `--check`
    // prints the same verdict before a run.
    let runtime_hint = match instance.runtime_hint.clone().map(normalize_runtime_hint) {
        Some(Ok(hint)) => Some(hint),
        Some(Err(err)) => {
            log::warn!("Ignoring this instance's runtimeHint: {err}");
            None
        }
        None => None,
    };

    activate_hosting(
        &sync_host,
        &manifest_cache,
        instance_id.clone(),
        data_dir.clone(),
        instance_root.clone(),
        sync_password,
        runtime_hint,
        sink.clone(),
    )?;
    log::info!(
        "Sync hosting active for instance \"{instance_name}\" ({instance_id}) on port {sync_port}"
    );

    let supervisor_handle = tokio::spawn(run_server_supervisor(
        port_rx,
        retry_rx,
        enabled_rx,
        server_ctx,
        sink.clone(),
    ));

    let process_manager = Arc::new(ProcessManager::new());
    if cli.sync_only {
        log::info!("--sync-only: not launching the TES3MP dedicated server");
    } else {
        process_manager.ensure_can_launch(&instance_id, ProcessRole::Server)?;
        launch_tes3mp_server(
            sink.clone(),
            process_manager.clone(),
            &instance_id,
            &instance_root,
            &data_dir,
        )?;
        log::info!(
            "TES3MP dedicated server launched for instance \"{instance_name}\" on port {}",
            hosted.tes3mp_server_port
        );
    }

    let stop = wait_for_stop(&process_manager, &instance_id, cli.sync_only).await;
    let exit_code = match stop {
        Stop::Signal => {
            log::info!("Shutting down...");
            0
        }
        Stop::ServerDied => {
            log::error!(
                "TES3MP dedicated server exited on its own; shutting down sync hosting too so \
                 the service manager can restart the whole instance (exit {EXIT_SERVER_DIED})"
            );
            EXIT_SERVER_DIED
        }
    };

    if !cli.sync_only {
        // Still worth trying after ServerDied: the tracked child is the
        // wrapper script, and TES3MP's real ELF runs as its child, so the
        // wrapper dying does not guarantee the game port is free. A
        // process-group kill is the only thing that reliably reaps both.
        match process_manager.stop_blocking(Some(sink.clone()), &instance_id, ProcessRole::Server)
        {
            Ok(true) => log::info!("TES3MP dedicated server stopped"),
            Ok(false) => log::warn!("TES3MP dedicated server was not running at shutdown"),
            Err(err) => log::error!("Failed to stop TES3MP dedicated server: {err}"),
        }
    }

    if let Err(err) = deactivate_hosting(&sync_host, &manifest_cache, sink.clone()) {
        log::error!("Failed to deactivate hosting: {err}");
    }

    supervisor_handle.abort();
    log::info!("nerevar-host exiting with code {exit_code}");
    Ok(exit_code)
}

/// Waits for whichever comes first: a shutdown signal, or (unless
/// `--sync-only`) the TES3MP dedicated server exiting by itself. Without the
/// second arm the daemon sat there with sync hosting up and no game server —
/// the unit still `active (running)`, the friend group still unable to
/// connect, and nothing restarting anything.
async fn wait_for_stop(manager: &Arc<ProcessManager>, instance_id: &str, sync_only: bool) -> Stop {
    if sync_only {
        signal::wait_for_shutdown_signal().await;
        return Stop::Signal;
    }

    tokio::select! {
        _ = signal::wait_for_shutdown_signal() => Stop::Signal,
        _ = watch_server(manager, instance_id) => Stop::ServerDied,
    }
}

/// Resolves once the managed TES3MP server is no longer running. A lock error
/// counts as "gone": supervision is broken at that point, and exiting hands
/// the problem to the service manager instead of hiding it.
async fn watch_server(manager: &Arc<ProcessManager>, instance_id: &str) {
    loop {
        tokio::time::sleep(SERVER_WATCH_INTERVAL).await;
        match manager.is_running(instance_id, ProcessRole::Server) {
            Ok(true) => continue,
            Ok(false) => return,
            Err(err) => {
                log::error!("Cannot tell whether the TES3MP server is running: {err}");
                return;
            }
        }
    }
}
