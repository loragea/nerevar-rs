//! The admin store: `<data dir>/.nerevar/admins.json`.
//!
//! Owned by the headless daemon and its `nerevar-host admin` subcommands. The
//! GUI rewrites `config.json` wholesale and never touches this file, so an
//! operator can hand out co-admin tokens without the desktop app undoing it.
//!
//! A record stores `sha256(token)`, never the token: `admin add` prints the
//! token once and forgets it. Tokens are 32 bytes from the OS CSPRNG rendered
//! as 64 lower-case hex characters — URL-safe by construction, and high enough
//! entropy that a plain SHA-256 needs no password-hashing crate. Comparison
//! goes through the same constant-time helper the sync password uses.
//!
//! The file is re-read on every authenticated request, so `admin add` and
//! `admin revoke` take effect against a running daemon with no reload signal.
//! Writes are atomic (`admins.json.tmp` → rename) and 0600 on Unix.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::admin::capability::{known_role_names, role_is_known};
use crate::instance_data::checksum::hex_encode;
use crate::instance_data::nerevar_dir;
use crate::sync_auth::constant_time_eq;

/// File name under `<data dir>/.nerevar/`.
pub const ADMINS_FILE: &str = "admins.json";

/// On-disk schema version, alongside `load-order.json`'s and the manifest's.
pub const ADMIN_STORE_VERSION: u32 = 1;

/// Token length in bytes before hex encoding. 256 bits: brute force is not a
/// threat model, so the hash needs no salt or work factor.
pub const ADMIN_TOKEN_BYTES: usize = 32;

/// Where the store lives for an instance whose package data directory is
/// `data_dir`.
pub fn admins_file_path(data_dir: &Path) -> PathBuf {
    nerevar_dir(data_dir).join(ADMINS_FILE)
}

/// One co-admin. Deliberately not exported to TypeScript: it is daemon-side
/// credential material, not a frontend contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminRecord {
    /// Unique, non-empty, trimmed. Identifies the admin in audit lines and to
    /// `admin revoke`.
    pub name: String,
    /// A role name looked up in `capability::ROLE_TABLE`. A name the table
    /// does not know authenticates nothing.
    pub role: String,
    /// Lower-case hex SHA-256 of the token. The token itself is never stored.
    pub token_sha256: String,
    /// RFC 3339, UTC.
    pub created_at: String,
}

/// The whole file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminStore {
    pub version: u32,
    #[serde(default)]
    pub admins: Vec<AdminRecord>,
}

impl Default for AdminStore {
    fn default() -> Self {
        Self {
            version: ADMIN_STORE_VERSION,
            admins: Vec::new(),
        }
    }
}

impl AdminStore {
    /// The record named `name`, if any. Names are compared exactly (after the
    /// trimming `add_admin` applies), so `Ada` and `ada` are two admins.
    pub fn find_by_name(&self, name: &str) -> Option<&AdminRecord> {
        self.admins.iter().find(|record| record.name == name)
    }

    /// The record `token` authenticates as, if any.
    ///
    /// Every record is examined — no early exit — and each comparison runs
    /// through [`constant_time_eq`] over the hex digests. A record whose role
    /// the capability table does not know authenticates nothing, so an
    /// operator's typo in `admins.json` fails closed instead of granting an
    /// unnamed role whatever the first route happens to check.
    pub fn authenticate(&self, token: &str) -> Option<&AdminRecord> {
        if token.is_empty() {
            return None;
        }
        let presented = token_hash(token);
        let mut matched: Option<&AdminRecord> = None;
        for record in &self.admins {
            let same = constant_time_eq(presented.as_bytes(), record.token_sha256.as_bytes());
            if same && role_is_known(&record.role) {
                matched = Some(record);
            }
        }
        matched
    }
}

/// A freshly created admin plus the one and only sight of its token.
#[derive(Debug, Clone)]
pub struct NewAdmin {
    pub record: AdminRecord,
    /// Show once, then discard. Only its hash is on disk.
    pub token: String,
}

/// 32 CSPRNG bytes as 64 lower-case hex characters.
pub fn generate_token() -> Result<String, String> {
    let mut bytes = [0u8; ADMIN_TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|error| {
        format!("Failed to draw {ADMIN_TOKEN_BYTES} random bytes for an admin token: {error}")
    })?;
    Ok(hex_encode(bytes))
}

/// Lower-case hex SHA-256 of a token's bytes. What a record stores and what
/// [`AdminStore::authenticate`] compares.
pub fn token_hash(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex_encode(hasher.finalize())
}

/// Reads the store. A missing file is an empty store, not an error: a host
/// that has never created an admin simply has no admins.
pub fn load_admins(data_dir: &Path) -> Result<AdminStore, String> {
    let path = admins_file_path(data_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AdminStore::default())
        }
        Err(error) => return Err(format!("Failed to read {}: {error}", path.display())),
    };

    let store: AdminStore = serde_json::from_str(&raw)
        .map_err(|error| format!("Failed to parse {}: {error}", path.display()))?;
    warn_about_unknown_roles(&path, &store);
    Ok(store)
}

