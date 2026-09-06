//! `nerevar-host admin add|list|revoke`.
//!
//! Thin wiring: resolve the same config and instance a daemon run would, then
//! call `nerevar_core::admin`. The store lives beside the instance's data, so
//! these commands work whether or not a daemon is running — the daemon
//! re-reads `admins.json` on every `/admin` request, so an add or a revoke
//! takes effect on the next one with no reload signal.
//!
//! Output split: the token goes to stdout alone, so
//! `nerevar-host admin add ada > token` captures exactly the secret and
//! nothing else. Every human-facing line goes to stderr, where the daemon's
//! log already goes.

use std::io::Write;
use std::path::Path;

use nerevar_core::admin::{add_admin, admins_file_path, list_admins, revoke_admin};
use nerevar_core::instance_data::resolve_package_data_dir;

use crate::cli::{AdminAction, Cli};
use crate::{config, instance};

/// Runs one `admin` subcommand. `Ok(0)` on success; an `Err` becomes exit 1
/// through `main`, the daemon's existing convention for "config or startup
/// failure" (69 stays reserved for the TES3MP server dying).
pub fn run(cli: &Cli, action: &AdminAction) -> Result<i32, String> {
    let resolved = config::resolve_and_load_config(cli.config.as_deref())?;
    let instance = instance::select_owned_instance(&resolved.config, cli.instance.as_deref())?;
    let data_dir = resolve_package_data_dir(instance);

    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    execute(&data_dir, action, &mut stdout.lock(), &mut stderr.lock())
}

/// The command bodies, with both output streams injected so tests can read
/// them. `out` carries the command's data (the token, the listing); `notes`
/// carries everything a human reads.
pub fn execute(
    data_dir: &Path,
    action: &AdminAction,
    out: &mut dyn Write,
    notes: &mut dyn Write,
) -> Result<i32, String> {
    match action {
        AdminAction::Add { name, role } => {
            let created = add_admin(data_dir, name, role)?;
            writeln!(out, "{}", created.token).map_err(write_error)?;
            writeln!(
                notes,
                "Created admin \"{}\" with role \"{}\" in {}.",
                created.record.name,
                created.record.role,
                admins_file_path(data_dir).display()
            )
            .map_err(write_error)?;
            writeln!(
                notes,
                "The token above is shown once and is not recoverable; only its SHA-256 is \
                 stored. Send it over a private channel — it travels as the HTTP header \
                 `Authorization: Bearer <token>`."
            )
            .map_err(write_error)?;
        }
        AdminAction::List => {
            let admins = list_admins(data_dir)?;
            if admins.is_empty() {
                writeln!(
                    notes,
                    "No admins yet. Create one with: nerevar-host admin add <name>"
                )
                .map_err(write_error)?;
                return Ok(0);
            }
            writeln!(out, "{:<24}  {:<12}  CREATED", "NAME", "ROLE").map_err(write_error)?;
            for admin in admins {
                writeln!(
                    out,
                    "{:<24}  {:<12}  {}",
                    admin.name, admin.role, admin.created_at
                )
                .map_err(write_error)?;
            }
        }
        AdminAction::Revoke { name } => {
            if !revoke_admin(data_dir, name)? {
                return Err(format!("No admin named \"{}\" to revoke", name.trim()));
            }
            writeln!(
                notes,
                "Revoked \"{}\". Its token stops working on the next /admin request.",
                name.trim()
            )
            .map_err(write_error)?;
        }
    }
    Ok(0)
}

