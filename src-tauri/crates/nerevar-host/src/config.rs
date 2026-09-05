use std::path::{Path, PathBuf};

use nerevar_core::config::{load_nerevar_config_at, nerevar_config_file_path_opt};
use nerevar_core::data::NerevarConfig;

/// Same app identifier the Tauri GUI is packaged under (`tauri.conf.json`
/// `identifier`) — the daemon reads the same per-user config file so a
/// desktop user's `nerevar-host` sees the same instances as their app.
const APP_IDENTIFIER: &str = "dev.kyleaustad.nerevar";

/// Service-account fallback when no per-user data dir resolves (or the
/// per-user config simply doesn't exist there).
const SYSTEM_CONFIG_PATH: &str = "/etc/nerevar/config.json";

/// Resolves and loads `NerevarConfig`, never creating it. Order:
/// 1. `--config <path>` if given — error if the file is missing.
/// 2. The per-user GUI path (`dirs::data_dir()/dev.kyleaustad.nerevar/
///    config.json`) if the user data dir resolves AND the file exists.
///    (Checked directly, not via `nerevar_config_file_path`'s panicking
///    `dirs::data_dir().expect(..)`, so a service account with no resolvable
///    user dirs falls through cleanly instead of panicking.)
/// 3. `/etc/nerevar/config.json`.
/// Returns the loaded config, the path it came from, and (for `--check`)
/// every path that was tried.
pub struct ResolvedConfig {
    pub config: NerevarConfig,
    pub path: PathBuf,
}

pub fn resolve_and_load_config(explicit: Option<&Path>) -> Result<ResolvedConfig, String> {
    let mut tried: Vec<PathBuf> = Vec::new();

    if let Some(path) = explicit {
        return load_nerevar_config_at(path)
            .map(|config| ResolvedConfig {
                config,
                path: path.to_path_buf(),
            })
            .map_err(|err| format!("--config {}: {err}", path.display()));
    }

    if let Some(user_path) = nerevar_config_file_path_opt(APP_IDENTIFIER) {
        if user_path.exists() {
            let config = load_nerevar_config_at(&user_path)?;
            return Ok(ResolvedConfig {
                config,
                path: user_path,
            });
        }
        tried.push(user_path);
    }

    let system_path = PathBuf::from(SYSTEM_CONFIG_PATH);
    if system_path.exists() {
        let config = load_nerevar_config_at(&system_path)?;
        return Ok(ResolvedConfig {
            config,
            path: system_path,
        });
    }
    tried.push(system_path);

    Err(format!(
        "No config file found. Tried: {}. The daemon never creates config \
         automatically — run the GUI once, or write one of these paths by hand \
         (see docs/headless-hosting.md, \"The Nerevar config file\"), or pass \
         --config <path>.",
        tried
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nerevar_core::runtime::{RuntimeSource, DEFAULT_TES3MP_REPO};

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-host-config-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// The daemon reads the same `config.json` the GUI writes, including ones
    /// written before `runtime` existed: an old file must still load, and its
    /// instance must come back with the migrated runtime source rather than
    /// failing to parse.
    #[test]
    fn an_old_style_config_still_loads_and_gains_a_runtime_source() {
        let scratch = scratch("legacy");
        let path = scratch.0.join("config.json");
        std::fs::write(
            &path,
            r#"{
              "onboardingComplete": true,
              "ownedInstances": [
                {
                  "id": "host-1",
                  "name": "Host",
                  "description": "",
                  "path": "/instances/host",
                  "dataDir": "/instances/host/data",
                  "releaseId": "65767406"
                }
              ],
              "syncedInstances": null,
              "rootPath": "/instances",
              "syncPort": 25567
            }"#,
        )
        .unwrap();

        let resolved = resolve_and_load_config(Some(&path)).expect("config should load");
        assert_eq!(resolved.path, path);

        let owned = resolved.config.owned_instances.expect("owned instances");
        assert_eq!(owned.len(), 1);
        assert_eq!(
            owned[0].runtime,
            Some(RuntimeSource::GithubRelease {
                repo: DEFAULT_TES3MP_REPO.to_string(),
                release_id: "65767406".to_string(),
                tag: String::new(),
                asset_name: String::new(),
            })
        );
    }

    /// A hand-written host config (no `releaseId`, no `runtime`) loads too —
    /// the daemon never installs a runtime, it only reads the one on disk.
    #[test]
    fn a_config_with_no_runtime_information_loads_unchanged() {
        let scratch = scratch("bare");
        let path = scratch.0.join("config.json");
        std::fs::write(
            &path,
            r#"{
              "onboardingComplete": true,
              "ownedInstances": [
                {
                  "id": "host-1",
                  "name": "Host",
                  "description": "",
                  "path": "/instances/host",
                  "dataDir": "/instances/host/data"
                }
              ],
              "syncedInstances": null,
              "rootPath": "/instances",
              "syncPort": 25567
            }"#,
        )
        .unwrap();

        let resolved = resolve_and_load_config(Some(&path)).expect("config should load");
        let owned = resolved.config.owned_instances.expect("owned instances");
        assert!(owned[0].runtime.is_none());
        assert!(owned[0].release_id.is_none());
        assert!(owned[0].runtime_hint.is_none());
    }

    /// The operator's way to advertise a runtime is to add `runtimeHint` to
    /// an existing config by hand (docs/headless-hosting.md) — an old-style
    /// entry, legacy `releaseId` and all, plus the one new key.
    #[test]
    fn a_hand_added_runtime_hint_loads_from_an_old_style_config() {
        let scratch = scratch("hint");
        let path = scratch.0.join("config.json");
        std::fs::write(
            &path,
            r#"{
              "onboardingComplete": true,
              "ownedInstances": [
                {
                  "id": "host-1",
                  "name": "Host",
                  "description": "",
                  "path": "/instances/host",
                  "dataDir": "/instances/host/data",
                  "releaseId": "65767406",
                  "runtimeHint": {
                    "kind": "githubRelease",
                    "repo": "owner/name",
                    "releaseId": "",
                    "tag": "0.8.1"
                  }
                }
              ],
              "syncedInstances": null,
              "rootPath": "/instances",
              "syncPort": 25567
            }"#,
        )
        .unwrap();

        let resolved = resolve_and_load_config(Some(&path)).expect("config should load");
        let owned = resolved.config.owned_instances.expect("owned instances");
        assert_eq!(
            owned[0].runtime_hint,
            Some(RuntimeSource::GithubRelease {
                repo: "owner/name".to_string(),
                release_id: String::new(),
                tag: "0.8.1".to_string(),
                asset_name: String::new(),
            })
        );
        // The hint is a suggestion for players and never the host's own
        // runtime: the legacy migration still governs `runtime`.
        assert_eq!(
            owned[0].runtime,
            Some(RuntimeSource::GithubRelease {
                repo: DEFAULT_TES3MP_REPO.to_string(),
                release_id: "65767406".to_string(),
                tag: String::new(),
                asset_name: String::new(),
            })
        );
    }
}
