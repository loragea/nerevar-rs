//! Turning an uploaded archive into a staged package.
//!
//! `PUT /admin/packages/{name}` streams the request body to
//! `staging/<name>.part` and then hands it here. Everything below is blocking
//! filesystem and decompression work, so the route runs it on a blocking
//! thread.
//!
//! **The top-level-directory rule.** Mod archives are packed both ways: some
//! hold the package's files at the root (`meshes/`, `thing.esp`), others wrap
//! them in one folder (`Better Bodies/meshes/`, `Better Bodies/thing.esp`).
//! If, after extraction, the archive's root holds *exactly one entry and that
//! entry is a directory*, that directory becomes the package and the wrapper
//! is dropped; in every other case the archive root is the package as-is.
//! Only a single level is ever stripped, so a deliberately nested layout
//! (`Optional/` next to `Core/`) survives untouched.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::instance_data::{scan_package_dir, ProgressEmitter};
use crate::reporter::NullEventSink;
use crate::runtime::acquire::{detect_archive_format, extract_tar_gz, extract_zip, ArchiveFormat};

use super::staging::{
    now_rfc3339, staged_archive_path, staged_package_dir, tree_size_bytes, StagedPackage,
};

/// Extracts `staging/<name>.part` into `staging/<name>/` and describes what
/// landed.
///
/// The archive is deleted whether this succeeds or fails, and a failure
/// leaves no `staging/<name>/` behind: a rejected upload must not look like a
/// staged package to the next `GET /admin/status`.
///
/// `replaces_existing` is the caller's answer (it knows `data/`); it is
/// carried into the record rather than recomputed here.
pub fn stage_uploaded_archive(
    data_dir: &Path,
    name: &str,
    archive_bytes: u64,
    replaces_existing: bool,
    staged_by: &str,
) -> Result<StagedPackage, String> {
    let archive = staged_archive_path(data_dir, name);
    let destination = staged_package_dir(data_dir, name);
    let scratch = extraction_scratch(data_dir, name);

    let outcome = extract_into_place(&archive, &scratch, &destination);
    let _ = std::fs::remove_file(&archive);
    let _ = std::fs::remove_dir_all(&scratch);
    if let Err(error) = outcome {
        let _ = std::fs::remove_dir_all(&destination);
        return Err(error);
    }

    let scanned = scan_package_dir(name, &destination);
    Ok(StagedPackage {
        name: name.to_string(),
        kind: scanned.kind,
        plugins: scanned.plugins,
        replaces_existing,
        archive_bytes,
        extracted_bytes: tree_size_bytes(&destination),
        staged_at: now_rfc3339(),
        staged_by: staged_by.to_string(),
    })
}

/// Where an upload is unpacked before the top-level rule decides what the
/// package actually is. Dot-leading and inside `staging/`, so it is neither a
/// package name nor visible to a data-dir scan.
fn extraction_scratch(data_dir: &Path, name: &str) -> PathBuf {
    staged_archive_path(data_dir, name).with_file_name(format!(".{name}.extract"))
}

fn extract_into_place(archive: &Path, scratch: &Path, destination: &Path) -> Result<(), String> {
    if !archive.is_file() {
        return Err(format!("No uploaded archive at {}", archive.display()));
    }

    let format = detect_archive_format(archive).map_err(|_| {
        "Unsupported upload: expected a .zip or .tar.gz archive (detected by extension, then \
         by magic bytes)"
            .to_string()
    })?;

    let _ = std::fs::remove_dir_all(scratch);
    std::fs::create_dir_all(scratch)
        .map_err(|error| format!("Failed to create {}: {error}", scratch.display()))?;

    match format {
        // `extract_zip` reports progress to a sink; a staged upload has no UI
        // listening, so it goes to the null sink rather than growing a second
        // extractor that differs in its permission handling.
        ArchiveFormat::Zip => extract_zip(
            archive,
            scratch,
            &mut ProgressEmitter::new(
                Arc::new(NullEventSink),
                "admin-staging".to_string(),
                "admin-upload".to_string(),
            ),
        ),
        ArchiveFormat::TarGz => extract_tar_gz(archive, scratch),
    }?;

    let root = package_root(scratch)?;

    let _ = std::fs::remove_dir_all(destination);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create {}: {error}", parent.display()))?;
    }
    std::fs::rename(&root, destination).map_err(|error| {
        format!(
            "Failed to move the extracted package into {}: {error}",
            destination.display()
        )
    })
}

