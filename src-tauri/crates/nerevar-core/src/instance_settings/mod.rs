mod defaults;
pub mod openmw_cfg_overrides;
mod openmw_settings;
mod storage;
mod tes3mp_lua;
mod types;

use std::path::Path;

pub use defaults::{default_instance_settings, normalize_instance_settings, setting_definitions};
pub use storage::{
    instance_settings_path, launch_settings_overlay_path, load_instance_settings,
    persist_settings_from_manifest, save_instance_settings, write_launch_settings_overlay,
};
pub use openmw_cfg_overrides::apply_openmw_cfg_override_lines;
pub use openmw_settings::format_settings_overlay;
pub use types::{
    InstanceSettings, SettingCategory, SettingDefinition, SettingValue, SettingValueType,
    Tes3mpGameSettingEntry,
};

/// Moved from the app crate's `instance_settings::commands` in the mid-layer split
/// (step 6): `instance_data::manifest::build_manifest` (now core-side) calls this
/// directly to persist settings + the launch overlay + TES3MP lua during manifest
/// builds. It has no Tauri dependency of its own — only the two `#[tauri::command]`
/// wrappers that used to share its file (`get_instance_settings`,
/// `save_instance_settings_command`) stayed app-side.
pub fn apply_instance_settings_to_disk(
    instance_root: &Path,
    data_dir: &Path,
    settings: &InstanceSettings,
) -> Result<(), String> {
    save_instance_settings(data_dir, settings)?;
    write_launch_settings_overlay(data_dir, settings)?;
    tes3mp_lua::write_tes3mp_game_settings(
        &crate::instance_setup::instance_tes3mp_dir(instance_root),
        settings,
    )?;
    Ok(())
}
