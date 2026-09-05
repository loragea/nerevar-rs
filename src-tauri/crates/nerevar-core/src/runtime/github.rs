//! Resolving a GitHub release into the one asset that is this platform's
//! TES3MP runtime.

use crate::data::{GithubAssetResponse, GithubReleaseResponse};
use crate::github_getters::fetch_github_releases;

use super::source::{normalize_repo, TargetPlatform};

/// Fetches the release with `release_id` from `repo`.
///
/// One HTTP call: the GitHub releases listing is fetched once and searched
/// locally. (The old download path listed the releases *and* had its caller
/// list them again to populate the picker.)
///
/// `repo` reaches here from an instance's stored `RuntimeSource`, which a
/// custom-repo picker or a hand-edited config wrote, so it is normalised
/// before it becomes a URL.
pub async fn fetch_release_by_id(
    repo: &str,
    release_id: &str,
) -> Result<GithubReleaseResponse, String> {
    let repo = normalize_repo(repo)?;
    let wanted: u64 = release_id
        .parse()
        .map_err(|_| format!("Invalid release id: {release_id}"))?;

    let releases = fetch_github_releases(&repo, "Tes3MP").await?;
    releases
        .into_iter()
        .find(|release| release.id == wanted)
        .ok_or_else(|| format!("Release with id {release_id} not found"))
}

/// Picks the release asset that is `platform`'s TES3MP runtime.
///
/// The rules are the historical ones, with the `#[cfg]` dispatch they used to
/// sit behind replaced by an explicit platform argument: every rule is now
/// exercised by the test suite on every host.
pub fn select_tes3mp_asset(
    release: &GithubReleaseResponse,
    platform: TargetPlatform,
) -> Result<&GithubAssetResponse, String> {
    let release_id = release.id.to_string();
    let selected = match platform {
        TargetPlatform::Windows => select_windows_asset(&release.assets, &release_id),
        TargetPlatform::Linux => select_linux_asset(&release.assets, &release_id),
        TargetPlatform::MacOs => Err(format!(
            "No supported release asset exists for this platform (release {release_id})"
        )),
    };

    // A custom repository is the reason this matters: the rules are written
    // for the official asset names, so a fork that names its builds
    // differently fails here. Naming what the release *did* hold is what
    // separates "this fork names its assets differently" from "this release
    // has no builds attached".
    selected.map_err(|error| format!("{error}. {}", describe_assets(&release.assets)))
}

/// The tail of a no-matching-asset error: what the release actually offered.
fn describe_assets(assets: &[GithubAssetResponse]) -> String {
    if assets.is_empty() {
        return "This release has no assets attached.".to_string();
    }
    let names: Vec<&str> = assets.iter().map(|asset| asset.name.as_str()).collect();
    format!("Assets in this release: {}", names.join(", "))
}

/// Looks up an asset by its exact file name — the path taken when a caller
/// pinned `RuntimeSource::GithubRelease::asset_name` instead of leaving the
/// choice to `select_tes3mp_asset`.
pub fn find_asset_by_name<'a>(
    release: &'a GithubReleaseResponse,
    asset_name: &str,
) -> Result<&'a GithubAssetResponse, String> {
    release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)
        .ok_or_else(|| format!("Release {} has no asset named {asset_name}", release.id))
}

/// Selection rule for Windows: the Win64 zip asset.
fn select_windows_asset<'a>(
    assets: &'a [GithubAssetResponse],
    release_id: &str,
) -> Result<&'a GithubAssetResponse, String> {
    assets
        .iter()
        .find(|asset| asset.name.contains("Win64") && asset.name.ends_with(".zip"))
        .ok_or_else(|| format!("No Windows zip asset found for release {release_id}"))
}

