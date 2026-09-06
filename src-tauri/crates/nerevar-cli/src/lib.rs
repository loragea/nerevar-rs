//! `nerevar-cli` — Nerevar without the desktop app.
//!
//! Two surfaces, one binary:
//!
//! - [`sync`] is the player side: pull a synced instance from its host and
//!   optionally launch the TES3MP client, printing every core event as a JSON
//!   line. It is what a test rig drives instead of the GUI, and a fallback for
//!   a Linux user with no desktop.
//! - [`admin`] is the co-admin side: drive a headless host's `/admin` routes —
//!   upload a package, edit the load order, apply, restart — with a named
//!   bearer token. `docs/headless-hosting.md` is the operator guide.
//!
//! Both are clients of `nerevar-core`; no rule about what a sync or an apply
//! *means* lives here. The crate is a library with a thin `main.rs` on top so
//! its integration test can drive the same functions the binary does.

pub mod admin;
pub mod cli;
pub mod sync;

use cli::{Cli, Command};

/// Runs one invocation, returning the process exit code.
///
/// `sync` keeps the exit codes its rig depends on (0 ok, 1 the manifest did
/// not validate, 2 error); `admin` is 0 or 1, because a failed admin request
/// has only the one meaning.
pub async fn run(cli: Cli) -> i32 {
    match cli.command {
        Command::Sync(args) => sync::run_reporting_errors(args).await,
        Command::Admin(command) => {
            let result = run_admin(&command).await;
            match result {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("nerevar-cli admin: {error}");
                    1
                }
            }
        }
    }
}

async fn run_admin(command: &cli::AdminCommand) -> Result<(), String> {
    let client = admin::connect(&command.options)?;
    let mut out = std::io::stdout().lock();
    let result = admin::run(&client, &command.options, &command.action, &mut out).await;
    // Explicit because `main` leaves through `std::process::exit`, which runs
    // no destructors.
    use std::io::Write;
    out.flush()
        .map_err(|error| format!("Failed to write output: {error}"))?;
    result
}

/// Minimal stderr logger so core's `log::` diagnostics (what it launched, what
/// it restored) are visible; level from `RUST_LOG` (error|warn|info|debug|
/// trace), default info. No `env_logger` here — it is not a core dependency
/// and this needs nothing it adds.
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

/// Installs [`StderrLogger`] as the process logger. Idempotent and harmless
/// to call from a test binary that has already installed one.
pub fn install_logger() {
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|v| v.parse::<log::LevelFilter>().ok())
        .unwrap_or(log::LevelFilter::Info);
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level);
}
