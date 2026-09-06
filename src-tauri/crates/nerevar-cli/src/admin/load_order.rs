//! The load-order edits `enable`, `disable` and `order` make, as pure
//! functions over the document.
//!
//! The host has no "enable this one package" route: `POST /admin/load-order`
//! takes the whole document, the same one the desktop app writes. So each of
//! these commands is fetch, edit, post — and the edit is the only part worth
//! reasoning about, which is why it lives here with no HTTP and no
//! filesystem in sight.
//!
//! Effective order is the `priority` field, not the position in `entries`
//! (`instance_data::manifest` sorts by it), so [`reorder`] rewrites both and
//! keeps them agreeing.

use nerevar_core::admin::StagedPackage;
use nerevar_core::instance_data::{LoadOrder, LoadOrderEntry, PluginEntry};

/// Gap between adjacent priorities after a reorder, matching what a scan
/// hands out (`merge_scanned_with_data_dir` steps by 10) so a later scan
/// appends above everything rather than colliding.
const PRIORITY_STEP: u32 = 10;

/// Whether `entry` is the one a co-admin means by `key`: its package
/// directory, its display name, or its id, matched case-insensitively.
fn matches(entry: &LoadOrderEntry, key: &str) -> bool {
    let key = key.trim();
    entry.relative_dir.eq_ignore_ascii_case(key)
        || entry.name.eq_ignore_ascii_case(key)
        || entry.id.eq_ignore_ascii_case(key)
}

/// Finds the entry a co-admin means by `name`, preferring a package-directory
/// match over a display-name one over an id.
fn find_entry(order: &LoadOrder, name: &str) -> Option<usize> {
    let name = name.trim();
    let by = |pick: fn(&LoadOrderEntry) -> &str| {
        order
            .entries
            .iter()
            .position(|entry| pick(entry).eq_ignore_ascii_case(name))
    };
    by(|entry| entry.relative_dir.as_str())
        .or_else(|| by(|entry| entry.name.as_str()))
        .or_else(|| by(|entry| entry.id.as_str()))
}

fn unknown(order: &LoadOrder, name: &str) -> String {
    let mut known: Vec<&str> = order
        .entries
        .iter()
        .map(|entry| entry.relative_dir.as_str())
        .collect();
    known.sort_unstable_by_key(|name| name.to_lowercase());
    if known.is_empty() {
        return format!("No package \"{name}\" in the load order, which is empty.");
    }
    format!(
        "No package \"{name}\" in the load order. It holds: {}.",
        known.join(", ")
    )
}

/// Turns `name` on or off, returning the package directory it resolved to.
///
/// A name that is not in the load order yet but *is* staged gets an entry
/// appended: a freshly uploaded package has no entry until an apply rescans
/// `data/`, and "upload it, then enable it" is the one workflow that would
/// otherwise be impossible before the apply. The appended entry survives the
/// apply's rescan, which merges by package directory and leaves existing
/// entries alone.
pub fn set_enabled(
    order: &mut LoadOrder,
    staged: &[StagedPackage],
    name: &str,
    enabled: bool,
) -> Result<String, String> {
    if let Some(index) = find_entry(order, name) {
        order.entries[index].enabled = enabled;
        return Ok(order.entries[index].relative_dir.clone());
    }

    let package = staged
        .iter()
        .find(|package| package.name.eq_ignore_ascii_case(name.trim()))
        .ok_or_else(|| unknown(order, name))?;

    order.entries.push(LoadOrderEntry {
        id: fresh_entry_id(order, &package.name),
        name: package.name.clone(),
        kind: package.kind,
        relative_dir: package.name.clone(),
        enabled,
        priority: next_priority(order),
        plugins: package
            .plugins
            .iter()
            .map(|file| PluginEntry {
                file: file.clone(),
                enabled: true,
            })
            .collect(),
        tree_checksum: None,
    });
    Ok(package.name.clone())
}

/// An id no entry is using. Derived from the package name rather than random
/// so the same upload staged twice produces the same document.
fn fresh_entry_id(order: &LoadOrder, package: &str) -> String {
    let taken = |candidate: &str| order.entries.iter().any(|entry| entry.id == candidate);
    if !taken(package) {
        return package.to_string();
    }
    (2u32..)
        .map(|n| format!("{package}-{n}"))
        .find(|candidate| !taken(candidate))
        .expect("an unused suffix exists")
}

fn next_priority(order: &LoadOrder) -> u32 {
    order
        .entries
        .iter()
        .map(|entry| entry.priority)
        .max()
        .unwrap_or(0)
        .saturating_add(PRIORITY_STEP)
}

