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

/// Normalises a GitHub repository the user typed into the `owner/name` form
/// the releases API takes, or says why it is not one.
///
/// A pasted browser URL (`https://github.com/owner/name`, with or without a
/// trailing slash or a `.git` suffix) is the one shape that is rewritten
/// rather than rejected — it is what comes out of an address bar. Everything
/// else must already be `owner/name`: one slash, two non-empty segments of
/// GitHub's own name alphabet.
pub fn normalize_repo(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a GitHub repository as owner/name".to_string());
    }

    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let without_host = without_scheme
        .strip_prefix("www.github.com/")
        .or_else(|| without_scheme.strip_prefix("github.com/"))
        .unwrap_or(without_scheme);
    let candidate = without_host.trim_end_matches('/');
    let candidate = candidate.strip_suffix(".git").unwrap_or(candidate);

    let malformed = || format!("\"{trimmed}\" is not a GitHub repository — use owner/name");

    let mut parts = candidate.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(malformed());
    };
    if !is_repo_segment(owner) || !is_repo_segment(name) {
        return Err(malformed());
    }

    Ok(format!("{owner}/{name}"))
}

/// One side of an `owner/name` pair: GitHub's own alphabet for account and
/// repository names, and never a relative-path segment.
fn is_repo_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Checks a runtime a host means to *advertise* to its players, normalising
/// its repository and defaulting an empty one to [`DEFAULT_TES3MP_REPO`].
///
/// Only `githubRelease` can be advertised: a hint travels to machines that
/// are not the host's, and a path on the host's disk means nothing on a
/// player's. A client never downloads from anywhere but the named GitHub
/// repository, so that is the only shape a hint may take.
pub fn normalize_runtime_hint(hint: RuntimeSource) -> Result<RuntimeSource, String> {
    match hint {
        RuntimeSource::GithubRelease {
            repo,
            release_id,
            tag,
            asset_name,
        } => {
            let repo = if repo.trim().is_empty() {
                DEFAULT_TES3MP_REPO.to_string()
            } else {
                normalize_repo(&repo)?
            };
            Ok(RuntimeSource::GithubRelease {
                repo,
                release_id,
                tag,
                asset_name,
            })
        }
        RuntimeSource::LocalDirectory { .. } | RuntimeSource::Archive { .. } => Err(
            "A runtime hint must be a GitHub release: a path on the host's disk \
             means nothing on a player's machine."
                .to_string(),
        ),
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
    fn a_repo_already_in_owner_name_form_is_returned_unchanged() {
        assert_eq!(normalize_repo("tes3mp/tes3mp").unwrap(), "tes3mp/tes3mp");
        assert_eq!(normalize_repo("  owner/name  ").unwrap(), "owner/name");
        assert_eq!(
            normalize_repo("Owner-1/name_2.fork").unwrap(),
            "Owner-1/name_2.fork"
        );
    }

    /// The one shape that is rewritten rather than rejected: what a user
    /// copies out of the browser's address bar.
    #[test]
    fn a_pasted_github_url_normalises_to_owner_name() {
        for pasted in [
            "https://github.com/owner/name",
            "https://github.com/owner/name/",
            "http://github.com/owner/name",
            "https://www.github.com/owner/name",
            "github.com/owner/name",
            "https://github.com/owner/name.git",
            "owner/name/",
        ] {
            assert_eq!(
                normalize_repo(pasted).unwrap(),
                "owner/name",
                "{pasted} should normalise"
            );
        }
    }

    #[test]
    fn anything_that_is_not_a_repository_is_rejected() {
        for bad in [
            "",
            "   ",
            "tes3mp",
            "owner/name/extra",
            "/name",
            "owner/",
            "owner name",
            "owner/na me",
            "https://gitlab.com/owner/name",
            "https://github.com/owner/name/releases",
            "owner/../secrets",
        ] {
            assert!(
                normalize_repo(bad).is_err(),
                "{bad:?} should be rejected as a repository"
            );
        }
    }

    #[test]
    fn a_hint_normalises_its_repo_and_defaults_an_empty_one() {
        let hint = normalize_runtime_hint(RuntimeSource::GithubRelease {
            repo: "https://github.com/owner/name".to_string(),
            release_id: "42".to_string(),
            tag: "0.8.1".to_string(),
            asset_name: String::new(),
        })
        .expect("a github release is a valid hint");
        assert_eq!(
            hint,
            RuntimeSource::GithubRelease {
                repo: "owner/name".to_string(),
                release_id: "42".to_string(),
                tag: "0.8.1".to_string(),
                asset_name: String::new(),
            }
        );

        let defaulted = normalize_runtime_hint(RuntimeSource::GithubRelease {
            repo: String::new(),
            release_id: String::new(),
            tag: "0.8.1".to_string(),
            asset_name: String::new(),
        })
        .expect("an empty repo falls back to the official one");
        assert!(matches!(
            defaulted,
            RuntimeSource::GithubRelease { ref repo, .. } if repo == DEFAULT_TES3MP_REPO
        ));
    }

    /// A hint travels to other people's machines, so only a source they can
    /// actually fetch may be advertised.
    #[test]
    fn a_local_hint_is_rejected() {
        for local in [
            RuntimeSource::LocalDirectory {
                path: "/opt/tes3mp".to_string(),
            },
            RuntimeSource::Archive {
                path: "/opt/tes3mp.zip".to_string(),
            },
        ] {
            let error = normalize_runtime_hint(local).expect_err("a local hint is meaningless");
            assert!(
                error.contains("must be a GitHub release"),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn a_hint_with_an_unusable_repo_is_rejected() {
        let error = normalize_runtime_hint(RuntimeSource::GithubRelease {
            repo: "not a repo".to_string(),
            release_id: String::new(),
            tag: String::new(),
            asset_name: String::new(),
        })
        .expect_err("a malformed repo is not a hint");
        assert!(error.contains("owner/name"), "unexpected error: {error}");
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
