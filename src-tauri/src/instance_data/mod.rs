pub mod commands;

// Mid-layer split (step 6): every non-command file (types, load-order, manifest build,
// scan, resolver, progress, mo2 modlist import, ...) moved into nerevar-core. This
// re-export keeps every existing `crate::instance_data::X` path in `commands.rs` and in
// sibling app modules (`connection/`, `sync_client` residue, `sync_roundtrip_test.rs`)
// resolving unchanged.
pub use nerevar_core::instance_data::*;

#[cfg(test)]
mod bindings {
    use crate::app_update::{AppUpdateRelease, AppUpdateStatus};
    use ts_rs::{Config, TS};

    /// `app_update` stays app-side (GUI self-update), so its two ts-rs types can't move
    /// into nerevar-core's combined `instance_data::bindings::export_bindings` test with
    /// everything else that used to share this file — split out here in the mid-layer
    /// split (step 6). Run with `cargo test export_bindings` (both crates) to refresh
    /// `src/types/*.ts`.
    #[test]
    fn export_bindings() {
        let cfg = Config::default();
        AppUpdateStatus::export_all(&cfg).expect("export AppUpdateStatus");
        AppUpdateRelease::export_all(&cfg).expect("export AppUpdateRelease");
    }
}