/// Moves `names` to the front of the load order, in the order given;
/// everything else keeps its relative order behind them.
///
/// Rewrites every priority, which is what makes the result readable: the
/// document that comes back has priorities 10, 20, 30 … in the sequence the
/// game will load them.
pub fn reorder(order: &mut LoadOrder, names: &[String]) -> Result<(), String> {
    // Built beside the document, not in it: a reorder that names something
    // unknown must leave the caller's load order exactly as it found it.
    //
    // Sorted by priority first, because that is the order the host loads them
    // in, which is what "keep their relative order" has to mean.
    let mut rest: Vec<LoadOrderEntry> = order.entries.clone();
    rest.sort_by_key(|entry| entry.priority);

    let mut moved: Vec<LoadOrderEntry> = Vec::with_capacity(names.len());
    for name in names {
        let Some(position) = rest.iter().position(|entry| matches(entry, name)) else {
            return Err(if moved.iter().any(|entry| matches(entry, name)) {
                format!("\"{name}\" is listed twice in the new order.")
            } else {
                unknown(order, name)
            });
        };
        moved.push(rest.remove(position));
    }

    moved.append(&mut rest);
    for (index, entry) in moved.iter_mut().enumerate() {
        entry.priority = (index as u32 + 1).saturating_mul(PRIORITY_STEP);
    }
    order.entries = moved;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nerevar_core::admin::StagedPackage;
    use nerevar_core::instance_data::{PackageKind, LOAD_ORDER_VERSION};

    fn entry(name: &str, priority: u32, enabled: bool) -> LoadOrderEntry {
        LoadOrderEntry {
            id: format!("id-{name}"),
            name: name.to_string(),
            kind: PackageKind::Mod,
            relative_dir: name.to_string(),
            enabled,
            priority,
            plugins: vec![PluginEntry {
                file: format!("{name}.esp"),
                enabled: true,
            }],
            tree_checksum: None,
        }
    }

    fn order(entries: Vec<LoadOrderEntry>) -> LoadOrder {
        LoadOrder {
            version: LOAD_ORDER_VERSION,
            base_game_data: None,
            content_order: None,
            entries,
        }
    }

    fn staged(name: &str, plugins: &[&str]) -> StagedPackage {
        StagedPackage {
            name: name.to_string(),
            kind: PackageKind::Mod,
            plugins: plugins.iter().map(|p| p.to_string()).collect(),
            replaces_existing: false,
            archive_bytes: 1,
            extracted_bytes: 1,
            staged_at: "2026-09-06T10:00:00+00:00".to_string(),
            staged_by: "ada".to_string(),
        }
    }

    fn sequence(order: &LoadOrder) -> Vec<(String, u32)> {
        order
            .entries
            .iter()
            .map(|entry| (entry.relative_dir.clone(), entry.priority))
            .collect()
    }

    // ---- enable / disable ---------------------------------------------------------

    #[test]
    fn enabling_flips_the_entry_and_touches_nothing_else() {
        let mut doc = order(vec![entry("ModA", 10, false), entry("ModB", 20, true)]);
        assert_eq!(set_enabled(&mut doc, &[], "ModA", true).unwrap(), "ModA");
        assert!(doc.entries[0].enabled);
        assert!(doc.entries[1].enabled);
        assert_eq!(doc.entries[0].priority, 10);
        assert_eq!(doc.entries.len(), 2);
    }

    #[test]
    fn disabling_flips_it_back() {
        let mut doc = order(vec![entry("ModA", 10, true)]);
        set_enabled(&mut doc, &[], "moda", false).unwrap();
        assert!(!doc.entries[0].enabled);
    }

    #[test]
    fn a_package_is_found_by_directory_display_name_or_id() {
        let mut doc = order(vec![LoadOrderEntry {
            name: "Better Bodies".to_string(),
            relative_dir: "BetterBodies".to_string(),
            ..entry("x", 10, false)
        }]);
        for key in ["BetterBodies", "better bodies", "id-x"] {
            doc.entries[0].enabled = false;
            set_enabled(&mut doc, &[], key, true).unwrap_or_else(|e| panic!("{key}: {e}"));
            assert!(doc.entries[0].enabled, "for {key}");
        }
    }

    #[test]
    fn an_unknown_name_lists_what_is_there() {
        let mut doc = order(vec![entry("ModA", 10, true)]);
        let error = set_enabled(&mut doc, &[], "Nope", true).unwrap_err();
        assert!(error.contains("Nope"), "{error}");
        assert!(error.contains("ModA"), "{error}");
    }

    #[test]
    fn a_staged_package_with_no_entry_yet_gets_one_appended() {
        let mut doc = order(vec![entry("ModA", 10, true)]);
        let resolved = set_enabled(
            &mut doc,
            &[staged("New Mod", &["new.esp"])],
            "new mod",
            true,
        )
        .expect("staged packages are enablable before the apply that rescans them");
        assert_eq!(resolved, "New Mod");
        assert_eq!(doc.entries.len(), 2);
        let appended = &doc.entries[1];
        assert_eq!(appended.relative_dir, "New Mod");
        assert!(appended.enabled);
        assert_eq!(
            appended.priority, 20,
            "appended below what is already there"
        );
        assert_eq!(appended.plugins.len(), 1);
        assert!(appended.plugins[0].enabled);
        assert_ne!(appended.id, doc.entries[0].id);
    }

    /// `set_enabled` resolves an id before it considers appending, so this
    /// collision is only reachable from `fresh_entry_id` itself — which is
    /// the point of testing it here: the guard has to hold whether or not
    /// today's callers can trip it.
    #[test]
    fn an_appended_entry_does_not_reuse_an_id_already_in_the_document() {
        let doc = order(vec![
            LoadOrderEntry {
                id: "New Mod".to_string(),
                ..entry("ModA", 10, true)
            },
            LoadOrderEntry {
                id: "New Mod-2".to_string(),
                ..entry("ModB", 20, true)
            },
        ]);
        assert_eq!(fresh_entry_id(&doc, "New Mod"), "New Mod-3");
        assert_eq!(fresh_entry_id(&doc, "Other"), "Other");
    }

    #[test]
    fn a_staged_package_can_be_staged_disabled() {
        let mut doc = order(vec![]);
        set_enabled(&mut doc, &[staged("New Mod", &[])], "New Mod", false).unwrap();
        assert!(!doc.entries[0].enabled);
        assert_eq!(doc.entries[0].priority, 10);
    }

    // ---- order ---------------------------------------------------------------------

    #[test]
    fn the_named_entries_move_to_the_front_in_the_order_given() {
        let mut doc = order(vec![
            entry("ModA", 10, true),
            entry("ModB", 20, true),
            entry("ModC", 30, true),
            entry("ModD", 40, true),
        ]);
        reorder(&mut doc, &["ModC".into(), "ModA".into()]).unwrap();
        assert_eq!(
            sequence(&doc),
            vec![
                ("ModC".to_string(), 10),
                ("ModA".to_string(), 20),
                ("ModB".to_string(), 30),
                ("ModD".to_string(), 40),
            ]
        );
    }

    #[test]
    fn the_unnamed_rest_keeps_its_relative_order_by_priority_not_by_position() {
        // Stored out of priority order, the way a hand-edited document can be.
        let mut doc = order(vec![
            entry("ModD", 40, true),
            entry("ModB", 20, true),
            entry("ModC", 30, true),
            entry("ModA", 10, true),
        ]);
        reorder(&mut doc, &["ModD".into()]).unwrap();
        assert_eq!(
            sequence(&doc),
            vec![
                ("ModD".to_string(), 10),
                ("ModA".to_string(), 20),
                ("ModB".to_string(), 30),
                ("ModC".to_string(), 40),
            ]
        );
    }

    #[test]
    fn naming_every_entry_is_a_full_reorder() {
        let mut doc = order(vec![entry("ModA", 10, true), entry("ModB", 20, true)]);
        reorder(&mut doc, &["ModB".into(), "ModA".into()]).unwrap();
        assert_eq!(
            sequence(&doc),
            vec![("ModB".to_string(), 10), ("ModA".to_string(), 20)]
        );
    }

    #[test]
    fn reordering_preserves_enabled_flags_and_plugins() {
        let mut doc = order(vec![entry("ModA", 10, false), entry("ModB", 20, true)]);
        reorder(&mut doc, &["ModB".into()]).unwrap();
        assert!(doc.entries[0].enabled);
        assert!(!doc.entries[1].enabled);
        assert_eq!(doc.entries[1].plugins[0].file, "ModA.esp");
    }

    #[test]
    fn an_unknown_name_in_a_reorder_is_refused_and_changes_nothing() {
        let mut doc = order(vec![entry("ModA", 10, true), entry("ModB", 20, true)]);
        let before = sequence(&doc);
        let error = reorder(&mut doc, &["ModB".into(), "Nope".into()]).unwrap_err();
        assert!(error.contains("Nope"), "{error}");
        assert_eq!(sequence(&doc), before, "a refused reorder is a no-op");
    }

    #[test]
    fn naming_the_same_entry_twice_is_refused_as_a_duplicate() {
        let mut doc = order(vec![entry("ModA", 10, true), entry("ModB", 20, true)]);
        let error = reorder(&mut doc, &["ModA".into(), "moda".into()]).unwrap_err();
        assert!(error.contains("twice"), "{error}");
    }

    #[test]
    fn reordering_an_empty_document_with_no_names_is_a_no_op() {
        let mut doc = order(vec![]);
        reorder(&mut doc, &[]).unwrap();
        assert!(doc.entries.is_empty());
    }
}
