//! The co-admin staging area: `<data dir>/.nerevar/staging/`.
//!
//! A write to `/admin` never touches `data/`. It lands here instead — an
//! uploaded package as `staging/<name>/`, everything else as a line in
//! `staging/pending.json` — and `POST /admin/apply` is the one step that
//! turns the whole set into a new `data/`, a new `load-order.json` and a new
//! manifest. That split is what makes "upload, review, approve" possible with
//! one shared instance and no second copy of the mod tree.
//!
//! Staging lives under `.nerevar/`, which [`should_skip_package_dir`] skips,
//! so a scan of `data/` never sees a staged package and never writes one into
//! the load order behind the admin's back.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::instance_data::{
    nerevar_dir, should_skip_package_dir, validate_package_dir_name, LoadOrder, PackageKind,
    LOAD_ORDER_VERSION,
};

/// Directory under `.nerevar/` holding staged packages and [`PENDING_FILE`].
pub const STAGING_DIR: &str = "staging";

/// The pending change set, next to the staged packages it describes.
pub const PENDING_FILE: &str = "pending.json";

/// On-disk schema version for `pending.json`, alongside `load-order.json`'s,
/// `admins.json`'s and the manifest's.
pub const PENDING_SET_VERSION: u32 = 1;

/// Longest package name an upload may use. Long enough for the wordiest real
/// mod folder, short enough that `data/<name>/<deep path>` stays inside the
/// path limits of every platform the app targets.
pub const MAX_PACKAGE_NAME_LEN: usize = 128;

/// `<data dir>/.nerevar/staging/`.
pub fn staging_dir(data_dir: &Path) -> PathBuf {
    nerevar_dir(data_dir).join(STAGING_DIR)
}

/// Where an upload's extracted tree lives until apply moves it into `data/`.
pub fn staged_package_dir(data_dir: &Path, name: &str) -> PathBuf {
    staging_dir(data_dir).join(name)
}

/// The `.part` file an upload streams into before it is extracted, mirroring
/// the sync client's download naming: a half-written archive is never
/// mistaken for a staged package.
pub fn staged_archive_path(data_dir: &Path, name: &str) -> PathBuf {
    staging_dir(data_dir).join(format!("{name}.part"))
}

/// `<data dir>/.nerevar/staging/pending.json`.
pub fn pending_path(data_dir: &Path) -> PathBuf {
    staging_dir(data_dir).join(PENDING_FILE)
}

/// One uploaded package waiting for apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StagedPackage {
    /// The directory name it will take in `data/`.
    pub name: String,
    /// Mod or Replacer, from the same classification the data-dir scan uses.
    pub kind: PackageKind,
    /// Plugin files found anywhere in the extracted tree, scan's ordering.
    pub plugins: Vec<String>,
    /// Whether `data/<name>/` already exists, i.e. whether apply replaces a
    /// package rather than adding one. Recorded at upload time; both
    /// `GET /admin/status` and the apply planner recompute it against `data/`
    /// as it stands, because a package can appear or vanish in between.
    pub replaces_existing: bool,
    /// Bytes received in the request body (the archive).
    pub archive_bytes: u64,
    /// Bytes of the extracted tree, which is what apply moves into `data/`.
    pub extracted_bytes: u64,
    /// RFC 3339, UTC.
    pub staged_at: String,
    /// Which co-admin uploaded it, for `GET /admin/status`.
    pub staged_by: String,
}

/// Everything an apply would do, as it stands right now.
///
/// Serialized to `pending.json`. Absent file means an empty set: a host that
/// has never staged anything is not an error state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingChanges {
    pub version: u32,
    #[serde(default)]
    pub staged: Vec<StagedPackage>,
    /// Names of packages in `data/` marked for deletion at apply.
    #[serde(default)]
    pub removals: Vec<String>,
    /// The load order to save at apply, or `None` to keep the one on disk
    /// (merged with a fresh scan either way, so a staged package still gets
    /// an entry).
    #[serde(default)]
    pub load_order: Option<LoadOrder>,
}

