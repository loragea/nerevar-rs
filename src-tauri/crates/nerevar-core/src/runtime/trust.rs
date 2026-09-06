//! Which GitHub repositories Nerevar is willing to download a TES3MP runtime
//! from.
//!
//! A server tells its players which *version* to run; it never tells them
//! where to get it. A compromised or malicious host that could name the
//! repository could name one whose "TES3MP" is an executable of its choosing,
//! and Nerevar runs what it installs. So the repository is the client's
//! anchor: a built-in list the app ships, plus repositories the player added
//! themselves, and nothing else — a host's `runtimeHint` may only *select
//! among* those, never extend them.
//!
//! The check lives here, in core, and is applied by every path that can put a
//! runtime on disk (`runtime::acquire`) as well as by the command that creates
//! a synced connection, so the CLI cannot reach a download the GUI would have
//! refused.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::data::NerevarConfig;

use super::source::{normalize_repo, RuntimeSource, DEFAULT_TES3MP_REPO};

/// The repositories every Nerevar install trusts without the player doing
/// anything.
///
/// Official TES3MP only, for now. A maintainer adding the fork here is
/// making a distribution-wide statement about it — that is the point of the
/// list being a constant in the source rather than a value a server, a
/// config file, or an update can reach.
pub const BUILTIN_TRUSTED_RUNTIME_REPOS: &[&str] = &[DEFAULT_TES3MP_REPO];

/// One row of the trusted-source list as a settings screen shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TrustedRepo {
    /// `owner/name`, lower-cased.
    pub repo: String,
    /// True for [`BUILTIN_TRUSTED_RUNTIME_REPOS`], which cannot be removed.
    pub builtin: bool,
}

/// Normalises a repository for the trust list: `owner/name`, lower-cased.
///
/// GitHub treats owner and repository names case-insensitively, so `TES3MP/
/// TES3MP` and `tes3mp/tes3mp` are one repository and must be one entry —
/// otherwise a hint could dodge the check by changing a letter's case.
pub fn normalize_trusted_repo(input: &str) -> Result<String, String> {
    Ok(normalize_repo(input)?.to_lowercase())
}

/// The repositories one Nerevar install trusts: the built-ins plus whatever
/// the player added.
///
/// A value rather than a borrow of the config so that every caller that needs
/// the check — including `runtime::acquire`, which runs while the config lock
/// is long released — can hold one cheaply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedRuntimeRepos {
    /// Player-added repositories, normalised. Built-ins are not stored here:
    /// they come from the constant, so a stale copy in a config file can
    /// neither shrink nor stale-pin the list.
    added: Vec<String>,
}

impl TrustedRuntimeRepos {
    /// Just the built-in list — what a context with no player config trusts
    /// (a test, a headless tool pointed at nothing).
    pub fn builtin_only() -> Self {
        Self { added: Vec::new() }
    }

    /// The trust list of the player this config belongs to.
    pub fn from_config(config: &NerevarConfig) -> Self {
        Self {
            added: config
                .trusted_runtime_repos
                .iter()
                .filter_map(|repo| normalize_trusted_repo(repo).ok())
                .collect(),
        }
    }

    /// Whether Nerevar may download a runtime from `repo`.
    pub fn is_trusted(&self, repo: &str) -> bool {
        let Ok(repo) = normalize_trusted_repo(repo) else {
            return false;
        };
        BUILTIN_TRUSTED_RUNTIME_REPOS
            .iter()
            .any(|builtin| builtin.eq_ignore_ascii_case(&repo))
            || self.added.iter().any(|added| added == &repo)
    }

    /// The list a settings screen renders: built-ins first, then what the
    /// player added, each in the order it was added.
    pub fn list(&self) -> Vec<TrustedRepo> {
        BUILTIN_TRUSTED_RUNTIME_REPOS
            .iter()
            .map(|repo| TrustedRepo {
                repo: repo.to_lowercase(),
                builtin: true,
            })
            .chain(self.added.iter().map(|repo| TrustedRepo {
                repo: repo.clone(),
                builtin: false,
            }))
            .collect()
    }

