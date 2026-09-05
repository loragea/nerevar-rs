//! What a TES3MP runtime directory actually contains.
//!
//! Every acquisition ends here: the pieces the rest of Nerevar goes looking
//! for later (the executables the process manager launches, the two default
//! cfg files instance setup patches, `server/data` the required-data-files
//! writer needs, the engine version the plugin-list builder keys off) are
//! located once, up front, so a create fails on the spot with a list of what
//! is missing instead of at first launch.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::instance_setup::{find_client_defaults_cfg, find_server_defaults_cfg};
use crate::instance_setup::{
    find_tes3mp_server_data_dir, openmw_runtime_version, parse_openmw_version,
};
use crate::process_manager::spawn::{find_executable, CLIENT_EXE_NAMES, SERVER_EXE_NAMES};

use super::acquire::{detect_archive_format, ArchiveFormat};
use super::source::RuntimeSource;

/// How deep under the runtime root the pieces are searched for. The same
/// depths the individual finders already used from their own call sites, so
/// inspection never fails to see something a later step would have found.
const EXE_SEARCH_DEPTH: u32 = 5;

/// What was found in a runtime directory.
#[derive(Debug, Clone)]
pub struct RuntimeInfo {
    /// The directory that was inspected (an instance's `tes3mp/`).
    pub root: PathBuf,
    pub client_exe: Option<PathBuf>,
    pub server_exe: Option<PathBuf>,
    pub client_cfg: Option<PathBuf>,
    pub server_cfg: Option<PathBuf>,
    pub server_data_dir: Option<PathBuf>,
    /// `(major, minor)` from `resources/version`, when the build records one.
    pub openmw_version: Option<(u32, u32)>,
    /// Non-fatal observations: an unknown engine version, a runtime with only
    /// one of the two executables.
    pub warnings: Vec<String>,
}

impl RuntimeInfo {
    /// The required pieces this runtime is missing, named the way a user
    /// would recognize them. Empty means the runtime is usable.
    pub fn missing_required(&self) -> Vec<String> {
        let mut missing = Vec::new();
        if self.client_exe.is_none() && self.server_exe.is_none() {
            missing.push("a TES3MP executable (tes3mp or tes3mp-server)".to_string());
        }
        if self.client_cfg.is_none() {
            missing.push("tes3mp-client-default.cfg".to_string());
        }
        if self.server_cfg.is_none() {
            missing.push("tes3mp-server-default.cfg".to_string());
        }
        if self.server_data_dir.is_none() {
            missing.push("the server/data directory".to_string());
        }
        missing
    }

    /// `Ok(())` when nothing required is missing, otherwise an error naming
    /// everything that is.
    pub fn require_complete(&self) -> Result<(), String> {
        let missing = self.missing_required();
        if missing.is_empty() {
            return Ok(());
        }
        Err(format!(
            "The TES3MP runtime at {} is incomplete — missing {}",
            self.root.display(),
            missing.join(", ")
        ))
    }

    /// `0.48`-style engine version, or `unknown`.
    pub fn version_display(&self) -> String {
        match self.openmw_version {
            Some((major, minor)) => format!("{major}.{minor}"),
            None => "unknown".to_string(),
        }
    }

    /// The parts of an inspection a UI shows: enough to tell the user whether
    /// the thing they picked is a runtime, without handing them absolute
    /// paths they did not ask about.
    pub fn summary(&self) -> RuntimeInspection {
        RuntimeInspection {
            root: self.root.to_string_lossy().into_owned(),
            version_display: self.version_display(),
            has_client_exe: self.client_exe.is_some(),
            has_server_exe: self.server_exe.is_some(),
            missing: self.missing_required(),
            warnings: self.warnings.clone(),
        }
    }
}

/// What the UI renders for a runtime source it can check before installing.
///
/// `missing` empty is the whole test: a source with nothing missing installs,
/// one with entries listed there is not a TES3MP runtime and the create form
/// stays invalid until the user picks something else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RuntimeInspection {
    /// What was inspected: the directory, or the archive file.
    pub root: String,
    /// `0.48`-style engine version, or `unknown`.
    pub version_display: String,
    pub has_client_exe: bool,
    pub has_server_exe: bool,
    /// Required pieces that are absent, named the way a user would recognize
    /// them. Empty means the source is usable.
    pub missing: Vec<String>,
    /// Non-fatal observations — a server-only build, an unknown engine
    /// version.
    pub warnings: Vec<String>,
}

