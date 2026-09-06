//! The version lock: the server names the TES3MP *tag*, the client decides
//! which repository that tag may come from.
//!
//! Two pure decisions live here, both taken in core so the desktop app, the
//! CLI and the tests reach the same answer:
//!
//! - [`resolve_runtime_hint`] — what a host's advertised runtime does to the
//!   runtime picker before an instance exists. A hint naming a repository the
//!   player trusts preselects that repository and tag; one naming anything
//!   else preselects nothing but the official repository, and is reported.
//! - [`runtime_mismatch`] — whether the runtime an instance already has still
//!   satisfies what its host asks for, computed at every sync.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::source::{RuntimeSource, DEFAULT_TES3MP_REPO};
use super::trust::TrustedRuntimeRepos;

/// What a host's runtime suggestion means for this client's picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "camelCase")]
#[ts(export)]
pub enum RuntimeHintResolution {
    /// The host advertises nothing (or advertises something that cannot be a
    /// hint at all). The picker keeps whatever it had.
    NoHint,
    /// The host names a repository this client trusts: the picker takes both
    /// the repository and the tag, exactly as it did before the trust list
    /// existed.
    #[serde(rename_all = "camelCase")]
    Trusted {
        repo: String,
        tag: String,
        release_id: String,
    },
    /// The host names a repository this client does not trust. Nothing is
    /// preselected from it: the picker falls back to the official repository
    /// with no release chosen, and `message` says why.
    #[serde(rename_all = "camelCase")]
    Untrusted {
        /// What the host named, for the message and nothing else.
        repo: String,
        tag: String,
        /// What the picker is set to instead.
        fallback_repo: String,
        message: String,
    },
}

/// Decides what a host's `runtime_hint` may do to this client's picker.
///
/// The ruling this implements: a server that could name the repository could
/// name one whose "TES3MP" is an executable of its choosing. So the tag is the
/// server's to set and the repository is not — a hinted repository is honoured
/// only when the player already trusts it, and is otherwise reported without
/// being preselected, downloaded, or stored.
pub fn resolve_runtime_hint(
    hint: Option<&RuntimeSource>,
    trusted: &TrustedRuntimeRepos,
) -> RuntimeHintResolution {
    let Some(RuntimeSource::GithubRelease {
        repo,
        release_id,
        tag,
        ..
    }) = hint
    else {
        return RuntimeHintResolution::NoHint;
    };

    // An empty repo in a hint means "the official one" (`normalize_runtime_hint`
    // defaults it on the host side; a hand-written config may still send it).
    let repo = if repo.trim().is_empty() {
        DEFAULT_TES3MP_REPO.to_string()
    } else {
        repo.clone()
    };

    if trusted.is_trusted(&repo) {
        RuntimeHintResolution::Trusted {
            repo,
            tag: tag.clone(),
            release_id: release_id.clone(),
        }
    } else {
        RuntimeHintResolution::Untrusted {
            message: untrusted_hint_message(&repo, tag),
            repo,
            tag: tag.clone(),
            fallback_repo: DEFAULT_TES3MP_REPO.to_string(),
        }
    }
}

/// What a player is told when a server suggests a runtime from a repository
/// they have not trusted.
pub fn untrusted_hint_message(repo: &str, tag: &str) -> String {
    let version = if tag.trim().is_empty() {
        "an unnamed version".to_string()
    } else {
        format!("TES3MP {tag}")
    };
    format!(
        "This server suggests {version} from {repo}, which is not one of your \
         trusted sources. Nerevar will not download from it unless you add it \
         under Settings."
    )
}

/// A host requiring a TES3MP version this instance does not have installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RuntimeMismatch {
    pub instance_id: String,
    /// The tag the host asks for.
    pub required_tag: String,
    /// The tag of the runtime this instance has. Empty when the instance's
    /// runtime is not a GitHub release, or was installed by a build that did
    /// not record the tag.
    pub installed_tag: String,
    /// The repository an update would be fetched from: the instance's own,
    /// which the player chose. Never the hinted one on the strength of the
    /// hint alone.
    pub repo: String,
    /// Whether this blocks launching. False when there is nothing to compare —
    /// a local-directory or archive runtime, or an instance with no recorded
    /// source — in which case the mismatch is reported and nothing else.
    pub enforced: bool,
    /// The repository the host named, which may not be `repo`.
    pub hint_repo: String,
    /// Whether the player trusts `hint_repo`.
    pub hint_repo_trusted: bool,
    /// The whole thing in one sentence, so the app, the CLI and the daemon
    /// say the same words.
    pub message: String,
}

