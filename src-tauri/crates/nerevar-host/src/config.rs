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
         (see notes/nerevar-host-design.md), or pass --config <path>.",
        tried
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}