impl Default for PendingChanges {
    fn default() -> Self {
        Self {
            version: PENDING_SET_VERSION,
            staged: Vec::new(),
            removals: Vec::new(),
            load_order: None,
        }
    }
}

impl PendingChanges {
    /// Whether apply would do nothing but rebuild.
    pub fn is_empty(&self) -> bool {
        self.staged.is_empty() && self.removals.is_empty() && self.load_order.is_none()
    }

    /// Drops a staged package from the set, reporting whether it was there.
    /// The caller deletes its directory; this only edits the record.
    pub fn remove_staged(&mut self, name: &str) -> bool {
        let before = self.staged.len();
        self.staged.retain(|package| package.name != name);
        before != self.staged.len()
    }

    /// Records an upload, replacing any earlier upload under the same name —
    /// re-uploading is how an admin corrects a mistake, not an error.
    pub fn upsert_staged(&mut self, package: StagedPackage) {
        self.remove_staged(&package.name);
        // A name that was marked for removal and is now being uploaded is an
        // ordinary replace: apply installs the new tree over the old one.
        self.removals.retain(|name| name != &package.name);
        self.staged.push(package);
        self.staged.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Marks an existing `data/` package for deletion at apply.
    pub fn mark_removal(&mut self, name: &str) {
        if !self.removals.iter().any(|existing| existing == name) {
            self.removals.push(name.to_string());
            self.removals.sort();
        }
    }
}

/// The name an upload may be stored under.
///
/// One path segment ([`validate_package_dir_name`]), plus the rules only an
/// upload needs: it may not be a directory the scan treats as special
/// (`.nerevar`, `tes3mp`, `data`, anything dot-leading), and it has a length
/// cap. Returns the trimmed name to use.
pub fn validate_staged_package_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    validate_package_dir_name(trimmed)?;
    if trimmed.len() > MAX_PACKAGE_NAME_LEN {
        return Err(format!(
            "Package name is longer than {MAX_PACKAGE_NAME_LEN} characters"
        ));
    }
    if should_skip_package_dir(trimmed) {
        return Err(format!(
            "\"{trimmed}\" is a reserved directory name, not a package"
        ));
    }
    Ok(trimmed.to_string())
}

/// Reads `pending.json`. A missing file is an empty set.
pub fn load_pending(data_dir: &Path) -> Result<PendingChanges, String> {
    let path = pending_path(data_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PendingChanges::default())
        }
        Err(error) => return Err(format!("Failed to read {}: {error}", path.display())),
    };
    serde_json::from_str(&raw).map_err(|error| format!("Invalid {}: {error}", path.display()))
}

/// Writes `pending.json` atomically (`.tmp` → rename), creating the staging
/// directory if this is the first stage.
pub fn save_pending(data_dir: &Path, pending: &PendingChanges) -> Result<(), String> {
    let dir = staging_dir(data_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("Failed to create {}: {error}", dir.display()))?;

    let path = pending_path(data_dir);
    let contents = serde_json::to_string_pretty(pending)
        .map_err(|error| format!("Failed to serialize the pending change set: {error}"))?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, contents)
        .map_err(|error| format!("Failed to write {}: {error}", temp.display()))?;
    std::fs::rename(&temp, &path)
        .map_err(|error| format!("Failed to finalize {}: {error}", path.display()))
}

/// Deletes the whole staging directory: every staged tree, every half-written
/// `.part`, and `pending.json`. What `POST /admin/discard` does, and what
/// apply does once the change set has landed in `data/`.
pub fn clear_staging(data_dir: &Path) -> Result<(), String> {
    let dir = staging_dir(data_dir);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Failed to clear {}: {error}", dir.display())),
    }
}

/// The package directory names currently in `data/`, sorted.
///
/// Directory entries only — no plugin search, no hashing — because every
/// caller here only needs to know which names are taken.
pub fn data_package_names(data_dir: &Path) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    let entries = match std::fs::read_dir(data_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(names),
        Err(error) => return Err(format!("Failed to read {}: {error}", data_dir.display())),
    };

    for entry in entries {
        let entry =
            entry.map_err(|error| format!("Failed to read {}: {error}", data_dir.display()))?;
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if should_skip_package_dir(&name) {
            continue;
        }
        names.push(name);
    }

    names.sort();
    Ok(names)
}

