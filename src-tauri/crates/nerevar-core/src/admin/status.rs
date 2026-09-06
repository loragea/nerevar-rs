//! The `GET /admin/status` response: what an admin needs to see before
//! deciding to change anything.
//!
//! Every section is optional because every source can be absent on a real
//! host: an instance that is not hosting has no data directory, a fresh
//! instance has no `load-order.json`, a `--no-manifest-rebuild` start may have
//! no `manifest.json`, and an embedder with no process manager cannot say
//! whether TES3MP is up. Absent is reported as `null`, never guessed.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::admin::staging::{data_package_names, load_pending, StagedPackage};
use crate::instance_data::{load_load_order, load_manifest};

/// Counts from `load-order.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AdminLoadOrderSummary {
    /// Packages in the load order, enabled or not.
    pub entry_count: u32,
    /// Plugins that will actually be loaded: enabled plugins inside enabled
    /// entries. A plugin left on inside a disabled package does not count.
    pub enabled_plugin_count: u32,
}

/// What the served `manifest.json` says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AdminManifestSummary {
    /// RFC 3339, as written by the manifest build.
    pub generated_at: String,
    /// Seconds since `generated_at`, or `null` when it does not parse as
    /// RFC 3339 (a hand-edited manifest) or is somehow in the future.
    pub age_seconds: Option<i64>,
}

/// The host's state as an admin sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AdminStatus {
    /// Whether an instance is activated for sync hosting right now.
    pub hosting: bool,
    pub instance_id: Option<String>,
    /// From the manifest, the only place the hosting state records a name.
    pub instance_name: Option<String>,
    pub load_order: Option<AdminLoadOrderSummary>,
    pub manifest: Option<AdminManifestSummary>,
    /// Whether the TES3MP dedicated server is running, or `null` when the
    /// embedder supervises no process (the desktop app's server, or a daemon
    /// built without a process manager wired into the HTTP context).
    pub tes3mp_server_running: Option<bool>,
    /// RFC 3339 instant the running TES3MP dedicated server was launched, or
    /// `null` when it is not running or the embedder supervises no process. A
    /// restart moves it forward, which is how an admin confirms the game
    /// server really did come back rather than never having stopped.
    pub tes3mp_server_started_at: Option<String>,
    /// Staged, not-yet-applied changes, or `null` when nothing is pending.
    /// `null` and an object with empty lists would mean the same thing, so
    /// only one of them is ever sent: nothing pending is `null`.
    pub pending_changes: Option<AdminPendingChanges>,
    /// Whether a running TES3MP dedicated server is enforcing a plugin list
    /// older than the manifest now being served. Set by `POST /admin/apply`,
    /// because apply deliberately does not restart the game server; cleared
    /// by a restart. `false` on a host that has not applied anything this
    /// run, which is also what a host with no game server reports.
    pub tes3mp_plugin_list_stale: bool,
}

/// The staging area as `GET /admin/status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AdminPendingChanges {
    /// Uploaded packages waiting for apply, by name.
    pub staged: Vec<StagedPackage>,
    /// Packages in `data/` marked for deletion at apply.
    pub removals: Vec<String>,
    /// Whether a `POST /admin/load-order` is waiting to be written. The order
    /// itself is not inlined here — it is as long as the mod list, and
    /// `GET /admin/load-order` serves it.
    pub has_load_order: bool,
}

/// Assembles the status from what is on disk under `data_dir` plus the two
/// facts only the embedder knows.
///
/// Reads `load-order.json` and `manifest.json` directly rather than through
/// the manifest cache: this is an occasional admin call, and it must report
/// what is on disk now, not what a client download is being served from.
pub fn build_admin_status(
    instance_id: Option<String>,
    data_dir: Option<&Path>,
    tes3mp_server_running: Option<bool>,
    tes3mp_server_started_at: Option<String>,
    tes3mp_plugin_list_stale: bool,
) -> AdminStatus {
    let hosting = instance_id.is_some() && data_dir.is_some();

    let load_order = data_dir
        .and_then(|dir| load_load_order(dir).ok())
        .map(|order| AdminLoadOrderSummary {
            entry_count: order.entries.len() as u32,
            enabled_plugin_count: order
                .entries
                .iter()
                .filter(|entry| entry.enabled)
                .flat_map(|entry| entry.plugins.iter())
                .filter(|plugin| plugin.enabled)
                .count() as u32,
        });

    let manifest = data_dir.and_then(|dir| load_manifest(dir).ok());
    let instance_name = manifest.as_ref().map(|m| m.instance_name.clone());
    let manifest = manifest.map(|m| AdminManifestSummary {
        age_seconds: manifest_age_seconds(&m.generated_at),
        generated_at: m.generated_at,
    });

    AdminStatus {
        hosting,
        instance_id,
        instance_name,
        load_order,
        manifest,
        tes3mp_server_running,
        tes3mp_server_started_at,
        pending_changes: data_dir.and_then(pending_changes_for),
        tes3mp_plugin_list_stale,
    }
}

