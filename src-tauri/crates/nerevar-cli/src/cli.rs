//! The command line: `nerevar-cli sync …` and `nerevar-cli admin …`.
//!
//! Two surfaces in one binary because they are the same person's two jobs on a
//! headless machine — pull the host's mod list as a player, push one as a
//! co-admin — and because the player-side driver was already a rig tool that
//! wanted a home (`docs/headless-hosting.md` covers the admin side).
//!
//! The `sync` flags are a contract: a test rig drives them, so they are the
//! flags the `nerevar-core` example this graduated from accepted, unchanged.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "nerevar-cli", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Sync one *synced* instance from its host, headless, and optionally
    /// launch its TES3MP client afterwards. Prints every core event as one
    /// JSON line on stdout.
    Sync(SyncArgs),

    /// Drive a headless host's `/admin` routes as a co-admin.
    Admin(AdminCommand),
}

/// Exactly the flags the `sync_client` example took, so a rig switching to
/// this binary changes only the program name and the `sync` word.
#[derive(Args, Debug)]
pub struct SyncArgs {
    /// The `config.json` the desktop app writes (or a hand-written one).
    #[arg(long, value_name = "PATH")]
    pub config: PathBuf,

    /// Which synced instance to sync, matched by id first then name.
    #[arg(long, value_name = "ID-OR-NAME")]
    pub instance: String,

    /// Install the instance's configured TES3MP runtime into
    /// `<instance>/tes3mp/` before syncing, when that directory does not
    /// already hold a complete one.
    #[arg(long)]
    pub install_runtime: bool,

    /// Re-download every file instead of resuming from the sync state.
    #[arg(long)]
    pub force: bool,

    /// After a valid sync, launch the instance's TES3MP client and block
    /// until it exits or this process gets SIGTERM/SIGINT.
    #[arg(long)]
    pub launch: bool,

    /// First write the global OpenMW scaffold from this Morrowind `Data
    /// Files` directory, exactly like the app's onboarding.
    #[arg(long, value_name = "DATA FILES")]
    pub onboard: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct AdminCommand {
    #[command(flatten)]
    pub options: AdminOptions,

    #[command(subcommand)]
    pub action: AdminAction,
}

/// Where to send the request and who to send it as. Global, so they may be
/// written before or after the subcommand.
#[derive(Args, Debug, Default)]
pub struct AdminOptions {
    /// The host: a hostname or IP (combined with `--port`), or a full
    /// `http(s)://` URL, which then carries its own port. Resolved by the
    /// same rule the desktop app's connection form uses.
    #[arg(long, global = true, value_name = "ADDRESS")]
    pub host: Option<String>,

    /// Sync port, for a bare hostname or IP. Ignored for a URL host.
    #[arg(long, global = true, value_name = "PORT", default_value_t = 25567)]
    pub port: u16,

    /// The co-admin token. Prefer `--token-file` or `NEREVAR_ADMIN_TOKEN`:
    /// an argument is visible in the process list.
    #[arg(long, global = true, value_name = "TOKEN")]
    pub token: Option<String>,

    /// A file holding the token, as `nerevar-host admin add ada > ada.token`
    /// writes it.
    #[arg(long, global = true, value_name = "PATH")]
    pub token_file: Option<PathBuf>,

    /// Print the server's response body verbatim instead of a summary.
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum AdminAction {
    /// What the host is hosting, how old its manifest is, and what is staged.
    Status,

    /// Stage a package: upload an archive (`.zip` or `.tar.gz`), which apply
    /// then installs into `data/`.
    Upload {
        /// The archive to send. Streamed, never read into memory.
        archive: PathBuf,

        /// Package directory name on the host. Defaults to the archive's
        /// file name without its extension.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
    },

    /// Drop a staged upload, or mark a package in `data/` for deletion at
    /// apply.
    Remove {
        /// Package name, as `status` lists it.
        name: String,
    },

    /// Read or stage the load-order document.
    LoadOrder {
        #[command(subcommand)]
        action: LoadOrderAction,
    },

    /// Enable a package in the staged load order.
    Enable {
        /// Package name, entry name, or entry id.
        name: String,
    },

    /// Disable a package in the staged load order.
    Disable {
        /// Package name, entry name, or entry id.
        name: String,
    },

    /// Move the named packages to the front of the load order, in the order
    /// given. Everything else keeps its relative order behind them.
    Order {
        /// Package names, highest priority first.
        #[arg(required = true, num_args = 1..)]
        names: Vec<String>,
    },

    /// Publish the staged changes: install, remove, save the load order,
    /// rebuild the manifest. Players pick it up on their next sync.
    Apply,

    /// Throw the staged changes away.
    Discard,

    /// Restart the host's TES3MP dedicated server so an applied plugin list
    /// takes effect. Kicks everyone connected.
    Restart {
        /// Skip the confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum LoadOrderAction {
    /// Print the load-order document.
    Get {
        /// Print the staged document waiting for apply instead of the one on
        /// disk. Fails when nothing is staged.
        #[arg(long)]
        pending: bool,
    },

    /// Stage a load-order document read from a file.
    Set {
        /// A JSON file in the same shape `load-order get` prints.
        file: PathBuf,
    },
}
