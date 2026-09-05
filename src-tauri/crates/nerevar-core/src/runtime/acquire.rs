//! Installing a TES3MP runtime into an instance's `tes3mp/` directory.

use std::fs::File;
use std::io::{copy, Read};
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
/// Every source installs *into* `dest`: a downloaded or on-disk archive is
/// extracted there, a local directory has its contents copied there. Nothing
/// is referenced in place, because Nerevar patches the cfgs and
/// `requiredDataFiles.json` inside the install and deletes it with the
/// instance.
///
/// A downloaded archive is streamed to a temporary file next to `dest` rather
/// than buffered in memory (the releases are 60–90MB), progress is emitted
/// through the standard background-operation events, and the temporary file is
/// removed on every exit path. The returned `RuntimeInfo` is the result of
/// inspecting `dest` afterwards; it is the caller's business whether an
/// incomplete runtime is fatal (`RuntimeInfo::require_complete`).
///
/// `operation_id` names the progress stream. A frontend that started the
/// operation passes its own id so the events land on the banner it is already
/// showing; anything else passes `None` and core mints one — no instance
/// exists yet when a runtime is installed, so there is no instance id to use.
pub async fn acquire(
    source: &RuntimeSource,
    dest: &Path,
    platform: TargetPlatform,
    sink: Arc<dyn EventSink>,
    operation_id: Option<String>,
) -> Result<RuntimeInfo, String> {
    let operation_id = operation_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut progress = ProgressEmitter::new(sink, operation_id.clone(), operation_id);

    match source {
        RuntimeSource::GithubRelease {
            repo,
            release_id,
            asset_name,
            ..
        } => {
            acquire_github_release(repo, release_id, asset_name, dest, platform, &mut progress)
                .await?;
        }
        RuntimeSource::LocalDirectory { path } => {
            acquire_local_directory(Path::new(path), dest, &mut progress)?;
        }
        RuntimeSource::Archive { path } => {
            acquire_archive(Path::new(path), dest, &mut progress)?;
        }
    }

    progress.emit(
        BackgroundOperationPhase::InspectingRuntime,
        "Checking the installed runtime",
        0,
        0,
        None,
        true,
    );

    let info = inspect(dest)?;
    for warning in &info.warnings {
        warn!("{warning}");
    }
    info!(
        "Installed TES3MP runtime (OpenMW {}) at {}",
        info.version_display(),
        dest.display()
    );
    // The version goes in the message, not just the log: it is the one piece
    // of the inspection a progress consumer (the banner, a rig reading the
    // event stream) can see without re-inspecting the install.
    progress.emit(
        BackgroundOperationPhase::InspectingRuntime,
        format!("TES3MP runtime ready (OpenMW {})", info.version_display()),
        1,
        1,
        None,
        true,
    );
    Ok(info)
}

async fn acquire_github_release(
    repo: &str,
    release_id: &str,
    asset_name: &str,
    dest: &Path,
    platform: TargetPlatform,
    progress: &mut ProgressEmitter,
) -> Result<(), String> {
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
        progress,
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
        extract_zip(&temp.0, dest, progress)?;
    } else if asset.name.ends_with(".tar.gz") {
        extract_tar_gz(&temp.0, dest)?;
    } else {
        return Err(format!(
            "Unsupported asset archive format for asset {}",
            asset.name
        ));
    }

    info!(
        "Extracted {} ({downloaded} bytes) to {}",
        asset.name,
        dest.display()
    );
    Ok(())
}

