//! `nerevar-core`: the Tauri-free heart of Nerevar.
//!
//! Holds instance, sync, and process-management logic shared by the Tauri
//! desktop app and the headless `nerevar-host` daemon. See AGENTS.md's
//! "Architecture" section for what lives in which crate.

pub mod app_state;
pub mod config;
pub mod connection;
pub mod data;
pub mod github_getters;
pub mod instance_data;
pub mod instance_setup;
pub mod instance_settings;
pub mod nerevar_server;
pub mod openmw_ini_importer;
pub mod port_conflict;
pub mod process_manager;
pub mod reporter;
pub mod runtime;
pub mod supervisor;
pub mod sync_auth;
pub mod sync_client;
pub mod sync_host;
pub mod sync_paths;

pub use app_state::AppState;
