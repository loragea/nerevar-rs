mod required_data_files;
mod server_cfg;

pub use required_data_files::{
    build_required_data_files, write_required_data_files, write_required_data_files_for_resolved,
};
// Runtime inspection (`runtime::inspect`) locates the same pieces these
// modules already know how to find; it reuses their finders rather than
// growing a second set that could drift from them.
pub(crate) use required_data_files::{find_tes3mp_server_data_dir, openmw_runtime_version};
pub(crate) use server_cfg::{find_client_defaults_cfg, find_server_defaults_cfg};
pub use server_cfg::{
    apply_server_defaults, create_instance_data_dir, instance_tes3mp_dir, INSTANCE_TES3MP_DIR,
    read_tes3mp_client_settings, read_tes3mp_server_settings, update_server_connection_settings,
    write_owned_client_connection, write_tes3mp_client_connection, Tes3mpClientSettings,
    Tes3mpServerSettings,
};