/// Total size of a directory tree in bytes, following no symlinks.
pub fn tree_size_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            total = total.saturating_add(tree_size_bytes(&entry.path()));
        } else if kind.is_file() {
            total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
        }
    }
    total
}

/// Checks a proposed pending load order against the packages that will exist
/// after apply.
///
/// `available` is the set of package directory names an apply would leave in
/// `data/`: what is there now, plus what is staged, minus what is marked for
/// removal. Everything else is the shape rules the on-disk load order already
/// obeys — one path segment per entry, no duplicate entries or plugins, a
/// version this build understands — checked here because a hand-written admin
/// body is the first thing that can break them.
pub fn validate_pending_load_order(
    load_order: &LoadOrder,
    available: &[String],
) -> Result<(), String> {
    if load_order.version != 0 && load_order.version != LOAD_ORDER_VERSION {
        return Err(format!(
            "Unsupported load order version {} (this host writes version {LOAD_ORDER_VERSION})",
            load_order.version
        ));
    }

    let available: HashSet<String> = available.iter().map(|name| name.to_lowercase()).collect();
    let mut seen_dirs: HashSet<String> = HashSet::new();
    let mut seen_ids: HashSet<&str> = HashSet::new();

    for entry in &load_order.entries {
        validate_package_dir_name(&entry.relative_dir)
            .map_err(|reason| format!("Load order entry \"{}\": {reason}", entry.name))?;

        let key = entry.relative_dir.to_lowercase();
        if !seen_dirs.insert(key.clone()) {
            return Err(format!(
                "Load order lists \"{}\" more than once",
                entry.relative_dir
            ));
        }
        if !available.contains(&key) {
            return Err(format!(
                "Load order references unknown package \"{}\" — it is neither in the data \
                 directory nor staged",
                entry.relative_dir
            ));
        }

        if entry.id.trim().is_empty() {
            return Err(format!(
                "Load order entry \"{}\" has an empty id",
                entry.relative_dir
            ));
        }
        if !seen_ids.insert(entry.id.as_str()) {
            return Err(format!("Load order reuses entry id \"{}\"", entry.id));
        }

        let mut seen_plugins: HashSet<String> = HashSet::new();
        for plugin in &entry.plugins {
            if plugin.file.trim().is_empty() {
                return Err(format!(
                    "Load order entry \"{}\" has an empty plugin name",
                    entry.relative_dir
                ));
            }
            if plugin.file.contains('/') || plugin.file.contains('\\') {
                return Err(format!(
                    "Plugin \"{}\" in \"{}\" must be a file name, not a path",
                    plugin.file, entry.relative_dir
                ));
            }
            if !seen_plugins.insert(plugin.file.to_lowercase()) {
                return Err(format!(
                    "Load order entry \"{}\" lists plugin \"{}\" more than once",
                    entry.relative_dir, plugin.file
                ));
            }
        }
    }

    Ok(())
}

/// The package names an apply would leave in `data/`: what is there now, plus
/// what is staged, minus what is marked for removal.
pub fn available_package_names(
    data_dir: &Path,
    pending: &PendingChanges,
) -> Result<Vec<String>, String> {
    let mut names = data_package_names(data_dir)?;
    for package in &pending.staged {
        if !names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&package.name))
        {
            names.push(package.name.clone());
        }
    }
    names.retain(|name| {
        !pending
            .removals
            .iter()
            .any(|removed| removed.eq_ignore_ascii_case(name))
    });
    names.sort();
    Ok(names)
}

