//! `POST /admin/apply`: the one step that changes `data/`.
//!
//! Everything else under `/admin` writes to the staging area. Apply reads the
//! pending set, works out the file operations it implies ([`plan_apply`], a
//! pure function so the decision is testable without a filesystem), performs
//! them, then rebuilds the load order and the manifest so what is served
//! matches what is on disk.
//!
//! Apply does **not** restart TES3MP. The dedicated server reads its plugin
//! list once at start, so after an apply a running server is still enforcing
//! the old list; the caller logs that and `GET /admin/status` reports it.

use std::path::Path;

use crate::instance_data::{
    build_manifest, load_load_order, package_abs_path, save_load_order, scan_and_merge_load_order,
    validate_package_dir_name, LoadOrder, NerevarManifest,
};

use super::staging::{
    clear_staging, data_package_names, load_pending, staged_package_dir, PendingChanges,
};

/// One staged package moving into `data/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedInstall {
    /// Directory name in both `staging/` and `data/`.
    pub name: String,
    /// The existing `data/` directory this replaces, under the exact name it
    /// has on disk (which may differ from `name` only in case). `None` for a
    /// package the host has never had.
    pub replaces: Option<String>,
}

/// The file operations one apply performs, in the order it performs them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplyPlan {
    /// `data/` directories to delete, from `DELETE /admin/packages/{name}`.
    pub removals: Vec<String>,
    /// Staged trees to move into `data/`.
    pub installs: Vec<PlannedInstall>,
    /// Marked for removal but no longer in `data/` — somebody deleted the
    /// directory by hand between the mark and the apply. Not an error: the
    /// outcome the admin asked for already holds.
    pub already_gone: Vec<String>,
    /// Whether a pending load order will be written before the rescan.
    pub writes_load_order: bool,
}

impl ApplyPlan {
    /// Whether apply would move no files at all, i.e. is a pure regenerate.
    pub fn is_noop(&self) -> bool {
        self.removals.is_empty() && self.installs.is_empty() && !self.writes_load_order
    }
}

/// Works out what an apply would do, from the names currently in `data/` and
/// the pending set. Pure: no filesystem access, so the decision can be tested
/// on its own.
///
/// `data_packages` is the package directory names in `data/` (see
/// `staging::data_package_names`). Names are matched case-insensitively,
/// because the filesystems this runs on disagree about whether `Better
/// Bodies` and `better bodies` are one directory.
pub fn plan_apply(data_packages: &[String], pending: &PendingChanges) -> Result<ApplyPlan, String> {
    let existing = |name: &str| -> Option<String> {
        data_packages
            .iter()
            .find(|candidate| candidate.eq_ignore_ascii_case(name))
            .cloned()
    };

    let mut plan = ApplyPlan {
        writes_load_order: pending.load_order.is_some(),
        ..ApplyPlan::default()
    };

    for name in &pending.removals {
        validate_package_dir_name(name)
            .map_err(|reason| format!("Pending removal \"{name}\": {reason}"))?;
        if pending
            .staged
            .iter()
            .any(|package| package.name.eq_ignore_ascii_case(name))
        {
            // Not reachable through the routes (an upload unmarks a removal,
            // and a delete of a staged package drops it from staging), so a
            // set that says both has been hand-edited into a contradiction.
            return Err(format!(
                "The pending set both stages and removes \"{name}\"; discard and start again"
            ));
        }
        match existing(name) {
            Some(actual) => plan.removals.push(actual),
            None => plan.already_gone.push(name.clone()),
        }
    }

    for package in &pending.staged {
        validate_package_dir_name(&package.name)
            .map_err(|reason| format!("Staged package \"{}\": {reason}", package.name))?;
        plan.installs.push(PlannedInstall {
            name: package.name.clone(),
            replaces: existing(&package.name),
        });
    }

    plan.removals.sort();
    plan.already_gone.sort();
    plan.installs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(plan)
}

/// What one apply did.
pub struct ApplyOutcome {
    pub plan: ApplyPlan,
    /// The manifest now being served.
    pub manifest: NerevarManifest,
}

/// Performs the apply against `data_dir` and rebuilds the manifest.
///
/// Blocking and CPU-heavy — the manifest rebuild hashes every enabled package
/// — so the route runs this on a blocking thread, behind the server's apply
/// mutex. It is the only thing in the process that writes `data/`,
/// `load-order.json` or `manifest.json`.
///
/// Staging is cleared only after the manifest is built: a rebuild that fails
/// leaves the change set intact so the admin can look, fix and re-apply,
/// rather than losing an upload to a bad load order.
pub fn execute_apply(
    instance_id: &str,
    instance_name: &str,
    instance_root: &Path,
    data_dir: &Path,
) -> Result<ApplyOutcome, String> {
    let pending = load_pending(data_dir)?;
    let plan = plan_apply(&data_package_names(data_dir)?, &pending)?;

    for name in &plan.removals {
        let path = package_abs_path(data_dir, name);
        std::fs::remove_dir_all(&path)
            .map_err(|error| format!("Failed to delete {}: {error}", path.display()))?;
    }

    for install in &plan.installs {
        let source = staged_package_dir(data_dir, &install.name);
        if !source.is_dir() {
            return Err(format!(
                "Staged package \"{}\" is recorded in pending.json but its directory is gone",
                install.name
            ));
        }
        // Replacing means removing first: a rename onto an existing directory
        // fails on every platform, and a merge would leave files from the old
        // version that the new one no longer ships.
        if let Some(replaced) = &install.replaces {
            let path = package_abs_path(data_dir, replaced);
            std::fs::remove_dir_all(&path)
                .map_err(|error| format!("Failed to replace {}: {error}", path.display()))?;
        }
        let destination = package_abs_path(data_dir, &install.name);
        std::fs::rename(&source, &destination).map_err(|error| {
            format!(
                "Failed to move {} into {}: {error}",
                source.display(),
                destination.display()
            )
        })?;
    }

    if let Some(load_order) = &pending.load_order {
        save_load_order(data_dir, load_order)?;
    }

    // The rescan is what gives a newly installed package its entry and drops
    // the entries of packages this apply deleted; it saves the result, so the
    // pending load order above is the starting point, not the last word.
    let load_order = scan_and_merge_load_order(data_dir, &mut None)?;
    let manifest = build_manifest(
        instance_id,
        instance_name,
        instance_root,
        data_dir,
        &load_order,
        &mut None,
    )?;

    clear_staging(data_dir)?;

    Ok(ApplyOutcome { plan, manifest })
}