/// Locates the pieces of the TES3MP runtime installed under `dir`.
///
/// Errors only when `dir` is not a directory at all; a directory that is
/// there but incomplete comes back as a `RuntimeInfo` with holes in it, so
/// the caller decides how strict to be (a create refuses, `--check` reports).
pub fn inspect(dir: &Path) -> Result<RuntimeInfo, String> {
    if !dir.is_dir() {
        return Err(format!("No TES3MP runtime directory at {}", dir.display()));
    }

    let client_exe = find_executable(dir, CLIENT_EXE_NAMES, EXE_SEARCH_DEPTH);
    let server_exe = find_executable(dir, SERVER_EXE_NAMES, EXE_SEARCH_DEPTH);
    let client_cfg = find_client_defaults_cfg(dir).ok();
    let server_cfg = find_server_defaults_cfg(dir).ok();
    let server_data_dir = find_tes3mp_server_data_dir(dir).ok();
    let openmw_version = openmw_runtime_version(dir);

    let warnings = runtime_warnings(
        &dir.display().to_string(),
        client_exe.is_some(),
        server_exe.is_some(),
        openmw_version.is_some(),
    );

    Ok(RuntimeInfo {
        root: dir.to_path_buf(),
        client_exe,
        server_exe,
        client_cfg,
        server_cfg,
        server_data_dir,
        openmw_version,
        warnings,
    })
}

/// The non-fatal observations both inspections make, from the same facts, so
/// a directory and an archive of that directory read identically.
fn runtime_warnings(
    label: &str,
    has_client_exe: bool,
    has_server_exe: bool,
    has_version: bool,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if !has_version {
        warnings.push(format!(
            "No resources/version under {label} — the engine version is unknown, so the \
             required-plugin list falls back to probing the install"
        ));
    }
    if !has_client_exe && has_server_exe {
        warnings
            .push("No TES3MP client executable — this runtime can host but not play".to_string());
    }
    if !has_server_exe && has_client_exe {
        warnings
            .push("No tes3mp-server executable — this runtime can play but not host".to_string());
    }
    warnings
}

/// What `source` would install, checked before installing it.
///
/// A `githubRelease` is the one source that cannot be checked in advance —
/// nothing of it is on disk until it is downloaded — so it is an error rather
/// than an empty answer that a caller might read as "fine".
pub fn inspect_source(source: &RuntimeSource) -> Result<RuntimeInspection, String> {
    match source {
        RuntimeSource::GithubRelease { .. } => Err(
            "A GitHub release is inspected after it downloads — there is nothing on disk to \
             check yet"
                .to_string(),
        ),
        RuntimeSource::LocalDirectory { path } => Ok(inspect(Path::new(path))?.summary()),
        RuntimeSource::Archive { path } => Ok(inspect_archive(Path::new(path))?.summary()),
    }
}

/// Locates the pieces of a TES3MP runtime inside an archive, from its entry
/// names, without extracting it.
///
/// The paths in the returned `RuntimeInfo` are entry names, not files on
/// disk — this answers "would extracting this give a runtime?", which is what
/// a user needs before committing to a 90MB extraction.
pub fn inspect_archive(archive: &Path) -> Result<RuntimeInfo, String> {
    if !archive.is_file() {
        return Err(format!("No archive at {}", archive.display()));
    }

    let mut scan = ArchiveScan::default();
    match detect_archive_format(archive)? {
        ArchiveFormat::Zip => scan_zip(archive, &mut scan)?,
        ArchiveFormat::TarGz => scan_tar_gz(archive, &mut scan)?,
    }
    Ok(scan.into_info(archive))
}

