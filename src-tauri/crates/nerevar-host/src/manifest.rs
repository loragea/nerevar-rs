use std::path::Path;

use nerevar_core::instance_data::scan_and_merge_load_order;

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