/// Copies an already-unpacked runtime into `dest`.
///
/// The *contents* of `src` land in `dest` (the user picked the runtime root,
/// so `dest/tes3mp` is the wrapper, not `dest/<their folder name>/tes3mp`).
///
/// Two refusals happen before a single byte moves, because a wrong pick is
/// cheap to make and expensive to undo once 200MB are in flight:
///
/// 1. `src` and `dest` must not be the same directory, nor may either contain
///    the other — copying a tree into itself never terminates.
/// 2. `src` must inspect as a complete runtime. A directory that is missing
///    the executables or the default cfgs is not a TES3MP install, and the
///    create flow would reject it after the copy anyway.
///
/// Unix mode bits ride along (`std::fs::copy` carries them). Symlinks are
/// recreated as symlinks pointing at the same relative target, provided that
/// target resolves inside `src`: official Linux builds ship ~175 of them
/// (`lib/libfoo.so.6 -> libfoo.so.6.21.2`), and dereferencing those would add
/// ~220MB of duplicated shared libraries to every instance. A symlink that
/// escapes `src`, or one that is broken, aborts the copy rather than silently
/// producing an install that depends on a path outside the instance.
fn acquire_local_directory(
    src: &Path,
    dest: &Path,
    progress: &mut ProgressEmitter,
) -> Result<(), String> {
    if !src.is_dir() {
        return Err(format!(
            "No TES3MP runtime directory at {} — pick the folder that holds the TES3MP install",
            src.display()
        ));
    }

    let src_root = src
        .canonicalize()
        .map_err(|e| format!("Failed to resolve {}: {e}", src.display()))?;
    let dest_resolved = resolve_for_compare(dest);
    if src_root == dest_resolved
        || dest_resolved.starts_with(&src_root)
        || src_root.starts_with(&dest_resolved)
    {
        return Err(format!(
            "Cannot install the runtime from {} into {}: one path contains the other",
            src.display(),
            dest.display()
        ));
    }

    // Fail on the pick, not after the copy.
    inspect(&src_root)?.require_complete()?;

    progress.emit(
        BackgroundOperationPhase::CopyingRuntime,
        format!("Measuring {}", src.display()),
        0,
        0,
        None,
        true,
    );
    let total = measure_tree(&src_root)?;

    std::fs::create_dir_all(dest)
        .map_err(|e| format!("Failed to create {}: {e}", dest.display()))?;

    info!(
        "Copying TES3MP runtime from {} to {} ({total} bytes)",
        src_root.display(),
        dest.display()
    );
    progress.emit(
        BackgroundOperationPhase::CopyingRuntime,
        "Copying the TES3MP runtime",
        0,
        total,
        None,
        true,
    );

    let mut copied = 0u64;
    copy_tree(&src_root, dest, &src_root, total, &mut copied, progress)?;

    progress.emit(
        BackgroundOperationPhase::CopyingRuntime,
        "Copied the TES3MP runtime",
        copied,
        total,
        None,
        true,
    );
    Ok(())
}

/// The path `p` would have if every component that exists were resolved.
/// `Path::canonicalize` needs the whole path to exist; `dest` usually does
/// not yet, and the containment check still has to be exact.
fn resolve_for_compare(p: &Path) -> PathBuf {
    if let Ok(resolved) = p.canonicalize() {
        return resolved;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) => resolve_for_compare(parent).join(name),
        _ => p.to_path_buf(),
    }
}

/// Total bytes of the regular files under `dir`. Symlinks count as zero: the
/// copy recreates them, it does not move their target's bytes.
fn measure_tree(dir: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("Failed to read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read {}: {e}", dir.display()))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("Failed to stat {}: {e}", path.display()))?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            total = total.saturating_add(measure_tree(&path)?);
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    Ok(total)
}

fn copy_tree(
    src: &Path,
    dest: &Path,
    src_root: &Path,
    total: u64,
    copied: &mut u64,
    progress: &mut ProgressEmitter,
) -> Result<(), String> {
    std::fs::create_dir_all(dest)
        .map_err(|e| format!("Failed to create {}: {e}", dest.display()))?;

    let entries =
        std::fs::read_dir(src).map_err(|e| format!("Failed to read {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read {}: {e}", src.display()))?;
        let path = entry.path();
        let target = dest.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("Failed to stat {}: {e}", path.display()))?;

        if meta.file_type().is_symlink() {
            copy_symlink(&path, &target, src_root)?;
        } else if meta.is_dir() {
            copy_tree(&path, &target, src_root, total, copied, progress)?;
        } else {
            std::fs::copy(&path, &target).map_err(|e| {
                format!(
                    "Failed to copy {} to {}: {e}",
                    path.display(),
                    target.display()
                )
            })?;
            *copied = copied.saturating_add(meta.len());
            progress.emit(
                BackgroundOperationPhase::CopyingRuntime,
                "Copying the TES3MP runtime",
                *copied,
                total,
                path.strip_prefix(src_root)
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned()),
                false,
            );
        }
    }
    Ok(())
}

