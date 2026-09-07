//! In-app update checking against this fork's GitHub releases.
//!
//! Two outcomes, decided by the platform the app is running on:
//!
//! - **Windows** — the release carries an NSIS installer (`*-setup.exe`), so
//!   Nerevar downloads it, launches it detached, and exits. Nothing else
//!   installs the app there.
//! - **Everywhere else** — the Linux packages (deb/rpm/PKGBUILD, and the
//!   AppImage a player may have downloaded) own the install, so self-updating
//!   would fight the package manager. The check still reports the newer
//!   version; the frontend gets the release page URL and opens it.
//!
//! The platform branch is a value (`AppUpdateAction`) rather than a `cfg!`
//! sprinkled through the code, so both arms are testable on either host.

use std::cmp::Ordering;
use std::path::PathBuf;
// Only the Windows installer path spawns a process (see
// `spawn_installer_detached`); elsewhere in-app updates are refused outright.
#[cfg(windows)]
use std::process::Command;

use log::info;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
#[cfg(not(windows))]
use log::error;

use crate::data::{GithubAssetResponse, GithubReleaseResponse};
use crate::github_getters;

/// The fork's repository. Upstream (`kyaustad/nerevar-rs`) stopped at 0.1.10;
/// this fork's releases are the ones an installed Nerevar tracks.
pub const NEREVAR_REPO: &str = "loragea/nerevar-rs";

/// What the frontend should offer the user for an available update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum AppUpdateAction {
    /// Windows: `download_and_run_nerevar_update` fetches the installer named
    /// by `installerAssetName` and runs it.
    RunInstaller,
    /// Linux/macOS: open `htmlUrl` in a browser. The distro package (or the
    /// AppImage the player downloaded) installs the new version, not Nerevar.
    OpenReleasePage,
}

/// Which install story the running build has. Split out from `cfg!` so the
/// selection logic can be tested for both platforms from one test run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePlatform {
    Windows,
    Other,
}

impl UpdatePlatform {
    /// The platform this binary was built for.
    pub fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AppUpdateRelease {
    pub id: u64,
    pub tag_name: String,
    pub name: String,
    pub version: String,
    pub body: String,
    pub published_at: String,
    pub html_url: String,
    /// What the frontend should do with this release.
    pub action: AppUpdateAction,
    /// The Windows installer asset, when `action` is `runInstaller`. `None`
    /// on every other platform — and on Windows when the release published no
    /// installer, which downgrades the action to `openReleasePage` rather than
    /// failing the whole check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer_asset_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer_download_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AppUpdateStatus {
    pub current_version: String,
    pub update_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_release: Option<AppUpdateRelease>,
}

pub fn current_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

pub async fn check_for_update() -> Result<AppUpdateStatus, String> {
    let current_version = current_app_version();
    let releases = github_getters::get_nerevar_releases(NEREVAR_REPO).await?;

    let Some(latest) = find_latest_release(&releases) else {
        return Ok(AppUpdateStatus {
            current_version,
            update_available: false,
            latest_release: None,
        });
    };

    let latest_version = release_version(latest)
        .ok_or_else(|| format!("Could not parse version for release {}", latest.tag_name))?;
    let update_available = version_compare(&latest_version, &current_version) == Ordering::Greater;

    let latest_release = if update_available {
        Some(to_update_release(latest, UpdatePlatform::host())?)
    } else {
        None
    };

    Ok(AppUpdateStatus {
        current_version,
        update_available,
        latest_release,
    })
}

pub async fn download_and_run_installer(release_id: u64) -> Result<(), String> {
    // Refuse before spending a download: on Linux and macOS the packages own
    // the install and there is nothing here that could run an installer.
    if UpdatePlatform::host() != UpdatePlatform::Windows {
        return Err(
            "In-app installation is Windows-only — update through your package manager, or \
             download the new release from GitHub."
                .into(),
        );
    }

    let releases = github_getters::get_nerevar_releases(NEREVAR_REPO).await?;
    let release = releases
        .iter()
        .find(|release| release.id == release_id)
        .ok_or_else(|| format!("Release {release_id} not found"))?;

    let asset = find_installer_asset(release)
        .ok_or_else(|| format!("No Windows installer (.exe) found for release {release_id}"))?;

    info!(
        "Downloading Nerevar update {} ({})",
        release.tag_name, asset.name
    );

    let client = Client::new();
    let bytes = client
        .get(&asset.browser_download_url)
        .header("User-Agent", format!("Nerevar-{}", current_app_version()))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;

    let temp_dir = std::env::temp_dir().join(format!("nerevar-update-{release_id}"));
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
    let installer_path: PathBuf = temp_dir.join(&asset.name);
    std::fs::write(&installer_path, bytes).map_err(|e| e.to_string())?;

    spawn_installer_detached(&installer_path)?;
    info!(
        "Launched installer at {} and exiting Nerevar",
        installer_path.display()
    );
    Ok(())
}

fn to_update_release(
    release: &GithubReleaseResponse,
    platform: UpdatePlatform,
) -> Result<AppUpdateRelease, String> {
    let version = release_version(release)
        .ok_or_else(|| format!("Could not parse version for release {}", release.tag_name))?;

    // Only Windows has an in-app install path, and only when the release
    // actually published an installer. A release with no matching asset falls
    // back to the release page instead of failing the update check outright —
    // a player being told "an update exists, here it is" beats an error.
    let installer = match platform {
        UpdatePlatform::Windows => find_installer_asset(release),
        UpdatePlatform::Other => None,
    };

    let (action, installer_asset_name, installer_download_url) = match installer {
        Some(asset) => (
            AppUpdateAction::RunInstaller,
            Some(asset.name.clone()),
            Some(asset.browser_download_url.clone()),
        ),
        None => (AppUpdateAction::OpenReleasePage, None, None),
    };

    Ok(AppUpdateRelease {
        id: release.id,
        tag_name: release.tag_name.clone(),
        name: release.name.clone(),
        version,
        body: release.body.clone(),
        published_at: release.published_at.clone(),
        html_url: release.html_url.clone(),
        action,
        installer_asset_name,
        installer_download_url,
    })
}

fn find_latest_release(releases: &[GithubReleaseResponse]) -> Option<&GithubReleaseResponse> {
    releases
        .iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| release_version(release).map(|version| (release, version)))
        .max_by(|(_, left), (_, right)| version_compare(left, right))
        .map(|(release, _)| release)
}