/// Writes the store atomically: a temporary file in the same directory, 0600
/// on Unix, then a rename over the real path. A reader — including a daemon
/// mid-request — sees either the old file or the new one, never a truncated
/// one, and a failed write leaves no `.tmp` behind.
pub fn save_admins(data_dir: &Path, store: &AdminStore) -> Result<(), String> {
    let dir = nerevar_dir(data_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("Failed to create {}: {error}", dir.display()))?;

    let path = admins_file_path(data_dir);
    let temp = temp_path(&path);
    let json = serde_json::to_string_pretty(store)
        .map_err(|error| format!("Failed to serialize the admin store: {error}"))?;

    // Remove first: the private mode below is applied at creation only, so
    // reusing a leftover temp file could keep whatever mode it had.
    let _ = std::fs::remove_file(&temp);
    if let Err(error) = write_private(&temp, json.as_bytes()) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!(
            "Failed to replace {} with {}: {error}",
            path.display(),
            temp.display()
        ));
    }
    Ok(())
}

/// Creates an admin and returns its token, which is shown once and never
/// recoverable. Refuses an empty name, a name already in the file, and a role
/// the capability table does not know.
pub fn add_admin(data_dir: &Path, name: &str, role: &str) -> Result<NewAdmin, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("An admin name cannot be empty".to_string());
    }
    let role = role.trim();
    if !role_is_known(role) {
        return Err(format!(
            "Unknown role \"{role}\". Known roles: {}",
            known_role_names().join(", ")
        ));
    }

    let mut store = load_admins(data_dir)?;
    if store.find_by_name(name).is_some() {
        return Err(format!("An admin named \"{name}\" already exists"));
    }

    let token = generate_token()?;
    let record = AdminRecord {
        name: name.to_string(),
        role: role.to_string(),
        token_sha256: token_hash(&token),
        created_at: Utc::now().to_rfc3339(),
    };

    store.version = ADMIN_STORE_VERSION;
    store.admins.push(record.clone());
    save_admins(data_dir, &store)?;

    Ok(NewAdmin { record, token })
}

/// Removes the admin named `name`, revoking its token. Returns whether a
/// record was actually removed.
pub fn revoke_admin(data_dir: &Path, name: &str) -> Result<bool, String> {
    let name = name.trim();
    let mut store = load_admins(data_dir)?;
    let before = store.admins.len();
    store.admins.retain(|record| record.name != name);
    if store.admins.len() == before {
        return Ok(false);
    }
    store.version = ADMIN_STORE_VERSION;
    save_admins(data_dir, &store)?;
    Ok(true)
}

