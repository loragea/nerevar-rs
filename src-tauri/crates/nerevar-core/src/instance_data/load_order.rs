use std::path::Path;

use super::paths::{ensure_instance_data_layout, load_order_path};
use super::progress::{BackgroundOperationPhase, ProgressEmitter};
use super::scan::{new_entry_id, scan_data_directory};
use super::types::{LoadOrder, LoadOrderEntry, PluginEntry, LOAD_ORDER_VERSION};
use crate::openmw_ini_importer;

pub fn load_load_order(data_dir: &Path) -> Result<LoadOrder, String> {
    let path = load_order_path(data_dir);
    if !path.exists() {
        return Ok(LoadOrder::default());
    }
    let contents =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read load order: {e}"))?;
    serde_json::from_str(&contents).map_err(|e| format!("Invalid load-order.json: {e}"))
}

pub fn save_load_order(data_dir: &Path, load_order: &LoadOrder) -> Result<(), String> {
    ensure_instance_data_layout(data_dir)?;
    let path = load_order_path(data_dir);
    let contents = serde_json::to_string_pretty(load_order)
        .map_err(|e| format!("Failed to serialize load order: {e}"))?;
    std::fs::write(&path, contents).map_err(|e| format!("Failed to write load order: {e}"))
}

/// Scan disk and merge results into load order: append new package
/// directories, drop vanished ones, and refresh each entry's plugin list.
///
/// Checksums are *not* refreshed. The scan reads directory entries and
/// filenames only and never hashes file contents, so it has no checksum to
/// offer; an entry's existing `tree_checksum` is carried through untouched.
/// The authoritative package checksums are the ones `build_manifest` computes
/// by hashing the packages on disk.
pub fn scan_and_merge_load_order(
    data_dir: &Path,
    progress: &mut Option<ProgressEmitter>,
) -> Result<LoadOrder, String> {
    ensure_instance_data_layout(data_dir)?;
    let mut load_order = load_load_order(data_dir)?;
    if load_order.version == 0 {
        load_order.version = LOAD_ORDER_VERSION;
    }

    if load_order.base_game_data.is_none() {
        load_order.base_game_data = openmw_ini_importer::read_first_global_data_path();
    }

    let scanned = scan_data_directory(data_dir, progress)?;

    if let Some(emitter) = progress.as_mut() {
        emitter.emit(
            BackgroundOperationPhase::MergingLoadOrder,
            "Merging scan results into load order",
            0,
            1,
            None,
            true,
        );
    }

    merge_scanned_with_data_dir(data_dir, &mut load_order, scanned);

    if let Some(emitter) = progress.as_mut() {
        emitter.emit(
            BackgroundOperationPhase::SavingLoadOrder,
            "Saving load-order.json",
            0,
            1,
            None,
            true,
        );
    }

    save_load_order(data_dir, &load_order)?;
    Ok(load_order)
}

fn sync_plugins(entry: &mut LoadOrderEntry, scanned_plugins: &[String]) {
    let mut next = Vec::new();
    for file in scanned_plugins {
        let existing = entry
            .plugins
            .iter()
            .find(|p| p.file.eq_ignore_ascii_case(file));
        next.push(PluginEntry {
            file: file.clone(),
            enabled: existing.map(|p| p.enabled).unwrap_or(true),
        });
    }
    entry.plugins = next;
}

pub fn merge_scanned_with_data_dir(
    data_dir: &Path,
    load_order: &mut LoadOrder,
    scanned: Vec<super::types::ScannedPackage>, // ScannedPackage from scan module
) {
    for package in scanned {
        if let Some(existing) = load_order
            .entries
            .iter_mut()
            .find(|e| e.relative_dir == package.relative_dir)
        {
            existing.name = package.name;
            existing.kind = package.kind;
            // A scan never produces a checksum (see `scan_data_directory`), so
            // in practice this leaves `existing.tree_checksum` alone. Written
            // as an overwrite-if-present so a future checksumming scanner
            // would refresh rather than be silently ignored.
            if let Some(tree_checksum) = package.tree_checksum {
                existing.tree_checksum = Some(tree_checksum);
            }
            sync_plugins(existing, &package.plugins);
            continue;
        }

        let max_priority = load_order.entries.iter().map(|e| e.priority).max().unwrap_or(0);
        let plugins = package
            .plugins
            .iter()
            .map(|file| PluginEntry {
                file: file.clone(),
                enabled: true,
            })
            .collect();

        load_order.entries.push(LoadOrderEntry {
            id: new_entry_id(),
            name: package.name,
            kind: package.kind,
            relative_dir: package.relative_dir,
            enabled: true,
            priority: max_priority + 10,
            plugins,
            tree_checksum: package.tree_checksum,
        });
    }

    load_order.entries.retain(|entry| {
        super::paths::package_abs_path(data_dir, &entry.relative_dir).exists()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance_data::types::PackageKind;

    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-load-order-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// A rescan refreshes names, kinds and plugin lists, but has no checksum
    /// to offer (`scan_data_directory` never hashes), so an entry keeps the
    /// checksum it already had. Pins the behaviour the daemon's `--scan` docs
    /// and `docs/headless-hosting.md` describe.
    #[test]
    fn merging_a_scan_leaves_existing_checksums_alone() {
        let scratch = scratch("checksum-preserved");
        let data_dir = &scratch.0;
        std::fs::create_dir_all(data_dir.join("Better Bodies")).unwrap();

        let mut load_order = LoadOrder {
            version: LOAD_ORDER_VERSION,
            base_game_data: None,
            content_order: None,
            entries: vec![LoadOrderEntry {
                id: "entry-1".to_string(),
                name: "Better Bodies".to_string(),
                kind: PackageKind::Mod,
                relative_dir: "Better Bodies".to_string(),
                enabled: true,
                priority: 10,
                plugins: Vec::new(),
                tree_checksum: Some("sha256:from-a-manifest".to_string()),
            }],
        };

        let scanned = scan_data_directory(data_dir, &mut None).unwrap();
        // The scan itself is the source of the `None`.
        assert!(scanned.iter().all(|p| p.tree_checksum.is_none()));

        merge_scanned_with_data_dir(data_dir, &mut load_order, scanned);

        assert_eq!(load_order.entries.len(), 1);
        assert_eq!(
            load_order.entries[0].tree_checksum.as_deref(),
            Some("sha256:from-a-manifest"),
            "a rescan must not clear or invent a checksum"
        );
    }

    /// A package the load order has never seen enters without a checksum,
    /// rather than with a placeholder that would compare unequal later.
    #[test]
    fn a_newly_scanned_package_enters_without_a_checksum() {
        let scratch = scratch("checksum-absent");
        let data_dir = &scratch.0;
        std::fs::create_dir_all(data_dir.join("Rock Replacer").join("textures")).unwrap();

        let mut load_order = LoadOrder::default();
        let scanned = scan_data_directory(data_dir, &mut None).unwrap();
        merge_scanned_with_data_dir(data_dir, &mut load_order, scanned);

        let entry = load_order
            .entries
            .iter()
            .find(|e| e.relative_dir == "Rock Replacer")
            .expect("Rock Replacer");
        assert!(entry.tree_checksum.is_none());
    }
}
