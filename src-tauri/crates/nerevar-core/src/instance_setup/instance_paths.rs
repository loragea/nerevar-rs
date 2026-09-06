//! Where a newly created instance's directories live.
//!
//! The derivation belongs on this side of the Tauri boundary because only
//! the backend knows the host platform: `Path::join` writes `\` on Windows
//! and `/` everywhere else, while a frontend that hard-codes either one is
//! wrong on the other platform.

use std::path::{Path, PathBuf};

/// Characters Windows forbids in a path component. `/` and `\` are stripped
/// on every platform as well, so a typed name can never introduce a
/// directory level of its own.
const FORBIDDEN_NAME_CHARS: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// The subdirectory of an instance root that holds its mod data.
pub const INSTANCE_DATA_DIR_NAME: &str = "data";

/// The directories a newly created instance occupies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstancePaths {
    /// The instance's own directory, `<nerevar root>/<folder name>`.
    pub root: PathBuf,
    /// Where mod data lands, `<instance root>/data`.
    pub data_dir: PathBuf,
}

/// Turns a user-typed instance name into a single path component.
///
/// Trims, drops the characters Windows forbids, and trims again — a name
/// that is only whitespace and forbidden characters leaves nothing to name
/// a folder with and is rejected rather than silently becoming the root
/// directory itself.
pub fn instance_folder_name(instance_name: &str) -> Result<String, String> {
    let folder = instance_name
        .trim()
        .replace(FORBIDDEN_NAME_CHARS, "")
        .trim()
        .to_string();
    if folder.is_empty() {
        return Err(format!(
            "\"{instance_name}\" leaves no usable folder name: a name needs at least one character that is not one of <>:\"/\\|?*"
        ));
    }
    Ok(folder)
}

/// Derives an instance's directories from the Nerevar data directory and the
/// instance's name, using the running platform's path separator.
pub fn instance_paths(nerevar_root: &Path, instance_name: &str) -> Result<InstancePaths, String> {
    if nerevar_root.as_os_str().is_empty() {
        return Err("The Nerevar data directory is not set".to_string());
    }
    let root = nerevar_root.join(instance_folder_name(instance_name)?);
    let data_dir = root.join(INSTANCE_DATA_DIR_NAME);
    Ok(InstancePaths { root, data_dir })
}

/// Refuses an instance root that is already taken, so a create never writes
/// into a directory somebody else owns.
pub fn ensure_instance_path_available(instance_root: &Path) -> Result<(), String> {
    if instance_root.exists() {
        return Err(format!(
            "Instance path already exists: {}",
            instance_root.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_with_the_platform_separator() {
        let root = Path::new("nerevar-root");
        let paths = instance_paths(root, "Vvardenfell").expect("derives paths");

        // `Path::join` semantics, not a hand-written separator: this is the
        // assertion that holds on Linux, Windows, and macOS alike.
        assert_eq!(paths.root, root.join("Vvardenfell"));
        assert_eq!(paths.data_dir, root.join("Vvardenfell").join("data"));

        // And the rendered string really does carry the host's separator —
        // the bug was a literal `\` reaching `Path::new` on Linux.
        let rendered = paths.data_dir.display().to_string();
        assert!(
            rendered.contains(std::path::MAIN_SEPARATOR),
            "{rendered} should be separated by {}",
            std::path::MAIN_SEPARATOR
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_paths_use_forward_slashes() {
        let paths = instance_paths(Path::new("/home/player/Games/Nerevar"), "Red Mountain")
            .expect("derives paths");
        assert_eq!(
            paths.root.display().to_string(),
            "/home/player/Games/Nerevar/Red Mountain"
        );
        assert_eq!(
            paths.data_dir.display().to_string(),
            "/home/player/Games/Nerevar/Red Mountain/data"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_use_backslashes() {
        let paths =
            instance_paths(Path::new("C:\\Games\\Nerevar"), "Red Mountain").expect("derives paths");
        assert_eq!(
            paths.root.display().to_string(),
            "C:\\Games\\Nerevar\\Red Mountain"
        );
        assert_eq!(
            paths.data_dir.display().to_string(),
            "C:\\Games\\Nerevar\\Red Mountain\\data"
        );
    }

    #[test]
    fn a_trailing_separator_on_the_root_does_not_double_up() {
        let with = instance_paths(Path::new("nerevar-root/"), "Balmora").expect("derives paths");
        let without = instance_paths(Path::new("nerevar-root"), "Balmora").expect("derives paths");
        assert_eq!(with.root, without.root);
    }

    #[test]
    fn the_folder_name_drops_characters_windows_forbids() {
        assert_eq!(
            instance_folder_name("  Vos: <the>/best*server?  ").expect("sanitises"),
            "Vos thebestserver"
        );
        assert_eq!(
            instance_folder_name("a\\b|c\"d").expect("sanitises"),
            "abcd"
        );
    }

    #[test]
    fn a_name_that_sanitises_to_nothing_is_rejected() {
        for name in ["", "   ", "///", " <>:\"/\\|?* "] {
            let err = instance_folder_name(name).expect_err("rejects an empty folder name");
            assert!(err.contains("no usable folder name"), "{err}");
        }
        assert!(instance_paths(Path::new("nerevar-root"), "  ").is_err());
    }

    #[test]
    fn an_unset_nerevar_root_is_rejected() {
        // Otherwise the instance would land relative to the process's
        // working directory.
        let err = instance_paths(Path::new(""), "Balmora").expect_err("rejects an unset root");
        assert!(err.contains("data directory is not set"), "{err}");
    }

    #[test]
    fn an_existing_instance_path_is_refused() {
        let dir =
            std::env::temp_dir().join(format!("nerevar-instance-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = instance_paths(&dir, "Suran").expect("derives paths");

        ensure_instance_path_available(&paths.root).expect("a fresh path is available");

        std::fs::create_dir_all(&paths.root).expect("creates the instance root");
        let err =
            ensure_instance_path_available(&paths.root).expect_err("refuses an existing path");
        assert!(err.starts_with("Instance path already exists:"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