/// Every admin, in file order. Hashes are in the records; callers that print
/// must not.
pub fn list_admins(data_dir: &Path) -> Result<Vec<AdminRecord>, String> {
    Ok(load_admins(data_dir)?.admins)
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Creates `path` readable and writable by its owner only, on platforms that
/// have file modes. Windows gets the default ACL — the daemon is a Linux
/// story, and a mode-less create is better than not compiling there.
fn write_private(path: &Path, contents: &[u8]) -> Result<(), String> {
    use std::io::Write as _;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    let mut file = options
        .open(path)
        .map_err(|error| format!("Failed to create {}: {error}", path.display()))?;
    file.write_all(contents)
        .map_err(|error| format!("Failed to write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("Failed to flush {}: {error}", path.display()))?;
    Ok(())
}

/// Warns once per process about each unknown role name seen in a store file.
///
/// The daemon re-reads `admins.json` on every authenticated request, so an
/// unconditional warning would repeat forever; a record with a role the table
/// does not know is a static configuration mistake, worth saying exactly once.
fn warn_about_unknown_roles(path: &Path, store: &AdminStore) {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(HashSet::new()));

    for record in &store.admins {
        if role_is_known(&record.role) {
            continue;
        }
        let key = format!("{}\u{0}{}", path.display(), record.role);
        let mut seen = match warned.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if seen.insert(key) {
            log::warn!(
                "{}: role \"{}\" is not a role this build knows, so admins holding it \
                 authenticate nothing. Known roles: {}",
                path.display(),
                record.role,
                known_role_names().join(", ")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::capability::ROLE_ADMIN;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-admin-store-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    #[test]
    fn a_missing_file_reads_as_an_empty_store() {
        let scratch = scratch("missing");
        let store = load_admins(&scratch.0).expect("missing file is not an error");
        assert!(store.admins.is_empty());
        assert_eq!(store.version, ADMIN_STORE_VERSION);
        assert!(store.authenticate("anything").is_none());
    }

    #[test]
    fn an_added_admin_round_trips_and_its_token_authenticates() {
        let scratch = scratch("roundtrip");
        let created = add_admin(&scratch.0, "ada", ROLE_ADMIN).expect("add");

        assert_eq!(created.token.len(), ADMIN_TOKEN_BYTES * 2);
        assert!(created.token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(created.record.name, "ada");
        assert_eq!(created.record.role, ROLE_ADMIN);
        assert_eq!(created.record.token_sha256, token_hash(&created.token));
        assert!(!created.record.created_at.is_empty());
        // The token is never written; only its digest is.
        let raw = std::fs::read_to_string(admins_file_path(&scratch.0)).unwrap();
        assert!(
            !raw.contains(&created.token),
            "the token must not be stored"
        );

        let reloaded = load_admins(&scratch.0).expect("reload");
        assert_eq!(reloaded.admins, vec![created.record.clone()]);
        let authenticated = reloaded.authenticate(&created.token).expect("authenticate");
        assert_eq!(authenticated.name, "ada");
        assert!(reloaded.authenticate("not-the-token").is_none());
        assert!(reloaded.authenticate("").is_none());
    }

    #[test]
    fn names_are_trimmed_and_must_be_unique_and_non_empty() {
        let scratch = scratch("unique");
        add_admin(&scratch.0, "  ada  ", ROLE_ADMIN).expect("add");
        assert_eq!(load_admins(&scratch.0).unwrap().admins[0].name, "ada");

        let duplicate = add_admin(&scratch.0, "ada", ROLE_ADMIN).unwrap_err();
        assert!(duplicate.contains("already exists"), "{duplicate}");
        // The refused add must not have appended anything.
        assert_eq!(load_admins(&scratch.0).unwrap().admins.len(), 1);

        assert!(add_admin(&scratch.0, "   ", ROLE_ADMIN).is_err());
        let unknown = add_admin(&scratch.0, "bob", "wizard").unwrap_err();
        assert!(unknown.contains("Unknown role"), "{unknown}");
        assert_eq!(load_admins(&scratch.0).unwrap().admins.len(), 1);
    }

    #[test]
    fn revoking_removes_the_record_and_kills_the_token() {
        let scratch = scratch("revoke");
        let ada = add_admin(&scratch.0, "ada", ROLE_ADMIN).expect("add ada");
        let bob = add_admin(&scratch.0, "bob", ROLE_ADMIN).expect("add bob");

        assert!(revoke_admin(&scratch.0, "ada").expect("revoke"));
        let store = load_admins(&scratch.0).expect("reload");
        assert_eq!(store.admins.len(), 1);
        assert!(store.authenticate(&ada.token).is_none(), "revoked token");
        assert!(store.authenticate(&bob.token).is_some(), "untouched token");

        // Revoking a name that is not there reports so rather than failing.
        assert!(!revoke_admin(&scratch.0, "ada").expect("second revoke"));
        assert!(!revoke_admin(&scratch.0, "nobody").expect("absent revoke"));
    }

    #[test]
    fn a_record_with_an_unknown_role_authenticates_nothing() {
        let scratch = scratch("unknown-role");
        let token = generate_token().expect("token");
        let store = AdminStore {
            version: ADMIN_STORE_VERSION,
            admins: vec![AdminRecord {
                name: "stranded".to_string(),
                role: "contributor".to_string(),
                token_sha256: token_hash(&token),
                created_at: Utc::now().to_rfc3339(),
            }],
        };
        save_admins(&scratch.0, &store).expect("save");

        // Loading succeeds — a role this build does not know is a warning, not
        // a corrupt file — but the record grants nothing.
        let reloaded = load_admins(&scratch.0).expect("load");
        assert_eq!(reloaded.admins.len(), 1);
        assert!(reloaded.authenticate(&token).is_none());
        assert!(list_admins(&scratch.0).unwrap()[0].role == "contributor");
    }

    #[test]
    fn a_successful_write_leaves_no_temp_file_and_is_owner_only() {
        let scratch = scratch("atomic");
        add_admin(&scratch.0, "ada", ROLE_ADMIN).expect("add");
        add_admin(&scratch.0, "bob", ROLE_ADMIN).expect("add again, rewriting the file");

        let path = admins_file_path(&scratch.0);
        assert!(path.is_file());
        assert!(
            !temp_path(&path).exists(),
            "the .tmp file must be renamed away, not left behind"
        );
        let strays: Vec<_> = std::fs::read_dir(nerevar_dir(&scratch.0))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "stray temp files: {strays:?}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "admins.json must be owner-only");
        }
    }

    #[test]
    fn two_tokens_are_never_the_same() {
        let first = generate_token().expect("first");
        let second = generate_token().expect("second");
        assert_ne!(first, second);
        assert_ne!(token_hash(&first), token_hash(&second));
        // The digest is a stable function of the token text.
        assert_eq!(token_hash(&first), token_hash(&first.clone()));
        assert_eq!(token_hash("").len(), 64);
    }
}
