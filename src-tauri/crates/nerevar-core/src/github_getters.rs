use crate::data::{GithubAssetResponse, GithubReleaseResponse};
use flate2::read::GzDecoder;
use reqwest::Client;
use std::fs::File;
use std::io::{copy, Cursor};
use std::path::Path;
use tar::Archive;
use log::{error, info};

pub async fn get_all_releases() -> Result<Vec<GithubReleaseResponse>, String> {
    fetch_github_releases("tes3mp/tes3mp", "Tes3MP").await
}

// Takes `repo` as a parameter (rather than reaching for the app crate's
// `app_update::NEREVAR_REPO` constant, as before the leaf-layer split) so
// this module has no dependency back on app-side code; nerevar-core can't
// depend on the `nerevar` crate that depends on it.
pub async fn get_nerevar_releases(repo: &str) -> Result<Vec<GithubReleaseResponse>, String> {
    fetch_github_releases(repo, "Nerevar").await
}

async fn fetch_github_releases(
    repo: &str,
    label: &str,
) -> Result<Vec<GithubReleaseResponse>, String> {
    info!("Attempting to fetch all {label} releases from GitHub...");
    let client = Client::new();
    let url = format!("https://api.github.com/repos/{repo}/releases");
    let user_agent = format!("Nerevar-{}", env!("CARGO_PKG_VERSION"));

    let response = match client
        .get(url)
        .header("User-Agent", user_agent)
        .send()
        .await
    {
        Ok(res) => res,
        Err(e) => {
            error!("fetch_github_releases request failed: {e}");
            return Err(e.to_string());
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        error!("fetch_github_releases HTTP {status}: {body}");
        return Err(format!("GitHub API returned HTTP {status}: {body}"));
    }

    match response.json::<Vec<GithubReleaseResponse>>().await {
        Ok(releases) => {
            info!("Successfully fetched all {label} releases from GitHub");
            Ok(releases)
        }
        Err(e) => {
            error!("fetch_github_releases JSON parsing failed: {e}");
            Err(e.to_string())
        }
    }
}

pub async fn download_and_extract_release_zip_by_id_to_path(
    release_id: String,
    path: String,
) -> Result<(), String> {
    info!("Downloading release {release_id} to {path}");

    let release_id_u64: u64 = release_id
        .parse()
        .map_err(|_| format!("Invalid release id: {release_id}"))?;

    let all_releases = get_all_releases().await?;
    let desired_release = all_releases
        .iter()
        .find(|release| release.id == release_id_u64)
        .ok_or_else(|| format!("Release with id {release_id} not found"))?;

    let asset = select_tes3mp_asset(&desired_release.assets, &release_id)?;

    let dest = Path::new(&path);
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;

    let client = Client::new();
    let bytes = client
        .get(&asset.browser_download_url)
        .header(
            "User-Agent",
            format!("Nerevar-{}", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;

    if asset.name.ends_with(".zip") {
        let reader = Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;

        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
            let Some(relative_path) = file.enclosed_name() else {
                continue;
            };
            let outpath = dest.join(relative_path);

            if file.name().ends_with('/') {
                std::fs::create_dir_all(&outpath).map_err(|e| e.to_string())?;
            } else {
                if let Some(parent) = outpath.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut outfile = File::create(&outpath).map_err(|e| e.to_string())?;
                copy(&mut file, &mut outfile).map_err(|e| e.to_string())?;
            }
        }
    } else if asset.name.ends_with(".tar.gz") {
        let reader = Cursor::new(bytes);
        let tar = GzDecoder::new(reader);
        let mut archive = Archive::new(tar);
        archive.unpack(dest).map_err(|e| e.to_string())?;
    } else {
        return Err(format!(
            "Unsupported asset archive format for asset {}",
            asset.name
        ));
    }

    info!("Extracted release {release_id} to {}", dest.display());
    Ok(())
}

/// Picks the release asset appropriate for the current platform.
///
/// The selection rules themselves live in `select_windows_asset` and
/// `select_linux_asset` so they can be unit tested regardless of the host
/// OS running the tests; only the platform dispatch below is `cfg`-gated.
#[cfg(windows)]
fn select_tes3mp_asset<'a>(
    assets: &'a [GithubAssetResponse],
    release_id: &str,
) -> Result<&'a GithubAssetResponse, String> {
    select_windows_asset(assets, release_id)
}

#[cfg(target_os = "linux")]
fn select_tes3mp_asset<'a>(
    assets: &'a [GithubAssetResponse],
    release_id: &str,
) -> Result<&'a GithubAssetResponse, String> {
    select_linux_asset(assets, release_id)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn select_tes3mp_asset<'a>(
    _assets: &'a [GithubAssetResponse],
    release_id: &str,
) -> Result<&'a GithubAssetResponse, String> {
    Err(format!(
        "No supported release asset exists for this platform (release {release_id})"
    ))
}

/// Selection rule for Windows: matches the historical behavior of picking
/// the Win64 zip asset.
#[allow(dead_code)]
fn select_windows_asset<'a>(
    assets: &'a [GithubAssetResponse],
    release_id: &str,
) -> Result<&'a GithubAssetResponse, String> {
    assets
        .iter()
        .find(|asset| asset.name.contains("Win64") && asset.name.ends_with(".zip"))
        .ok_or_else(|| format!("No Windows zip asset found for release {release_id}"))
}