/// The pieces `inspect` looks for, spotted by entry name.
#[derive(Default)]
struct ArchiveScan {
    client_exe: Option<PathBuf>,
    server_exe: Option<PathBuf>,
    client_cfg: Option<PathBuf>,
    server_cfg: Option<PathBuf>,
    server_data_dir: Option<PathBuf>,
    openmw_version: Option<(u32, u32)>,
}

impl ArchiveScan {
    /// `true` when this entry's *contents* are wanted (`resources/version`),
    /// so the caller reads only the one entry it has to.
    fn note(&mut self, raw_name: &str) -> bool {
        let name = raw_name.replace('\\', "/");
        let trimmed = name.trim_end_matches('/');
        let base = trimmed.rsplit('/').next().unwrap_or(trimmed);

        if self.client_exe.is_none() && CLIENT_EXE_NAMES.contains(&base) {
            self.client_exe = Some(PathBuf::from(trimmed));
        }
        if self.server_exe.is_none() && SERVER_EXE_NAMES.contains(&base) {
            self.server_exe = Some(PathBuf::from(trimmed));
        }
        if self.client_cfg.is_none() && base == "tes3mp-client-default.cfg" {
            self.client_cfg = Some(PathBuf::from(trimmed));
        }
        if self.server_cfg.is_none() && base == "tes3mp-server-default.cfg" {
            self.server_cfg = Some(PathBuf::from(trimmed));
        }
        if self.server_data_dir.is_none() && is_under(trimmed, "server/data") {
            self.server_data_dir = Some(PathBuf::from("server/data"));
        }
        self.openmw_version.is_none() && is_under(trimmed, "resources/version")
    }

    fn read_version(&mut self, contents: &str) {
        self.openmw_version = parse_openmw_version(contents);
    }

    fn into_info(self, archive: &Path) -> RuntimeInfo {
        let warnings = runtime_warnings(
            &archive.display().to_string(),
            self.client_exe.is_some(),
            self.server_exe.is_some(),
            self.openmw_version.is_some(),
        );
        RuntimeInfo {
            root: archive.to_path_buf(),
            client_exe: self.client_exe,
            server_exe: self.server_exe,
            client_cfg: self.client_cfg,
            server_cfg: self.server_cfg,
            server_data_dir: self.server_data_dir,
            openmw_version: self.openmw_version,
            warnings,
        }
    }
}

/// Whether the entry `name` is `suffix` itself or sits beneath it, at any
/// depth of leading directories — archives wrap the install in a top-level
/// directory (`TES3MP/`) about half the time.
fn is_under(name: &str, suffix: &str) -> bool {
    name == suffix
        || name.starts_with(&format!("{suffix}/"))
        || name.ends_with(&format!("/{suffix}"))
        || name.contains(&format!("/{suffix}/"))
}

