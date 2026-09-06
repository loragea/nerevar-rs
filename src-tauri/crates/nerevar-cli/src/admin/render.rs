//! Turning `/admin` response bodies into something readable in a terminal.
//!
//! Every command also has a `--json` mode that prints the host's body
//! verbatim, so nothing here has to be parseable — these are the lines a
//! person reads, and the raw body is the one a script reads.

use std::io::Write;

use nerevar_core::admin::{AdminPendingChanges, AdminStatus, StagedPackage};

type Out<'a> = &'a mut dyn Write;

fn write_error(error: std::io::Error) -> String {
    format!("Failed to write output: {error}")
}

macro_rules! say {
    ($out:expr, $($arg:tt)*) => {
        writeln!($out, $($arg)*).map_err(write_error)?
    };
}

/// Bytes as a person reads them. Decimal units, because that is what mod
/// hosting sites and the desktop app both quote.
fn bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{count} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A manifest age in the units a person cares about at that scale.
fn age(seconds: i64) -> String {
    match seconds {
        s if s < 90 => format!("{s} s ago"),
        s if s < 5400 => format!("{} min ago", s / 60),
        s if s < 172_800 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

fn kind_of(package: &StagedPackage) -> &'static str {
    match package.kind {
        nerevar_core::instance_data::PackageKind::Mod => "mod",
        nerevar_core::instance_data::PackageKind::Replacer => "replacer",
    }
}

/// `GET /admin/status`, as a summary.
pub fn status(status: &AdminStatus, base_url: &str, out: Out) -> Result<(), String> {
    say!(out, "Host        {base_url}");

    match (&status.instance_name, &status.instance_id) {
        _ if !status.hosting => say!(
            out,
            "Hosting     no — the host has no instance activated for sync"
        ),
        (Some(name), Some(id)) => say!(out, "Hosting     yes — {name} ({id})"),
        (None, Some(id)) => say!(out, "Hosting     yes — {id}"),
        _ => say!(out, "Hosting     yes"),
    }

    match &status.load_order {
        Some(summary) => say!(
            out,
            "Load order  {}, {} enabled",
            plural(summary.entry_count as usize, "package", "packages"),
            plural(summary.enabled_plugin_count as usize, "plugin", "plugins")
        ),
        None => say!(out, "Load order  none on disk yet"),
    }

    match &status.manifest {
        Some(manifest) => {
            let when = match manifest.age_seconds {
                Some(seconds) => format!("{} ({})", manifest.generated_at, age(seconds)),
                None => manifest.generated_at.clone(),
            };
            say!(out, "Manifest    generated {when}");
        }
        None => say!(out, "Manifest    none built yet"),
    }

    match (
        status.tes3mp_server_running,
        &status.tes3mp_server_started_at,
    ) {
        (Some(true), Some(since)) => say!(out, "TES3MP      running since {since}"),
        (Some(true), None) => say!(out, "TES3MP      running"),
        (Some(false), _) => say!(out, "TES3MP      not running"),
        (None, _) => say!(
            out,
            "TES3MP      unknown — this host supervises no game server"
        ),
    }

    if status.tes3mp_plugin_list_stale {
        say!(
            out,
            "            STALE: the running server still enforces the plugin list from before \
             the last apply."
        );
        say!(
            out,
            "            `nerevar-cli admin restart` publishes it — and kicks everyone \
             connected."
        );
    }

    pending(status.pending_changes.as_ref(), out)
}

/// The staged set, under a `Pending` heading. `None` is "nothing staged",
/// which is what the host sends rather than an empty object.
pub fn pending(changes: Option<&AdminPendingChanges>, out: Out) -> Result<(), String> {
    let Some(changes) = changes else {
        say!(out, "Pending     nothing staged");
        return Ok(());
    };

    let mut parts = Vec::new();
    if !changes.staged.is_empty() {
        parts.push(plural(changes.staged.len(), "upload", "uploads"));
    }
    if !changes.removals.is_empty() {
        parts.push(plural(changes.removals.len(), "removal", "removals"));
    }
    if changes.has_load_order {
        parts.push("a load order".to_string());
    }
    say!(out, "Pending     {}", parts.join(", "));

    for package in &changes.staged {
        let replaces = if package.replaces_existing {
            ", replaces the one in data/"
        } else {
            ""
        };
        say!(
            out,
            "            + {} ({}, {}, {} archive{replaces}) staged by {}",
            package.name,
            kind_of(package),
            plural(package.plugins.len(), "plugin", "plugins"),
            bytes(package.archive_bytes),
            package.staged_by
        );
    }
    for name in &changes.removals {
        say!(out, "            - {name}");
    }
    if changes.has_load_order {
        say!(
            out,
            "            ~ a load order is waiting (`load-order get --pending` shows it)"
        );
    }
    Ok(())
}

/// What `PUT /admin/packages/{name}` found in the archive it just took.
pub fn uploaded(package: &StagedPackage, out: Out) -> Result<(), String> {
    say!(
        out,
        "Staged \"{}\" — {}, {} archive, {} extracted.",
        package.name,
        kind_of(package),
        bytes(package.archive_bytes),
        bytes(package.extracted_bytes)
    );
    if package.plugins.is_empty() {
        say!(out, "  No plugins in it (assets only).");
    } else {
        say!(
            out,
            "  {}: {}",
            plural(package.plugins.len(), "plugin", "plugins"),
            package.plugins.join(", ")
        );
    }
    if package.replaces_existing {
        say!(
            out,
            "  A package of this name is already in data/; apply will replace it."
        );
    }
    say!(out, "Nothing is live until `nerevar-cli admin apply`.");
    Ok(())
}

/// The manifest summary `POST /admin/apply` answers with. Parsed loosely
/// because the summary type is private to the server's route module.
pub fn applied(body: &serde_json::Value, out: Out) -> Result<(), String> {
    let name = body["instanceName"].as_str().unwrap_or("the instance");
    let packages = body["packageCount"].as_u64().unwrap_or(0);
    let download = body["totalDownloadBytes"].as_u64().unwrap_or(0);
    say!(
        out,
        "Applied. \"{name}\" now serves {} totalling {}.",
        plural(packages as usize, "package", "packages"),
        bytes(download)
    );
    say!(out, "Players pick it up on their next sync.");
    say!(
        out,
        "A running TES3MP server keeps the old plugin list until `nerevar-cli admin restart`."
    );
    Ok(())
}

/// The outcome `POST /admin/restart` answers with.
pub fn restarted(body: &serde_json::Value, out: Out) -> Result<(), String> {
    let pid = match body["pid"].as_u64() {
        Some(pid) => format!(" (pid {pid})"),
        None => String::new(),
    };
    if body["wasRunning"].as_bool() == Some(false) {
        say!(out, "The TES3MP server was not running; started it{pid}.");
    } else {
        say!(out, "TES3MP restarted{pid}.");
    }
    if let Some(started) = body["startedAt"].as_str() {
        say!(out, "Up since {started}.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_reads_as_a_person_would_say_it() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1_000), "1.0 kB");
        assert_eq!(bytes(12_300_000), "12.3 MB");
        assert_eq!(bytes(4_000_000_000), "4.0 GB");
    }

    #[test]
    fn age_switches_units_rather_than_printing_a_million_seconds() {
        assert_eq!(age(5), "5 s ago");
        assert_eq!(age(600), "10 min ago");
        assert_eq!(age(7_200), "2 h ago");
        assert_eq!(age(864_000), "10 days ago");
    }

    #[test]
    fn a_host_that_is_not_hosting_says_so_rather_than_printing_blanks() {
        let mut out = Vec::new();
        let empty = AdminStatus {
            hosting: false,
            instance_id: None,
            instance_name: None,
            load_order: None,
            manifest: None,
            tes3mp_server_running: None,
            tes3mp_server_started_at: None,
            pending_changes: None,
            tes3mp_plugin_list_stale: false,
        };
        status(&empty, "http://myhost:25567", &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("http://myhost:25567"), "{text}");
        assert!(text.contains("Hosting     no"), "{text}");
        assert!(text.contains("none built yet"), "{text}");
        assert!(text.contains("nothing staged"), "{text}");
    }
}