/// Selection rule for Linux: picks the full client+server x86_64 tarball,
/// explicitly excluding the server-only tarball and other architectures
/// (e.g. armv7l).
#[allow(dead_code)]
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
        .ok_or_else(|| {
            format!("No Linux x86_64 tar.gz asset found for release {release_id}")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> GithubAssetResponse {
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

    fn assets_081() -> Vec<GithubAssetResponse> {
        vec![
            asset("tes3mp-GNU+Linux-x86_64-release-0.8.1-68954091c5-6da3fdea59.tar.gz"),
            asset("tes3mp-server-GNU+Linux-armv7l-release-0.8.1-37a4b2a103-096b6f1687.tar.gz"),
            asset("tes3mp-server-GNU+Linux-x86_64-release-0.8.1-68954091c5-6da3fdea59.tar.gz"),
            asset("tes3mp.Win64.release.0.8.1.zip"),
        ]
    }

    fn assets_080() -> Vec<GithubAssetResponse> {
        vec![
            asset("tes3mp-GNU+Linux-x86_64-release-0.8.0-6b1c83f629-14d7382e1e.tar.gz"),
            asset("tes3mp-server-GNU+Linux-x86_64-release-0.8.0-6b1c83f629-14d7382e1e.tar.gz"),
            asset("tes3mp.Win64.release.0.8.0.zip"),
        ]
    }

    #[test]
    fn windows_rule_picks_win64_zip() {
        let assets = assets_081();
        let selected = select_windows_asset(&assets, "123").expect("should find asset");
        assert_eq!(selected.name, "tes3mp.Win64.release.0.8.1.zip");
    }

    #[test]
    fn linux_rule_picks_client_tarball_081() {
        let assets = assets_081();
        let selected = select_linux_asset(&assets, "123").expect("should find asset");
        assert_eq!(
            selected.name,
            "tes3mp-GNU+Linux-x86_64-release-0.8.1-68954091c5-6da3fdea59.tar.gz"
        );
    }

    #[test]
    fn linux_rule_picks_client_tarball_080() {
        let assets = assets_080();
        let selected = select_linux_asset(&assets, "123").expect("should find asset");
        assert_eq!(
            selected.name,
            "tes3mp-GNU+Linux-x86_64-release-0.8.0-6b1c83f629-14d7382e1e.tar.gz"
        );
    }

    #[test]
    fn linux_rule_errors_on_windows_only_assets() {
        let assets = vec![asset("tes3mp.Win64.release.0.8.1.zip")];
        let result = select_linux_asset(&assets, "123");
        assert!(result.is_err());
        assert!(result
            .err()
            .unwrap()
            .contains("No Linux x86_64 tar.gz asset found for release 123"));
    }

    /// Test-local copy of `process_manager::spawn::find_executable`.
    /// `process_manager` hasn't moved into nerevar-core yet (that's a later
    /// migration step), and core can't depend back on the app crate, so this
    /// ignored network test carries its own copy of the search rather than
    /// reaching across the crate boundary. Revisit once process_manager
    /// lands in core.
    #[cfg(target_os = "linux")]
    fn find_executable(
        root: &std::path::Path,
        names: &[&str],
        max_depth: u32,
    ) -> Option<std::path::PathBuf> {
        if max_depth == 0 {
            return None;
        }

        let mut best: Option<(usize, std::path::PathBuf)> = None;
        let entries = std::fs::read_dir(root).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let rank = names.iter().position(|name| file_name.eq_ignore_ascii_case(name));
                if let Some(rank) = rank {
                    if best.as_ref().is_none_or(|(best_rank, _)| rank < *best_rank) {
                        best = Some((rank, path));
                    }
                }
            }
        }
        if let Some((_, path)) = best {
            return Some(path);
        }

        if max_depth > 1 {
            let entries = std::fs::read_dir(root).ok()?;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(found) = find_executable(&path, names, max_depth - 1) {
                        return Some(found);
                    }
                }
            }
        }

        None
    }

    /// End-to-end check of the real Linux download path: hits the live GitHub
    /// API, downloads the ~58MB tes3mp-0.8.1 Linux tarball, extracts it, and
    /// verifies the client/server wrapper scripts land where the process
    /// manager expects them and are executable. Ignored by default because it
    /// needs network access and a non-trivial download.
    #[tokio::test]
    #[ignore = "network: downloads ~58MB from GitHub"]
    #[cfg(target_os = "linux")]
    async fn downloads_and_extracts_linux_release_end_to_end() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "nerevar-download-e2e-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let releases = get_all_releases().await.expect("failed to fetch releases");
        let release = releases
            .iter()
            .find(|r| r.tag_name == "tes3mp-0.8.1")
            .expect("tes3mp-0.8.1 release not found");

        download_and_extract_release_zip_by_id_to_path(
            release.id.to_string(),
            dir.to_string_lossy().to_string(),
        )
        .await
        .expect("download and extract failed");

        let server_exe = find_executable(&dir, &["tes3mp-server"], 5)
            .expect("tes3mp-server wrapper not found");
        let mode = std::fs::metadata(&server_exe)
            .expect("failed to stat tes3mp-server")
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "tes3mp-server is not executable");

        find_executable(&dir, &["tes3mp"], 5).expect("tes3mp client wrapper not found");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
