use super::types::InstanceSettings;

pub fn format_settings_overlay(settings: &InstanceSettings) -> String {
    let mut out = String::from("# Nerevar instance launch settings overlay\n");
    for (section, entries) in &settings.openmw_settings {
        if entries.is_empty() {
            continue;
        }
        out.push('\n');
        out.push_str(&format!("[{section}]\n"));
        for (key, value) in entries {
            out.push_str(&format!("{} = {}\n", key, value.as_settings_cfg_value()));
        }
    }

    let manual = settings.openmw_settings_cfg_overrides.trim();
    if !manual.is_empty() {
        out.push('\n');
        out.push_str("# Manual settings.cfg overrides\n");
        out.push_str(manual);
        if !manual.ends_with('\n') {
            out.push('\n');
        }
    }

    out
}

// `merge_settings_overlay` and `merge_user_session_changes` (plus their
// private `parse_settings_cfg`/`render_settings_cfg` helpers) moved to
// `nerevar_core::openmw_ini_importer::settings_merge` in the leaf-layer
// split (step 5): `global_cfg` (now core-side) calls them during launch and
// restore, and they're pure `&str -> String` transforms with no dependency
// on `InstanceSettings`, so they could move ahead of the rest of
// `instance_settings`. Nothing in this crate still calls them by this path,
// so no re-export shim is needed here.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_settings_overlay_appends_manual_block() {
        use crate::instance_settings::InstanceSettings;

        let settings = InstanceSettings {
            version: 1,
            tes3mp_game_settings: vec![],
            openmw_settings: Default::default(),
            openmw_cfg_overrides: vec![],
            openmw_settings_cfg_overrides: "[Cells]\nviewing distance = 7168".into(),
        };
        let overlay = format_settings_overlay(&settings);
        assert!(overlay.contains("[Cells]"));
        assert!(overlay.contains("viewing distance = 7168"));
    }
}
