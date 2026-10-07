use std::path::Path;

/// Classic TES3 record plugins sorted by master dependencies.
pub const RECORD_PLUGIN_EXTENSIONS: &[&str] = &["esm", "esp"];

/// Additional OpenMW `content=` entries (Lua packs, OpenMW addons).
pub const EXTENDED_CONTENT_EXTENSIONS: &[&str] = &["omwscripts", "omwaddon"];

/// Asset archives a package may ship. OpenMW registers these with
/// `fallback-archive=`, never `content=`: its content loader rejects an
/// unknown extension, which is fatal at client start.
pub const ARCHIVE_EXTENSIONS: &[&str] = &["bsa"];

pub fn file_extension(name: &str) -> Option<&str> {
    Path::new(name).extension()?.to_str()
}

pub fn is_record_plugin(name: &str) -> bool {
    file_extension(name)
        .is_some_and(|ext| RECORD_PLUGIN_EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e)))
}

/// Whether OpenMW loads `name` through a `content=` line.
pub fn is_openmw_content_file(name: &str) -> bool {
    file_extension(name).is_some_and(is_content_extension)
}

/// Whether `name` is an asset archive, loaded through `fallback-archive=`.
pub fn is_archive_file(name: &str) -> bool {
    file_extension(name).is_some_and(is_archive_extension)
}

/// Whether a file belongs in a package's plugin list: a `content=` file or an
/// archive. The list is what admins enable and disable; which config line an
/// entry becomes is decided from its extension when the config is resolved.
pub fn is_package_plugin_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| is_content_extension(ext) || is_archive_extension(ext))
}

fn is_archive_extension(ext: &str) -> bool {
    ARCHIVE_EXTENSIONS
        .iter()
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
}

fn is_content_extension(ext: &str) -> bool {
    RECORD_PLUGIN_EXTENSIONS
        .iter()
        .chain(EXTENDED_CONTENT_EXTENSIONS.iter())
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_openmw_content_types() {
        assert!(is_openmw_content_file("TR_Mainland.esm"));
        assert!(is_openmw_content_file("Patch.esp"));
        assert!(is_openmw_content_file("my_mod.omwscripts"));
        assert!(is_openmw_content_file("scripts.omwaddon"));
        assert!(!is_openmw_content_file("TR_Mainland.bsa"));
        assert!(!is_openmw_content_file("readme.txt"));
    }

    #[test]
    fn archives_are_package_plugins_but_not_content() {
        assert!(is_archive_file("TR_Mainland.bsa"));
        assert!(is_archive_file("loud.BSA"));
        assert!(!is_archive_file("TR_Mainland.esm"));
        assert!(is_package_plugin_path(Path::new("pkg/TR_Mainland.bsa")));
        assert!(is_package_plugin_path(Path::new("pkg/TR_Mainland.esm")));
        assert!(is_package_plugin_path(Path::new("pkg/scripts/pack.omwscripts")));
        assert!(!is_package_plugin_path(Path::new("pkg/readme.txt")));
    }

    #[test]
    fn record_plugins_exclude_lua_and_archives() {
        assert!(is_record_plugin("TR_Mainland.esm"));
        assert!(!is_record_plugin("my_mod.omwscripts"));
        assert!(!is_record_plugin("TR_Mainland.bsa"));
    }
}
