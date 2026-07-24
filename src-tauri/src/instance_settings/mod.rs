pub mod commands;

// Mid-layer split (step 6): defaults/storage/openmw_settings/tes3mp_lua/types, plus the
// plain `apply_instance_settings_to_disk` helper, moved into nerevar-core (the latter is a
// forced relocation — `instance_data::manifest::build_manifest`, now core-side, calls it
// directly). Only the two `#[tauri::command]` wrappers in `commands.rs` stay app-side.
pub use nerevar_core::instance_settings::*;
