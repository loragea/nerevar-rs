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
    /// Staged, not-yet-applied changes. There is no staging area yet, so this
    /// is always `null`; `PUT /admin/packages/*` gives it a shape.
    #[ts(type = "null")]
    pub pending_changes: Option<serde_json::Value>,
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
        pending_changes: None,
    }
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
        let status = build_admin_status(None, None, None);
        assert!(!status.hosting);
        assert!(status.instance_id.is_none());
        assert!(status.instance_name.is_none());
        assert!(status.load_order.is_none());
        assert!(status.manifest.is_none());
        assert!(status.tes3mp_server_running.is_none());
        assert!(status.pending_changes.is_none());
    }

    #[test]
    fn pending_changes_serializes_as_a_present_null() {
        let value = serde_json::to_value(build_admin_status(None, None, Some(false))).unwrap();
        assert_eq!(value["pendingChanges"], serde_json::Value::Null);
        assert!(
            value.as_object().unwrap().contains_key("pendingChanges"),
            "the placeholder must be present, not omitted"
        );
        assert_eq!(value["tes3mpServerRunning"], serde_json::json!(false));
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
