//! The TES3MP runtime an instance runs on: where it comes from, how it is
//! installed, and what an installed one contains.
//!
//! Nerevar never ships a TES3MP build; every instance pulls its own. This
//! module owns that acquisition end to end — `RuntimeSource` says where from
//! (a GitHub release, a directory the user already has, an archive on disk),
//! `acquire` puts it on disk, and `inspect` reports what actually landed.
//!
//! Two of those pieces answer to the version lock: `trust` decides which
//! GitHub repositories may be downloaded from at all (the client's list, never
//! a server's), and `version_lock` decides what a host's advertised runtime may
//! do — preselect a picker, or require an `update` before the client launches.

pub mod acquire;
pub mod github;
pub mod inspect;
pub mod source;
pub mod trust;
pub mod update;
pub mod version_lock;

pub use acquire::acquire;
pub use github::select_tes3mp_asset;
pub use inspect::{inspect, inspect_archive, inspect_source, RuntimeInfo, RuntimeInspection};
pub use source::{
    normalize_repo, normalize_runtime_hint, RuntimeSource, TargetPlatform, DEFAULT_TES3MP_REPO,
};
pub use trust::{
    add_trusted_repo, is_trusted_repo, normalize_trusted_repo, remove_trusted_repo, trusted_repos,
    untrusted_repo_message, TrustedRepo, TrustedRuntimeRepos, BUILTIN_TRUSTED_RUNTIME_REPOS,
};
pub use update::update_instance_runtime;
pub use version_lock::{
    resolve_runtime_hint, runtime_mismatch, untrusted_hint_message, RuntimeHintResolution,
    RuntimeMismatch,
};
