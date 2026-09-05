//! Where an instance's TES3MP runtime comes from, and which platform build
//! of it we want.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The upstream TES3MP repository. Every runtime Nerevar has ever installed
/// came from here, and it stays the default: a fork that ships its own
/// builds points a `RuntimeSource` at its own repo instead of Nerevar
/// carrying a fork-specific default.
pub const DEFAULT_TES3MP_REPO: &str = "tes3mp/tes3mp";

/// Where the TES3MP runtime for an instance is fetched from.
///
/// Serialized as an internally tagged enum (`{"kind": "githubRelease", ...}`)
/// so a new variant never disturbs config files already on disk: a file
/// written before `localDirectory`/`archive` existed only ever holds
/// `githubRelease`, and a reader that predates a variant fails on that one
/// instance rather than on the whole config.
///
/// Every variant installs *into* the instance's `tes3mp/` directory. Nothing
/// is ever run in place: Nerevar patches the cfgs and `requiredDataFiles.json`
/// inside the install and deletes it with the instance, so a local directory
/// or archive is a template to copy from, not a runtime to borrow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum RuntimeSource {
    /// A release asset published on a GitHub repository.
    #[serde(rename_all = "camelCase")]
    GithubRelease {
        /// `owner/name`, e.g. `tes3mp/tes3mp`.
        repo: String,
        /// The release's numeric GitHub id, as a string (that is how the
        /// legacy `InstanceConfig::release_id` stored it).
        release_id: String,
        /// The release's tag (`tes3mp-0.8.1`). Empty when unknown — it is
        /// descriptive only; `release_id` is what resolves the release.
        #[serde(default)]
        tag: String,
        /// The exact asset file name to download. Empty means "let the
        /// platform rules pick", which is what the app sends: the selection
        /// rules live in core, not in the picker.
        #[serde(default)]
        asset_name: String,
    },
    /// A TES3MP install the user already has unpacked somewhere — an
    /// extracted official release, or a fork build such as MundusPatensMP.
    /// Its contents are copied into the instance's `tes3mp/`.
    #[serde(rename_all = "camelCase")]
    LocalDirectory {
        /// Absolute path to the runtime root (the directory holding the
        /// `tes3mp` wrapper, or one holding the release's own top-level
        /// directory — inspection recurses either way).
        path: String,
    },
    /// A release archive sitting on disk: `.zip`, `.tar.gz` or `.tgz`,
    /// extracted into the instance's `tes3mp/`.
    #[serde(rename_all = "camelCase")]
    Archive {
        /// Absolute path to the archive file.
        path: String,
    },
}

impl RuntimeSource {
    /// The runtime source implied by a pre-`runtime` config entry: every
    /// such instance was installed from a `tes3mp/tes3mp` release id, with
    /// no record of which tag or asset that was.
    pub fn from_legacy_release_id(release_id: &str) -> Self {
        RuntimeSource::GithubRelease {
            repo: DEFAULT_TES3MP_REPO.to_string(),
            release_id: release_id.to_string(),
            tag: String::new(),
            asset_name: String::new(),
        }
    }

    /// The legacy `release_id` this source corresponds to, if any — kept in
    /// `InstanceConfig::release_id` alongside `runtime` so a config written
    /// by this build still loads in an older one.
    pub fn legacy_release_id(&self) -> Option<String> {
        match self {
            RuntimeSource::GithubRelease { release_id, .. } => Some(release_id.clone()),
            // A local source has no release id to record; an older build
            // reading such a config sees no `releaseId` and treats the
            // instance as one it cannot reinstall, which is the truth.
            RuntimeSource::LocalDirectory { .. } | RuntimeSource::Archive { .. } => None,
        }
    }
}

/// The platform a runtime is being installed *for*. Explicit rather than
/// implied by `#[cfg]`, so the asset-selection rules for all platforms are
/// testable from any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetPlatform {
    Windows,
    Linux,
    MacOs,
}

