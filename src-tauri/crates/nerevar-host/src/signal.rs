/// Waits for a shutdown request: SIGTERM or SIGINT on Unix (what systemd and
/// an interactive Ctrl-C both send), or `ctrl_c` elsewhere so the crate still
/// builds and runs sensibly on Windows even though the daemon is Linux-first.
#[cfg(unix)]
pub async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm =
        signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => log::info!("Received SIGTERM"),
        _ = sigint.recv() => log::info!("Received SIGINT"),
    }
}

#[cfg(not(unix))]
pub async fn wait_for_shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    log::info!("Received Ctrl-C");
}