/// The instance name to build the manifest under.
///
/// The hosting state records an id but no name — the only place a name is
/// written is the manifest itself — so an apply reuses the name the current
/// manifest carries and falls back to the id when there is no manifest yet
/// (a first apply on a `--no-manifest-rebuild` host).
pub fn instance_name_for_rebuild(data_dir: &Path, instance_id: &str) -> String {
    crate::instance_data::load_manifest(data_dir)
        .map(|manifest| manifest.instance_name)
        .unwrap_or_else(|_| instance_id.to_string())
}

/// The load order an admin would edit: what is on disk now, and the pending
/// one if a `POST /admin/load-order` has been staged.
pub fn current_and_pending_load_order(
    data_dir: &Path,
) -> Result<(LoadOrder, Option<LoadOrder>), String> {
    let current = load_load_order(data_dir)?;
    let pending = load_pending(data_dir)?.load_order;
    Ok((current, pending))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::staging::{now_rfc3339, StagedPackage};
    use crate::instance_data::{PackageKind, LOAD_ORDER_VERSION};

    fn staged(name: &str) -> StagedPackage {
        StagedPackage {
            name: name.to_string(),
            kind: PackageKind::Mod,
            plugins: Vec::new(),
            replaces_existing: false,
            archive_bytes: 0,
            extracted_bytes: 0,
            staged_at: now_rfc3339(),
            staged_by: "ada".to_string(),
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn an_empty_pending_set_plans_nothing() {
        let plan = plan_apply(&names(&["Better Bodies"]), &PendingChanges::default()).unwrap();
        assert!(plan.is_noop());
        assert_eq!(plan, ApplyPlan::default());
    }

    #[test]
    fn a_staged_package_installs_and_reports_whether_it_replaces() {
        let mut pending = PendingChanges::default();
        pending.upsert_staged(staged("Better Bodies"));
        pending.upsert_staged(staged("New Mod"));

        let plan = plan_apply(&names(&["Better Bodies", "Untouched"]), &pending).unwrap();
        assert_eq!(
            plan.installs,
            vec![
                PlannedInstall {
                    name: "Better Bodies".to_string(),
                    replaces: Some("Better Bodies".to_string()),
                },
                PlannedInstall {
                    name: "New Mod".to_string(),
                    replaces: None,
                },
            ]
        );
        assert!(plan.removals.is_empty());
        assert!(!plan.is_noop());
    }

    /// The directory to delete is the one that is really on disk, so a
    /// case-insensitive filesystem does not leave the old spelling behind.
    #[test]
    fn a_replace_names_the_directory_as_it_is_spelled_on_disk() {
        let mut pending = PendingChanges::default();
        pending.upsert_staged(staged("better bodies"));
        let plan = plan_apply(&names(&["Better Bodies"]), &pending).unwrap();
        assert_eq!(
            plan.installs[0].replaces.as_deref(),
            Some("Better Bodies"),
            "the plan must name the directory that exists"
        );
    }

    #[test]
    fn a_removal_of_something_already_gone_is_recorded_not_refused() {
        let mut pending = PendingChanges::default();
        pending.mark_removal("Ghost Mod");
        pending.mark_removal("Old Mod");

        let plan = plan_apply(&names(&["Old Mod"]), &pending).unwrap();
        assert_eq!(plan.removals, names(&["Old Mod"]));
        assert_eq!(plan.already_gone, names(&["Ghost Mod"]));
    }

    #[test]
    fn a_pending_load_order_alone_makes_the_apply_more_than_a_regenerate() {
        let pending = PendingChanges {
            load_order: Some(LoadOrder {
                version: LOAD_ORDER_VERSION,
                base_game_data: None,
                content_order: None,
                entries: Vec::new(),
            }),
            ..PendingChanges::default()
        };
        let plan = plan_apply(&[], &pending).unwrap();
        assert!(plan.writes_load_order);
        assert!(!plan.is_noop());
    }

    #[test]
    fn a_hand_edited_set_that_both_stages_and_removes_a_name_is_refused() {
        let mut pending = PendingChanges::default();
        pending.upsert_staged(staged("Mod"));
        pending.removals.push("Mod".to_string());
        let error = plan_apply(&names(&["Mod"]), &pending).unwrap_err();
        assert!(error.contains("both stages and removes"), "{error}");
    }

    #[test]
    fn a_traversal_in_a_hand_edited_set_never_becomes_a_file_operation() {
        let mut escape = PendingChanges::default();
        escape.removals.push("../data".to_string());
        assert!(plan_apply(&[], &escape).is_err());

        let mut staged_escape = PendingChanges::default();
        staged_escape.staged.push(staged("../elsewhere"));
        assert!(plan_apply(&[], &staged_escape).is_err());
    }
}