/// The pending set for `data_dir`, or `None` when nothing is staged.
///
/// `replacesExisting` is recomputed against `data/` rather than read from the
/// record: a package can appear or vanish between the upload and the status
/// call, and an admin deciding whether to apply needs the answer as it stands
/// now. An unreadable staging area reports as nothing pending — status is a
/// read-only view and must not fail the whole response over it.
pub fn pending_changes_for(data_dir: &Path) -> Option<AdminPendingChanges> {
    let pending = load_pending(data_dir).ok()?;
    if pending.is_empty() {
        return None;
    }

    let existing = data_package_names(data_dir).unwrap_or_default();
    let staged = pending
        .staged
        .into_iter()
        .map(|mut package| {
            package.replaces_existing = existing
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&package.name));
            package
        })
        .collect();

    Some(AdminPendingChanges {
        staged,
        removals: pending.removals,
        has_load_order: pending.load_order.is_some(),
    })
}

fn manifest_age_seconds(generated_at: &str) -> Option<i64> {
    let generated = DateTime::parse_from_rfc3339(generated_at)
        .ok()?
        .with_timezone(&Utc);
    let age = Utc::now().signed_duration_since(generated).num_seconds();
    (age >= 0).then_some(age)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_with_nothing_on_disk_reports_nulls_rather_than_zeroes() {
        let status = build_admin_status(None, None, None, None, false);
        assert!(!status.hosting);
        assert!(status.instance_id.is_none());
        assert!(status.instance_name.is_none());
        assert!(status.load_order.is_none());
        assert!(status.manifest.is_none());
        assert!(status.tes3mp_server_running.is_none());
        assert!(status.tes3mp_server_started_at.is_none());
        assert!(status.pending_changes.is_none());
        assert!(!status.tes3mp_plugin_list_stale);
    }

    #[test]
    fn nothing_pending_serializes_as_a_present_null() {
        let value =
            serde_json::to_value(build_admin_status(None, None, Some(false), None, true)).unwrap();
        assert_eq!(value["pendingChanges"], serde_json::Value::Null);
        assert!(
            value.as_object().unwrap().contains_key("pendingChanges"),
            "the field must be present, not omitted"
        );
        assert_eq!(value["tes3mpServerRunning"], serde_json::json!(false));
        assert_eq!(value["tes3mpPluginListStale"], serde_json::json!(true));
        assert_eq!(value["tes3mpServerStartedAt"], serde_json::Value::Null);
    }

    #[test]
    fn a_staged_package_shows_up_in_the_pending_set() {
        use crate::admin::staging::{now_rfc3339, save_pending, PendingChanges, StagedPackage};
        use crate::instance_data::PackageKind;

        let data_dir = std::env::temp_dir().join(format!(
            "nerevar-admin-status-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(data_dir.join("Better Bodies")).unwrap();

        let mut pending = PendingChanges::default();
        pending.upsert_staged(StagedPackage {
            name: "Better Bodies".to_string(),
            kind: PackageKind::Mod,
            plugins: vec!["bb.esp".to_string()],
            // Stale on the record; status recomputes it against `data/`.
            replaces_existing: false,
            archive_bytes: 10,
            extracted_bytes: 5,
            staged_at: now_rfc3339(),
            staged_by: "ada".to_string(),
        });
        pending.mark_removal("Old Mod");
        save_pending(&data_dir, &pending).unwrap();

        let status = build_admin_status(
            Some("inst".to_string()),
            Some(&data_dir),
            Some(true),
            Some("2026-09-06T10:00:00+00:00".to_string()),
            false,
        );
        let changes = status.pending_changes.expect("a pending set");
        assert_eq!(changes.staged.len(), 1);
        assert!(
            changes.staged[0].replaces_existing,
            "data/Better Bodies exists, so this upload replaces it"
        );
        assert_eq!(changes.removals, vec!["Old Mod".to_string()]);
        assert!(!changes.has_load_order);

        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[test]
    fn a_fresh_timestamp_ages_to_roughly_zero_and_a_bad_one_to_null() {
        let now = Utc::now().to_rfc3339();
        assert!(manifest_age_seconds(&now).is_some_and(|age| (0..5).contains(&age)));
        assert!(manifest_age_seconds("last tuesday").is_none());
        // A clock-skewed future timestamp is reported as unknown, not negative.
        let future = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        assert!(manifest_age_seconds(&future).is_none());
    }
}
