//! What `nerevar-cli sync` does when the host requires a TES3MP version the
//! instance does not have.
//!
//! The decision is `sync::mismatch_action`, driven here through the library
//! exactly as `run` drives it. No host, no network and no TES3MP install are
//! needed for it — which is the point of the policy being a function rather
//! than a branch buried in the run loop.

use nerevar_cli::sync::{mismatch_action, MismatchAction};
use nerevar_core::runtime::{
    runtime_mismatch, RuntimeMismatch, RuntimeSource, TrustedRuntimeRepos,
};

fn github(tag: &str) -> RuntimeSource {
    RuntimeSource::GithubRelease {
        repo: "tes3mp/tes3mp".to_string(),
        release_id: "1".to_string(),
        tag: tag.to_string(),
        asset_name: String::new(),
    }
}

/// The mismatch a host requiring 0.8.1 produces for an instance on 0.8.0 —
/// built through core's own computation, so the test cannot drift from what
/// a real sync hands the CLI.
fn enforced_mismatch() -> RuntimeMismatch {
    runtime_mismatch(
        "instance-1",
        Some(&github("tes3mp-0.8.1")),
        Some(&github("tes3mp-0.8.0")),
        &TrustedRuntimeRepos::builtin_only(),
    )
    .expect("0.8.0 does not satisfy 0.8.1")
}

/// The headline case: asked to launch, given no permission to install
/// anything, the CLI refuses instead of starting a client the server is on a
/// different build from. `run` turns this into exit code 1.
#[test]
fn launch_without_install_runtime_is_refused() {
    let mismatch = enforced_mismatch();
    assert!(mismatch.enforced);
    assert_eq!(
        mismatch_action(&mismatch, false, true),
        MismatchAction::RefuseLaunch
    );
    assert!(
        mismatch.message.contains("requires TES3MP tes3mp-0.8.1"),
        "the refusal has to name the version: {}",
        mismatch.message
    );
}

/// `--install-runtime` is the flag that authorises Nerevar to fetch a build,
/// so it is also what authorises the update the version lock asks for.
#[test]
fn install_runtime_updates_instead_of_refusing() {
    assert_eq!(
        mismatch_action(&enforced_mismatch(), true, true),
        MismatchAction::Update
    );
    // Without --launch too: a sync-only run leaves the instance ready.
    assert_eq!(
        mismatch_action(&enforced_mismatch(), true, false),
        MismatchAction::Update
    );
}

/// A sync that is not going to launch anything reports the requirement and
/// exits 0: nothing is broken yet, and the run did what it was asked.
#[test]
fn a_sync_that_launches_nothing_only_reports() {
    assert_eq!(
        mismatch_action(&enforced_mismatch(), false, false),
        MismatchAction::Report
    );
}

/// A runtime installed from the user's own folder has no version Nerevar can
/// read, so the requirement is reported and nothing is blocked or replaced —
/// even with `--launch`.
#[test]
fn an_unenforceable_mismatch_never_blocks_a_launch() {
    let mismatch = runtime_mismatch(
        "instance-1",
        Some(&github("tes3mp-0.8.1")),
        Some(&RuntimeSource::LocalDirectory {
            path: "/opt/MundusPatensMP".to_string(),
        }),
        &TrustedRuntimeRepos::builtin_only(),
    )
    .expect("still worth reporting");
    assert!(!mismatch.enforced);

    for (install_runtime, launch) in [(false, true), (true, true), (false, false)] {
        assert_eq!(
            mismatch_action(&mismatch, install_runtime, launch),
            MismatchAction::Report,
            "install_runtime={install_runtime} launch={launch}"
        );
    }
}
