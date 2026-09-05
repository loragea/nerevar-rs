//! Installing a TES3MP runtime into an instance's `tes3mp/` directory.

use std::fs::File;
use std::io::copy;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::read::GzDecoder;
use log::{info, warn};
use reqwest::Client;
use tar::Archive;
use tokio::io::AsyncWriteExt;

use crate::instance_data::{BackgroundOperationPhase, ProgressEmitter};
use crate::reporter::EventSink;

use super::github::{fetch_release_by_id, find_asset_by_name, select_tes3mp_asset};
use super::inspect::{inspect, RuntimeInfo};
use super::source::{RuntimeSource, TargetPlatform};

/// Deletes its path when it goes out of scope, so the half-downloaded
/// archive never survives an early return, an error, or a panic.
struct TempDownload(PathBuf);

impl Drop for TempDownload {
    fn drop(&mut self) {
        if self.0.exists() {
            if let Err(err) = std::fs::remove_file(&self.0) {
                warn!(
                    "Failed to remove temporary download {}: {err}",
                    self.0.display()
                );
            }
        }
    }
}

/// Installs the runtime described by `source` into `dest`, then reports what
/// landed there.
///
/// The archive is streamed to a temporary file next to `dest` rather than
/// buffered in memory (the releases are 60–90MB), progress is emitted through
/// the standard background-operation events, and the temporary file is
/// removed on every exit path. The returned `RuntimeInfo` is the result of
/// inspecting `dest` afterwards; it is the caller's business whether an
/// incomplete runtime is fatal (`RuntimeInfo::require_complete`).
pub async fn acquire(
    source: &RuntimeSource,
    dest: &Path,
    platform: TargetPlatform,
    sink: Arc<dyn EventSink>,
) -> Result<RuntimeInfo, String> {
    match source {
        RuntimeSource::GithubRelease {
            repo,
            release_id,
            asset_name,
            ..
        } => {
            acquire_github_release(repo, release_id, asset_name, dest, platform, sink).await?;
        }
    }

    let info = inspect(dest)?;
    for warning in &info.warnings {
        warn!("{warning}");
    }
    info!(
        "Installed TES3MP runtime (OpenMW {}) at {}",
        info.version_display(),
        dest.display()
    );
    Ok(info)
}

async fn acquire_github_release(
    repo: &str,
    release_id: &str,
    asset_name: &str,
    dest: &Path,
    platform: TargetPlatform,
    sink: Arc<dyn EventSink>,
) -> Result<(), String> {
    // No instance exists yet when a runtime is installed (the create flow
    // persists it only after this succeeds), so the progress stream gets its
    // own operation id rather than an instance's.
    let operation_id = uuid::Uuid::new_v4().to_string();
    let mut progress = ProgressEmitter::new(sink, operation_id.clone(), operation_id);

    progress.emit(
        BackgroundOperationPhase::DownloadingRuntime,
        format!("Looking up release {release_id} in {repo}"),
        0,
        0,
        None,
        true,
    );

    let release = fetch_release_by_id(repo, release_id).await?;
    let asset = if asset_name.is_empty() {
        select_tes3mp_asset(&release, platform)?
    } else {
        find_asset_by_name(&release, asset_name)?
    };

    info!(
        "Downloading {} from release {release_id} ({repo}) to {}",
        asset.name,
        dest.display()
    );

    std::fs::create_dir_all(dest)
        .map_err(|e| format!("Failed to create {}: {e}", dest.display()))?;

    let temp_dir = dest.parent().unwrap_or(dest);
    std::fs::create_dir_all(temp_dir)
        .map_err(|e| format!("Failed to create {}: {e}", temp_dir.display()))?;
    let temp = TempDownload(temp_dir.join(format!(
        ".nerevar-runtime-{}.download",
        uuid::Uuid::new_v4()
    )));

    let downloaded = stream_asset_to_file(
        &asset.browser_download_url,
        &temp.0,
        &asset.name,
        &mut progress,
    )
    .await?;

    progress.emit(
        BackgroundOperationPhase::ExtractingRuntime,
        format!("Extracting {}", asset.name),
        0,
        0,
        None,
        true,
    );

    if asset.name.ends_with(".zip") {
        extract_zip(&temp.0, dest, &mut progress)?;
    } else if asset.name.ends_with(".tar.gz") {
        extract_tar_gz(&temp.0, dest)?;
    } else {
        return Err(format!(
            "Unsupported asset archive format for asset {}",
            asset.name
        ));
    }

    progress.emit(
        BackgroundOperationPhase::InspectingRuntime,
        "Checking the installed runtime",
        0,
        0,
        None,
        true,
    );

    info!(
        "Extracted {} ({downloaded} bytes) to {}",
        asset.name,
        dest.display()
    );
    Ok(())
}