/// Picks the Windows installer out of a release's assets.
///
/// The release workflow publishes both Windows bundles Tauri produces:
/// `Nerevar_<version>_x64-setup.exe` (NSIS) and
/// `Nerevar_<version>_x64_en-US.msi` (WiX). Only the `.exe` is selected: an
/// `.msi` is data for `msiexec`, not something `CreateProcess` can run, so
/// launching one detached would silently do nothing.
///
/// Ranked rather than first-match so an `-x64-setup.exe` always wins over some
/// other `.exe` that merely happens to have `x64` in its name.
fn find_installer_asset(release: &GithubReleaseResponse) -> Option<&GithubAssetResponse> {
    release
        .assets
        .iter()
        .filter_map(|asset| installer_rank(&asset.name).map(|rank| (rank, asset)))
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, asset)| asset)
}

/// Lower is better; `None` means "not a Windows installer".
fn installer_rank(asset_name: &str) -> Option<u8> {
    let name = asset_name.to_ascii_lowercase();
    if !name.ends_with(".exe") {
        return None;
    }
    if name.contains("setup") {
        Some(0)
    } else if name.contains("installer") {
        Some(1)
    } else if name.contains("x64") || name.contains("x86_64") {
        Some(2)
    } else {
        None
    }
}

fn release_version(release: &GithubReleaseResponse) -> Option<String> {
    parse_semver_from_tag(&release.tag_name).or_else(|| parse_semver_from_name(&release.name))
}

fn parse_semver_from_tag(tag: &str) -> Option<String> {
    let trimmed = tag.trim().trim_start_matches(['v', 'V']);
    let core = trimmed.split('-').next()?.trim();
    parse_version(core).map(|_| core.to_string())
}

fn parse_semver_from_name(name: &str) -> Option<String> {
    for token in name.split_whitespace() {
        if let Some(version) = parse_semver_from_tag(token) {
            return Some(version);
        }
    }
    None
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.split('.');
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ))
}

