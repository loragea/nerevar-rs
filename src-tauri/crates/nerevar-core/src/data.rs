use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::runtime::RuntimeSource;

#[derive(TS, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InstanceConfig {
    pub id: String,
    pub name: String,
    pub description: String,
    pub path: String,
    pub data_dir: String,
    /// Legacy: the `tes3mp/tes3mp` release id an instance was installed
    /// from, written by builds that predate `runtime`. Still written
    /// alongside `runtime` for `githubRelease` sources so a config this
    /// build writes stays readable by those builds; `runtime` is what is
    /// read. `load_nerevar_config_at` fills `runtime` from this field when
    /// it is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_id: Option<String>,
    /// Where this instance's TES3MP runtime came from. Absent only in
    /// configs written before the field existed, which the loader migrates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeSource>,
    /// A runtime this instance's *host operator* advertises to players
    /// connecting to it: the client's runtime picker preselects it and says
    /// where it came from, and the player is free to change it. Only
    /// `RuntimeSource::GithubRelease` is meaningful (see
    /// `runtime::normalize_runtime_hint`) — a path on the host's disk means
    /// nothing on a player's machine. Absent means "no suggestion", which is
    /// what every config written before this field says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_hint: Option<RuntimeSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_sync_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tes3mp_server_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_password: Option<String>,
}

#[derive(TS, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InstanceEditPayload {
    pub id: String,
    pub name: String,
    pub description: String,
    pub host: String,
    pub port: u16,
    pub password: String,
}

#[derive(TS, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InstanceConnectionSettings {
    pub name: String,
    pub description: String,
    pub host: String,
    pub port: u16,
    pub password: String,
    pub is_synced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_port: Option<u16>,
}

#[derive(TS, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NewConnectionConfig {
    pub runtime: RuntimeSource,
    pub connection_name: String,
    pub connection_description: String,
    /// The Nerevar data directory the instance is created under. The
    /// instance's own directories are derived from this and the connection
    /// name by `instance_setup::instance_paths`, on the backend, because
    /// only it knows the platform's path separator.
    pub root_path: String,
    pub remote_host: String,
    pub remote_sync_port: u16,
    pub sync_password: String,
}

#[derive(Serialize, Deserialize, Clone, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NewInstanceConfig {
    pub runtime: RuntimeSource,
    pub instance_name: String,
    pub instance_description: String,
    pub instance_root_path: String,
    pub instance_data_dir: String,
    pub server_host_name: String,
    pub max_players: u32,
    pub server_port: u16,
    pub password: String,
    pub master_server_enabled: bool,
}

#[derive(TS, Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NerevarConfig {
    pub onboarding_complete: bool,
    pub owned_instances: Option<Vec<InstanceConfig>>,
    pub synced_instances: Option<Vec<InstanceConfig>>,
    pub root_path: Option<String>,
    pub sync_port: i32,
    /// The Morrowind `Data Files` directory onboarding was pointed at.
    ///
    /// Written by `config::nerevar_config::set_morrowind_data_files` when the
    /// OpenMW scaffold is generated from it, so the path can be re-checked or
    /// repaired later instead of surviving only as a `data=` line inside
    /// `openmw.nerevar.cfg`. Absent in every config written before the field
    /// existed, and omitted again when unset, so an older build reads a config
    /// this one wrote unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub morrowind_data_files: Option<String>,
}

// impl Default for NerevarConfig {
//     fn default() -> Self {
//         Self {
//             onboarding_complete: false,
//             instances: None,
//             root_instance_path: None,
//             sync_port: 25567,
//         }
//     }
// }

#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub struct GithubAssetResponse {
    pub url: String,
    pub id: u64,
    pub node_id: String,
    pub name: String,
    pub label: Option<String>,
    pub content_type: String,
    pub state: String,
    pub size: u64,
    pub download_count: u64,
    pub created_at: String,
    pub updated_at: String,
    pub browser_download_url: String,
}

#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub struct GithubReleaseResponse {
    pub url: String,
    pub assets_url: String,
    pub upload_url: String,
    pub html_url: String,
    pub id: u64,
    pub node_id: String,
    pub tag_name: String,
    pub target_commitish: String,
    pub name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub created_at: String,
    pub published_at: String,
    pub assets: Vec<GithubAssetResponse>,
    pub tarball_url: String,
    pub zipball_url: String,
    pub body: String,
}

// `ResolvedOpenMwConfig` and `RequiredDataFileEntry` originally lived in the
// app crate's `instance_data::types`. `instance_setup::required_data_files`
// (moved here in the same step) takes/returns them directly, and
// `instance_data` doesn't move until a later step, so they relocate here
// ahead of the rest of `instance_data` to avoid a core -> app dependency.
// `instance_data::types` re-exports both so its own consumers (including
// `NerevarManifest`, which embeds them) are unaffected.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ResolvedOpenMwConfig {
    pub encoding: String,
    pub data_paths: Vec<String>,
    pub content: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RequiredDataFileEntry {
    pub file: String,
    pub checksums: Vec<String>,
}