/// Recreates one symlink, refusing any whose target leaves `src_root`.
#[cfg_attr(not(unix), allow(unused_variables))]
fn copy_symlink(src: &Path, dest: &Path, src_root: &Path) -> Result<(), String> {
    let link_target = std::fs::read_link(src)
        .map_err(|e| format!("Failed to read the link {}: {e}", src.display()))?;
    let joined = if link_target.is_absolute() {
        link_target.clone()
    } else {
        src.parent().unwrap_or(src_root).join(&link_target)
    };
    let resolved = joined.canonicalize().map_err(|e| {
        format!(
            "The link {} points at {}, which cannot be resolved: {e}",
            src.display(),
            link_target.display()
        )
    })?;
    if !resolved.starts_with(src_root) {
        return Err(format!(
            "The link {} points outside the runtime, at {} — copy a self-contained install",
            src.display(),
            resolved.display()
        ));
    }

    #[cfg(unix)]
    {
        if dest.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(dest);
        }
        std::os::unix::fs::symlink(&link_target, dest).map_err(|e| {
            format!(
                "Failed to link {} -> {}: {e}",
                dest.display(),
                link_target.display()
            )
        })
    }
    // Windows needs a privilege for symlinks and its TES3MP builds ship none,
    // so the rare link found there is copied as the file it points at.
    #[cfg(not(unix))]
    {
        std::fs::copy(&resolved, dest)
            .map(|_| ())
            .map_err(|e| format!("Failed to copy {} to {}: {e}", resolved.display(), dest.display()))
    }
}

/// Which archive container a file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArchiveFormat {
    Zip,
    TarGz,
}