/// Selection rule for Linux: the full client+server x86_64 tarball,
/// explicitly excluding the server-only tarball and other architectures
/// (e.g. armv7l).
fn select_linux_asset<'a>(
    assets: &'a [GithubAssetResponse],
    release_id: &str,
) -> Result<&'a GithubAssetResponse, String> {
    assets
        .iter()
        .find(|asset| {
            asset.name.contains("GNU+Linux")
                && asset.name.contains("x86_64")
                && asset.name.ends_with(".tar.gz")
                && !asset.name.starts_with("tes3mp-server")
        })
        .ok_or_else(|| format!("No Linux x86_64 tar.gz asset found for release {release_id}"))
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::*;

    pub fn asset(name: &str) -> GithubAssetResponse {
        GithubAssetResponse {
            url: String::new(),
            id: 0,
            node_id: String::new(),
            name: name.to_string(),
            label: None,
            content_type: String::new(),
            state: String::new(),
            size: 0,
            download_count: 0,
            created_at: String::new(),
            updated_at: String::new(),
            browser_download_url: String::new(),
        }
    }

    pub fn release(id: u64, assets: Vec<GithubAssetResponse>) -> GithubReleaseResponse {
        GithubReleaseResponse {
            url: String::new(),
            assets_url: String::new(),
            upload_url: String::new(),
            html_url: String::new(),
            id,
            node_id: String::new(),
            tag_name: String::new(),
            target_commitish: String::new(),
            name: String::new(),
            draft: false,
            prerelease: false,
            created_at: String::new(),
            published_at: String::new(),
            assets,
            tarball_url: String::new(),
            zipball_url: String::new(),
            body: String::new(),
        }
    }

    pub fn release_081() -> GithubReleaseResponse {
        release(
            123,
            vec![
                asset("tes3mp-GNU+Linux-x86_64-release-0.8.1-68954091c5-6da3fdea59.tar.gz"),
                asset("tes3mp-server-GNU+Linux-armv7l-release-0.8.1-37a4b2a103-096b6f1687.tar.gz"),
                asset("tes3mp-server-GNU+Linux-x86_64-release-0.8.1-68954091c5-6da3fdea59.tar.gz"),
                asset("tes3mp.Win64.release.0.8.1.zip"),
            ],
        )
    }

    pub fn release_080() -> GithubReleaseResponse {
        release(
            123,
            vec![
                asset("tes3mp-GNU+Linux-x86_64-release-0.8.0-6b1c83f629-14d7382e1e.tar.gz"),
                asset("tes3mp-server-GNU+Linux-x86_64-release-0.8.0-6b1c83f629-14d7382e1e.tar.gz"),
                asset("tes3mp.Win64.release.0.8.0.zip"),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::*;
    use super::*;

    #[test]
    fn windows_rule_picks_win64_zip() {
        let release = release_081();
        let selected =
            select_tes3mp_asset(&release, TargetPlatform::Windows).expect("should find asset");
        assert_eq!(selected.name, "tes3mp.Win64.release.0.8.1.zip");
    }

    #[test]
    fn linux_rule_picks_client_tarball_081() {
        let release = release_081();
        let selected =
            select_tes3mp_asset(&release, TargetPlatform::Linux).expect("should find asset");
        assert_eq!(
            selected.name,
            "tes3mp-GNU+Linux-x86_64-release-0.8.1-68954091c5-6da3fdea59.tar.gz"
        );
    }

    #[test]
    fn linux_rule_picks_client_tarball_080() {
        let release = release_080();
        let selected =
            select_tes3mp_asset(&release, TargetPlatform::Linux).expect("should find asset");
        assert_eq!(
            selected.name,
            "tes3mp-GNU+Linux-x86_64-release-0.8.0-6b1c83f629-14d7382e1e.tar.gz"
        );
    }

    #[test]
    fn linux_rule_errors_on_windows_only_assets() {
        let release = release(123, vec![asset("tes3mp.Win64.release.0.8.1.zip")]);
        let result = select_tes3mp_asset(&release, TargetPlatform::Linux);
        assert!(result.is_err());
        assert!(result
            .err()
            .unwrap()
            .contains("No Linux x86_64 tar.gz asset found for release 123"));
    }

    #[test]
    fn windows_rule_errors_on_linux_only_assets() {
        let release = release(
            123,
            vec![asset(
                "tes3mp-GNU+Linux-x86_64-release-0.8.1-68954091c5-6da3fdea59.tar.gz",
            )],
        );
        let result = select_tes3mp_asset(&release, TargetPlatform::Windows);
        assert!(result
            .err()
            .unwrap()
            .contains("No Windows zip asset found for release 123"));
    }

    #[test]
    fn macos_has_no_asset() {
        let release = release_081();
        let result = select_tes3mp_asset(&release, TargetPlatform::MacOs);
        assert!(result
            .err()
            .unwrap()
            .contains("No supported release asset exists for this platform (release 123)"));
    }

    /// A fork that publishes its runtime under its own asset names fails the
    /// platform rule; the error must show the names so the user can tell a
    /// naming mismatch from an empty release.
    #[test]
    fn a_no_match_error_names_the_assets_that_were_there() {
        let release = release(
            77,
            vec![
                asset("runtime-linux-x86_64.7z"),
                asset("runtime-windows-x64.7z"),
            ],
        );

        for platform in [
            TargetPlatform::Windows,
            TargetPlatform::Linux,
            TargetPlatform::MacOs,
        ] {
            let error = select_tes3mp_asset(&release, platform)
                .err()
                .expect("no rule can match these names");
            assert!(
                error.contains(
                    "Assets in this release: runtime-linux-x86_64.7z, runtime-windows-x64.7z"
                ),
                "{platform:?} error did not name the assets: {error}"
            );
        }
    }

    #[test]
    fn a_release_with_no_assets_says_so() {
        let release = release(78, vec![]);
        let error = select_tes3mp_asset(&release, TargetPlatform::Linux)
            .err()
            .expect("nothing to select");
        assert!(
            error.contains("This release has no assets attached."),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn pinned_asset_name_wins_over_the_platform_rule() {
        let release = release_081();
        let selected = find_asset_by_name(
            &release,
            "tes3mp-server-GNU+Linux-x86_64-release-0.8.1-68954091c5-6da3fdea59.tar.gz",
        )
        .expect("should find asset");
        assert!(selected.name.starts_with("tes3mp-server"));

        assert!(find_asset_by_name(&release, "nope.zip")
            .err()
            .unwrap()
            .contains("has no asset named nope.zip"));
    }
}
