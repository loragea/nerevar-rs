use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::runtime::RuntimeSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SyncPhase {
    CheckingUpdates,
    VerifyingExisting,
    FetchingManifest,
    Downloading,
    ApplyingLoadOrder,
    Validating,
    WritingLaunchCfg,
    Complete,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SyncProgressEvent {
    pub instance_id: String,
    pub phase: SyncPhase,
    pub message: String,
    /// Byte counters, whose meaning follows the phase:
    ///
    /// - `VerifyingExisting`, `Downloading`, `ApplyingLoadOrder`, `Cancelled`
    ///   and the resuming `CheckingUpdates` event count every manifest byte
    ///   confirmed on disk (bytes adopted from files that were already correct
    ///   included) against the manifest total. That is what drives the download
    ///   bar and the resume percentage.
    /// - `Complete` counts what *this* sync transferred against what it needed
    ///   to transfer, so a sync that found everything already in place reports
    ///   `0`/`0` rather than the size of the modlist.
    /// - The remaining events carry no byte progress and report `0`/`1`.
    pub bytes_done: u64,
    pub bytes_total: u64,
    #[serde(default)]
    pub files_done: u64,
    #[serde(default)]
    pub files_total: u64,
    #[serde(default)]
    pub overall_percent: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InstanceSyncStatus {
    pub has_manifest: bool,
    pub can_resume: bool,
    pub is_complete: bool,
    pub bytes_verified: u64,
    pub bytes_total: u64,
    pub files_verified: u32,
    pub files_total: u32,
    #[serde(default)]
    pub percent_complete: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RemoteManifestSummary {
    pub instance_id: String,
    pub instance_name: String,
    pub total_download_bytes: u64,
    pub package_count: u32,
    pub tes3mp_server_port: u16,
    pub password_required: bool,
    pub packages: Vec<RemotePackageSummary>,
    /// The TES3MP runtime the host suggests to players (always a
    /// `githubRelease`, see `InstanceConfig::runtime_hint`). `None` when the
    /// host advertises nothing — which is also what a host older than the
    /// field sends, since it simply omits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_hint: Option<RuntimeSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RemotePackageSummary {
    pub id: String,
    pub name: String,
    pub relative_dir: String,
    pub total_size_bytes: u64,
    pub file_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ProcessStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProcessOutputEvent {
    pub instance_id: String,
    pub role: String,
    pub stream: ProcessStream,
    pub line: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProcessStatusEvent {
    pub instance_id: String,
    pub role: String,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMMARY_WITHOUT_HINT: &str = r#"{
        "instanceId": "host-1",
        "instanceName": "Host",
        "totalDownloadBytes": 1024,
        "packageCount": 1,
        "tes3mpServerPort": 25565,
        "passwordRequired": false,
        "packages": []
    }"#;

    /// A host older than the field simply omits it, and its clients must
    /// still parse the summary — reading the omission as "no suggestion".
    #[test]
    fn a_summary_without_a_hint_still_parses() {
        let summary: RemoteManifestSummary =
            serde_json::from_str(SUMMARY_WITHOUT_HINT).expect("deserialize");
        assert!(summary.runtime_hint.is_none());
    }

    /// The other direction: a client older than the field ignores it rather
    /// than failing. `RemoteManifestSummary` carries no
    /// `deny_unknown_fields`, so an unknown key is skipped — pinned here so
    /// nobody adds one and breaks every older client at once.
    #[test]
    fn an_unknown_field_in_a_summary_is_ignored() {
        let summary: RemoteManifestSummary = serde_json::from_str(
            r#"{
                "instanceId": "host-1",
                "instanceName": "Host",
                "totalDownloadBytes": 1024,
                "packageCount": 1,
                "tes3mpServerPort": 25565,
                "passwordRequired": false,
                "packages": [],
                "somethingAHostOfTheFutureSends": {"deep": [1, 2, 3]}
            }"#,
        )
        .expect("an unknown field must not fail the parse");
        assert_eq!(summary.instance_id, "host-1");
        assert!(summary.runtime_hint.is_none());
    }

    #[test]
    fn a_hint_round_trips_through_the_summary_json() {
        let mut summary: RemoteManifestSummary =
            serde_json::from_str(SUMMARY_WITHOUT_HINT).expect("deserialize");
        summary.runtime_hint = Some(RuntimeSource::GithubRelease {
            repo: "owner/name".to_string(),
            release_id: "999".to_string(),
            tag: "v1.2.3".to_string(),
            asset_name: String::new(),
        });

        let json = serde_json::to_value(&summary).expect("serialize");
        assert_eq!(json["runtimeHint"]["kind"], "githubRelease");
        assert_eq!(json["runtimeHint"]["repo"], "owner/name");
        assert_eq!(json["runtimeHint"]["tag"], "v1.2.3");

        let back: RemoteManifestSummary = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.runtime_hint, summary.runtime_hint);
    }

    /// A summary with no suggestion carries no key, so a host that advertises
    /// nothing sends byte-for-byte what it sent before the field existed.
    #[test]
    fn no_hint_means_no_key_on_the_wire() {
        let summary: RemoteManifestSummary =
            serde_json::from_str(SUMMARY_WITHOUT_HINT).expect("deserialize");
        let json = serde_json::to_string(&summary).expect("serialize");
        assert!(!json.contains("runtimeHint"), "unexpected key in {json}");
    }
}