fn write_error(error: std::io::Error) -> String {
    format!("Failed to write output: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use nerevar_core::admin::{load_admins, ROLE_ADMIN};
    use std::path::PathBuf;

    use crate::cli::Command;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-host-admin-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// Runs a subcommand as `main` would and returns (stdout, stderr).
    fn call(data_dir: &Path, action: &AdminAction) -> Result<(String, String), String> {
        let mut out: Vec<u8> = Vec::new();
        let mut notes: Vec<u8> = Vec::new();
        execute(data_dir, action, &mut out, &mut notes)?;
        Ok((
            String::from_utf8(out).unwrap(),
            String::from_utf8(notes).unwrap(),
        ))
    }

    fn parse(args: &[&str]) -> AdminAction {
        let cli = Cli::try_parse_from(args).expect("parse");
        match cli.command.expect("a subcommand") {
            Command::Admin { action } => action,
        }
    }

    #[test]
    fn add_prints_only_the_token_on_stdout_and_stores_its_hash() {
        let scratch = scratch("add");
        let action = parse(&["nerevar-host", "admin", "add", "ada"]);
        let (out, notes) = call(&scratch.0, &action).expect("add");

        let token = out.trim();
        assert_eq!(out.lines().count(), 1, "stdout must be the token alone");
        assert_eq!(token.len(), 64);
        assert!(notes.contains("Created admin \"ada\""), "{notes}");
        assert!(!notes.contains(token), "stderr must not repeat the token");

        let store = load_admins(&scratch.0).expect("store");
        assert_eq!(store.admins.len(), 1);
        assert_eq!(store.admins[0].role, ROLE_ADMIN);
        assert_eq!(
            store.authenticate(token).map(|r| r.name.as_str()),
            Some("ada")
        );
    }

    #[test]
    fn add_refuses_a_duplicate_name_and_an_unknown_role() {
        let scratch = scratch("dupe");
        call(&scratch.0, &parse(&["nerevar-host", "admin", "add", "ada"])).expect("first");

        let error = call(&scratch.0, &parse(&["nerevar-host", "admin", "add", "ada"]))
            .expect_err("duplicate must fail");
        assert!(error.contains("already exists"), "{error}");

        let error = call(
            &scratch.0,
            &parse(&["nerevar-host", "admin", "add", "bob", "--role", "wizard"]),
        )
        .expect_err("unknown role must fail");
        assert!(error.contains("Unknown role"), "{error}");

        assert_eq!(load_admins(&scratch.0).unwrap().admins.len(), 1);
    }

    #[test]
    fn list_shows_names_roles_and_dates_but_never_hashes() {
        let scratch = scratch("list");
        let (empty_out, empty_notes) =
            call(&scratch.0, &parse(&["nerevar-host", "admin", "list"])).expect("empty list");
        assert!(empty_out.is_empty());
        assert!(empty_notes.contains("No admins yet"), "{empty_notes}");

        call(&scratch.0, &parse(&["nerevar-host", "admin", "add", "ada"])).expect("ada");
        call(&scratch.0, &parse(&["nerevar-host", "admin", "add", "bob"])).expect("bob");

        let (out, _) = call(&scratch.0, &parse(&["nerevar-host", "admin", "list"])).expect("list");
        assert!(out.contains("NAME"));
        assert!(out.contains("ada") && out.contains("bob"));
        assert!(out.contains(ROLE_ADMIN));
        for record in load_admins(&scratch.0).unwrap().admins {
            assert!(
                !out.contains(&record.token_sha256),
                "the listing must not print token hashes"
            );
        }
    }

    #[test]
    fn revoke_removes_a_name_and_fails_on_one_that_is_not_there() {
        let scratch = scratch("revoke");
        let (out, _) =
            call(&scratch.0, &parse(&["nerevar-host", "admin", "add", "ada"])).expect("add");
        let token = out.trim().to_string();

        let (_, notes) = call(
            &scratch.0,
            &parse(&["nerevar-host", "admin", "revoke", "ada"]),
        )
        .expect("revoke");
        assert!(notes.contains("Revoked \"ada\""), "{notes}");
        assert!(load_admins(&scratch.0)
            .unwrap()
            .authenticate(&token)
            .is_none());

        let error = call(
            &scratch.0,
            &parse(&["nerevar-host", "admin", "revoke", "ada"]),
        )
        .expect_err("second revoke must fail");
        assert!(error.contains("No admin named"), "{error}");
    }

    #[test]
    fn a_run_with_no_subcommand_is_still_a_daemon_run() {
        let cli = Cli::try_parse_from(["nerevar-host", "--sync-only"]).expect("parse");
        assert!(cli.command.is_none());
        assert!(cli.sync_only);
    }
}