impl TargetPlatform {
    /// The platform this build runs on. Anything that is neither Windows nor
    /// macOS is treated as Linux — the same assumption the `#[cfg]`-gated
    /// selection it replaces made.
    pub fn current() -> Self {
        #[cfg(windows)]
        {
            TargetPlatform::Windows
        }
        #[cfg(target_os = "macos")]
        {
            TargetPlatform::MacOs
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            TargetPlatform::Linux
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_release_round_trips_through_json() {
        let source = RuntimeSource::GithubRelease {
            repo: DEFAULT_TES3MP_REPO.to_string(),
            release_id: "65767406".to_string(),
            tag: "tes3mp-0.8.1".to_string(),
            asset_name: "tes3mp.Win64.release.0.8.1.zip".to_string(),
        };

        let json = serde_json::to_value(&source).expect("serialize");
        assert_eq!(json["kind"], "githubRelease");
        assert_eq!(json["repo"], DEFAULT_TES3MP_REPO);
        assert_eq!(json["releaseId"], "65767406");
        assert_eq!(json["tag"], "tes3mp-0.8.1");
        assert_eq!(json["assetName"], "tes3mp.Win64.release.0.8.1.zip");

        let back: RuntimeSource = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, source);
    }

    /// `tag` and `assetName` are `#[serde(default)]`, so a source written by
    /// a build that only knew the release id still loads.
    #[test]
    fn github_release_defaults_missing_tag_and_asset() {
        let source: RuntimeSource = serde_json::from_str(
            r#"{"kind":"githubRelease","repo":"tes3mp/tes3mp","releaseId":"65767406"}"#,
        )
        .expect("deserialize");

        assert_eq!(source, RuntimeSource::from_legacy_release_id("65767406"));
        assert_eq!(source.legacy_release_id().as_deref(), Some("65767406"));
    }

    #[test]
    fn unknown_kind_is_rejected_rather_than_silently_defaulted() {
        let result: Result<RuntimeSource, _> =
            serde_json::from_str(r#"{"kind":"carrierPigeon","path":"/opt/tes3mp"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn local_directory_and_archive_round_trip_through_json() {
        for (source, kind) in [
            (
                RuntimeSource::LocalDirectory {
                    path: "/opt/MundusPatensMP".to_string(),
                },
                "localDirectory",
            ),
            (
                RuntimeSource::Archive {
                    path: "/downloads/tes3mp-0.8.1.tar.gz".to_string(),
                },
                "archive",
            ),
        ] {
            let json = serde_json::to_value(&source).expect("serialize");
            assert_eq!(json["kind"], kind);
            assert!(json["path"].is_string());
            let back: RuntimeSource = serde_json::from_value(json).expect("deserialize");
            assert_eq!(back, source);
        }
    }

    /// The reason the enum is tagged: a config written before the local
    /// variants existed still loads, unchanged, into this build.
    #[test]
    fn a_pre_local_variant_config_still_loads() {
        let source: RuntimeSource = serde_json::from_str(
            r#"{"kind":"githubRelease","repo":"tes3mp/tes3mp","releaseId":"65767406",
                "tag":"tes3mp-0.8.1","assetName":""}"#,
        )
        .expect("deserialize");

        assert!(matches!(source, RuntimeSource::GithubRelease { .. }));
        assert_eq!(source.legacy_release_id().as_deref(), Some("65767406"));
    }

    /// Only a GitHub release maps back onto the legacy `releaseId` field.
    #[test]
    fn local_sources_have_no_legacy_release_id() {
        assert_eq!(
            RuntimeSource::LocalDirectory {
                path: "/opt/tes3mp".to_string()
            }
            .legacy_release_id(),
            None
        );
        assert_eq!(
            RuntimeSource::Archive {
                path: "/opt/tes3mp.zip".to_string()
            }
            .legacy_release_id(),
            None
        );
    }

    #[test]
    fn current_platform_matches_the_host_cfg() {
        let current = TargetPlatform::current();
        if cfg!(windows) {
            assert_eq!(current, TargetPlatform::Windows);
        } else if cfg!(target_os = "macos") {
            assert_eq!(current, TargetPlatform::MacOs);
        } else {
            assert_eq!(current, TargetPlatform::Linux);
        }
    }
}
