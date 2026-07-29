pub mod sync;

// Mid-layer split (step 6): apply/coordinator/download/fetch/metadata/progress/sync_state/
// types, plus everything in sync.rs except `get_instance_sync_status`, moved into
// nerevar-core. `sync_state` is re-exported as a module (not just its members) because
// this crate's own `sync.rs` residue addresses it by qualified path
// (`crate::sync_client::sync_state::...`). `download` used to be re-exported the same way
// for `sync_roundtrip_test.rs`; that test moved into core's own `tests/` (fe943c3), so
// nothing here needs it any more.
pub use nerevar_core::sync_client::sync_state;
pub use nerevar_core::sync_client::*;
