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

pub mod capability;
pub mod status;
pub mod store;

pub use capability::{
    capabilities_for_role, known_role_names, role_grants, role_is_known, Capability, ROLE_ADMIN,
};
pub use status::{build_admin_status, AdminLoadOrderSummary, AdminManifestSummary, AdminStatus};
pub use store::{
    add_admin, admins_file_path, generate_token, list_admins, load_admins, revoke_admin,
    save_admins, token_hash, AdminRecord, AdminStore, NewAdmin, ADMINS_FILE, ADMIN_STORE_VERSION,
    ADMIN_TOKEN_BYTES,
};
