//! `settings.cfg` overlay merging, used by `global_cfg`'s launch/restore flow.
//!
//! Relocated here (from the app crate's `instance_settings::openmw_settings`)
//! in the leaf-layer split (step 5): `global_cfg` calls these during launch
//! and restore, but `instance_settings` doesn't move to `nerevar-core` until
//! a later step. The functions are pure `&str -> String` transforms with no
//! dependency on `InstanceSettings` (unlike `format_settings_overlay`, which
//! stays in the app crate), so they move independently to break the
//! otherwise-circular dependency.

use std::collections::BTreeMap;

pub fn merge_settings_overlay(base: &str, overlay: &str) -> String {
    let mut base_sections = parse_settings_cfg(base);
    let overlay_sections = parse_settings_cfg(overlay);

    for (section, entries) in overlay_sections {
        let target = base_sections.entry(section).or_default();
        for (key, value) in entries {
            target.insert(key, value);
        }
    }

    render_settings_cfg(&base_sections)
}

pub fn merge_user_session_changes(
    before_launch: &str,
    after_session: &str,
    overlay: &str,
) -> String {
    let mut restored = parse_settings_cfg(before_launch);
    let session = parse_settings_cfg(after_session);
    let managed = parse_settings_cfg(overlay);

    for (section, entries) in session {
        let target = restored.entry(section.clone()).or_default();
        let managed_section = managed.get(&section);
        for (key, value) in entries {
            let managed_key = managed_section
                .and_then(|keys| keys.get(&key))
                .is_some();
            if managed_key {
                continue;
            }
            target.insert(key, value);
        }
    }

    render_settings_cfg(&restored)
}

fn parse_settings_cfg(contents: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut current = String::from("default");

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current = trimmed[1..trimmed.len() - 1].trim().to_string();
            sections.entry(current.clone()).or_default();
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        sections
            .entry(current.clone())
            .or_default()
            .insert(key.trim().to_string(), value.trim().to_string());
    }

    sections
}

fn render_settings_cfg(sections: &BTreeMap<String, BTreeMap<String, String>>) -> String {
    let mut out = String::from("# Nerevar active OpenMW settings (managed during TES3MP launch)\n");
    for (section, entries) in sections {
        if entries.is_empty() {
            continue;
        }
        out.push('\n');
        out.push_str(&format!("[{section}]\n"));
        for (key, value) in entries {
            out.push_str(&format!("{key} = {value}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_replaces_section_keys() {
        let base = "[Shaders]\nauto use object normal maps = false\nforce shaders = false\n";
        let overlay = "[Shaders]\nauto use object normal maps = true\n";
        let merged = merge_settings_overlay(base, overlay);
        assert!(merged.contains("auto use object normal maps = true"));
        assert!(merged.contains("force shaders = false"));
    }

    #[test]
    fn merge_user_session_changes_keeps_unmanaged_and_skips_managed() {
        let before = "[GUI]\nsubtitles = false\n\n[Shaders]\nforce shaders = false\n";
        let after = "[GUI]\nsubtitles = true\n\n[Shaders]\nforce shaders = true\n";
        let overlay = "[Shaders]\nforce shaders = true\n";
        let merged = merge_user_session_changes(before, after, overlay);
        assert!(merged.contains("subtitles = true"));
        assert!(merged.contains("force shaders = false"));
    }
}
