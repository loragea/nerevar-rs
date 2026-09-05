//! The TES3MP runtime an instance runs on: where it comes from, how it is
//! installed, and what an installed one contains.
//!
//! Nerevar never ships a TES3MP build; every instance pulls its own. This
//! module owns that acquisition end to end — `RuntimeSource` says where from,
//! `acquire` puts it on disk, and `inspect` reports what actually landed.

pub mod acquire;
pub mod github;
pub mod inspect;
pub mod source;

pub use acquire::acquire;
pub use github::select_tes3mp_asset;
pub use inspect::{inspect, RuntimeInfo};
pub use source::{RuntimeSource, TargetPlatform, DEFAULT_TES3MP_REPO};
