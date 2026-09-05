//! What a TES3MP runtime directory actually contains.
//!
//! Every acquisition ends here: the pieces the rest of Nerevar goes looking
//! for later (the executables the process manager launches, the two default
//! cfg files instance setup patches, `server/data` the required-data-files
//! writer needs, the engine version the plugin-list builder keys off) are
//! located once, up front, so a create fails on the spot with a list of what
//! is missing instead of at first launch.

use std::path::{Path, PathBuf};

use crate::instance_setup::{find_client_defaults_cfg, find_server_defaults_cfg};
use crate::instance_setup::{find_tes3mp_server_data_dir, openmw_runtime_version};
use crate::process_manager::spawn::{find_executable, CLIENT_EXE_NAMES, SERVER_EXE_NAMES};

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

    let mut warnings = Vec::new();
    if openmw_version.is_none() {
        warnings.push(format!(
            "No resources/version under {} — the engine version is unknown, so the \
             required-plugin list falls back to probing the install",
            dir.display()
        ));
    }
    if client_exe.is_none() && server_exe.is_some() {
        warnings
            .push("No TES3MP client executable — this runtime can host but not play".to_string());
    }
    if server_exe.is_none() && client_exe.is_some() {
        warnings
            .push("No tes3mp-server executable — this runtime can play but not host".to_string());
    }

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
    fn a_missing_directory_is_an_error() {
        let missing = std::env::temp_dir().join("nerevar-inspect-does-not-exist");
        let _ = fs::remove_dir_all(&missing);
        assert!(inspect(&missing).is_err());
    }
}
