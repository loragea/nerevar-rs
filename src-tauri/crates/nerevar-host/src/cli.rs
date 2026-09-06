use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Headless Nerevar host: activates sync hosting for one owned instance and
/// (unless `--sync-only`) launches its TES3MP dedicated server. Foreground
/// process — run it under systemd for service-ification. See
/// `docs/headless-hosting.md` for the full operator guide.
#[derive(Parser, Debug)]
#[command(name = "nerevar-host", version, about, long_about = None)]
pub struct Cli {
    /// A management subcommand instead of a daemon run. Omit it and the
    /// process hosts the instance, exactly as it always has.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Config file path. Default resolution: the per-user GUI config path
    /// if it exists, else /etc/nerevar/config.json. Never auto-created.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Owned instance to host, matched by id first then name. Optional
    /// when the config has exactly one owned instance.
    #[arg(long, value_name = "ID-OR-NAME")]
    pub instance: Option<String>,

    /// Sync-port override for this run only; does not write the config file.
    #[arg(long, value_name = "PORT")]
    pub port: Option<i32>,

    /// Skip launching the TES3MP dedicated server; host sync only.
    #[arg(long)]
    pub sync_only: bool,

    /// Rescan the instance's data directory and merge the result into
    /// load-order.json before hosting: new package folders are added
    /// (enabled), vanished ones dropped. Run this after dropping mods onto
    /// the host. Combines with --check to preview the result without
    /// starting anything.
    #[arg(long)]
    pub scan: bool,

    /// Host the manifest.json already on disk instead of rebuilding it from
    /// load-order.json at startup. Faster restarts, but the manifest — and
    /// TES3MP's required-plugin list — can then disagree with what is on
    /// disk.
    #[arg(long)]
    pub no_manifest_rebuild: bool,

    /// Resolve config + instance, validate paths, print a summary, and
    /// exit 0/1 without starting any servers.
    #[arg(long)]
    pub check: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage the instance's co-admins: named bearer tokens for the daemon's
    /// `/admin` HTTP routes. Works against a running daemon — the store is
    /// re-read on every admin request.
    Admin {
        #[command(subcommand)]
        action: AdminAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum AdminAction {
    /// Create a co-admin and print its token to stdout, once. The token is
    /// not recoverable afterwards; only its SHA-256 is stored.
    Add {
        /// Unique name for this co-admin; shown in `admin list` and in the
        /// daemon's audit lines.
        name: String,

        /// Role granting the capability set this co-admin gets.
        #[arg(long, default_value = "admin")]
        role: String,
    },

    /// List co-admins: name, role, created-at. Never token hashes.
    List,

    /// Remove a co-admin, revoking its token immediately.
    Revoke {
        /// The name given to `admin add`.
        name: String,
    },
}
