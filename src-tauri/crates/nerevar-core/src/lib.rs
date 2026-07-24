//! `nerevar-core`: the Tauri-free heart of Nerevar.
//!
//! This crate will hold the instance, sync, and process-management logic
//! that today lives in the `nerevar` (src-tauri) crate, so it can be reused
//! by both the Tauri desktop app and a future headless `nerevar-host`
//! daemon. It is intentionally empty for now — this is step 4 of the
//! nerevar-core split (see the PM plan doc), which only introduces the
//! cargo workspace and wires up the dependency. Module moves happen in
//! later steps.