fn version_compare(left: &str, right: &str) -> Ordering {
    match (parse_version(left), parse_version(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        _ => Ordering::Equal,
    }
}

// `path` is Windows-only by construction: the not(windows) arm refuses the
// operation outright, so a Linux/macOS build sees it as unused.
#[cfg_attr(not(windows), allow(unused_variables))]
fn spawn_installer_detached(path: &PathBuf) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;

        Command::new(path)
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(|e| format!("Failed to launch installer: {e}"))?;
        return Ok(());
    }

    #[cfg(not(windows))]
    {
        error!("Nerevar in-app updates are only supported on Windows");
        Err("In-app updates are only supported on Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> GithubAssetResponse {
        GithubAssetResponse {
            name: name.to_string(),
            browser_download_url: format!("https://example.invalid/{name}"),
            ..Default::default()
        }
    }

    fn release(tag: &str, assets: Vec<GithubAssetResponse>) -> GithubReleaseResponse {
        GithubReleaseResponse {
            id: 7,
            tag_name: tag.to_string(),
            name: format!("Nerevar {tag}"),
            html_url: format!("https://github.com/{NEREVAR_REPO}/releases/tag/{tag}"),
            assets,
            ..Default::default()
        }
    }

    /// The release workflow's actual Windows asset names, plus the Linux ones
    /// that share the release.
    fn release_assets() -> Vec<GithubAssetResponse> {
        vec![
            asset("Nerevar_0.2.0_amd64.deb"),
            asset("Nerevar-0.2.0-1.x86_64.rpm"),
            asset("Nerevar_0.2.0_amd64.AppImage"),
            asset("nerevar-host_0.2.0_amd64.deb"),
            asset("Nerevar_0.2.0_x64_en-US.msi"),
            asset("Nerevar_0.2.0_x64-setup.exe"),
        ]
    }

    #[test]
    fn compares_semver_versions() {
        assert_eq!(version_compare("0.1.4", "0.1.3"), Ordering::Greater);
        assert_eq!(version_compare("0.1.4", "0.1.4"), Ordering::Equal);
        assert_eq!(version_compare("0.1.3", "0.2.0"), Ordering::Less);
        // The fork's first line is ahead of everything upstream shipped.
        assert_eq!(version_compare("0.2.0", "0.1.10"), Ordering::Greater);
    }

    #[test]
    fn parses_version_from_tag_and_name() {
        assert_eq!(parse_semver_from_tag("v0.1.4"), Some("0.1.4".into()));
        assert_eq!(
            parse_semver_from_name("Nerevar v0.1.4"),
            Some("0.1.4".into())
        );
    }

    #[test]
    fn picks_the_nsis_installer_out_of_a_full_release() {
        let release = release("v0.2.0", release_assets());
        let asset = find_installer_asset(&release).expect("an installer");
        assert_eq!(asset.name, "Nerevar_0.2.0_x64-setup.exe");
    }

    /// An `.msi` is data for msiexec, not a runnable image — selecting one
    /// would leave the user staring at a window that never opens.
    #[test]
    fn never_selects_the_msi_or_a_linux_artifact() {
        let release = release(
            "v0.2.0",
            vec![
                asset("Nerevar_0.2.0_x64_en-US.msi"),
                asset("Nerevar_0.2.0_amd64.AppImage"),
                asset("sha256sums.txt"),
            ],
        );
        assert!(find_installer_asset(&release).is_none());
    }

    #[test]
    fn prefers_a_setup_exe_over_a_bare_x64_exe() {
        let release = release(
            "v0.2.0",
            vec![
                asset("nerevar_0.2.0_x64.exe"),
                asset("Nerevar_0.2.0_x64-setup.exe"),
            ],
        );
        assert_eq!(
            find_installer_asset(&release).unwrap().name,
            "Nerevar_0.2.0_x64-setup.exe"
        );
    }

    #[test]
    fn windows_gets_the_installer_and_its_download_url() {
        let release = release("v0.2.0", release_assets());
        let update = to_update_release(&release, UpdatePlatform::Windows).unwrap();
        assert_eq!(update.action, AppUpdateAction::RunInstaller);
        assert_eq!(
            update.installer_asset_name.as_deref(),
            Some("Nerevar_0.2.0_x64-setup.exe")
        );
        assert!(update
            .installer_download_url
            .as_deref()
            .unwrap()
            .ends_with("Nerevar_0.2.0_x64-setup.exe"));
    }

    /// Linux packages own the install, so even a release that carries a
    /// perfectly good Windows installer sends a Linux client to the release
    /// page.
    #[test]
    fn other_platforms_get_the_release_page_and_no_installer() {
        let release = release("v0.2.0", release_assets());
        let update = to_update_release(&release, UpdatePlatform::Other).unwrap();
        assert_eq!(update.action, AppUpdateAction::OpenReleasePage);
        assert_eq!(update.installer_asset_name, None);
        assert_eq!(update.installer_download_url, None);
        assert_eq!(
            update.html_url,
            format!("https://github.com/{NEREVAR_REPO}/releases/tag/v0.2.0")
        );
    }

    /// A release with no Windows installer must not fail the check on
    /// Windows; it degrades to the release page.
    #[test]
    fn windows_falls_back_to_the_release_page_when_no_installer_shipped() {
        let release = release("v0.2.0", vec![asset("Nerevar_0.2.0_amd64.deb")]);
        let update = to_update_release(&release, UpdatePlatform::Windows).unwrap();
        assert_eq!(update.action, AppUpdateAction::OpenReleasePage);
        assert_eq!(update.installer_asset_name, None);
    }

    #[test]
    fn the_newest_stable_release_wins_over_drafts_and_prereleases() {
        let mut newer_draft = release("v0.9.0", release_assets());
        newer_draft.draft = true;
        let mut prerelease = release("v0.8.0", release_assets());
        prerelease.prerelease = true;
        let stable = release("v0.2.0", release_assets());
        let older = release("v0.1.10", release_assets());

        let releases = vec![newer_draft, prerelease, stable, older];
        assert_eq!(
            find_latest_release(&releases).map(|r| r.tag_name.as_str()),
            Some("v0.2.0")
        );
    }

    /// The update check points at this fork, not the upstream repository it
    /// was forked from — upstream stopped at 0.1.10 and would never offer one.
    #[test]
    fn the_update_repository_is_the_fork() {
        assert_eq!(NEREVAR_REPO, "loragea/nerevar-rs");
    }
}
