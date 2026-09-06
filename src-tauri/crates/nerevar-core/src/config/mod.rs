pub mod nerevar_config;
pub use nerevar_config::{
    add_instance, complete_onboarding, default_instances_dir, default_instances_dir_under,
    ensure_default_instances_dir, generate_default_global_openmw_config, load_nerevar_config_at,
    load_or_create_nerevar_config, nerevar_config_file_path, nerevar_config_file_path_opt,
    set_morrowind_data_files, set_root_path, set_sync_port, unique_synced_instance_name,
    update_owned_instance, update_synced_instance, validate_global_openmw_config,
};
