use std::path::PathBuf;

use clap::Parser;

/// Headless Nerevar host: activates sync hosting for one owned instance and
/// (unless `--sync-only`) launches its TES3MP dedicated server. Foreground
/// process — run it under systemd for service-ification. See
/// notes/nerevar-host-design.md (PM root) for the full design.
#[derive(Parser, Debug)]
#[command(name = "nerevar-host", version, about, long_about = None)]
pub struct Cli {
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
