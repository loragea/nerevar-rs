use std::path::Path;

use nerevar_core::data::InstanceConfig;
use nerevar_core::instance_data::resolve_package_data_dir;
use nerevar_core::instance_setup::instance_tes3mp_dir;
use nerevar_core::process_manager::find_tes3mp_server_exe;

use crate::config::ResolvedConfig;

/// Resolves everything a run would need (paths, exe) and prints a summary,
/// without starting any servers. Returns `true` when the instance is
/// runnable (instance root and data dir exist on disk) — the TES3MP exe is
/// reported but doesn't by itself fail the check, since `--sync-only` runs
/// never need it.
pub fn run_check(resolved: &ResolvedConfig, instance: &InstanceConfig, sync_port: i32) -> bool {
    let instance_root = Path::new(&instance.path);
    let data_dir = resolve_package_data_dir(instance);
    let tes3mp_dir = instance_tes3mp_dir(instance_root);
    let exe = find_tes3mp_server_exe(&tes3mp_dir);

    println!("nerevar-host --check");
    println!("  config path:    {}", resolved.path.display());
    println!("  instance:       {} ({})", instance.name, instance.id);
    println!("  instance root:  {}", instance_root.display());
    println!("  data dir:       {}", data_dir.display());
    println!("  sync port:      {sync_port}");
    match &exe {
        Some(path) => println!("  tes3mp server:  found ({})", path.display()),
        None => println!("  tes3mp server:  not found under {}", tes3mp_dir.display()),
    }

    let root_ok = instance_root.is_dir();
    let data_ok = data_dir.is_dir();
    if !root_ok {
        println!("  ERROR: instance root does not exist");
    }
    if !data_ok {
        println!("  ERROR: data dir does not exist");
    }

    let ok = root_ok && data_ok;
    println!("  result:         {}", if ok { "OK" } else { "FAILED" });
    ok
}
