//! Finding the player's Morrowind `Data Files` directory without asking.
//!
//! Onboarding used to open a native file picker and hope the player knew
//! where their game lives. Most installs sit in one of a handful of places, so
//! Nerevar looks there first and asks only when nothing turns up — or when the
//! player says the found directory is the wrong one.
//!
//! A candidate is a directory *plus how it was found*, so the screen can say
//! where the suggestion came from. Only a directory that actually holds
//! [`MORROWIND_ESM`] is ever reported: a Steam library folder left behind by an
//! uninstall is not a Morrowind install.
//!
//! [`confirmed_candidates`] takes the list to check rather than building it,
//! which is what makes the search testable — [`find_morrowind_data_files`] is
//! the thin wrapper that feeds it this machine's real candidates.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::openmw_ini_importer::{parse_cfg_contents, resolve_global_openmw_cfg_path};

/// The file whose presence makes a directory a Morrowind `Data Files`.
pub const MORROWIND_ESM: &str = "Morrowind.esm";

/// Where a candidate directory came from, so the player can judge the
/// suggestion instead of being handed a bare path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum MorrowindCandidateSource {
    /// A Steam library at its default location.
    SteamLibrary,
    /// A GOG install at its default location.
    GogInstall,
    /// A `data=` line of the player's existing global `openmw.cfg`.
    OpenmwConfig,
    /// The install path Morrowind's own installer writes to the registry.
    WindowsRegistry,
}

/// A directory that holds `Morrowind.esm`, and how Nerevar found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MorrowindCandidate {
    pub path: String,
    pub source: MorrowindCandidateSource,
}

/// Every `Data Files` directory this machine offers, best first.
///
/// The order is the order of [`default_candidate_paths`]: the packaged
/// installs first (they are where an unmodified copy of the game sits), then
/// whatever the player's own OpenMW configuration already points at, then the
/// Windows registry.
pub fn find_morrowind_data_files() -> Vec<MorrowindCandidate> {
    confirmed_candidates(default_candidate_paths())
}

/// The candidates worth checking on this machine, in preference order.
///
/// Split out from the check itself so the filtering can be tested against a
/// list a test controls.
pub fn default_candidate_paths() -> Vec<(PathBuf, MorrowindCandidateSource)> {
    let mut candidates = Vec::new();

    for path in packaged_install_paths() {
        candidates.push(path);
    }

    for path in global_openmw_data_paths() {
        candidates.push((path, MorrowindCandidateSource::OpenmwConfig));
    }

    #[cfg(windows)]
    if let Some(path) = registry_data_files_path() {
        candidates.push((path, MorrowindCandidateSource::WindowsRegistry));
    }

    candidates
}

/// Keeps the candidates that really are a Morrowind installation, in the order
/// given. A directory named twice — the same install found two ways — is
/// reported once, under the first source that named it.
pub fn confirmed_candidates<I>(candidates: I) -> Vec<MorrowindCandidate>
where
    I: IntoIterator<Item = (PathBuf, MorrowindCandidateSource)>,
{
    let mut found: Vec<MorrowindCandidate> = Vec::new();

    for (path, source) in candidates {
        if !is_morrowind_data_files(&path) {
            continue;
        }
        let path = path.to_string_lossy().into_owned();
        if found.iter().any(|candidate| candidate.path == path) {
            continue;
        }
        found.push(MorrowindCandidate { path, source });
    }

    found
}

/// True when `path` holds `Morrowind.esm` — the one test that separates a
/// Morrowind installation from any other directory.
pub fn is_morrowind_data_files(path: &Path) -> bool {
    path.join(MORROWIND_ESM).is_file()
}

/// The `data=` entries of an OpenMW cfg, in file order and unquoted.
///
/// OpenMW quotes a path containing spaces (see
/// `openmw_ini_importer::quote_data_path`, which writes them the same way), so
/// the surrounding quotes come off here.
pub fn data_paths_in_openmw_cfg(contents: &str) -> Vec<PathBuf> {
    let cfg = parse_cfg_contents(contents);
    let Some(values) = cfg.get("data") else {
        return Vec::new();
    };

    values
        .iter()
        .map(|value| {
            let trimmed = value.trim();
            let unquoted = trimmed
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .unwrap_or(trimmed);
            PathBuf::from(unquoted)
        })
        .collect()
}

/// The `data=` directories of the player's own global `openmw.cfg`, if they
/// have one. An unreadable or absent file simply contributes nothing.
fn global_openmw_data_paths() -> Vec<PathBuf> {
    let Some(path) = resolve_global_openmw_cfg_path() else {
        return Vec::new();
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    data_paths_in_openmw_cfg(&contents)
}

/// Default install locations of the two ways the game is sold.
#[cfg(windows)]
fn packaged_install_paths() -> Vec<(PathBuf, MorrowindCandidateSource)> {
    vec![
        (
            PathBuf::from(
                r"C:\Program Files (x86)\Steam\steamapps\common\Morrowind\Data Files",
            ),
            MorrowindCandidateSource::SteamLibrary,
        ),
        (
            PathBuf::from(r"C:\GOG Games\Morrowind\Data Files"),
            MorrowindCandidateSource::GogInstall,
        ),
    ]
}

#[cfg(not(windows))]
fn packaged_install_paths() -> Vec<(PathBuf, MorrowindCandidateSource)> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    vec![
        (
            home.join(".steam/steam/steamapps/common/Morrowind/Data Files"),
            MorrowindCandidateSource::SteamLibrary,
        ),
        (
            home.join(".local/share/Steam/steamapps/common/Morrowind/Data Files"),
            MorrowindCandidateSource::SteamLibrary,
        ),
        (
            home.join("GOG Games/Morrowind/Data Files"),
            MorrowindCandidateSource::GogInstall,
        ),
    ]
}

