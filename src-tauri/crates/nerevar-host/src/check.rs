use std::path::Path;

use nerevar_core::data::InstanceConfig;
use nerevar_core::instance_data::{
    load_load_order, load_manifest, load_order_path, manifest_path, resolve_package_data_dir,
};
use nerevar_core::instance_setup::instance_tes3mp_dir;
use nerevar_core::runtime::{inspect, normalize_runtime_hint, RuntimeSource};

use crate::config::ResolvedConfig;

/// Resolves everything a run would need (paths, exe, mod list, manifest) and
/// prints a summary, without starting any servers. Returns `true` when the
/// instance is actually hostable.
///
/// The TES3MP runtime is reported but doesn't by itself fail the check, since
/// `--sync-only` runs never need it. A missing load order *does* fail: a run
/// would refuse to guess a mod list, and a client hitting a manifest-less host
/// gets 404s rather than an empty-but-valid sync.
pub fn run_check(resolved: &ResolvedConfig, instance: &InstanceConfig, sync_port: i32) -> bool {
    let instance_root = Path::new(&instance.path);
    let data_dir = resolve_package_data_dir(instance);
    let tes3mp_dir = instance_tes3mp_dir(instance_root);
    let runtime = inspect(&tes3mp_dir);

    println!("nerevar-host --check");
    println!("  config path:    {}", resolved.path.display());
    println!("  instance:       {} ({})", instance.name, instance.id);
    println!("  instance root:  {}", instance_root.display());
    println!("  data dir:       {}", data_dir.display());
    println!("  sync port:      {sync_port}");
    match &runtime {
        Ok(info) => {
            println!(
                "  tes3mp runtime: OpenMW {} at {}",
                info.version_display(),
                tes3mp_dir.display()
            );
            match &info.server_exe {
                Some(path) => println!("  tes3mp server:  found ({})", path.display()),
                None => println!("  tes3mp server:  not found under {}", tes3mp_dir.display()),
            }
            let missing = info.missing_required();
            if !missing.is_empty() {
                println!("  runtime gaps:   missing {}", missing.join(", "));
            }
            for warning in &info.warnings {
                println!("  runtime note:   {warning}");
            }
        }
        Err(err) => println!("  tes3mp runtime: {err}"),
    }
    match instance.runtime_hint.clone().map(normalize_runtime_hint) {
        Some(Ok(RuntimeSource::GithubRelease { repo, tag, .. })) => {
            let tag = if tag.is_empty() { "(no tag)" } else { &tag };
            println!("  runtime hint:   suggests {repo} {tag} to players");
        }
        // `normalize_runtime_hint` only ever returns a `GithubRelease`.
        Some(Ok(_)) => unreachable!("a normalized hint is always a github release"),
        Some(Err(err)) => println!("  runtime hint:   IGNORED ({err})"),
        None => {}
    }

    let root_ok = instance_root.is_dir();
    let data_ok = data_dir.is_dir();

    let has_load_order = data_ok && load_order_path(&data_dir).exists();
    if has_load_order {
        match load_load_order(&data_dir) {
            Ok(load_order) => {
                let enabled = load_order.entries.iter().filter(|e| e.enabled).count();
                let plugins: usize = load_order
                    .entries
                    .iter()
                    .filter(|e| e.enabled)
                    .map(|e| e.plugins.iter().filter(|p| p.enabled).count())
                    .sum();
                println!(
                    "  load order:     {} package(s), {enabled} enabled, {plugins} plugin(s)",
                    load_order.entries.len()
                );
            }
            Err(err) => println!("  load order:     UNREADABLE ({err})"),
        }
    } else if data_ok {
        println!(
            "  load order:     missing ({}) — run with --scan",
            load_order_path(&data_dir).display()
        );
    }

    if data_ok {
        match load_manifest(&data_dir) {
            Ok(manifest) => println!(
                "  manifest:       {} package(s), {:.1} MiB, generated {}",
                manifest.packages.len(),
                manifest.total_download_bytes as f64 / (1024.0 * 1024.0),
                manifest.generated_at
            ),
            // Not a failure on its own: a normal run rebuilds it from the
            // load order before hosting.
            Err(_) => println!(
                "  manifest:       none yet ({}) — a run rebuilds it",
                manifest_path(&data_dir).display()
            ),
        }
    }

    if !root_ok {
        println!("  ERROR: instance root does not exist");
    }
    if !data_ok {
        println!("  ERROR: data dir does not exist");
    }
    if data_ok && !has_load_order {
        println!("  ERROR: no load order to build a manifest from");
    }

    let ok = root_ok && data_ok && has_load_order;
    println!("  result:         {}", if ok { "OK" } else { "FAILED" });
    ok
}