/// Timestamp helper so every staged record and audit line agrees on format.
pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance_data::{LoadOrderEntry, PluginEntry};

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-staging-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn staged(name: &str, replaces: bool) -> StagedPackage {
        StagedPackage {
            name: name.to_string(),
            kind: PackageKind::Mod,
            plugins: vec!["thing.esp".to_string()],
            replaces_existing: replaces,
            archive_bytes: 128,
            extracted_bytes: 64,
            staged_at: now_rfc3339(),
            staged_by: "ada".to_string(),
        }
    }

    fn entry(id: &str, relative_dir: &str, plugins: &[&str]) -> LoadOrderEntry {
        LoadOrderEntry {
            id: id.to_string(),
            name: relative_dir.to_string(),
            kind: PackageKind::Mod,
            relative_dir: relative_dir.to_string(),
            enabled: true,
            priority: 10,
            plugins: plugins
                .iter()
                .map(|file| PluginEntry {
                    file: (*file).to_string(),
                    enabled: true,
                })
                .collect(),
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

    #[test]
    fn an_absent_pending_file_reads_as_an_empty_set() {
        let scratch = scratch("absent");
        let pending = load_pending(&scratch.0).unwrap();
        assert!(pending.is_empty());
        assert_eq!(pending.version, PENDING_SET_VERSION);
    }

    #[test]
    fn a_pending_set_round_trips_through_disk() {
        let scratch = scratch("round-trip");
        let mut pending = PendingChanges::default();
        pending.upsert_staged(staged("Better Bodies", true));
        pending.mark_removal("Old Mod");
        pending.load_order = Some(order(vec![entry("e1", "Better Bodies", &["thing.esp"])]));

        save_pending(&scratch.0, &pending).unwrap();
        let read_back = load_pending(&scratch.0).unwrap();
        assert_eq!(read_back, pending);

        // The file is where the routes and the docs say it is.
        assert!(pending_path(&scratch.0).is_file());
        assert!(!pending_path(&scratch.0).with_extension("json.tmp").exists());
    }

    #[test]
    fn re_uploading_a_name_replaces_its_record_and_unmarks_removal() {
        let mut pending = PendingChanges::default();
        pending.mark_removal("Better Bodies");
        pending.upsert_staged(staged("Better Bodies", true));
        assert_eq!(pending.staged.len(), 1);
        assert!(pending.removals.is_empty());

        let mut second = staged("Better Bodies", true);
        second.plugins = vec!["other.esp".to_string()];
        pending.upsert_staged(second);
        assert_eq!(pending.staged.len(), 1);
        assert_eq!(pending.staged[0].plugins, vec!["other.esp".to_string()]);

        assert!(pending.remove_staged("Better Bodies"));
        assert!(!pending.remove_staged("Better Bodies"));
        assert!(pending.is_empty());
    }

    #[test]
    fn a_name_must_be_one_ordinary_path_segment() {
        assert_eq!(
            validate_staged_package_name("  Better Bodies  ").unwrap(),
            "Better Bodies"
        );
        for bad in [
            "",
            "   ",
            "../escape",
            "nested/mod",
            "back\\slash",
            "C:evil",
            ".nerevar",
            ".hidden",
            "tes3mp",
            "data",
            "DATA",
        ] {
            assert!(
                validate_staged_package_name(bad).is_err(),
                "{bad:?} should be refused"
            );
        }
        assert!(validate_staged_package_name(&"a".repeat(MAX_PACKAGE_NAME_LEN)).is_ok());
        assert!(validate_staged_package_name(&"a".repeat(MAX_PACKAGE_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn staged_paths_live_under_the_nerevar_dir_so_a_scan_never_sees_them() {
        let data_dir = Path::new("/srv/instance/data");
        let staged = staged_package_dir(data_dir, "Better Bodies");
        assert!(staged.starts_with(nerevar_dir(data_dir)));
        assert!(staged_archive_path(data_dir, "Better Bodies").starts_with(nerevar_dir(data_dir)));
        assert!(pending_path(data_dir).starts_with(nerevar_dir(data_dir)));
        // And the directory that holds them is one a data-dir scan skips.
        assert!(should_skip_package_dir(
            nerevar_dir(data_dir).file_name().unwrap().to_str().unwrap()
        ));
    }

    #[test]
    fn data_package_names_lists_package_dirs_only() {
        let scratch = scratch("names");
        for dir in ["Better Bodies", "Rock Replacer", ".nerevar", "tes3mp"] {
            std::fs::create_dir_all(scratch.0.join(dir)).unwrap();
        }
        std::fs::write(scratch.0.join("loose.txt"), b"x").unwrap();
        assert_eq!(
            data_package_names(&scratch.0).unwrap(),
            vec!["Better Bodies".to_string(), "Rock Replacer".to_string()]
        );
    }

    #[test]
    fn available_names_add_staged_and_drop_removed() {
        let scratch = scratch("available");
        std::fs::create_dir_all(scratch.0.join("Old Mod")).unwrap();
        std::fs::create_dir_all(scratch.0.join("Kept")).unwrap();

        let mut pending = PendingChanges::default();
        pending.upsert_staged(staged("New Mod", false));
        pending.mark_removal("Old Mod");

        assert_eq!(
            available_package_names(&scratch.0, &pending).unwrap(),
            vec!["Kept".to_string(), "New Mod".to_string()]
        );
    }

    #[test]
    fn a_load_order_may_reference_staged_packages_but_not_unknown_ones() {
        let available = vec!["Better Bodies".to_string()];
        assert!(validate_pending_load_order(
            &order(vec![entry("e1", "Better Bodies", &[])]),
            &available
        )
        .is_ok());
        let unknown =
            validate_pending_load_order(&order(vec![entry("e1", "Ghost Mod", &[])]), &available)
                .unwrap_err();
        assert!(unknown.contains("Ghost Mod"), "{unknown}");
    }

    #[test]
    fn a_load_order_is_refused_when_its_shape_is_wrong() {
        let available = vec!["A".to_string(), "B".to_string()];

        let duplicate = order(vec![entry("e1", "A", &[]), entry("e2", "a", &[])]);
        assert!(validate_pending_load_order(&duplicate, &available).is_err());

        let reused_id = order(vec![entry("e1", "A", &[]), entry("e1", "B", &[])]);
        assert!(validate_pending_load_order(&reused_id, &available).is_err());

        let empty_id = order(vec![entry("", "A", &[])]);
        assert!(validate_pending_load_order(&empty_id, &available).is_err());

        let traversal = order(vec![entry("e1", "../A", &[])]);
        assert!(validate_pending_load_order(&traversal, &available).is_err());

        let duplicate_plugin = order(vec![entry("e1", "A", &["x.esp", "X.ESP"])]);
        assert!(validate_pending_load_order(&duplicate_plugin, &available).is_err());

        let plugin_path = order(vec![entry("e1", "A", &["sub/x.esp"])]);
        assert!(validate_pending_load_order(&plugin_path, &available).is_err());

        let mut wrong_version = order(vec![]);
        wrong_version.version = LOAD_ORDER_VERSION + 7;
        assert!(validate_pending_load_order(&wrong_version, &available).is_err());
    }

    #[test]
    fn clearing_staging_removes_everything_and_is_idempotent() {
        let scratch = scratch("clear");
        let mut pending = PendingChanges::default();
        pending.upsert_staged(staged("New Mod", false));
        save_pending(&scratch.0, &pending).unwrap();
        std::fs::create_dir_all(staged_package_dir(&scratch.0, "New Mod")).unwrap();

        clear_staging(&scratch.0).unwrap();
        assert!(!staging_dir(&scratch.0).exists());
        clear_staging(&scratch.0).unwrap();
        assert!(load_pending(&scratch.0).unwrap().is_empty());
    }

    #[test]
    fn tree_size_adds_up_nested_files() {
        let scratch = scratch("size");
        std::fs::create_dir_all(scratch.0.join("meshes")).unwrap();
        std::fs::write(scratch.0.join("a.esp"), b"1234").unwrap();
        std::fs::write(scratch.0.join("meshes/b.nif"), b"123456").unwrap();
        assert_eq!(tree_size_bytes(&scratch.0), 10);
    }
}
