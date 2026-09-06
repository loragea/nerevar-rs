//! The version lock, end to end against a real host: a server that requires a
//! TES3MP version from a repository the player does not trust.
//!
//! Drives the same `sync_client::sync::sync_if_needed` the app and the CLI
//! call, against the real embedded axum server serving a real manifest, and
//! checks the two halves of the ruling:
//!
//! - the *version* the host names reaches the client, as a `runtime-mismatch`
//!   event and in the sync outcome, and blocks a launch;
//! - the *repository* it names does not. Nothing is downloaded from it,
//!   nothing about the instance's runtime changes, and the untrusted
//!   suggestion is reported as such.
//!
//! The mods still sync: a host may pin a runtime version without that
//! stopping its players from receiving its files.

use std::path::Path;
use std::sync::Arc;

use nerevar_core::data::InstanceConfig;
use nerevar_core::instance_data::{build_manifest, load_load_order, scan_and_merge_load_order};
use nerevar_core::nerevar_server::state::ServerContext;
use nerevar_core::nerevar_server::{serve, try_bind};
use nerevar_core::reporter::CollectingEventSink;
use nerevar_core::runtime::{
    resolve_runtime_hint, RuntimeHintResolution, RuntimeSource, TrustedRuntimeRepos,
};
use nerevar_core::sync_client::{fetch_manifest_summary, sync_if_needed, SyncCoordinator};
use nerevar_core::sync_host::{new_shared_hosting_manifest_cache, new_shared_sync_host};

/// A repository no Nerevar install trusts out of the box — the thing the
/// ruling exists to keep a server from pointing a client at.
const UNTRUSTED_REPO: &str = "attacker/tes3mp";
/// The version the host demands, and which the client does not have.
const REQUIRED_TAG: &str = "tes3mp-0.9.9";
/// The version the client's instance was installed with.
const INSTALLED_TAG: &str = "tes3mp-0.8.1";

fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_untrusted_hint_pins_the_version_but_never_the_repository() {
    let root = std::env::temp_dir().join(format!("nerevar-version-lock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    // ---- HOST: one package, a manifest, and a runtime hint from a repo the
    // player has never trusted --------------------------------------------
    let host_root = root.join("host");
    let host_data = host_root.join("data");
    write(&host_data.join("ModA/plugin.esp"), b"ModA plugin payload");
    write(
        &host_root.join("tes3mp/tes3mp-server-default.cfg"),
        b"[General]\nhostname = Version Lock\nport = 25565\npassword = \n",
    );
    std::fs::create_dir_all(host_root.join("tes3mp/server/data")).unwrap();
    write(
        &host_root.join("tes3mp/server/scripts/config.lua"),
        b"config = {}\n",
    );

    let mut no_progress = None;
    scan_and_merge_load_order(&host_data, &mut no_progress).expect("scan");
    let load_order = load_load_order(&host_data).expect("load order");
    build_manifest(
        "version-lock-host",
        "Version Lock Host",
        &host_root,
        &host_data,
        &load_order,
        &mut no_progress,
    )
    .expect("build manifest");

    let sync_host = new_shared_sync_host();
    {
        let mut host = sync_host.lock().unwrap();
        host.hosting_instance_id = Some("version-lock-host".to_string());
        host.hosting_data_dir = Some(host_data.clone());
        host.hosting_instance_root = Some(host_root.clone());
        host.hosting_runtime_hint = Some(RuntimeSource::GithubRelease {
            repo: UNTRUSTED_REPO.to_string(),
            release_id: "999".to_string(),
            tag: REQUIRED_TAG.to_string(),
            asset_name: String::new(),
        });
    }
    let ctx = ServerContext::new(sync_host, new_shared_hosting_manifest_cache());
    let listener = try_bind(0).await.expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    tokio::spawn(async move {
        let _ = serve(listener, ctx).await;
    });

    // ---- CLIENT: an instance on 0.8.1 from the official repository --------
    let instance_root = root.join("client-instance");
    let instance_data = instance_root.join("data");
    std::fs::create_dir_all(&instance_data).unwrap();
    let client_cfg = instance_root.join("tes3mp/tes3mp-client-default.cfg");
    write(
        &client_cfg,
        b"[General]\ndestinationAddress = 0.0.0.0\nport = 0\npassword = \n",
    );

    let instance = InstanceConfig {
        id: "version-lock-client".to_string(),
        name: "Version Lock Client".to_string(),
        description: String::new(),
        path: instance_root.to_string_lossy().into_owned(),
        data_dir: instance_data.to_string_lossy().into_owned(),
        release_id: None,
        runtime: Some(RuntimeSource::GithubRelease {
            repo: "tes3mp/tes3mp".to_string(),
            release_id: "65767406".to_string(),
            tag: INSTALLED_TAG.to_string(),
            asset_name: String::new(),
        }),
        runtime_hint: None,
        remote_host: Some("127.0.0.1".to_string()),
        remote_sync_port: Some(port),
        last_synced_at: None,
        tes3mp_server_port: None,
        sync_password: None,
    };

    // The player's trust list is the untouched default: official only.
    let trusted = TrustedRuntimeRepos::builtin_only();
    assert!(!trusted.is_trusted(UNTRUSTED_REPO));

    let sink = Arc::new(CollectingEventSink::default());
    let outcome = sync_if_needed(
        sink.clone(),
        Arc::new(SyncCoordinator::new()),
        &instance,
        false,
        &trusted,
    )
    .await
    .expect("the sync itself must still succeed");

    // The mods synced: a version requirement does not stop the file transfer.
    assert!(
        outcome.validation.valid,
        "sync issues: {:?}",
        outcome.validation.issues
    );
    assert!(
        instance_data.join("ModA/plugin.esp").is_file(),
        "the host's package must have been downloaded"
    );

    // The version reached the client and blocks the launch.
    let mismatch = outcome
        .runtime_mismatch
        .as_ref()
        .expect("the host requires a version this instance does not have");
    assert!(outcome.blocks_launch());
    assert_eq!(mismatch.instance_id, "version-lock-client");
    assert_eq!(mismatch.required_tag, REQUIRED_TAG);
    assert_eq!(mismatch.installed_tag, INSTALLED_TAG);
    assert!(mismatch.enforced);

    // The repository did not. An update would look in the instance's own
    // repository, and the suggestion is reported as untrusted.
    assert_eq!(mismatch.hint_repo, UNTRUSTED_REPO);
    assert!(!mismatch.hint_repo_trusted);
    assert_eq!(
        mismatch.repo, "tes3mp/tes3mp",
        "an update must come from the repository the player chose, never the host's"
    );
    assert!(
        mismatch.message.contains("not one of your trusted sources"),
        "the message must say why the suggestion was not followed: {}",
        mismatch.message
    );

    // The same information reaches a UI as an event, once.
    let events: Vec<serde_json::Value> = sink
        .events()
        .into_iter()
        .filter(|(name, _)| *name == "runtime-mismatch")
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(events.len(), 1, "one runtime-mismatch per sync: {events:?}");
    assert_eq!(events[0]["requiredTag"], REQUIRED_TAG);
    assert_eq!(events[0]["repo"], "tes3mp/tes3mp");
    assert_eq!(events[0]["hintRepo"], UNTRUSTED_REPO);
    assert_eq!(events[0]["hintRepoTrusted"], false);

    // Nothing was fetched: the instance's runtime directory holds exactly what
    // the fixture put there, and its recorded source is untouched.
    let runtime_entries: Vec<String> = std::fs::read_dir(instance_root.join("tes3mp"))
        .expect("runtime dir")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        runtime_entries,
        vec!["tes3mp-client-default.cfg".to_string()],
        "a hinted repository must never become a download"
    );
    assert!(matches!(
        instance.runtime,
        Some(RuntimeSource::GithubRelease { ref tag, .. }) if tag == INSTALLED_TAG
    ));

    // And the picker resolution over the same summary refuses to preselect it.
    let summary = fetch_manifest_summary("127.0.0.1", port, None)
        .await
        .expect("summary");
    let resolution = resolve_runtime_hint(summary.runtime_hint.as_ref(), &trusted);
    let RuntimeHintResolution::Untrusted {
        repo,
        fallback_repo,
        ..
    } = resolution
    else {
        panic!("an untrusted hint must not preselect: {resolution:?}");
    };
    assert_eq!(repo, UNTRUSTED_REPO);
    assert_eq!(fallback_repo, "tes3mp/tes3mp");

    let _ = std::fs::remove_dir_all(&root);
}