/// The install path Morrowind's installer records, plus `Data Files`.
///
/// Read through `reg.exe` rather than a registry crate: the only registry
/// binding in the lock file (`winreg`) is a *build* dependency of
/// `tauri-build`, so binding to it here would add a shipped dependency for one
/// string. `reg.exe` ships with Windows and prints
/// `    Installed Path    REG_SZ    C:\...`, whose value is everything after
/// the type column.
#[cfg(windows)]
fn registry_data_files_path() -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    /// Same flag `process_manager::spawn` uses: no console window flashes up.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let output = Command::new("reg.exe")
        .args([
            "query",
            r"HKLM\SOFTWARE\WOW6432Node\Bethesda Softworks\Morrowind",
            "/v",
            "Installed Path",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let install_path = registry_query_value(&stdout, "Installed Path")?;
    Some(PathBuf::from(install_path).join("Data Files"))
}

/// The value of `name` in `reg.exe query` output: the line naming it holds the
/// value name, its type, and then the value, separated by runs of whitespace.
/// A value containing spaces survives because only the first two columns are
/// split off.
#[cfg(windows)]
fn registry_query_value(output: &str, name: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(name) else {
            continue;
        };
        let rest = rest.trim_start();
        // The type column ("REG_SZ", "REG_EXPAND_SZ", ...) and then the value.
        let (_, value) = rest.split_once(char::is_whitespace)?;
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        return Some(value.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-morrowind-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// A directory that looks like a real `Data Files`.
    fn install(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MORROWIND_ESM), b"esm").unwrap();
        dir
    }

    #[test]
    fn a_directory_holding_the_esm_is_a_candidate() {
        let scratch = scratch("found");
        let steam = install(&scratch.0, "steam");

        let found = confirmed_candidates(vec![(
            steam.clone(),
            MorrowindCandidateSource::SteamLibrary,
        )]);

        assert_eq!(
            found,
            vec![MorrowindCandidate {
                path: steam.to_string_lossy().into_owned(),
                source: MorrowindCandidateSource::SteamLibrary,
            }]
        );
    }

    #[test]
    fn a_directory_without_the_esm_is_not_a_candidate() {
        let scratch = scratch("not-found");
        let empty = scratch.0.join("leftover");
        std::fs::create_dir_all(&empty).unwrap();
        let missing = scratch.0.join("never-existed");

        let found = confirmed_candidates(vec![
            (empty, MorrowindCandidateSource::SteamLibrary),
            (missing, MorrowindCandidateSource::GogInstall),
        ]);

        assert!(found.is_empty(), "found {found:?}");
    }

    #[test]
    fn candidates_keep_their_order_and_a_repeat_keeps_its_first_source() {
        let scratch = scratch("order");
        let steam = install(&scratch.0, "steam");
        let gog = install(&scratch.0, "gog");

        let found = confirmed_candidates(vec![
            (steam.clone(), MorrowindCandidateSource::SteamLibrary),
            (gog.clone(), MorrowindCandidateSource::GogInstall),
            // The same install, reached a second way.
            (steam.clone(), MorrowindCandidateSource::OpenmwConfig),
        ]);

        assert_eq!(
            found,
            vec![
                MorrowindCandidate {
                    path: steam.to_string_lossy().into_owned(),
                    source: MorrowindCandidateSource::SteamLibrary,
                },
                MorrowindCandidate {
                    path: gog.to_string_lossy().into_owned(),
                    source: MorrowindCandidateSource::GogInstall,
                },
            ]
        );
    }

    #[test]
    fn data_lines_are_read_in_file_order_and_unquoted() {
        let cfg = concat!(
            "encoding=win1252\n",
            "data=/home/player/Morrowind/Data Files\n",
            "data=\"/home/player/Games/My Morrowind/Data Files\"\n",
            "content=Morrowind.esm\n",
        );

        assert_eq!(
            data_paths_in_openmw_cfg(cfg),
            vec![
                PathBuf::from("/home/player/Morrowind/Data Files"),
                PathBuf::from("/home/player/Games/My Morrowind/Data Files"),
            ]
        );
    }

    #[test]
    fn a_cfg_without_data_lines_yields_nothing() {
        assert!(data_paths_in_openmw_cfg("encoding=win1252\n").is_empty());
    }

    /// The cfg is the third source, so a Steam install still wins — but a game
    /// only the cfg knows about is found.
    #[test]
    fn a_data_line_can_be_the_only_candidate() {
        let scratch = scratch("cfg-only");
        let elsewhere = install(&scratch.0, "Games/My Morrowind/Data Files");
        let cfg = format!("data=\"{}\"\n", elsewhere.display());

        let candidates: Vec<_> = data_paths_in_openmw_cfg(&cfg)
            .into_iter()
            .map(|path| (path, MorrowindCandidateSource::OpenmwConfig))
            .collect();

        assert_eq!(
            confirmed_candidates(candidates),
            vec![MorrowindCandidate {
                path: elsewhere.to_string_lossy().into_owned(),
                source: MorrowindCandidateSource::OpenmwConfig,
            }]
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_registry_value_with_spaces_survives_the_parse() {
        let output = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\WOW6432Node\\Bethesda Softworks\\Morrowind\r\n    Installed Path    REG_SZ    C:\\Program Files (x86)\\Bethesda Softworks\\Morrowind\r\n";

        assert_eq!(
            registry_query_value(output, "Installed Path").as_deref(),
            Some(r"C:\Program Files (x86)\Bethesda Softworks\Morrowind")
        );
        assert_eq!(registry_query_value(output, "Nothing Here"), None);
    }
}
