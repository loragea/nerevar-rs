//! Co-admin identity for the headless host.
//!
//! A player authenticates with the shared sync password (`sync_auth`); a
//! co-admin authenticates as *somebody*, with a named bearer token stored in
//! `<data dir>/.nerevar/admins.json` ([`store`]). What a co-admin may do is
//! decided by their role's capability set ([`capability`]), never by an
//! is-admin flag, so later roles are table entries rather than new branches.
//!
//! The HTTP side — bearer extraction, the capability guard, the audit line —
//! lives in `nerevar_server::routes::admin`; this module is the model.

/// Event name for the one audit line every authenticated `/admin` request
/// emits through the `EventSink` (admin name, role, method, path, response
/// status). The daemon's sink logs it at `info` so it lands in journald with
/// the other host lifecycle events.
pub const ADMIN_REQUEST_EVENT: &str = "admin-request";

/// Event name for the summary `POST /admin/apply` emits once the new manifest
/// is live: what was installed and removed, and whether a running TES3MP
/// server is now behind that manifest. The daemon's sink logs it at `info`
/// next to the audit line, because the stale-plugin-list warning is the one
/// thing an operator has to act on after an apply.
pub const ADMIN_APPLY_EVENT: &str = "admin-apply";

pub mod apply;
pub mod capability;
pub mod staging;
pub mod status;
pub mod store;
pub mod upload;

pub use apply::{
    current_and_pending_load_order, execute_apply, instance_name_for_rebuild, plan_apply,
    ApplyOutcome, ApplyPlan, PlannedInstall,
};
#[cfg(any(test, feature = "test-util"))]
pub use capability::ROLE_TEST_STATUS_ONLY;
pub use capability::{
    capabilities_for_role, known_role_names, role_grants, role_is_known, Capability, ROLE_ADMIN,
};
pub use staging::{
    available_package_names, clear_staging, data_package_names, load_pending, pending_path,
    save_pending, staged_archive_path, staged_package_dir, staging_dir,
    validate_pending_load_order, validate_staged_package_name, PendingChanges, StagedPackage,
    MAX_PACKAGE_NAME_LEN, PENDING_FILE, PENDING_SET_VERSION, STAGING_DIR,
};
pub use status::{
    build_admin_status, pending_changes_for, AdminLoadOrderSummary, AdminManifestSummary,
    AdminPendingChanges, AdminStatus,
};
pub use store::{
    add_admin, admins_file_path, generate_token, list_admins, load_admins, revoke_admin,
    save_admins, token_hash, AdminRecord, AdminStore, NewAdmin, ADMINS_FILE, ADMIN_STORE_VERSION,
    ADMIN_TOKEN_BYTES,
};
pub use upload::stage_uploaded_archive;
