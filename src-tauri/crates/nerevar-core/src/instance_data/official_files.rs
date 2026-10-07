//! The official Morrowind game files, which Nerevar never packages, serves or
//! syncs.
//!
//! `Morrowind.esm`, `Tribunal.esm`, `Bloodmoon.esm` and their three `.bsa`
//! archives are Bethesda's game data. Every player brings their own copy (the
//! client registers them from the player's install), so a package that
//! contains one is either a mistake or redistribution. Each layer enforces
//! this on its own: the admin upload refuses such an archive and apply
//! refuses such a staged tree, the manifest build leaves the files out (so
//! they are never hashed, served or counted), the sync server will not serve
//! one, and the sync client drops any a manifest lists before downloading.

use std::path::Path;

use super::types::NerevarManifest;

/// The six official files, matched by file name ignoring ASCII case.
pub const OFFICIAL_GAME_FILES: &[&str] = &[
    "Morrowind.esm",
    "Tribunal.esm",
    "Bloodmoon.esm",
    "Morrowind.bsa",
    "Tribunal.bsa",
    "Bloodmoon.bsa",
];

/// Whether the last component of `path` (a bare file name, or a relative path
/// with `/` or `\` separators) is one of the [`OFFICIAL_GAME_FILES`].
pub fn is_official_game_file(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path).trim();
    OFFICIAL_GAME_FILES
        .iter()
        .any(|official| name.eq_ignore_ascii_case(official))
}

/// The refusal for an official file found in a package, naming it by the last
/// component of `path`.
pub fn official_game_file_message(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    format!("{name} is official game data and cannot be distributed; players bring their own copy")
}

/// Every official file anywhere under `dir`, as `/`-separated paths relative to
/// it, sorted. Symlinks are not followed.
pub fn find_official_game_files(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    walk(dir, dir, &mut found);
    found.sort();
    found
}

fn walk(root: &Path, current: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            walk(root, &path, out);
        } else if kind.is_file() {
            let name = entry.file_name();
            if is_official_game_file(&name.to_string_lossy()) {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(relative);
            }
        }
    }
}

/// Removes every official file from a manifest's package file lists, plugin
/// lists and archive list, keeping sizes and counts consistent, and logs a
/// WARN for each file dropped. Returns how many files were dropped.
///
/// The sync client runs this on every manifest it fetches, so a host that
/// lists one (built before this rule, or hostile) never gets the client to
/// download it. The base-game ESMs stay in `resolved.content`: they are loaded
/// from the player's own install, which is exactly what should happen.
pub fn strip_official_game_files(manifest: &mut NerevarManifest) -> usize {
    let mut dropped = 0usize;
    for package in &mut manifest.packages {
        let before = package.files.len();
        package.files.retain(|file| {
            if is_official_game_file(&file.path) {
                log::warn!(
                    "Package \"{}\" lists {} — official game data is never synced; skipping it",
                    package.name,
                    file.path
                );
                manifest.total_download_bytes =
                    manifest.total_download_bytes.saturating_sub(file.size);
                package.total_size_bytes = package.total_size_bytes.saturating_sub(file.size);
                false
            } else {
                true
            }
        });
        let removed = before - package.files.len();
        if removed > 0 {
            dropped += removed;
            package.file_count = package.files.len() as u32;
        }
        package
            .plugins
            .retain(|plugin| !is_official_game_file(&plugin.file));
    }
    manifest
        .resolved
        .archives
        .retain(|archive| !is_official_game_file(archive));
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance_data::types::{
        ManifestFileEntry, ManifestPackage, PackageKind, PluginEntry, ResolvedOpenMwConfig,
    };
    use crate::instance_settings::InstanceSettings;

    #[test]
    fn matches_the_six_by_name_at_any_depth_ignoring_case() {
        for path in [
            "Morrowind.esm",
            "tribunal.ESM",
            "Data Files/Bloodmoon.esm",
            "nested\\deep\\MORROWIND.BSA",
            "Tribunal.bsa",
            "a/b/c/bloodmoon.bsa",
        ] {
            assert!(is_official_game_file(path), "{path}");
        }
        for path in ["TR_Mainland.esm", "Morrowind.esp", "Morrowind.bsa.bak", "MorrowindX.bsa"] {
            assert!(!is_official_game_file(path), "{path}");
        }
    }

    #[test]
    fn the_message_names_the_file() {
        assert_eq!(
            official_game_file_message("Data Files/Morrowind.bsa"),
            "Morrowind.bsa is official game data and cannot be distributed; players bring their own copy"
        );
    }

    #[test]
    fn finds_official_files_anywhere_in_a_tree() {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-official-find-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(dir.join("Data Files/meshes")).unwrap();
        std::fs::write(dir.join("Data Files/Morrowind.bsa"), b"x").unwrap();
        std::fs::write(dir.join("Data Files/meshes/tribunal.esm"), b"x").unwrap();
        std::fs::write(dir.join("Mod.esp"), b"x").unwrap();

        assert_eq!(
            find_official_game_files(&dir),
            vec![
                "Data Files/Morrowind.bsa".to_string(),
                "Data Files/meshes/tribunal.esm".to_string()
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn file(path: &str, size: u64) -> ManifestFileEntry {
        ManifestFileEntry {
            path: path.into(),
            size,
            checksum: format!("sha256:{path}"),
        }
    }

    #[test]
    fn stripping_a_manifest_drops_the_files_and_fixes_the_totals() {
        let mut manifest = NerevarManifest {
            version: 1,
            instance_id: "x".into(),
            instance_name: "x".into(),
            generated_at: String::new(),
            base_game_data: None,
            packages: vec![ManifestPackage {
                id: "p".into(),
                name: "Hostile".into(),
                kind: PackageKind::Mod,
                relative_dir: "Hostile".into(),
                priority: 10,
                tree_checksum: "sha256:tree".into(),
                total_size_bytes: 111,
                file_count: 3,
                files: vec![
                    file("Mod.esp", 1),
                    file("Morrowind.bsa", 100),
                    file("sub/Tribunal.esm", 10),
                ],
                plugins: vec![
                    PluginEntry { file: "Mod.esp".into(), enabled: true },
                    PluginEntry { file: "Morrowind.bsa".into(), enabled: true },
                ],
            }],
            resolved: ResolvedOpenMwConfig {
                encoding: "win1252".into(),
                data_paths: vec![],
                content: vec!["Morrowind.esm".into(), "Mod.esp".into()],
                archives: vec!["Morrowind.bsa".into()],
            },
            total_download_bytes: 111,
            tes3mp_server_port: 25565,
            tes3mp_server_password: String::new(),
            required_data_files: vec![],
            instance_settings: InstanceSettings::default(),
        };

        assert_eq!(strip_official_game_files(&mut manifest), 2);
        let package = &manifest.packages[0];
        assert_eq!(package.files.len(), 1);
        assert_eq!(package.files[0].path, "Mod.esp");
        assert_eq!(package.file_count, 1);
        assert_eq!(package.total_size_bytes, 1);
        assert_eq!(manifest.total_download_bytes, 1);
        assert_eq!(package.plugins.len(), 1);
        assert!(manifest.resolved.archives.is_empty());
        // The base ESMs still load, from the player's own install.
        assert_eq!(manifest.resolved.content[0], "Morrowind.esm");
    }
}