/// Applies the top-level-directory rule documented at the top of this module
/// and returns the directory that is the package.
fn package_root(scratch: &Path) -> Result<PathBuf, String> {
    let entries: Vec<_> = std::fs::read_dir(scratch)
        .map_err(|error| format!("Failed to read the extracted archive: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Failed to read the extracted archive: {error}"))?;

    if entries.is_empty() {
        return Err("The uploaded archive is empty".to_string());
    }

    if entries.len() == 1 {
        let only = entries[0].path();
        if only.is_dir() {
            return Ok(only);
        }
    }

    Ok(scratch.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::staging::staging_dir;
    use crate::instance_data::PackageKind;
    use std::io::Write;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-upload-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// Writes a zip whose entry names are exactly `files`, at the archive
    /// root — the caller supplies any wrapper directory in the names.
    fn write_zip(path: &Path, files: &[(&str, &[u8])]) {
        use zip::write::SimpleFileOptions;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (name, bytes) in files {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    fn write_tar_gz(path: &Path, files: &[(&str, &[u8])]) {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(path).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    fn stage(data_dir: &Path, name: &str) -> Result<StagedPackage, String> {
        stage_uploaded_archive(data_dir, name, 1234, false, "ada")
    }

    #[test]
    fn a_single_wrapper_directory_is_stripped() {
        let scratch = scratch("wrapper");
        write_zip(
            &staged_archive_path(&scratch.0, "Better Bodies"),
            &[
                ("Better Bodies/thing.esp", b"TES3"),
                ("Better Bodies/meshes/a.nif", b"nif"),
            ],
        );

        let staged = stage(&scratch.0, "Better Bodies").unwrap();
        let dir = staged_package_dir(&scratch.0, "Better Bodies");
        assert!(dir.join("thing.esp").is_file(), "wrapper should be gone");
        assert!(dir.join("meshes/a.nif").is_file());
        assert_eq!(staged.kind, PackageKind::Mod);
        assert_eq!(staged.plugins, vec!["thing.esp".to_string()]);
        assert_eq!(staged.archive_bytes, 1234);
        assert_eq!(staged.extracted_bytes, 7);
        // The archive is gone once it has been unpacked.
        assert!(!staged_archive_path(&scratch.0, "Better Bodies").exists());
    }

    #[test]
    fn a_root_level_archive_is_taken_as_is() {
        let scratch = scratch("root");
        write_zip(
            &staged_archive_path(&scratch.0, "Rock Replacer"),
            &[("textures/rock.dds", b"dds"), ("readme.txt", b"hi")],
        );

        let staged = stage(&scratch.0, "Rock Replacer").unwrap();
        let dir = staged_package_dir(&scratch.0, "Rock Replacer");
        assert!(dir.join("textures/rock.dds").is_file());
        assert!(dir.join("readme.txt").is_file());
        assert_eq!(staged.kind, PackageKind::Replacer);
        assert!(staged.plugins.is_empty());
    }

    /// Only one level comes off: two top-level directories are the package's
    /// own layout, not a wrapper.
    #[test]
    fn two_top_level_directories_are_kept() {
        let scratch = scratch("two-dirs");
        write_zip(
            &staged_archive_path(&scratch.0, "Big Mod"),
            &[("Core/core.esp", b"TES3"), ("Optional/extra.esp", b"TES3")],
        );

        stage(&scratch.0, "Big Mod").unwrap();
        let dir = staged_package_dir(&scratch.0, "Big Mod");
        assert!(dir.join("Core/core.esp").is_file());
        assert!(dir.join("Optional/extra.esp").is_file());
    }

    /// A wrapper plus a loose sibling file is not a wrapper.
    #[test]
    fn a_directory_next_to_a_root_file_is_kept() {
        let scratch = scratch("dir-and-file");
        write_zip(
            &staged_archive_path(&scratch.0, "Mixed"),
            &[("Data Files/x.esp", b"TES3"), ("readme.txt", b"hi")],
        );

        stage(&scratch.0, "Mixed").unwrap();
        let dir = staged_package_dir(&scratch.0, "Mixed");
        assert!(dir.join("Data Files/x.esp").is_file());
        assert!(dir.join("readme.txt").is_file());
    }

    #[test]
    fn tar_gz_uploads_extract_the_same_way() {
        let scratch = scratch("targz");
        write_tar_gz(
            &staged_archive_path(&scratch.0, "Tar Mod"),
            &[("Tar Mod/tar.esp", b"TES3")],
        );

        let staged = stage(&scratch.0, "Tar Mod").unwrap();
        assert!(staged_package_dir(&scratch.0, "Tar Mod")
            .join("tar.esp")
            .is_file());
        assert_eq!(staged.plugins, vec!["tar.esp".to_string()]);
    }

    #[test]
    fn a_second_upload_of_the_same_name_replaces_the_staged_tree() {
        let scratch = scratch("replace");
        write_zip(
            &staged_archive_path(&scratch.0, "Mod"),
            &[("first.esp", b"TES3")],
        );
        stage(&scratch.0, "Mod").unwrap();

        write_zip(
            &staged_archive_path(&scratch.0, "Mod"),
            &[("second.esp", b"TES3")],
        );
        let staged = stage(&scratch.0, "Mod").unwrap();
        let dir = staged_package_dir(&scratch.0, "Mod");
        assert!(!dir.join("first.esp").exists(), "the old tree must be gone");
        assert!(dir.join("second.esp").is_file());
        assert_eq!(staged.plugins, vec!["second.esp".to_string()]);
    }

    #[test]
    fn a_body_that_is_not_an_archive_leaves_nothing_behind() {
        let scratch = scratch("garbage");
        let archive = staged_archive_path(&scratch.0, "Junk");
        std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
        std::fs::write(&archive, b"this is not an archive at all").unwrap();

        let error = stage(&scratch.0, "Junk").unwrap_err();
        assert!(error.contains(".zip"), "{error}");
        assert!(!archive.exists(), "the .part file must be cleaned up");
        assert!(!staged_package_dir(&scratch.0, "Junk").exists());
        // Nothing but the staging directory itself survives.
        let leftovers: Vec<_> = std::fs::read_dir(staging_dir(&scratch.0))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert!(leftovers.is_empty(), "unexpected leftovers: {leftovers:?}");
    }

    /// An archive that unpacks to nothing is refused rather than staged as an
    /// empty package that apply would then install.
    #[test]
    fn an_archive_with_no_entries_is_refused() {
        let scratch = scratch("empty");
        write_tar_gz(&staged_archive_path(&scratch.0, "Nothing"), &[]);
        let error = stage(&scratch.0, "Nothing").unwrap_err();
        assert!(error.contains("empty"), "{error}");
        assert!(!staged_package_dir(&scratch.0, "Nothing").exists());
    }

    /// An upload is stored as `<name>.part`, so its format is always decided
    /// by the magic bytes and never by a file extension. A zip with no
    /// entries starts with the end-of-central-directory signature rather than
    /// a local file header, so it does not look like a zip and is refused as
    /// one — the same answer, by a different route, as the test above.
    #[test]
    fn an_entryless_zip_does_not_look_like_an_archive() {
        let scratch = scratch("empty-zip");
        write_zip(&staged_archive_path(&scratch.0, "Nothing"), &[]);
        let error = stage(&scratch.0, "Nothing").unwrap_err();
        assert!(error.contains(".zip"), "{error}");
        assert!(!staged_package_dir(&scratch.0, "Nothing").exists());
    }
}
