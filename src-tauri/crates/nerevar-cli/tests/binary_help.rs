//! The binary parses its own command line.
//!
//! `nerevar-cli` is the first crate here with a subcommand tree, and a clap
//! definition can be wrong in ways a library test never sees — two options
//! claiming the same short flag, a global argument on a subcommand that does
//! not accept globals — because clap only asserts them when it builds the
//! command. `--help` is the cheapest invocation that forces that build, so
//! this runs the real binary rather than calling into the library.
//!
//! No `assert_cmd` in the tree, and none added for two spawns: Cargo hands an
//! integration test the path to its crate's binaries in
//! `CARGO_BIN_EXE_<name>`, so `std::process::Command` is enough.

use std::process::Command;

const BINARY: &str = env!("CARGO_BIN_EXE_nerevar-cli");

fn help(args: &[&str]) -> String {
    let output = Command::new(BINARY)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("failed to run {BINARY} {args:?}: {error}"));
    assert!(
        output.status.success(),
        "{BINARY} {args:?} exited {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("help output is UTF-8")
}

#[test]
fn sync_help_parses_and_lists_the_flags_the_rig_drives() {
    let text = help(&["sync", "--help"]);
    for flag in [
        "--config",
        "--instance",
        "--install-runtime",
        "--force",
        "--launch",
        "--onboard",
    ] {
        assert!(
            text.contains(flag),
            "sync --help is missing {flag}:\n{text}"
        );
    }
}

#[test]
fn admin_help_parses_and_lists_every_subcommand() {
    let text = help(&["admin", "--help"]);
    for command in [
        "status",
        "upload",
        "remove",
        "load-order",
        "enable",
        "disable",
        "order",
        "apply",
        "discard",
        "restart",
    ] {
        assert!(
            text.contains(command),
            "admin --help is missing {command}:\n{text}"
        );
    }
    for flag in ["--host", "--port", "--token", "--token-file", "--json"] {
        assert!(
            text.contains(flag),
            "admin --help is missing {flag}:\n{text}"
        );
    }
}

#[test]
fn the_top_level_help_names_both_surfaces() {
    let text = help(&["--help"]);
    assert!(text.contains("sync"), "{text}");
    assert!(text.contains("admin"), "{text}");
}
