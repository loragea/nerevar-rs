use std::path::Path;

use nerevar_core::instance_data::{
    build_manifest, load_order_path, load_manifest, manifest_path, scan_and_merge_load_order,
    NerevarManifest,
};

/// Rescans the instance data directory and merges the result into
/// `load-order.json`: new package folders are appended (enabled, lowest
/// priority), vanished ones dropped, plugin lists and tree checksums
/// refreshed. This is the headless equivalent of the GUI data manager's
/// rescan, and the second half of "drop a mod folder on the host" — the
/// first half being `rsync`.
pub fn scan_data_dir(data_dir: &Path) -> Result<(), String> {
    let mut no_progress = None;
    let load_order = scan_and_merge_load_order(data_dir, &mut no_progress)?;
    let enabled = load_order.entries.iter().filter(|e| e.enabled).count();
    log::info!(
        "Scanned {}: load order holds {} package(s), {enabled} enabled",
        data_dir.display(),
        load_order.entries.len()
    );
    Ok(())
}

/// Produces the manifest to host, mirroring what the GUI's
/// `set_hosting_instance` does before it activates hosting: rebuild from
/// `load-order.json` (hashing every enabled package) so clients are served a
/// manifest that matches what is actually on the host's disk, and so
/// TES3MP's `requiredDataFiles.json` — which `build_manifest` rewrites —
/// enforces the same plugin list the manifest ships.
///
/// With `rebuild == false` the manifest already on disk is served verbatim
/// (fast restarts, or a manifest deliberately built elsewhere); a load order
/// newer than that manifest is then only a warning, since choosing not to
/// rebuild is the operator's call.
///
/// Blocking and CPU-heavy (rayon-parallel hashing over the whole data dir),
/// called on the runtime thread on purpose: it runs at startup before any
/// server is up, so there is nothing yet to starve.
pub fn prepare(
    instance_id: &str,
    instance_name: &str,
    instance_root: &Path,
    data_dir: &Path,
    rebuild: bool,
) -> Result<NerevarManifest, String> {
    if !rebuild {
        let manifest = load_manifest(data_dir).map_err(|err| {
            format!(
                "{err} ({}). Nothing has built a manifest for this instance, so clients \
                 would get 404s: drop --no-manifest-rebuild to build one now.",
                manifest_path(data_dir).display()
            )
        })?;
        if load_order_is_newer_than_manifest(data_dir) {
            log::warn!(
                "load-order.json is newer than manifest.json and --no-manifest-rebuild was \
                 passed: serving the older manifest. Clients will not see recent mod changes."
            );
        }
        log_manifest(&manifest, "Hosting existing manifest");
        return Ok(manifest);
    }

    if !load_order_path(data_dir).exists() {
        return Err(format!(
            "No load order at {}. The daemon will not guess a mod list: run once with \
             --scan to build one from the package folders in {}, or copy a load-order.json \
             in from the GUI.",
            load_order_path(data_dir).display(),
            data_dir.display()
        ));
    }

    // `build_manifest` reads the load order itself via the caller-supplied
    // value, so load it here (the GUI does the same in `set_hosting_instance`).
    let load_order = nerevar_core::instance_data::load_load_order(data_dir)?;
    let mut no_progress = None;
    let manifest = build_manifest(
        instance_id,
        instance_name,
        instance_root,
        data_dir,
        &load_order,
        &mut no_progress,
    )?;
    log_manifest(&manifest, "Built manifest");
    Ok(manifest)
}

fn log_manifest(manifest: &NerevarManifest, what: &str) {
    log::info!(
        "{what}: {} package(s), {} file(s), {:.1} MiB, {} required plugin(s)",
        manifest.packages.len(),
        manifest
            .packages
            .iter()
            .map(|package| package.file_count as u64)
            .sum::<u64>(),
        manifest.total_download_bytes as f64 / (1024.0 * 1024.0),
        manifest.required_data_files.len(),
    );
}

fn load_order_is_newer_than_manifest(data_dir: &Path) -> bool {
    let modified = |path| std::fs::metadata(path).and_then(|meta| meta.modified()).ok();
    match (
        modified(load_order_path(data_dir)),
        modified(manifest_path(data_dir)),
    ) {
        (Some(load_order), Some(manifest)) => load_order > manifest,
        // No timestamps to compare (a filesystem without mtimes, or the
        // manifest vanished between load and stat) — say nothing rather than
        // warn on a guess.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-host-manifest-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".nerevar")).unwrap();
        dir
    }

    #[test]
    fn rebuild_without_load_order_names_the_scan_flag() {
        let dir = temp_dir("no-load-order");
        let err = prepare("id", "name", &dir, &dir, true).unwrap_err();
        assert!(err.contains("--scan"), "unexpected error: {err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_rebuild_without_manifest_explains_the_404() {
        let dir = temp_dir("no-manifest");
        let err = prepare("id", "name", &dir, &dir, false).unwrap_err();
        assert!(err.contains("--no-manifest-rebuild"), "unexpected error: {err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn staleness_check_compares_mtimes() {
        let dir = temp_dir("staleness");
        fs::write(manifest_path(&dir), "{}").unwrap();
        // Coarse filesystem timestamps would make an immediate second write
        // compare equal, not newer.
        std::thread::sleep(Duration::from_millis(1100));
        fs::write(load_order_path(&dir), "{}").unwrap();
        assert!(load_order_is_newer_than_manifest(&dir));

        std::thread::sleep(Duration::from_millis(1100));
        fs::write(manifest_path(&dir), "{}").unwrap();
        assert!(!load_order_is_newer_than_manifest(&dir));
        let _ = fs::remove_dir_all(&dir);
    }
}