fn scan_zip(archive: &Path, scan: &mut ArchiveScan) -> Result<(), String> {
    use std::io::Read;

    let file = std::fs::File::open(archive)
        .map_err(|e| format!("Failed to open {}: {e}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| format!("Failed to read {}: {e}", archive.display()))?;

    // The central directory carries every name, so the whole scan happens
    // without decompressing anything; only `resources/version` is read.
    let names: Vec<String> = zip.file_names().map(|n| n.to_string()).collect();

    let mut version_entry = None;
    for name in &names {
        if scan.note(name) {
            version_entry = Some(name.clone());
        }
    }

    if let Some(name) = version_entry {
        if let Ok(mut entry) = zip.by_name(&name) {
            let mut contents = String::new();
            if entry.read_to_string(&mut contents).is_ok() {
                scan.read_version(&contents);
            }
        }
    }
    Ok(())
}

fn scan_tar_gz(archive: &Path, scan: &mut ArchiveScan) -> Result<(), String> {
    use flate2::read::GzDecoder;
    use std::io::Read;

    let file = std::fs::File::open(archive)
        .map_err(|e| format!("Failed to open {}: {e}", archive.display()))?;
    let mut tar = tar::Archive::new(GzDecoder::new(std::io::BufReader::new(file)));
    let entries = tar
        .entries()
        .map_err(|e| format!("Failed to read {}: {e}", archive.display()))?;

    for entry in entries {
        let mut entry =
            entry.map_err(|e| format!("Failed to read {}: {e}", archive.display()))?;
        let name = entry
            .path()
            .map_err(|e| format!("Failed to read {}: {e}", archive.display()))?
            .to_string_lossy()
            .into_owned();
        if scan.note(&name) {
            let mut contents = String::new();
            if entry.read_to_string(&mut contents).is_ok() {
                scan.read_version(&contents);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A minimal stand-in for an extracted official release: the nested
    /// `TES3MP/` directory the real archives unpack into, both wrapper
    /// executables, both default cfgs, `server/data`, `resources/version`.
    fn synthetic_runtime(label: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("nerevar-inspect-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let install = root.join("TES3MP");
        fs::create_dir_all(install.join("server/data")).unwrap();
        fs::create_dir_all(install.join("resources")).unwrap();
        fs::write(install.join(exe_name("tes3mp")), b"#!/bin/sh\n").unwrap();
        fs::write(install.join(exe_name("tes3mp-server")), b"#!/bin/sh\n").unwrap();
        fs::write(install.join("tes3mp-client-default.cfg"), b"[General]\n").unwrap();
        fs::write(install.join("tes3mp-server-default.cfg"), b"[General]\n").unwrap();
        fs::write(install.join("resources/version"), b"0.48.0\n7f9b\n").unwrap();
        Scratch(root)
    }

    fn exe_name(stem: &str) -> String {
        if cfg!(windows) {
            format!("{stem}.exe")
        } else {
            stem.to_string()
        }
    }

    #[test]
    fn finds_every_piece_of_a_complete_runtime() {
        let scratch = synthetic_runtime("complete");
        let info = inspect(&scratch.0).expect("inspect");

        assert!(info.client_exe.is_some(), "client exe");
        assert!(info.server_exe.is_some(), "server exe");
        assert!(info.client_cfg.is_some(), "client cfg");
        assert!(info.server_cfg.is_some(), "server cfg");
        assert!(info.server_data_dir.is_some(), "server/data");
        assert_eq!(info.openmw_version, Some((0, 48)));
        assert_eq!(info.version_display(), "0.48");
        assert!(info.warnings.is_empty(), "warnings: {:?}", info.warnings);
        assert!(info.missing_required().is_empty());
        info.require_complete().expect("complete runtime");
    }

    #[test]
    fn missing_pieces_are_listed_not_erased() {
        let scratch = synthetic_runtime("incomplete");
        let install = scratch.0.join("TES3MP");
        fs::remove_file(install.join("tes3mp-client-default.cfg")).unwrap();
        fs::remove_dir_all(install.join("server")).unwrap();

        let info = inspect(&scratch.0).expect("inspect");
        let missing = info.missing_required();
        assert!(missing
            .iter()
            .any(|m| m.contains("tes3mp-client-default.cfg")));
        assert!(missing.iter().any(|m| m.contains("server/data")));
        assert_eq!(missing.len(), 2);

        let err = info.require_complete().unwrap_err();
        assert!(err.contains("incomplete"));
        assert!(err.contains("tes3mp-client-default.cfg"));
    }

    #[test]
    fn missing_version_is_a_warning_not_a_failure() {
        let scratch = synthetic_runtime("no-version");
        fs::remove_file(scratch.0.join("TES3MP/resources/version")).unwrap();

        let info = inspect(&scratch.0).expect("inspect");
        assert_eq!(info.openmw_version, None);
        assert_eq!(info.version_display(), "unknown");
        assert!(info
            .warnings
            .iter()
            .any(|w| w.contains("resources/version")));
        assert!(info.missing_required().is_empty());
        info.require_complete().expect("version is not required");
    }

    #[test]
    fn a_server_only_runtime_warns_but_still_counts_as_usable() {
        let scratch = synthetic_runtime("server-only");
        fs::remove_file(scratch.0.join("TES3MP").join(exe_name("tes3mp"))).unwrap();

        let info = inspect(&scratch.0).expect("inspect");
        assert!(info.client_exe.is_none());
        assert!(info.server_exe.is_some());
        assert!(info
            .warnings
            .iter()
            .any(|w| w.contains("client executable")));
        assert!(info.missing_required().is_empty());
    }

    #[test]
    fn a_runtime_with_no_executable_at_all_is_incomplete() {
        let scratch = synthetic_runtime("no-exe");
        let install = scratch.0.join("TES3MP");
        fs::remove_file(install.join(exe_name("tes3mp"))).unwrap();
        fs::remove_file(install.join(exe_name("tes3mp-server"))).unwrap();

        let info = inspect(&scratch.0).expect("inspect");
        assert!(info
            .missing_required()
            .iter()
            .any(|m| m.contains("TES3MP executable")));
    }

    #[test]
    fn a_summary_reports_what_the_ui_needs() {
        let scratch = synthetic_runtime("summary");
        let summary = inspect(&scratch.0).expect("inspect").summary();

        assert_eq!(summary.version_display, "0.48");
        assert!(summary.has_client_exe);
        assert!(summary.has_server_exe);
        assert!(summary.missing.is_empty());
        assert!(summary.warnings.is_empty());
        assert_eq!(summary.root, scratch.0.to_string_lossy());
    }

    /// A zip of a synthetic runtime inspects to the same verdict the
    /// extracted directory would — from entry names alone, nothing unpacked.
    #[test]
    fn an_archive_inspects_from_its_entry_names() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let scratch = synthetic_runtime("archive-inspect");
        let archive = scratch.0.join("release.zip");
        {
            let mut writer = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            for entry in [
                format!("TES3MP/{}", exe_name("tes3mp")),
                format!("TES3MP/{}", exe_name("tes3mp-server")),
                "TES3MP/tes3mp-client-default.cfg".to_string(),
                "TES3MP/tes3mp-server-default.cfg".to_string(),
                "TES3MP/server/data/placeholder".to_string(),
            ] {
                writer
                    .start_file(entry, SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(b"x").unwrap();
            }
            writer
                .start_file("TES3MP/resources/version", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"0.48.0\n7f9b\n").unwrap();
            writer.finish().unwrap();
        }

        let info = inspect_archive(&archive).expect("inspect archive");
        assert!(info.missing_required().is_empty(), "{:?}", info.missing_required());
        assert_eq!(info.openmw_version, Some((0, 48)));
        assert!(info.client_exe.is_some());
        assert!(info.server_exe.is_some());
        assert!(info.server_data_dir.is_some());
    }

    #[test]
    fn an_archive_of_the_wrong_thing_lists_what_is_missing() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let scratch = synthetic_runtime("archive-wrong");
        let archive = scratch.0.join("mods.zip");
        {
            let mut writer = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            writer
                .start_file("SomeMod/mod.esp", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"x").unwrap();
            writer.finish().unwrap();
        }

        let info = inspect_archive(&archive).expect("inspect archive");
        let missing = info.missing_required();
        assert!(missing.iter().any(|m| m.contains("TES3MP executable")), "{missing:?}");
        assert!(missing.iter().any(|m| m.contains("server/data")), "{missing:?}");
        assert!(info.require_complete().is_err());
    }

    /// The one source that cannot be checked before it installs says so
    /// rather than answering with an empty summary a caller might read as
    /// "fine".
    #[test]
    fn a_github_release_is_not_inspectable_before_it_downloads() {
        let err = inspect_source(&RuntimeSource::from_legacy_release_id("65767406"))
            .expect_err("nothing on disk to inspect");
        assert!(err.contains("after it downloads"), "{err}");
    }

    #[test]
    fn inspect_source_checks_a_local_directory() {
        let scratch = synthetic_runtime("source-dir");
        let summary = inspect_source(&RuntimeSource::LocalDirectory {
            path: scratch.0.to_string_lossy().into_owned(),
        })
        .expect("inspect");
        assert!(summary.missing.is_empty());
        assert_eq!(summary.version_display, "0.48");
    }

    #[test]
    fn a_missing_directory_is_an_error() {
        let missing = std::env::temp_dir().join("nerevar-inspect-does-not-exist");
        let _ = fs::remove_dir_all(&missing);
        assert!(inspect(&missing).is_err());
    }
}
