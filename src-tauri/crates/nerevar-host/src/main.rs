mod check;
mod cli;
mod config;
mod instance;
mod manifest;
mod signal;
mod sink;

use std::path::Path;
use std::sync::Arc;

use clap::Parser;

use nerevar_core::instance_data::resolve_package_data_dir;
use nerevar_core::instance_setup::{instance_tes3mp_dir, read_tes3mp_server_settings};
use nerevar_core::nerevar_server::state::ServerContext;
use nerevar_core::process_manager::{launch_tes3mp_server, ProcessManager, ProcessRole};
use nerevar_core::reporter::EventSink;
use nerevar_core::supervisor::run_server_supervisor;
use nerevar_core::sync_host::{
    activate_hosting, deactivate_hosting, new_shared_hosting_manifest_cache, new_shared_sync_host,
};

use cli::Cli;
use sink::LogEventSink;

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

    // Same pattern as the GUI's `save_and_host_instance`: best-effort read,
    // empty password if the server cfg can't be parsed rather than a hard
    // failure (sync hosting can still come up; only the TES3MP launch below
    // needs the cfg to actually be there).
    let sync_password = read_tes3mp_server_settings(&instance_tes3mp_dir(&instance_root))
        .map(|settings| settings.password)
        .unwrap_or_default();

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

    activate_hosting(
        &sync_host,
        &manifest_cache,
        instance_id.clone(),
        data_dir.clone(),
        instance_root.clone(),
        sync_password,
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
        log::info!("TES3MP dedicated server launched for instance \"{instance_name}\"");
    }

    signal::wait_for_shutdown_signal().await;
    log::info!("Shutting down...");

    if !cli.sync_only {
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
    log::info!("nerevar-host exiting cleanly");
    Ok(0)
}