    /// Refuses a runtime source that would download from an untrusted
    /// repository. The local sources are unaffected: a directory or an
    /// archive the player picked out of their own filesystem is a file they
    /// already have, not something Nerevar fetches on a server's say-so.
    pub fn require_trusted_source(&self, source: &RuntimeSource) -> Result<(), String> {
        match source {
            RuntimeSource::GithubRelease { repo, .. } => {
                if self.is_trusted(repo) {
                    Ok(())
                } else {
                    Err(untrusted_repo_message(repo))
                }
            }
            RuntimeSource::LocalDirectory { .. } | RuntimeSource::Archive { .. } => Ok(()),
        }
    }
}

/// Why Nerevar will not download from a repository, and what to do about it.
/// One wording, so the picker's refusal and a hint's warning read the same.
pub fn untrusted_repo_message(repo: &str) -> String {
    format!(
        "{repo} is not one of your trusted runtime sources. Nerevar will not \
         download from it unless you add it under Settings."
    )
}

/// Whether this player's config trusts `repo`.
pub fn is_trusted_repo(config: &NerevarConfig, repo: &str) -> bool {
    TrustedRuntimeRepos::from_config(config).is_trusted(repo)
}

/// The trusted-source list for a settings screen.
pub fn trusted_repos(config: &NerevarConfig) -> Vec<TrustedRepo> {
    TrustedRuntimeRepos::from_config(config).list()
}

/// Adds a repository to the player's trust list, returning the normalised
/// form that was stored.
///
/// Adding one that is already trusted is not an error — it is what the player
/// asked for, and it stays a single entry.
pub fn add_trusted_repo(config: &mut NerevarConfig, repo: &str) -> Result<String, String> {
    let repo = normalize_trusted_repo(repo)?;
    if !TrustedRuntimeRepos::from_config(config).is_trusted(&repo) {
        config.trusted_runtime_repos.push(repo.clone());
    }
    Ok(repo)
}

