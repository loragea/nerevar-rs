//! Embedded sync-server lifecycle: startup port-conflict probing and the
//! supervisor loop that (re)starts the server as its port, retry, and
//! enabled signals change.
//!
//! Top-layer split (step 7, see notes/core-split-plan.md): moved into
//! nerevar-core wholesale — already Tauri-free, since `run_server_supervisor`
//! spawns its worker tasks with `tokio::spawn` rather than
//! `tauri::async_runtime::spawn`, as it always runs inside a Tokio runtime
//! regardless of which binary (Tauri app or headless daemon) drives it. The
//! app crate's top-level setup spawns still use `tauri::async_runtime::spawn`
//! to enter this module's async fns (by design until the host exists).

use std::sync::Arc;

use tokio::sync::watch;

use crate::data::NerevarConfig;
use crate::nerevar_server;
use crate::port_conflict::{self, PortConflict};
use crate::reporter::EventSink;

/// Checks the ports `config` would need for conflicts and emits any that
/// are found. Callers gate this on `config.onboarding_complete` — matches
/// the prior lib.rs setup behavior where the startup probe only ran once
/// onboarding was complete.
pub async fn probe_startup_port_conflicts(config: NerevarConfig, sink: Arc<dyn EventSink>) {
    if let Ok(conflicts) = port_conflict::check_startup_conflicts(&config) {
        port_conflict::emit_port_conflicts(&*sink, conflicts);
    }
}

/// Owns the embedded sync server's lifecycle: starts it on `port_rx`'s
/// current value once `enabled_rx` is true, and restarts it whenever
/// `port_rx`, `retry_rx`, or `enabled_rx` change. Runs until any of the
/// three watch channels closes.
pub async fn run_server_supervisor(
    mut port_rx: watch::Receiver<i32>,
    mut retry_rx: watch::Receiver<u64>,
    mut enabled_rx: watch::Receiver<bool>,
    ctx: Arc<nerevar_server::state::ServerContext>,
    sink: Arc<dyn EventSink>,
) {
    let mut current_task: Option<tokio::task::JoinHandle<()>>;

    let start = |port: i32,
                 ctx: Arc<nerevar_server::state::ServerContext>,
                 sink: Arc<dyn EventSink>| {
        tokio::spawn(async move {
            match nerevar_server::try_bind(port).await {
                Ok(listener) => {
                    if let Err(err) = nerevar_server::serve(listener, ctx).await {
                        log::error!("NEREVAR SERVER: stopped on port {port}: {err}");
                    }
                }
                Err(err) => {
                    log::error!("NEREVAR SERVER: failed to bind on port {port}: {err}");
                    if port_conflict::is_addr_in_use_error(&err) {
                        match port_conflict::conflict_for_port(
                            port as u16,
                            port_conflict::PortRole::NerevarSync,
                            None,
                            None,
                        ) {
                            Ok(Some(conflict)) => {
                                port_conflict::emit_port_conflicts(&*sink, vec![conflict]);
                            }
                            Ok(None) => {
                                port_conflict::emit_port_conflicts(
                                    &*sink,
                                    vec![PortConflict {
                                        port: port as u16,
                                        role: port_conflict::PortRole::NerevarSync,
                                        pid: 0,
                                        process_name: "Unknown process".to_string(),
                                        executable_path: None,
                                        instance_id: None,
                                        instance_name: None,
                                    }],
                                );
                            }
                            Err(parse_err) => {
                                log::error!("Failed to inspect port {port}: {parse_err}");
                            }
                        }
                    }
                }
            }
        })
    };

    let mut port = *port_rx.borrow();

    if *enabled_rx.borrow() {
        log::info!("NEREVAR SERVER: starting on port {port}");
        current_task = Some(start(port, ctx.clone(), sink.clone()));
    } else {
        log::info!("NEREVAR SERVER: waiting for onboarding to complete");
        current_task = None;
    }

    loop {
        tokio::select! {
            changed = enabled_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                if !*enabled_rx.borrow() {
                    if let Some(task) = current_task.take() {
                        task.abort();
                    }
                    current_task = None;
                    continue;
                }

                port = *port_rx.borrow();
                log::info!(
                    "NEREVAR SERVER: onboarding complete — starting on port {port}"
                );
                if let Some(task) = current_task.take() {
                    task.abort();
                }
                current_task =
                    Some(start(port, ctx.clone(), sink.clone()));
            }
            changed = port_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                if !*enabled_rx.borrow() {
                    continue;
                }
                port = *port_rx.borrow();
                log::info!(
                    "NEREVAR SERVER: restarting on port {port}"
                );
                if let Some(task) = current_task.take() {
                    task.abort();
                }
                current_task =
                    Some(start(port, ctx.clone(), sink.clone()));
            }
            changed = retry_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                if !*enabled_rx.borrow() {
                    continue;
                }
                log::info!(
                    "NEREVAR SERVER: retrying bind on port {port}"
                );
                if let Some(task) = current_task.take() {
                    task.abort();
                }
                current_task =
                    Some(start(port, ctx.clone(), sink.clone()));
            }
        }
    }
}
