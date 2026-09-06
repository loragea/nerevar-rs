//! Replacing an instance's TES3MP runtime with the version its host requires.
//!
//! The version comes from the host; the repository does not (see
//! `runtime::trust`). So an update looks up `required_tag` in the repository
//! the *instance* was installed from, and refuses outright if the player no
//! longer trusts that repository.
//!
//! The old install stays where it is until the new one has been downloaded,
//! extracted and inspected: a failed update leaves a playable instance
//! behind, not an empty `tes3mp/` directory.

use std::path::Path;
use std::sync::Arc;

use log::{info, warn};

use crate::github_getters::fetch_github_releases;
use crate::reporter::EventSink;

use super::acquire::acquire;
use super::source::{RuntimeSource, TargetPlatform};
use super::trust::TrustedRuntimeRepos;

/// Installs `required_tag` over the runtime currently in `tes3mp_dir`,
/// returning the [`RuntimeSource`] the caller should record for the instance.
///
/// `installed` is the instance's current source and decides where the new
/// build comes from: only a `githubRelease` can be updated, because only it
/// names a repository whose releases can be searched for a tag.
pub async fn update_instance_runtime(
    tes3mp_dir: &Path,
    installed: &RuntimeSource,
    required_tag: &str,
    trusted: &TrustedRuntimeRepos,
    platform: TargetPlatform,
    sink: Arc<dyn EventSink>,
    operation_id: Option<String>,
) -> Result<RuntimeSource, String> {
    let RuntimeSource::GithubRelease { repo, .. } = installed else {
        return Err(
            "This instance's TES3MP runtime came from a folder or archive of your own, so \
             Nerevar cannot fetch a different version for it. Reinstall the runtime from \
             the version this server asks for."
                .to_string(),
        );
    };
    trusted.require_trusted_source(installed)?;

    let releases = fetch_github_releases(repo, "Tes3MP").await?;
    let release = releases
        .iter()
        .find(|release| release.tag_name == required_tag)
        .ok_or_else(|| {
            format!(
                "{repo} publishes no release tagged {required_tag}. Ask the server's operator \
                 which build to install."
            )
        })?;

    let updated = RuntimeSource::GithubRelease {
        repo: repo.clone(),
        release_id: release.id.to_string(),
        tag: release.tag_name.clone(),
        // Not carried over: a file name pinned for the old release names
        // nothing in the new one. The platform rules pick again.
        asset_name: String::new(),
    };

    let parent = tes3mp_dir
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", tes3mp_dir.display()))?;
    let token = uuid::Uuid::new_v4();
    let staging = parent.join(format!("tes3mp.updating-{token}"));
    let retired = parent.join(format!("tes3mp.previous-{token}"));

    let installed_info = match acquire(&updated, &staging, platform, sink, operation_id, trusted)
        .await
        .and_then(|info| info.require_complete().map(|_| info))
    {
        Ok(info) => info,
        Err(err) => {
            // The old runtime was never touched, so the instance is still
            // playable — say so rather than leaving the user guessing.
            remove_dir(&staging);
            return Err(format!(
                "{err} The runtime already installed for this instance was left in place."
            ));
        }
    };

    if tes3mp_dir.exists() {
        std::fs::rename(tes3mp_dir, &retired).map_err(|e| {
            remove_dir(&staging);
            format!(
                "Failed to set the old runtime aside ({}): {e}",
                tes3mp_dir.display()
            )
        })?;
    }
    if let Err(e) = std::fs::rename(&staging, tes3mp_dir) {
        // Put the old one back: a half-swapped instance is worse than one
        // that never updated.
        if retired.exists() {
            let _ = std::fs::rename(&retired, tes3mp_dir);
        }
        remove_dir(&staging);
        return Err(format!(
            "Failed to install the new runtime at {}: {e}",
            tes3mp_dir.display()
        ));
    }
    remove_dir(&retired);

    info!(
        "Updated the TES3MP runtime at {} to {} (OpenMW {})",
        tes3mp_dir.display(),
        required_tag,
        installed_info.version_display()
    );
    Ok(updated)
}

fn remove_dir(path: &Path) {
    if path.exists() {
        if let Err(err) = std::fs::remove_dir_all(path) {
            warn!("Failed to remove {}: {err}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporter::CollectingEventSink;

    /// A local runtime has no repository to look a tag up in; the refusal has
    /// to say that rather than failing later against an empty repo string.
    #[tokio::test]
    async fn a_local_runtime_cannot_be_updated() {
        let dir = std::env::temp_dir().join("nerevar-update-local");
        let error = update_instance_runtime(
            &dir.join("tes3mp"),
            &RuntimeSource::LocalDirectory {
                path: "/opt/tes3mp".to_string(),
            },
            "0.8.1",
            &TrustedRuntimeRepos::builtin_only(),
            TargetPlatform::Linux,
            Arc::new(CollectingEventSink::default()),
            None,
        )
        .await
        .expect_err("there is no release list to search");
        assert!(error.contains("folder or archive of your own"), "{error}");
    }

    /// The trust check comes before the network call: an untrusted repository
    /// is refused without Nerevar ever contacting it.
    #[tokio::test]
    async fn an_untrusted_repository_is_refused_before_any_request() {
        let dir = std::env::temp_dir().join("nerevar-update-untrusted");
        let error = update_instance_runtime(
            &dir.join("tes3mp"),
            &RuntimeSource::GithubRelease {
                repo: "attacker/tes3mp".to_string(),
                release_id: "1".to_string(),
                tag: "0.8.0".to_string(),
                asset_name: String::new(),
            },
            "0.8.1",
            &TrustedRuntimeRepos::builtin_only(),
            TargetPlatform::Linux,
            Arc::new(CollectingEventSink::default()),
            None,
        )
        .await
        .expect_err("an untrusted repository is never fetched from");
        assert!(
            error.contains("not one of your trusted runtime sources"),
            "{error}"
        );
        assert!(!dir.exists(), "nothing may be written for a refused update");
    }
}