/// Compares the runtime an instance has against the one its host requires.
///
/// `None` means "nothing to say": the host advertises no runtime, advertises
/// one with no tag, or the installed runtime already carries that tag.
///
/// A host may only pin the *version*. Where that version comes from is the
/// instance's own repository — so a mismatch reported against an untrusted
/// hint still points the update at the repository the player chose, and says
/// that the suggestion itself was not trusted.
pub fn runtime_mismatch(
    instance_id: &str,
    hint: Option<&RuntimeSource>,
    installed: Option<&RuntimeSource>,
    trusted: &TrustedRuntimeRepos,
) -> Option<RuntimeMismatch> {
    let Some(RuntimeSource::GithubRelease {
        repo: hint_repo,
        tag: required_tag,
        ..
    }) = hint
    else {
        return None;
    };
    if required_tag.trim().is_empty() {
        return None;
    }

    let hint_repo = if hint_repo.trim().is_empty() {
        DEFAULT_TES3MP_REPO.to_string()
    } else {
        hint_repo.clone()
    };
    let hint_repo_trusted = trusted.is_trusted(&hint_repo);

    let (repo, installed_tag, enforced, reason) = match installed {
        Some(RuntimeSource::GithubRelease { repo, tag, .. }) => {
            if tag == required_tag {
                return None;
            }
            let have = if tag.trim().is_empty() {
                "a version this instance never recorded".to_string()
            } else {
                tag.clone()
            };
            (
                repo.clone(),
                tag.clone(),
                true,
                format!("This server now requires TES3MP {required_tag}; you have {have}."),
            )
        }
        Some(RuntimeSource::LocalDirectory { .. }) => (
            hint_repo.clone(),
            String::new(),
            false,
            format!(
                "This server requires TES3MP {required_tag}. This instance runs a build you \
                 installed from a folder of your own, so Nerevar cannot check its version."
            ),
        ),
        Some(RuntimeSource::Archive { .. }) => (
            hint_repo.clone(),
            String::new(),
            false,
            format!(
                "This server requires TES3MP {required_tag}. This instance runs a build you \
                 installed from an archive of your own, so Nerevar cannot check its version."
            ),
        ),
        None => (
            hint_repo.clone(),
            String::new(),
            false,
            format!(
                "This server requires TES3MP {required_tag}. This instance records no runtime \
                 source, so Nerevar cannot check its version."
            ),
        ),
    };

    let mut message = reason;
    if !hint_repo_trusted {
        message.push(' ');
        message.push_str(&format!(
            "The server suggests {hint_repo}, which is not one of your trusted sources; \
             Nerevar looks for {required_tag} in {repo} instead."
        ));
    }

    Some(RuntimeMismatch {
        instance_id: instance_id.to_string(),
        required_tag: required_tag.clone(),
        installed_tag,
        repo,
        enforced,
        hint_repo,
        hint_repo_trusted,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::NerevarConfig;
    use crate::runtime::trust::add_trusted_repo;

    fn github(repo: &str, tag: &str) -> RuntimeSource {
        RuntimeSource::GithubRelease {
            repo: repo.to_string(),
            release_id: "4242".to_string(),
            tag: tag.to_string(),
            asset_name: String::new(),
        }
    }

    fn trusting(repo: &str) -> TrustedRuntimeRepos {
        let mut config = NerevarConfig::default();
        add_trusted_repo(&mut config, repo).expect("a repository");
        TrustedRuntimeRepos::from_config(&config)
    }

    #[test]
    fn no_hint_resolves_to_nothing() {
        assert_eq!(
            resolve_runtime_hint(None, &TrustedRuntimeRepos::builtin_only()),
            RuntimeHintResolution::NoHint
        );
    }

    /// Only a GitHub release can be a hint; anything else is treated as no
    /// suggestion at all rather than as an instruction to read a path.
    #[test]
    fn a_local_hint_resolves_to_nothing() {
        let local = RuntimeSource::LocalDirectory {
            path: "/srv/tes3mp".to_string(),
        };
        assert_eq!(
            resolve_runtime_hint(Some(&local), &TrustedRuntimeRepos::builtin_only()),
            RuntimeHintResolution::NoHint
        );
    }

    #[test]
    fn a_trusted_hint_preselects_the_repository_and_the_tag() {
        let hint = github(DEFAULT_TES3MP_REPO, "tes3mp-0.8.1");
        assert_eq!(
            resolve_runtime_hint(Some(&hint), &TrustedRuntimeRepos::builtin_only()),
            RuntimeHintResolution::Trusted {
                repo: DEFAULT_TES3MP_REPO.to_string(),
                tag: "tes3mp-0.8.1".to_string(),
                release_id: "4242".to_string(),
            }
        );
    }

    /// The trust list, not the app's defaults, is what widens this: a fork the
    /// player added is honoured exactly like the official repository.
    #[test]
    fn a_player_added_repository_makes_its_hint_trusted() {
        let hint = github("Victor/MundusPatensMP", "0.9.0");
        assert_eq!(
            resolve_runtime_hint(Some(&hint), &trusting("victor/munduspatensmp")),
            RuntimeHintResolution::Trusted {
                repo: "Victor/MundusPatensMP".to_string(),
                tag: "0.9.0".to_string(),
                release_id: "4242".to_string(),
            }
        );
    }

    #[test]
    fn an_untrusted_hint_preselects_the_official_repo_and_no_tag() {
        let hint = github("attacker/tes3mp", "0.8.1");
        let resolution = resolve_runtime_hint(Some(&hint), &TrustedRuntimeRepos::builtin_only());
        let RuntimeHintResolution::Untrusted {
            repo,
            tag,
            fallback_repo,
            message,
        } = resolution
        else {
            panic!("an untrusted repository must not be preselected: {resolution:?}");
        };
        assert_eq!(repo, "attacker/tes3mp");
        assert_eq!(tag, "0.8.1");
        assert_eq!(fallback_repo, DEFAULT_TES3MP_REPO);
        assert_eq!(
            message,
            "This server suggests TES3MP 0.8.1 from attacker/tes3mp, which is not one of \
             your trusted sources. Nerevar will not download from it unless you add it \
             under Settings."
        );
    }

    #[test]
    fn an_empty_hint_repo_means_the_official_one() {
        let hint = github("", "0.8.1");
        assert!(matches!(
            resolve_runtime_hint(Some(&hint), &TrustedRuntimeRepos::builtin_only()),
            RuntimeHintResolution::Trusted { ref repo, .. } if repo == DEFAULT_TES3MP_REPO
        ));
    }

    #[test]
    fn the_same_tag_is_not_a_mismatch() {
        assert_eq!(
            runtime_mismatch(
                "i1",
                Some(&github(DEFAULT_TES3MP_REPO, "tes3mp-0.8.1")),
                Some(&github(DEFAULT_TES3MP_REPO, "tes3mp-0.8.1")),
                &TrustedRuntimeRepos::builtin_only(),
            ),
            None
        );
    }

    #[test]
    fn no_hint_is_not_a_mismatch() {
        assert_eq!(
            runtime_mismatch(
                "i1",
                None,
                Some(&github(DEFAULT_TES3MP_REPO, "tes3mp-0.8.0")),
                &TrustedRuntimeRepos::builtin_only(),
            ),
            None
        );
    }

    /// A host that advertises a repository but no tag pins nothing: there is
    /// no version to compare against.
    #[test]
    fn a_hint_without_a_tag_is_not_a_mismatch() {
        assert_eq!(
            runtime_mismatch(
                "i1",
                Some(&github(DEFAULT_TES3MP_REPO, "")),
                Some(&github(DEFAULT_TES3MP_REPO, "tes3mp-0.8.0")),
                &TrustedRuntimeRepos::builtin_only(),
            ),
            None
        );
    }

    #[test]
    fn a_different_tag_is_an_enforced_mismatch_against_the_instances_own_repo() {
        let mismatch = runtime_mismatch(
            "i1",
            Some(&github(DEFAULT_TES3MP_REPO, "tes3mp-0.8.1")),
            Some(&github(DEFAULT_TES3MP_REPO, "tes3mp-0.8.0")),
            &TrustedRuntimeRepos::builtin_only(),
        )
        .expect("0.8.0 does not satisfy 0.8.1");

        assert_eq!(mismatch.instance_id, "i1");
        assert_eq!(mismatch.required_tag, "tes3mp-0.8.1");
        assert_eq!(mismatch.installed_tag, "tes3mp-0.8.0");
        assert_eq!(mismatch.repo, DEFAULT_TES3MP_REPO);
        assert!(mismatch.enforced);
        assert!(mismatch.hint_repo_trusted);
        assert_eq!(
            mismatch.message,
            "This server now requires TES3MP tes3mp-0.8.1; you have tes3mp-0.8.0."
        );
    }

    /// The update never follows the server's repository on the server's word:
    /// an untrusted hint still pins the version, and the client looks for it
    /// where the player's own instance came from.
    #[test]
    fn an_untrusted_hint_still_pins_the_tag_but_not_the_repository() {
        let mismatch = runtime_mismatch(
            "i1",
            Some(&github("attacker/tes3mp", "0.9.9")),
            Some(&github(DEFAULT_TES3MP_REPO, "tes3mp-0.8.1")),
            &TrustedRuntimeRepos::builtin_only(),
        )
        .expect("a different tag is a mismatch whoever suggested it");

        assert_eq!(mismatch.repo, DEFAULT_TES3MP_REPO);
        assert_eq!(mismatch.hint_repo, "attacker/tes3mp");
        assert!(!mismatch.hint_repo_trusted);
        assert!(mismatch.enforced);
        assert!(
            mismatch.message.contains("not one of your trusted sources"),
            "{}",
            mismatch.message
        );
    }

    #[test]
    fn a_local_runtime_is_reported_but_not_enforced() {
        for installed in [
            RuntimeSource::LocalDirectory {
                path: "/opt/tes3mp".to_string(),
            },
            RuntimeSource::Archive {
                path: "/opt/tes3mp.tar.gz".to_string(),
            },
        ] {
            let mismatch = runtime_mismatch(
                "i1",
                Some(&github(DEFAULT_TES3MP_REPO, "tes3mp-0.8.1")),
                Some(&installed),
                &TrustedRuntimeRepos::builtin_only(),
            )
            .expect("the requirement is still worth reporting");
            assert!(!mismatch.enforced, "{installed:?} must not block a launch");
            assert!(mismatch.installed_tag.is_empty());
            assert!(
                mismatch.message.contains("cannot check its version"),
                "{}",
                mismatch.message
            );
        }
    }

    #[test]
    fn an_instance_with_no_recorded_runtime_is_reported_but_not_enforced() {
        let mismatch = runtime_mismatch(
            "i1",
            Some(&github(DEFAULT_TES3MP_REPO, "tes3mp-0.8.1")),
            None,
            &TrustedRuntimeRepos::builtin_only(),
        )
        .expect("worth reporting");
        assert!(!mismatch.enforced);
        assert!(mismatch.message.contains("records no runtime source"));
    }

    /// An instance installed before Nerevar recorded tags cannot be shown to
    /// satisfy the requirement, so it is treated as a mismatch — with a
    /// message that says the version is unknown rather than naming a wrong one.
    #[test]
    fn an_instance_with_an_unrecorded_tag_is_a_mismatch() {
        let mismatch = runtime_mismatch(
            "i1",
            Some(&github(DEFAULT_TES3MP_REPO, "tes3mp-0.8.1")),
            Some(&RuntimeSource::from_legacy_release_id("65767406")),
            &TrustedRuntimeRepos::builtin_only(),
        )
        .expect("an unknown version cannot be shown to match");
        assert!(mismatch.enforced);
        assert!(mismatch.installed_tag.is_empty());
        assert!(
            mismatch.message.contains("never recorded"),
            "{}",
            mismatch.message
        );
    }
}
