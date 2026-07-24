pub mod sync;

// Mid-layer split (step 6): apply/coordinator/download/fetch/metadata/progress/sync_state/
// types, plus everything in sync.rs except `get_instance_sync_status`, moved into
// nerevar-core. `sync_state` is re-exported as a module (not just its members) because
// this crate's own `sync.rs` residue addresses it by qualified path
// (`crate::sync_client::sync_state::...`); `download` likewise, but only
// `sync_roundtrip_test.rs` (`#[cfg(test)]`) still needs it that way in the app crate now
// that `sync.rs`'s own download call lives in core — gate it to avoid an unused-import
// warning on non-test builds.
#[cfg(test)]
pub use nerevar_core::sync_client::download;
pub use nerevar_core::sync_client::sync_state;
pub use nerevar_core::sync_client::*;
