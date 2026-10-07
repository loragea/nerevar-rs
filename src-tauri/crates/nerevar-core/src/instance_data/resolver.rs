use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::paths::package_abs_path;
use super::types::{LoadOrder, NerevarManifest, ResolvedOpenMwConfig};
use crate::openmw_ini_importer::{
    is_archive_file, quote_data_path, sort_content_plugins, PluginIndex,
};

const DEFAULT_BASE_ESMS: &[&str] = &["Morrowind.esm", "Tribunal.esm", "Bloodmoon.esm"];

pub fn resolve_load_order(
    data_dir: &Path,
    load_order: &LoadOrder,
) -> Result<ResolvedOpenMwConfig, String> {
    let mut data_paths = Vec::new();

    if let Some(base) = &load_order.base_game_data {
        if !base.is_empty() {
            data_paths.push(PathBuf::from(strip_quotes(base)));
        }
    }

    let mut enabled_entries: Vec<_> = load_order.entries.iter().filter(|e| e.enabled).collect();
    enabled_entries.sort_by_key(|e| e.priority);

    for entry in &enabled_entries {
        data_paths.push(package_abs_path(data_dir, &entry.relative_dir));
    }

    let index = PluginIndex::build(&data_paths);

    // A package's plugin list holds both `content=` files and archives; the
    // extension decides which line each becomes. Archives go to
    // `fallback-archive=` in package priority order and never to `content=`.
    let mut plugin_names = Vec::new();
    let mut archives: Vec<String> = Vec::new();
    for entry in enabled_entries {
        for plugin in &entry.plugins {
            if !plugin.enabled {
                continue;
            }
            if is_archive_file(&plugin.file) {
                push_archive(&mut archives, &index, &plugin.file);
            } else {
                plugin_names.push(plugin.file.clone());
            }
        }
    }
    let has_explicit_content_order = load_order.content_order.is_some();

    if let Some(base_path) = load_order
        .base_game_data
        .as_ref()
        .map(|s| PathBuf::from(strip_quotes(s)))
    {
        for esm in DEFAULT_BASE_ESMS {
            if base_path.join(esm).exists()
                && !plugin_names
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(esm))
            {
                plugin_names.push((*esm).to_string());
            }
        }
    }
    let mut content = resolve_content_plugins(load_order, &index, &plugin_names)?;

    if !has_explicit_content_order {
        content = reorder_base_esms(content);
    }

    let data_paths_quoted: Vec<String> = data_paths.iter().map(|p| quote_data_path(p)).collect();

    Ok(ResolvedOpenMwConfig {
        encoding: "win1252".to_string(),
        data_paths: data_paths_quoted,
        content,
        archives,
    })
}

/// Appends `name` to `archives` when it is on disk under a `data=` path and
/// not already listed (ignoring case). A missing `fallback-archive` is fatal
/// to OpenMW, so an archive that is not there is left out rather than listed.
fn push_archive(archives: &mut Vec<String>, index: &PluginIndex, name: &str) {
    if archives.iter().any(|listed| listed.eq_ignore_ascii_case(name)) {
        return;
    }
    if index.find(name).is_some() {
        archives.push(name.to_string());
    }
}

/// Build launch config for a synced client using local data paths and the host's plugin order.
pub fn resolve_synced_load_order(
    data_dir: &Path,
    load_order: &LoadOrder,
    manifest: &NerevarManifest,
) -> Result<ResolvedOpenMwConfig, String> {
    let local = resolve_load_order(data_dir, load_order)?;
    let data_paths: Vec<PathBuf> = local
        .data_paths
        .iter()
        .map(|p| PathBuf::from(strip_quotes(p)))
        .collect();

    let index = PluginIndex::build(&data_paths);
    let mut content = Vec::new();
    let mut archives = Vec::new();
    for plugin in &manifest.resolved.archives {
        push_archive(&mut archives, &index, plugin);
    }
    for plugin in &manifest.resolved.content {
        // A host that predates `archives` listed a package's `.bsa` here;
        // OpenMW refuses one as `content=`, so it becomes an archive instead.
        if is_archive_file(plugin) {
            push_archive(&mut archives, &index, plugin);
        } else if index.find(plugin).is_some() {
            content.push(plugin.clone());
        }
    }

    if content.is_empty() {
        content = local.content;
    }
    if archives.is_empty() {
        archives = local.archives;
    }

    Ok(ResolvedOpenMwConfig {
        encoding: local.encoding,
        data_paths: local.data_paths,
        content,
        archives,
    })
}

fn resolve_content_plugins(
    load_order: &LoadOrder,
    index: &PluginIndex,
    enabled_plugin_names: &[String],
) -> Result<Vec<String>, String> {
    if let Some(content_order) = &load_order.content_order {
        return Ok(resolve_explicit_content_order(
            index,
            content_order,
            enabled_plugin_names,
        ));
    }

    sort_content_plugins(index, enabled_plugin_names)
}

