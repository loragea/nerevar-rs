//! End-to-end host→client sync roundtrip test, exercised WITHOUT the Tauri layer.
//!
//! This drives the product's core loop — a host builds a manifest from an instance
//! data directory, serves it over the real embedded axum server, and a client fetches
//! the manifest and downloads every file back, verifying checksums — using the same
//! non-Tauri functions the Tauri commands call.
//!
//! Lives in `nerevar-core/tests/` as a real integration test: every module it needs
//! (`nerevar_server`, `sync_client`, `sync_host`, `instance_data`, `sync_auth`,
//! `reporter`) is `pub mod` on `nerevar-core`, so it only reaches the crate's public
//! API — same as any other consumer, including the future `nerevar-host` daemon.
//! `CollectingEventSink` is reachable here because it's gated
//! `cfg(any(test, feature = "test-util"))`; this crate's own `[dev-dependencies]`
//! enable `test-util` on itself so integration-test binaries can see it too (`cfg(test)`
//! alone only covers unit tests compiled inside the crate, not this external binary).
//!
//! COVERED as of the EventSink migration: the parallel download engine
//! (`sync_client/download.rs::download_manifest_files`) — the 16-worker pool, checksum
//! verification, sync-state persistence, and progress reporting. It used to require a
//! Tauri `AppHandle` to emit progress events; now it takes an `Arc<dyn EventSink>`, so
//! the test drives it end-to-end against the live server with a `CollectingEventSink`
//! (see the second client phase below). The first client phase still downloads each file
//! with a plain `reqwest` GET, exercising the raw package-file route directly.
//!
//! NOT covered here: mid-flight cancellation (the `cancel` AtomicBool flipping while
//! workers are in flight -> `DownloadOutcome::Cancelled`) and resume-after-partial (a
//! second `download_manifest_files` call adopting files a prior run left on disk and
//! skipping them via sync-state). This test only drives the clean, from-empty happy path
//! through the engine; those two paths live in `sync_cancel_resume.rs`.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use nerevar_core::instance_data::{
    build_manifest, file_checksum, load_load_order, manifest_path, package_abs_path,
    scan_and_merge_load_order,
};
use nerevar_core::nerevar_server::state::ServerContext;
use nerevar_core::nerevar_server::{serve, try_bind};
use nerevar_core::reporter::CollectingEventSink;
use nerevar_core::runtime::RuntimeSource;
use nerevar_core::sync_auth::SYNC_PASSWORD_HEADER;
use nerevar_core::sync_client::download::{download_manifest_files, DownloadOutcome};
use nerevar_core::sync_client::{fetch_full_manifest, fetch_manifest_summary};
use nerevar_core::sync_host::{new_shared_hosting_manifest_cache, new_shared_sync_host};

/// Sync password used to exercise the auth path end to end. Distinct from the (empty)
/// TES3MP server password so the two concepts stay clearly separated.
const SYNC_PASSWORD: &str = "swordfish";

fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

