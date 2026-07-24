//! `nerevar-core`: the Tauri-free heart of Nerevar.
//!
//! Holds instance, sync, and process-management logic shared by the Tauri
//! desktop app and (eventually) a headless `nerevar-host` daemon. Modules
//! land here incrementally — see the PM plan doc's migration sequence for
//! what has moved so far and what's still app-side.

pub mod data;
pub mod github_getters;
pub mod instance_setup;
pub mod openmw_ini_importer;
pub mod reporter;
pub mod sync_auth;
pub mod sync_paths;