/// Streams `url` into `path`, emitting download progress. Returns the number
/// of bytes written.
async fn stream_asset_to_file(
    url: &str,
    path: &Path,
    asset_name: &str,
    progress: &mut ProgressEmitter,
) -> Result<u64, String> {
    let client = Client::new();
    let mut response = client
        .get(url)
        .header(
            "User-Agent",
            format!("Nerevar-{}", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|e| format!("Failed to download {asset_name}: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Failed to download {asset_name}: HTTP {}",
            response.status()
        ));
    }

    let total = response.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| format!("Failed to create {}: {e}", path.display()))?;

    let mut downloaded = 0u64;
    progress.emit(
        BackgroundOperationPhase::DownloadingRuntime,
        format!("Downloading {asset_name}"),
        0,
        total,
        None,
        true,
    );

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("Failed to read {asset_name}: {e}"))?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        progress.emit(
            BackgroundOperationPhase::DownloadingRuntime,
            format!("Downloading {asset_name}"),
            downloaded,
            total,
            None,
            false,
        );
    }

    file.flush()
        .await
        .map_err(|e| format!("Failed to flush {}: {e}", path.display()))?;
    drop(file);

    if total != 0 && downloaded != total {
        return Err(format!(
            "Incomplete download of {asset_name} (expected {total} bytes, got {downloaded})"
        ));
    }

    progress.emit(
        BackgroundOperationPhase::DownloadingRuntime,
        format!("Downloaded {asset_name}"),
        downloaded,
        total,
        None,
        true,
    );

    Ok(downloaded)
}

/// Extracts a zip archive, restoring unix permission bits on unix.
///
/// The old in-memory extractor dropped them, which is harmless for the
/// Windows release (the only zip TES3MP publishes) but silently produces a
/// non-executable install anywhere else — so the bits are applied wherever
/// the entry records them.
pub(crate) fn extract_zip(
    archive_path: &Path,
    dest: &Path,
    progress: &mut ProgressEmitter,
) -> Result<(), String> {
    let file = File::open(archive_path)
        .map_err(|e| format!("Failed to open {}: {e}", archive_path.display()))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| format!("Failed to read zip archive: {e}"))?;

    let total = archive.len() as u64;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read zip entry {i}: {e}"))?;
        let Some(relative_path) = entry.enclosed_name() else {
            continue;
        };
        let outpath = dest.join(relative_path);

        if entry.name().ends_with('/') {
            std::fs::create_dir_all(&outpath)
                .map_err(|e| format!("Failed to create {}: {e}", outpath.display()))?;
        } else {
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
            }
            let mut outfile = File::create(&outpath)
                .map_err(|e| format!("Failed to create {}: {e}", outpath.display()))?;
            copy(&mut entry, &mut outfile)
                .map_err(|e| format!("Failed to write {}: {e}", outpath.display()))?;
        }

        apply_unix_mode(&entry, &outpath)?;

        progress.emit(
            BackgroundOperationPhase::ExtractingRuntime,
            "Extracting runtime",
            i as u64 + 1,
            total,
            None,
            false,
        );
    }

    Ok(())
}

#[cfg(unix)]
fn apply_unix_mode<R: std::io::Read>(
    entry: &zip::read::ZipFile<'_, R>,
    outpath: &Path,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let Some(mode) = entry.unix_mode() else {
        return Ok(());
    };
    if !outpath.exists() {
        return Ok(());
    }
    std::fs::set_permissions(outpath, std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("Failed to set permissions on {}: {e}", outpath.display()))
}

#[cfg(not(unix))]
fn apply_unix_mode<R: std::io::Read>(
    _entry: &zip::read::ZipFile<'_, R>,
    _outpath: &Path,
) -> Result<(), String> {
    Ok(())
}