/// Removes a player-added repository. A built-in cannot be removed: it is
/// what the app itself vouches for, and an install with an empty trust list
/// could not fetch the official runtime at all.
pub fn remove_trusted_repo(config: &mut NerevarConfig, repo: &str) -> Result<(), String> {
    let repo = normalize_trusted_repo(repo)?;
    if BUILTIN_TRUSTED_RUNTIME_REPOS
        .iter()
        .any(|builtin| builtin.eq_ignore_ascii_case(&repo))
    {
        return Err(format!(
            "{repo} is one of Nerevar's built-in runtime sources and cannot be removed."
        ));
    }
    let before = config.trusted_runtime_repos.len();
    config
        .trusted_runtime_repos
        .retain(|entry| normalize_trusted_repo(entry).as_deref() != Ok(repo.as_str()));
    if config.trusted_runtime_repos.len() == before {
        return Err(format!("{repo} is not in your trusted runtime sources."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_config() -> NerevarConfig {
        NerevarConfig::default()
    }

    #[test]
    fn the_official_repository_is_trusted_out_of_the_box() {
        let config = empty_config();
        assert!(is_trusted_repo(&config, DEFAULT_TES3MP_REPO));
        assert!(is_trusted_repo(&config, "TES3MP/TES3MP"));
        assert!(is_trusted_repo(&config, "https://github.com/tes3mp/tes3mp"));
    }

    #[test]
    fn an_unknown_repository_is_not_trusted() {
        let config = empty_config();
        assert!(!is_trusted_repo(&config, "attacker/tes3mp"));
        assert!(!is_trusted_repo(&config, "not a repository"));
        assert!(!is_trusted_repo(&config, ""));
    }

    /// The list a player has never touched is exactly the built-in one, and
    /// the built-in row is marked so the settings screen can say so.
    #[test]
    fn the_default_list_is_the_builtin_one() {
        let listed = trusted_repos(&empty_config());
        assert_eq!(
            listed,
            vec![TrustedRepo {
                repo: DEFAULT_TES3MP_REPO.to_string(),
                builtin: true,
            }]
        );
    }

    #[test]
    fn adding_a_repository_trusts_it_and_stores_the_normalised_form() {
        let mut config = empty_config();
        let stored = add_trusted_repo(&mut config, "https://github.com/Victor/MundusPatensMP")
            .expect("a pasted URL is a repository");
        assert_eq!(stored, "victor/munduspatensmp");
        assert_eq!(config.trusted_runtime_repos, vec!["victor/munduspatensmp"]);
        assert!(is_trusted_repo(&config, "Victor/MundusPatensMP"));
        assert_eq!(
            trusted_repos(&config)
                .into_iter()
                .map(|entry| (entry.repo, entry.builtin))
                .collect::<Vec<_>>(),
            vec![
                (DEFAULT_TES3MP_REPO.to_string(), true),
                ("victor/munduspatensmp".to_string(), false),
            ]
        );
    }

    #[test]
    fn adding_the_same_repository_twice_keeps_one_entry() {
        let mut config = empty_config();
        add_trusted_repo(&mut config, "victor/fork").unwrap();
        add_trusted_repo(&mut config, "Victor/Fork").unwrap();
        assert_eq!(config.trusted_runtime_repos, vec!["victor/fork"]);
    }

    #[test]
    fn adding_a_builtin_does_not_duplicate_it() {
        let mut config = empty_config();
        add_trusted_repo(&mut config, DEFAULT_TES3MP_REPO).unwrap();
        assert!(config.trusted_runtime_repos.is_empty());
        assert_eq!(trusted_repos(&config).len(), 1);
    }

    #[test]
    fn adding_something_that_is_not_a_repository_is_refused() {
        let mut config = empty_config();
        assert!(add_trusted_repo(&mut config, "just-a-name").is_err());
        assert!(config.trusted_runtime_repos.is_empty());
    }

    #[test]
    fn removing_a_player_added_repository_untrusts_it() {
        let mut config = empty_config();
        add_trusted_repo(&mut config, "victor/fork").unwrap();
        remove_trusted_repo(&mut config, "Victor/Fork").expect("removable");
        assert!(config.trusted_runtime_repos.is_empty());
        assert!(!is_trusted_repo(&config, "victor/fork"));
    }

    #[test]
    fn a_builtin_cannot_be_removed() {
        let mut config = empty_config();
        let error = remove_trusted_repo(&mut config, DEFAULT_TES3MP_REPO)
            .expect_err("the built-in list is the app's own statement");
        assert!(error.contains("built-in"), "unexpected error: {error}");
        assert!(is_trusted_repo(&config, DEFAULT_TES3MP_REPO));
    }

    #[test]
    fn removing_something_that_was_never_added_says_so() {
        let mut config = empty_config();
        let error = remove_trusted_repo(&mut config, "victor/fork").expect_err("not present");
        assert!(error.contains("not in your trusted"), "{error}");
    }

    /// A config written before the field existed has no `trustedRuntimeRepos`
    /// key at all; it must load, and it must trust the built-in list.
    #[test]
    fn a_config_without_the_field_loads_and_trusts_the_builtins() {
        let config: NerevarConfig = serde_json::from_str(
            r#"{"onboardingComplete":true,"ownedInstances":null,
                "syncedInstances":null,"rootPath":null,"syncPort":25567}"#,
        )
        .expect("an old config must still load");
        assert!(config.trusted_runtime_repos.is_empty());
        assert!(is_trusted_repo(&config, DEFAULT_TES3MP_REPO));
        assert!(!is_trusted_repo(&config, "victor/fork"));
    }

    /// A hand-edited config entry that is not a repository is ignored rather
    /// than trusted by accident.
    #[test]
    fn a_malformed_stored_entry_trusts_nothing() {
        let mut config = empty_config();
        config.trusted_runtime_repos = vec!["../../etc".to_string(), "Victor/Fork".to_string()];
        assert!(!is_trusted_repo(&config, "../../etc"));
        assert!(is_trusted_repo(&config, "victor/fork"));
    }

    #[test]
    fn only_a_github_source_is_subject_to_the_check() {
        let trusted = TrustedRuntimeRepos::builtin_only();
        trusted
            .require_trusted_source(&RuntimeSource::LocalDirectory {
                path: "/opt/whatever".to_string(),
            })
            .expect("a directory the player picked is not a download");
        trusted
            .require_trusted_source(&RuntimeSource::Archive {
                path: "/opt/whatever.zip".to_string(),
            })
            .expect("an archive on disk is not a download");

        let error = trusted
            .require_trusted_source(&RuntimeSource::GithubRelease {
                repo: "attacker/tes3mp".to_string(),
                release_id: "1".to_string(),
                tag: "0.8.1".to_string(),
                asset_name: String::new(),
            })
            .expect_err("an untrusted repository is refused");
        assert_eq!(error, untrusted_repo_message("attacker/tes3mp"));
        assert!(error.contains("under Settings"), "{error}");
    }
}