#[tokio::test]
async fn host_client_sync_roundtrip() {
    // ---- FIXTURE: a temp instance with two fake mod packages -----------------------
    let root = std::env::temp_dir().join(format!("nerevar-roundtrip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    let instance_root = root.join("instance");
    let data_dir = instance_root.join("data");
    let client_dir = root.join("client");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&client_dir).unwrap();

    // ModA: a plugin plus a loose texture (2 files, nested dir).
    let mod_a_plugin = b"ModA plugin payload".as_slice();
    let mod_a_texture = b"ModA texture bytes, deterministic".as_slice();
    write(&data_dir.join("ModA/plugin.esp"), mod_a_plugin);
    write(&data_dir.join("ModA/textures/a.dds"), mod_a_texture);
    // ModB: a single plugin (1 file).
    let mod_b_plugin = b"ModB plugin payload, also deterministic".as_slice();
    write(&data_dir.join("ModB/other.esp"), mod_b_plugin);

    // Minimal TES3MP layout that build_manifest reads/writes through:
    //   - tes3mp-server-default.cfg    -> read_tes3mp_server_settings (port/password)
    //   - server/data/                 -> write_required_data_files target
    //   - server/scripts/config.lua    -> write_tes3mp_game_settings target
    let tes3mp_dir = instance_root.join("tes3mp");
    write(
        &tes3mp_dir.join("tes3mp-server-default.cfg"),
        b"[General]\nhostname = Roundtrip Test\nport = 25565\npassword = \n",
    );
    std::fs::create_dir_all(tes3mp_dir.join("server/data")).unwrap();
    write(&tes3mp_dir.join("server/scripts/config.lua"), b"config = {}\n");

    // Real host scan + load-order build (the `scan_instance_data` command's core).
    let mut no_progress = None;
    scan_and_merge_load_order(&data_dir, &mut no_progress).expect("scan + merge load order");
    let load_order = load_load_order(&data_dir).expect("load order");

    // Real manifest build (the `build_instance_manifest` / `save_and_host_instance` core).
    let host_manifest = build_manifest(
        "roundtrip-instance",
        "Roundtrip Instance",
        &instance_root,
        &data_dir,
        &load_order,
        &mut no_progress,
    )
    .expect("build manifest");

    // A real manifest.json must have been written to disk.
    assert!(
        manifest_path(&data_dir).is_file(),
        "manifest.json should be written under the data dir"
    );

    // Sanity on what the host built, so the client-side assertions below are meaningful.
    assert_eq!(host_manifest.packages.len(), 2, "expected ModA + ModB");
    let host_mod_a = host_manifest
        .packages
        .iter()
        .find(|p| p.name == "ModA")
        .expect("ModA package");
    let host_mod_b = host_manifest
        .packages
        .iter()
        .find(|p| p.name == "ModB")
        .expect("ModB package");
    assert_eq!(host_mod_a.file_count, 2, "ModA: plugin.esp + textures/a.dds");
    assert_eq!(host_mod_b.file_count, 1, "ModB: other.esp");
    let expected_total_bytes: u64 =
        (mod_a_plugin.len() + mod_a_texture.len() + mod_b_plugin.len()) as u64;
    assert_eq!(host_manifest.total_download_bytes, expected_total_bytes);

    // ---- HOST: wire the sync host state exactly as production does, minus Tauri ------
    let sync_host = new_shared_sync_host();
    {
        let mut host = sync_host.lock().unwrap();
        host.hosting_instance_id = Some("roundtrip-instance".to_string());
        host.hosting_data_dir = Some(data_dir.clone());
        host.hosting_instance_root = Some(instance_root.clone());
        host.hosting_sync_password = Some(SYNC_PASSWORD.to_string());
        // What `activate_hosting` snapshots from the hosted instance's
        // `runtime_hint`: the runtime this host suggests to its players.
        host.hosting_runtime_hint = Some(RuntimeSource::GithubRelease {
            repo: "owner/name".to_string(),
            release_id: "4242".to_string(),
            tag: "0.8.1".to_string(),
            asset_name: String::new(),
        });
    }
    let manifest_cache = new_shared_hosting_manifest_cache();
    let ctx = ServerContext::new(sync_host.clone(), manifest_cache.clone());

    // Bind an ephemeral localhost port (port 0) and serve the real router.
    let listener = try_bind(0).await.expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    let server = tokio::spawn(async move {
        let _ = serve(listener, ctx).await;
    });

    let host = "127.0.0.1";
    let base = format!("http://{host}:{port}");
    let client = reqwest::Client::new();

    // ---- CLIENT: auth negatives (raw requests expose the status code) ---------------
    // No password header -> 401.
    let unauth = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(
        unauth.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "summary without password must be 401"
    );
    // Wrong password header -> 401.
    let wrong = client
        .get(format!("{base}/"))
        .header(SYNC_PASSWORD_HEADER, "not-the-password")
        .send()
        .await
        .unwrap();
    assert_eq!(
        wrong.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "summary with wrong password must be 401"
    );
    // The real client fetch function surfaces the same failure without a password.
    assert!(
        fetch_manifest_summary(host, port, None).await.is_err(),
        "client fetch without password must fail"
    );

    // ---- CLIENT: authorized fetch via the real sync_client functions ----------------
    let summary = fetch_manifest_summary(host, port, Some(SYNC_PASSWORD))
        .await
        .expect("authorized summary");
    assert_eq!(summary.package_count, 2);
    assert!(summary.password_required);
    assert_eq!(summary.total_download_bytes, expected_total_bytes);
    // The advertised runtime reaches the client through the summary, intact.
    assert_eq!(
        summary.runtime_hint,
        Some(RuntimeSource::GithubRelease {
            repo: "owner/name".to_string(),
            release_id: "4242".to_string(),
            tag: "0.8.1".to_string(),
            asset_name: String::new(),
        }),
        "the host's runtime suggestion must be served in the summary"
    );

    let fetched = fetch_full_manifest(host, port, Some(SYNC_PASSWORD))
        .await
        .expect("authorized full manifest");

    // The fetched manifest matches what the host built.
    assert_eq!(fetched.packages.len(), host_manifest.packages.len());
    assert_eq!(fetched.total_download_bytes, host_manifest.total_download_bytes);
    for host_pkg in &host_manifest.packages {
        let got = fetched
            .packages
            .iter()
            .find(|p| p.id == host_pkg.id)
            .unwrap_or_else(|| panic!("package {} missing from fetched manifest", host_pkg.id));
        assert_eq!(got.file_count, host_pkg.file_count, "file count for {}", host_pkg.name);
        assert_eq!(got.files.len() as u32, got.file_count);
        assert_eq!(got.total_size_bytes, host_pkg.total_size_bytes);
    }

    // ---- CLIENT: download every file and verify checksums ---------------------------
    let mut files_downloaded = 0usize;
    for pkg in &fetched.packages {
        for file in &pkg.files {
            let url = format!("{base}/packages/{}/files/{}", pkg.id, file.path);
            let resp = client
                .get(&url)
                .header(SYNC_PASSWORD_HEADER, SYNC_PASSWORD)
                .send()
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                reqwest::StatusCode::OK,
                "download of {}/{} should succeed",
                pkg.id,
                file.path
            );
            let bytes = resp.bytes().await.unwrap();
            assert_eq!(bytes.len() as u64, file.size, "size for {}", file.path);

            // Write to a client-side temp dir and checksum with the SAME function the
            // manifest was built with.
            let local = client_dir.join(format!("{}-{}", pkg.id, file.path.replace('/', "_")));
            write(&local, &bytes);
            let got = file_checksum(&local).expect("client checksum");
            assert_eq!(got, file.checksum, "checksum mismatch for {}", file.path);
            files_downloaded += 1;
        }
    }
    assert_eq!(files_downloaded, 3, "should have downloaded all 3 files");

    // ---- CLIENT: drive the REAL parallel download engine end-to-end ------------------
    // Second client dir, downloaded through `download_manifest_files` — the same 16-worker
    // pool / checksum-verify / sync-state path the Tauri sync flow calls, now decoupled
    // from `AppHandle` and reported through an `EventSink`. Setup is minimal because the
    // engine provisions its own package dirs and sync-state; we only supply a target data
    // dir, the fetched manifest, a fresh cancel flag, and a collecting sink.
    let engine_dir = root.join("client-engine");
    std::fs::create_dir_all(&engine_dir).unwrap();

    let sink = Arc::new(CollectingEventSink::default());
    let cancel = Arc::new(AtomicBool::new(false));
    let outcome = download_manifest_files(
        sink.clone(),
        "roundtrip-instance",
        host,
        port,
        Some(SYNC_PASSWORD),
        &engine_dir,
        &fetched,
        false,
        cancel.clone(),
    )
    .await
    .expect("engine download should succeed");

    let engine_bytes_done = match outcome {
        DownloadOutcome::Complete { bytes_done } => bytes_done,
        DownloadOutcome::Cancelled { .. } => panic!("engine download must not be cancelled"),
    };
    assert_eq!(
        engine_bytes_done, expected_total_bytes,
        "engine should report every byte downloaded"
    );

    // Every manifest file must have landed at its real on-disk location with the right
    // checksum — verified with the SAME function the manifest was built with.
    let mut engine_files_verified = 0usize;
    for pkg in &fetched.packages {
        for file in &pkg.files {
            let dest = package_abs_path(&engine_dir, &pkg.relative_dir).join(&file.path);
            assert!(
                dest.is_file(),
                "engine should have written {} to disk",
                dest.display()
            );
            let got = file_checksum(&dest).expect("engine-side checksum");
            assert_eq!(got, file.checksum, "engine checksum mismatch for {}", file.path);
            engine_files_verified += 1;
        }
    }
    assert_eq!(engine_files_verified, 3, "engine should have written all 3 files");

    // The sink must have observed progress: every event is a `sync-progress` with a
    // camelCase payload (the frontend contract), and the terminal event the engine
    // guarantees reports all bytes and files done under the Downloading phase.
    let events = sink.events();
    assert!(
        !events.is_empty(),
        "engine must emit at least one sync-progress event"
    );
    for (name, _) in &events {
        assert_eq!(*name, "sync-progress", "engine only emits sync-progress events");
    }

    let (_, last) = events.last().expect("at least one event");
    // camelCase keys are present; snake_case is NOT (proves the ts-rs serde contract).
    for key in [
        "instanceId",
        "phase",
        "message",
        "bytesDone",
        "bytesTotal",
        "filesDone",
        "filesTotal",
        "overallPercent",
    ] {
        assert!(
            last.get(key).is_some(),
            "terminal event payload missing camelCase key {key}: {last}"
        );
    }
    assert!(
        last.get("bytes_done").is_none(),
        "payload must be camelCase, not snake_case: {last}"
    );

    assert_eq!(last["instanceId"], "roundtrip-instance");
    assert_eq!(last["phase"], "downloading");
    assert_eq!(
        last["bytesDone"], expected_total_bytes,
        "terminal event bytesDone should equal total bytes"
    );
    assert_eq!(
        last["bytesTotal"], expected_total_bytes,
        "terminal event bytesTotal should equal total bytes"
    );
    assert_eq!(last["filesDone"], 3, "terminal event filesDone");
    assert_eq!(last["filesTotal"], 3, "terminal event filesTotal");
    assert_eq!(
        last["overallPercent"], 85,
        "Downloading at 100% of bytes maps to overallPercent 85"
    );

    // ---- CLIENT: the host address given as a full URL --------------------------------
    // A host behind an HTTPS reverse proxy is configured as a URL instead of a bare
    // hostname, and the sync port then plays no part in the address. Same in-process
    // server, reached through `http://127.0.0.1:{port}` with a deliberately wrong
    // `syncPort`: everything must still resolve, fetch, and download.
    let url_host = format!("http://127.0.0.1:{port}");
    let bogus_sync_port = 1u16;

    let url_summary = fetch_manifest_summary(&url_host, bogus_sync_port, Some(SYNC_PASSWORD))
        .await
        .expect("URL host must reach the server with the sync port ignored");
    assert_eq!(url_summary.package_count, 2);
    assert_eq!(url_summary.total_download_bytes, expected_total_bytes);

    let url_manifest = fetch_full_manifest(&url_host, bogus_sync_port, Some(SYNC_PASSWORD))
        .await
        .expect("URL host must serve the full manifest");
    assert_eq!(url_manifest.packages.len(), fetched.packages.len());

    let url_dir = root.join("client-url");
    std::fs::create_dir_all(&url_dir).unwrap();
    let url_sink = Arc::new(CollectingEventSink::default());
    let url_outcome = download_manifest_files(
        url_sink,
        "roundtrip-instance",
        &url_host,
        bogus_sync_port,
        Some(SYNC_PASSWORD),
        &url_dir,
        &url_manifest,
        false,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .expect("URL host must drive the download engine");
    match url_outcome {
        DownloadOutcome::Complete { bytes_done } => assert_eq!(
            bytes_done, expected_total_bytes,
            "URL-host download should report every byte"
        ),
        DownloadOutcome::Cancelled { .. } => panic!("URL-host download must not be cancelled"),
    }
    for pkg in &url_manifest.packages {
        for file in &pkg.files {
            let dest = package_abs_path(&url_dir, &pkg.relative_dir).join(&file.path);
            let got = file_checksum(&dest).expect("URL-host checksum");
            assert_eq!(got, file.checksum, "URL-host checksum mismatch for {}", file.path);
        }
    }

    // An address the resolver rejects fails before any request is made.
    assert!(
        fetch_manifest_summary("ftp://127.0.0.1", port, Some(SYNC_PASSWORD))
            .await
            .is_err(),
        "an unsupported scheme must be rejected"
    );

    // ---- CLIENT: negatives ----------------------------------------------------------
    // Path traversal: fully percent-encoded so the URL layer does not collapse `..`
    // before it reaches the handler. Must NOT return file content.
    let traversal_url = format!(
        "{base}/packages/{}/files/%2e%2e%2f%2e%2e%2f%2e%2e%2fetc%2fpasswd",
        host_mod_a.id
    );
    let traversal = client
        .get(&traversal_url)
        .header(SYNC_PASSWORD_HEADER, SYNC_PASSWORD)
        .send()
        .await
        .unwrap();
    assert!(
        traversal.status().is_client_error(),
        "path traversal must be rejected with a 4xx, got {}",
        traversal.status()
    );

    // A file that is not in the manifest -> 404.
    let missing = client
        .get(format!("{base}/packages/{}/files/does-not-exist.esp", host_mod_a.id))
        .header(SYNC_PASSWORD_HEADER, SYNC_PASSWORD)
        .send()
        .await
        .unwrap();
    assert_eq!(
        missing.status(),
        reqwest::StatusCode::NOT_FOUND,
        "file not in manifest must be 404"
    );

    // ---- TEARDOWN -------------------------------------------------------------------
    server.abort();
    let _ = std::fs::remove_dir_all(&root);
}
