use nerevar_core::data::{InstanceConfig, NerevarConfig};

/// Selects the owned instance to host: matched by id first, then by name,
/// when `--instance` is given; otherwise the config's sole owned instance.
/// Only `owned_instances` are eligible — hosting a synced (client-only)
/// instance's TES3MP server makes no sense, and sync hosting itself is
/// defined in terms of an owned instance's local data.
pub fn select_owned_instance<'a>(
    config: &'a NerevarConfig,
    requested: Option<&str>,
) -> Result<&'a InstanceConfig, String> {
    let owned = config.owned_instances.as_deref().unwrap_or(&[]);

    if owned.is_empty() {
        return Err("Config has no owned instances to host.".to_string());
    }

    match requested {
        Some(needle) => owned
            .iter()
            .find(|i| i.id == needle)
            .or_else(|| owned.iter().find(|i| i.name == needle))
            .ok_or_else(|| {
                format!(
                    "No owned instance matches --instance {needle:?}. Available: {}",
                    describe_instances(owned)
                )
            }),
        None => {
            if owned.len() == 1 {
                Ok(&owned[0])
            } else {
                Err(format!(
                    "Config has {} owned instances; pass --instance <id-or-name>. Available: {}",
                    owned.len(),
                    describe_instances(owned)
                ))
            }
        }
    }
}

fn describe_instances(owned: &[InstanceConfig]) -> String {
    owned
        .iter()
        .map(|i| format!("{} ({})", i.name, i.id))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(id: &str, name: &str) -> InstanceConfig {
        InstanceConfig {
            id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            path: String::new(),
            data_dir: String::new(),
            release_id: None,
            runtime: None,
            remote_host: None,
            remote_sync_port: None,
            last_synced_at: None,
            tes3mp_server_port: None,
            sync_password: None,
        }
    }

    // `Result::unwrap_err` requires `T: Debug`, which `InstanceConfig`
    // doesn't implement (it's not needed anywhere in production code); this
    // sidesteps that instead of adding a `#[derive(Debug)]` just for tests.
    fn expect_err<T>(result: Result<T, String>) -> String {
        match result {
            Ok(_) => panic!("expected Err, got Ok"),
            Err(err) => err,
        }
    }

    fn config_with(owned: Vec<InstanceConfig>) -> NerevarConfig {
        NerevarConfig {
            onboarding_complete: true,
            owned_instances: Some(owned),
            synced_instances: None,
            root_path: None,
            sync_port: 25567,
        }
    }

    #[test]
    fn sole_owned_instance_used_when_none_requested() {
        let config = config_with(vec![instance("id-1", "Only")]);
        let picked = select_owned_instance(&config, None).unwrap();
        assert_eq!(picked.id, "id-1");
    }

    #[test]
    fn ambiguous_without_instance_flag_errors() {
        let config = config_with(vec![instance("id-1", "A"), instance("id-2", "B")]);
        let err = expect_err(select_owned_instance(&config, None));
        assert!(err.contains("--instance"));
    }

    #[test]
    fn matches_by_id_before_name() {
        // An instance named "id-2" would collide with the other's id; id match wins.
        let config = config_with(vec![instance("id-1", "id-2"), instance("id-2", "Real")]);
        let picked = select_owned_instance(&config, Some("id-2")).unwrap();
        assert_eq!(picked.id, "id-2");
        assert_eq!(picked.name, "Real");
    }

    #[test]
    fn matches_by_name_when_no_id_matches() {
        let config = config_with(vec![instance("id-1", "Alpha"), instance("id-2", "Beta")]);
        let picked = select_owned_instance(&config, Some("Beta")).unwrap();
        assert_eq!(picked.id, "id-2");
    }

    #[test]
    fn unknown_selector_lists_available() {
        let config = config_with(vec![instance("id-1", "Alpha")]);
        let err = expect_err(select_owned_instance(&config, Some("nope")));
        assert!(err.contains("Alpha"));
        assert!(err.contains("id-1"));
    }

    #[test]
    fn no_owned_instances_errors() {
        let config = config_with(vec![]);
        let err = expect_err(select_owned_instance(&config, None));
        assert!(err.contains("no owned instances"));
    }
}