fn extract_tar_gz(archive_path: &Path, dest: &Path) -> Result<(), String> {
    let file = File::open(archive_path)
        .map_err(|e| format!("Failed to open {}: {e}", archive_path.display()))?;
    let tar = GzDecoder::new(std::io::BufReader::new(file));
    Archive::new(tar)
        .unpack(dest)
        .map_err(|e| format!("Failed to extract archive: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporter::CollectingEventSink;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("nerevar-acquire-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn emitter() -> ProgressEmitter {
        ProgressEmitter::new(
            Arc::new(CollectingEventSink::default()),
            "test".to_string(),
            "test".to_string(),
        )
    }

    /// A zip with one 0755 entry and one 0644 entry, written the way GitHub's
    /// release archives record modes.
    fn write_zip_with_modes(path: &Path) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let file = File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);

        writer
            .start_file(
                "TES3MP/tes3mp-server",
                SimpleFileOptions::default().unix_permissions(0o755),
            )
            .unwrap();
        writer.write_all(b"#!/bin/sh\nexit 0\n").unwrap();

        writer
            .start_file(
                "TES3MP/tes3mp-server-default.cfg",
                SimpleFileOptions::default().unix_permissions(0o644),
            )
            .unwrap();
        writer.write_all(b"[General]\n").unwrap();

        writer.finish().unwrap();
    }

    #[test]
    fn zip_extraction_keeps_entry_mode_bits() {
        let scratch = scratch("zip-modes");
        let archive = scratch.0.join("release.zip");
        let dest = scratch.0.join("tes3mp");
        write_zip_with_modes(&archive);

        extract_zip(&archive, &dest, &mut emitter()).expect("extract");

        let exe = dest.join("TES3MP/tes3mp-server");
        let cfg = dest.join("TES3MP/tes3mp-server-default.cfg");
        assert!(exe.is_file());
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "[General]\n");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let exe_mode = std::fs::metadata(&exe).unwrap().permissions().mode();
            assert!(
                exe_mode & 0o111 != 0,
                "extracted tes3mp-server is not executable (mode {exe_mode:o})"
            );
            let cfg_mode = std::fs::metadata(&cfg).unwrap().permissions().mode();
            assert_eq!(
                cfg_mode & 0o111,
                0,
                "extracted cfg should not be executable (mode {cfg_mode:o})"
            );
        }
    }

    /// End-to-end check of the real Linux acquisition path: hits the live
    /// GitHub API, downloads the ~58MB tes3mp-0.8.1 Linux tarball, extracts
    /// it, and verifies `inspect` finds a complete runtime with executable
    /// wrapper scripts where the process manager expects them. Ignored by
    /// default because it needs network access and a non-trivial download.
    #[tokio::test]
    #[ignore = "network: downloads ~58MB from GitHub"]
    #[cfg(target_os = "linux")]
    async fn downloads_and_extracts_linux_release_end_to_end() {
        use super::super::source::DEFAULT_TES3MP_REPO;
        use crate::github_getters::get_all_releases;
        use std::os::unix::fs::PermissionsExt;

        let scratch = scratch("e2e");
        let dest = scratch.0.join("tes3mp");

        let releases = get_all_releases().await.expect("failed to fetch releases");
        let release = releases
            .iter()
            .find(|r| r.tag_name == "tes3mp-0.8.1")
            .expect("tes3mp-0.8.1 release not found");

        let source = RuntimeSource::GithubRelease {
            repo: DEFAULT_TES3MP_REPO.to_string(),
            release_id: release.id.to_string(),
            tag: release.tag_name.clone(),
            asset_name: String::new(),
        };

        let info = acquire(
            &source,
            &dest,
            TargetPlatform::Linux,
            Arc::new(CollectingEventSink::default()),
        )
        .await
        .expect("acquire failed");

        info.require_complete().expect("runtime should be complete");
        // The official 0.8.1 build records its OpenMW base in
        // resources/version; which version that is doesn't matter here, only
        // that inspection read it.
        assert!(info.openmw_version.is_some(), "no resources/version found");

        let server_exe = info.server_exe.expect("tes3mp-server wrapper not found");
        let mode = std::fs::metadata(&server_exe)
            .expect("failed to stat tes3mp-server")
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "tes3mp-server is not executable");
        assert!(info.client_exe.is_some(), "tes3mp client wrapper not found");

        // The streamed archive must not survive the acquisition.
        let leftovers: Vec<_> = std::fs::read_dir(&scratch.0)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".download"))
            .collect();
        assert!(leftovers.is_empty(), "temporary download was left behind");
    }

    #[test]
    fn temp_download_is_removed_when_it_goes_out_of_scope() {
        let scratch = scratch("temp-cleanup");
        let path = scratch.0.join("partial.download");
        {
            let temp = TempDownload(path.clone());
            std::fs::write(&temp.0, b"partial").unwrap();
            assert!(path.exists());
        }
        assert!(!path.exists(), "temporary download outlived its guard");
    }
}
