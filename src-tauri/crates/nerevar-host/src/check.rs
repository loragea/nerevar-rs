use std::path::Path;

use nerevar_core::data::InstanceConfig;
use nerevar_core::instance_data::{
    load_load_order, load_manifest, load_order_path, manifest_path, resolve_package_data_dir,
};
use nerevar_core::instance_setup::instance_tes3mp_dir;
use nerevar_core::runtime::{inspect, normalize_runtime_hint, RuntimeSource};

use crate::config::ResolvedConfig;
use crate::tls::TlsSettings;

/// Resolves everything a run would need (paths, exe, mod list, manifest) and
/// prints a summary, without starting any servers. Returns `true` when the
/// instance is actually hostable.
///
/// The TES3MP runtime is reported but doesn't by itself fail the check, since
/// `--sync-only` runs never need it. A missing load order *does* fail: a run
/// would refuse to guess a mod list, and a client hitting a manifest-less host
/// gets 404s rather than an empty-but-valid sync.
///
/// TLS *does* fail the check when it is configured and unloadable, for the
/// same reason: a run would exit 1 on it, and `--check` exists to find that
/// out before the unit starts.
pub fn run_check(
    resolved: &ResolvedConfig,
    instance: &InstanceConfig,
    sync_port: i32,
    tls: Option<&TlsSettings>,
) -> bool {
    let instance_root = Path::new(&instance.path);
    let data_dir = resolve_package_data_dir(instance);
    let tes3mp_dir = instance_tes3mp_dir(instance_root);
    let runtime = inspect(&tes3mp_dir);

    println!("nerevar-host --check");
    println!("  config path:    {}", resolved.path.display());
    println!("  instance:       {} ({})", instance.name, instance.id);
    println!("  instance root:  {}", instance_root.display());
    println!("  data dir:       {}", data_dir.display());
    println!(
        "  sync port:      {sync_port} ({})",
        if tls.is_some() { "https" } else { "http" }
    );
    let tls_ok = check_tls(tls);
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

    let ok = root_ok && data_ok && has_load_order && tls_ok;
    println!("  result:         {}", if ok { "OK" } else { "FAILED" });
    ok
}

/// Loads the configured certificate and key and prints what they say. `true`
/// when TLS is either not configured or fully usable.
fn check_tls(tls: Option<&TlsSettings>) -> bool {
    let Some(settings) = tls else {
        return true;
    };
    println!("  tls cert:       {}", settings.cert_path.display());
    println!("  tls key:        {}", settings.key_path.display());
    match settings.load() {
        Ok(loaded) => {
            println!("  tls identity:   {}", loaded.leaf.describe());
            if let Some(days) = loaded.leaf.expiring_soon() {
                if days < 0 {
                    println!("  tls expiry:     EXPIRED — renew and restart the daemon");
                } else {
                    println!(
                        "  tls expiry:     WARNING: {days} day(s) left; a renewal only takes \
                         effect on restart"
                    );
                }
            }
            true
        }
        Err(err) => {
            println!("  tls:            UNUSABLE ({err})");
            println!("  ERROR: TLS is configured but the certificate or key cannot be served");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tls::tests::{scratch, self_signed_pair, Scratch};
    use nerevar_core::instance_data::{scan_and_merge_load_order, ProgressEmitter};

    /// A hostable instance on disk plus the config that points at it — the
    /// minimum `run_check` needs to reach a verdict, so that the only thing
    /// varying between the assertions below is TLS.
    fn hostable_fixture(label: &str) -> (Scratch, ResolvedConfig) {
        let dir = scratch(label);
        let instance_root = dir.0.join("instance");
        let data_dir = instance_root.join("data");
        std::fs::create_dir_all(data_dir.join("ModA")).unwrap();
        std::fs::write(data_dir.join("ModA/plugin.esp"), b"payload").unwrap();

        let mut no_progress: Option<ProgressEmitter> = None;
        scan_and_merge_load_order(&data_dir, &mut no_progress).expect("scan");

        let config_path = dir.0.join("config.json");
        std::fs::write(
            &config_path,
            format!(
                r#"{{
                  "onboardingComplete": true,
                  "ownedInstances": [
                    {{
                      "id": "host-1",
                      "name": "Host",
                      "description": "",
                      "path": {root},
                      "dataDir": {data}
                    }}
                  ],
                  "syncedInstances": null,
                  "rootPath": {rootdir},
                  "syncPort": 25567
                }}"#,
                root = serde_json::to_string(&instance_root.to_string_lossy()).unwrap(),
                data = serde_json::to_string(&data_dir.to_string_lossy()).unwrap(),
                rootdir = serde_json::to_string(&dir.0.to_string_lossy()).unwrap(),
            ),
        )
        .unwrap();

        let resolved =
            crate::config::resolve_and_load_config(Some(&config_path)).expect("config loads");
        (dir, resolved)
    }

    /// `--check` with the TLS flags loads the pair and passes; a certificate
    /// it cannot load fails the check, so `ExecStartPre` catches it before the
    /// unit binds anything.
    #[test]
    fn check_validates_the_tls_pair_it_was_given() {
        let (dir, resolved) = hostable_fixture("check");
        let instance = &resolved.config.owned_instances.as_ref().unwrap()[0];

        assert!(
            run_check(&resolved, instance, 25567, None),
            "the fixture must pass without TLS, or the TLS assertions mean nothing"
        );

        let (settings, _) = self_signed_pair(&dir.0);
        assert!(
            run_check(&resolved, instance, 25567, Some(&settings)),
            "a loadable certificate and key must not fail the check"
        );

        let broken = crate::tls::TlsSettings {
            cert_path: dir.0.join("absent.pem"),
            key_path: settings.key_path.clone(),
        };
        assert!(
            !run_check(&resolved, instance, 25567, Some(&broken)),
            "a certificate that cannot be loaded must fail the check"
        );
    }
}