/// The archive format of `path`: the extension decides when it is one we
/// publish (`.zip`, `.tar.gz`, `.tgz`), and the magic bytes decide otherwise —
/// a release renamed on the way to disk is still the archive it was.
pub(crate) fn detect_archive_format(path: &Path) -> Result<ArchiveFormat, String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.ends_with(".zip") {
        return Ok(ArchiveFormat::Zip);
    }
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        return Ok(ArchiveFormat::TarGz);
    }

    let mut file =
        File::open(path).map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
    let mut magic = [0u8; 4];
    let read = read_up_to(&mut file, &mut magic)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    if read >= 4 && magic == [b'P', b'K', 0x03, 0x04] {
        return Ok(ArchiveFormat::Zip);
    }
    if read >= 2 && magic[0] == 0x1f && magic[1] == 0x8b {
        return Ok(ArchiveFormat::TarGz);
    }

    Err(format!(
        "{} is not a TES3MP release archive — expected a .zip or .tar.gz",
        path.display()
    ))
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Extracts a release archive that is already on disk into `dest`.
fn acquire_archive(
    archive: &Path,
    dest: &Path,
    progress: &mut ProgressEmitter,
) -> Result<(), String> {
    if !archive.is_file() {
        return Err(format!("No archive at {}", archive.display()));
    }
    let format = detect_archive_format(archive)?;

    std::fs::create_dir_all(dest)
        .map_err(|e| format!("Failed to create {}: {e}", dest.display()))?;

    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| archive.display().to_string());
    info!("Extracting {} to {}", archive.display(), dest.display());
    progress.emit(
        BackgroundOperationPhase::ExtractingRuntime,
        format!("Extracting {name}"),
        0,
        0,
        None,
        true,
    );

    match format {
        ArchiveFormat::Zip => extract_zip(archive, dest, progress),
        ArchiveFormat::TarGz => extract_tar_gz(archive, dest),
    }
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

        // Named explicitly rather than left to default, so this is also the
        // one test that drives the custom-repo listing path against the real
        // API — with the official repo as the repository.
        let releases = get_all_releases(Some(DEFAULT_TES3MP_REPO))
            .await
            .expect("failed to fetch releases");
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
            None,
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

    fn exe_name(stem: &str) -> String {
        if cfg!(windows) {
            format!("{stem}.exe")
        } else {
            stem.to_string()
        }
    }

    /// The pieces `inspect` requires, laid out the way an extracted official
    /// release is: a top-level `TES3MP/` under the runtime root.
    fn write_runtime_tree(root: &Path) {
        let install = root.join("TES3MP");
        std::fs::create_dir_all(install.join("server/data")).unwrap();
        std::fs::create_dir_all(install.join("resources")).unwrap();
        std::fs::write(install.join(exe_name("tes3mp")), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(install.join(exe_name("tes3mp-server")), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(install.join("tes3mp-client-default.cfg"), b"[General]\n").unwrap();
        std::fs::write(install.join("tes3mp-server-default.cfg"), b"[General]\n").unwrap();
        std::fs::write(install.join("resources/version"), b"0.48.0\n7f9b\n").unwrap();
        std::fs::write(install.join("server/data/placeholder"), b"x").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for stem in ["tes3mp", "tes3mp-server"] {
                std::fs::set_permissions(
                    install.join(stem),
                    std::fs::Permissions::from_mode(0o755),
                )
                .unwrap();
            }
        }
    }

    fn source_dir(path: &Path) -> RuntimeSource {
        RuntimeSource::LocalDirectory {
            path: path.to_string_lossy().into_owned(),
        }
    }

    fn source_archive(path: &Path) -> RuntimeSource {
        RuntimeSource::Archive {
            path: path.to_string_lossy().into_owned(),
        }
    }

    async fn run_acquire(source: &RuntimeSource, dest: &Path) -> Result<RuntimeInfo, String> {
        acquire(
            source,
            dest,
            TargetPlatform::current(),
            Arc::new(CollectingEventSink::default()),
            Some("test-op".to_string()),
        )
        .await
    }

    #[tokio::test]
    async fn local_directory_copy_keeps_the_layout_and_the_exec_bit() {
        let scratch = scratch("copy-tree");
        let src = scratch.0.join("MundusPatensMP");
        let dest = scratch.0.join("instance/tes3mp");
        write_runtime_tree(&src);

        let info = run_acquire(&source_dir(&src), &dest)
            .await
            .expect("acquire");
        info.require_complete().expect("complete runtime");

        // The *contents* of the picked directory land in dest.
        let install = dest.join("TES3MP");
        assert!(install.join("tes3mp-client-default.cfg").is_file());
        assert!(install.join("server/data").is_dir());
        assert_eq!(info.openmw_version, Some((0, 48)));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(install.join("tes3mp-server"))
                .unwrap()
                .permissions()
                .mode();
            assert!(mode & 0o111 != 0, "copied server is not executable ({mode:o})");
        }
    }

    /// The refusal that matters most: a directory that is not a runtime is
    /// rejected on the pick, before anything is copied.
    #[tokio::test]
    async fn a_directory_that_is_not_a_runtime_is_refused_before_copying() {
        let scratch = scratch("copy-not-runtime");
        let src = scratch.0.join("Downloads");
        let dest = scratch.0.join("instance/tes3mp");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("readme.txt"), b"not a runtime").unwrap();

        let err = run_acquire(&source_dir(&src), &dest)
            .await
            .expect_err("a plain folder is not a runtime");
        assert!(err.contains("incomplete"), "{err}");
        assert!(!dest.exists(), "the destination was created anyway");
    }

    #[tokio::test]
    async fn nested_source_and_destination_are_refused() {
        let scratch = scratch("copy-nested");
        let outer = scratch.0.join("runtime");
        write_runtime_tree(&outer);

        // dest inside src
        let inside = outer.join("tes3mp");
        let err = run_acquire(&source_dir(&outer), &inside)
            .await
            .expect_err("dest inside src");
        assert!(err.contains("contains the other"), "{err}");

        // src inside dest
        let err = run_acquire(&source_dir(&outer.join("TES3MP")), &outer)
            .await
            .expect_err("src inside dest");
        assert!(err.contains("contains the other"), "{err}");

        // src == dest
        let err = run_acquire(&source_dir(&outer), &outer)
            .await
            .expect_err("src is dest");
        assert!(err.contains("contains the other"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_inside_the_tree_stay_symlinks() {
        let scratch = scratch("copy-symlink");
        let src = scratch.0.join("runtime");
        let dest = scratch.0.join("instance/tes3mp");
        write_runtime_tree(&src);
        let lib = src.join("TES3MP/lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("libfoo.so.6.21.2"), b"binary").unwrap();
        std::os::unix::fs::symlink("libfoo.so.6.21.2", lib.join("libfoo.so.6")).unwrap();

        run_acquire(&source_dir(&src), &dest).await.expect("acquire");

        let link = dest.join("TES3MP/lib/libfoo.so.6");
        assert!(
            std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
            "the copy dereferenced the link"
        );
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("libfoo.so.6.21.2")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_leaving_the_tree_aborts_the_copy() {
        let scratch = scratch("copy-escape");
        let src = scratch.0.join("runtime");
        let dest = scratch.0.join("instance/tes3mp");
        let outside = scratch.0.join("outside.so");
        write_runtime_tree(&src);
        std::fs::write(&outside, b"elsewhere").unwrap();
        std::os::unix::fs::symlink(&outside, src.join("TES3MP/escape.so")).unwrap();

        let err = run_acquire(&source_dir(&src), &dest)
            .await
            .expect_err("an escaping link must abort");
        assert!(err.contains("points outside the runtime"), "{err}");
    }

    /// Both archive formats install and inspect the same way the copy does.
    #[tokio::test]
    async fn archive_sources_extract_and_inspect() {
        let scratch = scratch("archive-formats");
        let tree = scratch.0.join("runtime");
        write_runtime_tree(&tree);

        let zip_path = scratch.0.join("release.zip");
        write_zip_of_tree(&tree, &zip_path);
        let tgz_path = scratch.0.join("release.tar.gz");
        write_tar_gz_of_tree(&tree, &tgz_path);

        for (archive, label) in [(zip_path, "zip"), (tgz_path, "tar.gz")] {
            let dest = scratch.0.join(format!("dest-{label}"));
            let info = run_acquire(&source_archive(&archive), &dest)
                .await
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            info.require_complete()
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            assert_eq!(info.openmw_version, Some((0, 48)), "{label}");
        }
    }

    /// A `.tgz` and an extension-less archive are recognized: the extension
    /// decides first, the magic bytes decide when it says nothing.
    #[tokio::test]
    async fn archive_format_falls_back_to_the_magic_bytes() {
        let scratch = scratch("archive-magic");
        let tree = scratch.0.join("runtime");
        write_runtime_tree(&tree);

        let tgz = scratch.0.join("release.tgz");
        write_tar_gz_of_tree(&tree, &tgz);
        assert_eq!(detect_archive_format(&tgz).unwrap(), ArchiveFormat::TarGz);

        let nameless = scratch.0.join("download.bin");
        std::fs::copy(&tgz, &nameless).unwrap();
        assert_eq!(
            detect_archive_format(&nameless).unwrap(),
            ArchiveFormat::TarGz
        );

        let zip = scratch.0.join("release.zip");
        write_zip_of_tree(&tree, &zip);
        let renamed = scratch.0.join("download2.bin");
        std::fs::copy(&zip, &renamed).unwrap();
        assert_eq!(detect_archive_format(&renamed).unwrap(), ArchiveFormat::Zip);
    }

    #[tokio::test]
    async fn a_file_that_is_not_an_archive_errors_cleanly() {
        let scratch = scratch("archive-bogus");
        let bogus = scratch.0.join("notes.txt");
        std::fs::write(&bogus, b"this is not an archive at all\n").unwrap();
        let dest = scratch.0.join("dest");

        let err = run_acquire(&source_archive(&bogus), &dest)
            .await
            .expect_err("a text file is not an archive");
        assert!(err.contains("not a TES3MP release archive"), "{err}");

        let missing = scratch.0.join("gone.zip");
        let err = run_acquire(&source_archive(&missing), &dest)
            .await
            .expect_err("a missing archive");
        assert!(err.contains("No archive at"), "{err}");
    }

    fn write_zip_of_tree(tree: &Path, out: &Path) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let mut writer = zip::ZipWriter::new(File::create(out).unwrap());
        for (relative, bytes, mode) in flatten_tree(tree) {
            writer
                .start_file(relative, SimpleFileOptions::default().unix_permissions(mode))
                .unwrap();
            writer.write_all(&bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    fn write_tar_gz_of_tree(tree: &Path, out: &Path) {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let encoder = GzEncoder::new(File::create(out).unwrap(), Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (relative, bytes, mode) in flatten_tree(tree) {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            builder
                .append_data(&mut header, &relative, bytes.as_slice())
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    /// Every regular file under `tree` as `(relative path, contents, mode)`.
    fn flatten_tree(tree: &Path) -> Vec<(String, Vec<u8>, u32)> {
        fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>, u32)>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, out);
                    continue;
                }
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let mode = mode_of(&path);
                out.push((relative, std::fs::read(&path).unwrap(), mode));
            }
        }
        let mut out = Vec::new();
        walk(tree, tree, &mut out);
        out.sort();
        out
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[cfg(not(unix))]
    fn mode_of(_path: &Path) -> u32 {
        0o644
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