fn resolve_explicit_content_order(
    index: &PluginIndex,
    content_order: &[String],
    enabled_plugin_names: &[String],
) -> Vec<String> {
    let enabled: HashSet<String> = enabled_plugin_names
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect();

    let mut result = Vec::new();
    let mut seen = HashSet::new();

    for name in content_order {
        let key = name.to_ascii_lowercase();
        if !enabled.contains(&key) {
            continue;
        }
        if index.find(name).is_none() {
            continue;
        }
        if seen.insert(key) {
            result.push(name.clone());
        }
    }

    for name in enabled_plugin_names {
        let key = name.to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        if index.find(name).is_some() {
            result.push(name.clone());
        }
    }

    result
}

fn strip_quotes(path: &str) -> String {
    let trimmed = path.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(trimmed)
        .to_string()
}

fn reorder_base_esms(mut content: Vec<String>) -> Vec<String> {
    let mut ordered = Vec::new();
    for base in DEFAULT_BASE_ESMS {
        if let Some(pos) = content.iter().position(|c| c.eq_ignore_ascii_case(base)) {
            ordered.push(content.remove(pos));
        }
    }
    ordered.extend(content);
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance_data::paths::ensure_instance_data_layout;
    use crate::instance_data::types::{
        LoadOrder, LoadOrderEntry, NerevarManifest, PackageKind, ResolvedOpenMwConfig,
        LOAD_ORDER_VERSION,
    };
    use crate::instance_settings::InstanceSettings;

    #[test]
    fn resolve_orders_data_paths_by_priority() {
        let dir = std::env::temp_dir().join(format!("nerevar-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_instance_data_layout(&dir).unwrap();

        let low = dir.join("low");
        let high = dir.join("high");
        std::fs::create_dir_all(&low).unwrap();
        std::fs::create_dir_all(&high).unwrap();

        let load_order = LoadOrder {
            version: LOAD_ORDER_VERSION,
            base_game_data: None,
            content_order: None,
            entries: vec![
                LoadOrderEntry {
                    id: "1".into(),
                    name: "low".into(),
                    kind: PackageKind::Replacer,
                    relative_dir: "low".into(),
                    enabled: true,
                    priority: 10,
                    plugins: vec![],
                    tree_checksum: None,
                },
                LoadOrderEntry {
                    id: "2".into(),
                    name: "high".into(),
                    kind: PackageKind::Replacer,
                    relative_dir: "high".into(),
                    enabled: true,
                    priority: 20,
                    plugins: vec![],
                    tree_checksum: None,
                },
            ],
        };

        let resolved = resolve_load_order(&dir, &load_order).unwrap();
        assert_eq!(resolved.data_paths.len(), 2);
        assert!(resolved.data_paths[1].contains("high"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_synced_uses_host_content_when_local_plugins_empty() {
        let dir = std::env::temp_dir().join(format!("nerevar-synced-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_instance_data_layout(&dir).unwrap();

        let pkg = dir.join("Better Bodies");
        std::fs::create_dir_all(pkg.join("nested")).unwrap();
        std::fs::write(pkg.join("nested/Better Bodies.esp"), b"").unwrap();

        let load_order = LoadOrder {
            version: LOAD_ORDER_VERSION,
            base_game_data: None,
            content_order: None,
            entries: vec![LoadOrderEntry {
                id: "1".into(),
                name: "Better Bodies".into(),
                kind: PackageKind::Mod,
                relative_dir: "Better Bodies".into(),
                enabled: true,
                priority: 10,
                plugins: vec![],
                tree_checksum: None,
            }],
        };

        let manifest = NerevarManifest {
            version: 1,
            instance_id: "x".into(),
            instance_name: "x".into(),
            generated_at: String::new(),
            base_game_data: None,
            packages: vec![],
            resolved: ResolvedOpenMwConfig {
                encoding: "win1252".into(),
                data_paths: vec![],
                content: vec!["Better Bodies.esp".into()],
                archives: vec![],
            },
            total_download_bytes: 0,
            tes3mp_server_port: 25565,
            tes3mp_server_password: String::new(),
            required_data_files: vec![],
            instance_settings: InstanceSettings::default(),
        };

        let resolved = resolve_synced_load_order(&dir, &load_order, &manifest).unwrap();
        assert!(
            resolved
                .content
                .iter()
                .any(|p| p.eq_ignore_ascii_case("Better Bodies.esp")),
            "expected Better Bodies.esp in synced content, got {:?}",
            resolved.content
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_content_order_can_reposition_base_esms() {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-explicit-base-order-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_instance_data_layout(&dir).unwrap();

        let base = dir.join("Base Data");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("Morrowind.esm"), b"TES3").unwrap();
        std::fs::write(base.join("Tribunal.esm"), b"TES3").unwrap();
        std::fs::write(base.join("Bloodmoon.esm"), b"TES3").unwrap();

        let load_order = LoadOrder {
            version: LOAD_ORDER_VERSION,
            base_game_data: Some(base.to_string_lossy().to_string()),
            content_order: Some(vec![
                "Morrowind.esm".into(),
                "Bloodmoon.esm".into(),
                "Tribunal.esm".into(),
            ]),
            entries: vec![],
        };

        let resolved = resolve_load_order(&dir, &load_order).unwrap();
        let morrowind_pos = resolved
            .content
            .iter()
            .position(|name| name.eq_ignore_ascii_case("Morrowind.esm"))
            .unwrap();
        let bloodmoon_pos = resolved
            .content
            .iter()
            .position(|name| name.eq_ignore_ascii_case("Bloodmoon.esm"))
            .unwrap();
        let tribunal_pos = resolved
            .content
            .iter()
            .position(|name| name.eq_ignore_ascii_case("Tribunal.esm"))
            .unwrap();
        assert!(morrowind_pos < bloodmoon_pos);
        assert!(bloodmoon_pos < tribunal_pos);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn plugin(file: &str) -> crate::instance_data::types::PluginEntry {
        crate::instance_data::types::PluginEntry {
            file: file.into(),
            enabled: true,
        }
    }

    fn package_entry(id: &str, dir: &str, enabled: bool, priority: u32, plugins: &[&str]) -> LoadOrderEntry {
        LoadOrderEntry {
            id: id.into(),
            name: dir.into(),
            kind: PackageKind::Mod,
            relative_dir: dir.into(),
            enabled,
            priority,
            plugins: plugins.iter().map(|file| plugin(file)).collect(),
            tree_checksum: None,
        }
    }

    /// Two packages with an `.esp` and a `.bsa` each, plus a disabled one.
    fn archive_fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nerevar-resolve-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_instance_data_layout(&dir).unwrap();
        for (pkg, files) in [
            ("Foo", &["Foo.esp", "Foo.bsa"][..]),
            ("Bar", &["Bar.bsa"][..]),
            ("Off", &["Off.esp", "Off.bsa"][..]),
        ] {
            std::fs::create_dir_all(dir.join(pkg)).unwrap();
            for file in files {
                std::fs::write(dir.join(pkg).join(file), b"").unwrap();
            }
        }
        dir
    }

    fn archive_load_order(content_order: Option<Vec<String>>) -> LoadOrder {
        LoadOrder {
            version: LOAD_ORDER_VERSION,
            base_game_data: None,
            content_order,
            entries: vec![
                // Bar has the higher priority, so its archive loads last.
                package_entry("2", "Bar", true, 20, &["Bar.bsa"]),
                package_entry("1", "Foo", true, 10, &["Foo.bsa", "Foo.esp"]),
                package_entry("3", "Off", false, 30, &["Off.bsa", "Off.esp"]),
            ],
        }
    }

    #[test]
    fn an_enabled_packages_bsa_is_an_archive_never_content() {
        let dir = archive_fixture("archives");
        let resolved = resolve_load_order(&dir, &archive_load_order(None)).unwrap();
        assert_eq!(resolved.content, vec!["Foo.esp".to_string()]);
        assert_eq!(
            resolved.archives,
            vec!["Foo.bsa".to_string(), "Bar.bsa".to_string()],
            "archives follow package priority; the disabled package contributes none"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The GUI's global order is built from every enabled plugin, archives
    /// included; naming a `.bsa` there must not put it in `content=`.
    #[test]
    fn an_explicit_content_order_naming_a_bsa_still_keeps_it_out_of_content() {
        let dir = archive_fixture("archives-explicit");
        let order = archive_load_order(Some(vec![
            "Bar.bsa".into(),
            "Foo.bsa".into(),
            "Foo.esp".into(),
            "Off.esp".into(),
        ]));
        let resolved = resolve_load_order(&dir, &order).unwrap();
        assert_eq!(resolved.content, vec!["Foo.esp".to_string()]);
        assert_eq!(resolved.archives, vec!["Foo.bsa".to_string(), "Bar.bsa".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_disabled_archive_plugin_is_left_out() {
        let dir = archive_fixture("archives-disabled");
        let mut order = archive_load_order(None);
        order.entries[1].plugins[0].enabled = false; // Foo.bsa
        let resolved = resolve_load_order(&dir, &order).unwrap();
        assert_eq!(resolved.archives, vec!["Bar.bsa".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A manifest from a host that predates `archives` lists the `.bsa` in
    /// `content`; the client moves it to `archives`.
    #[test]
    fn synced_resolution_moves_a_legacy_hosts_bsa_out_of_content() {
        let dir = archive_fixture("archives-synced");
        let manifest = NerevarManifest {
            version: 1,
            instance_id: "x".into(),
            instance_name: "x".into(),
            generated_at: String::new(),
            base_game_data: None,
            packages: vec![],
            resolved: ResolvedOpenMwConfig {
                encoding: "win1252".into(),
                data_paths: vec![],
                content: vec!["Foo.esp".into(), "Foo.bsa".into(), "Bar.bsa".into()],
                archives: vec![],
            },
            total_download_bytes: 0,
            tes3mp_server_port: 25565,
            tes3mp_server_password: String::new(),
            required_data_files: vec![],
            instance_settings: InstanceSettings::default(),
        };
        let resolved = resolve_synced_load_order(&dir, &archive_load_order(None), &manifest).unwrap();
        assert_eq!(resolved.content, vec!["Foo.esp".to_string()]);
        assert_eq!(resolved.archives, vec!["Foo.bsa".to_string(), "Bar.bsa".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
