//! End-to-end host→client sync roundtrip test, exercised WITHOUT the Tauri layer.
//!
//! This drives the product's core loop — a host builds a manifest from an instance
//! data directory, serves it over the real embedded axum server, and a client fetches
//! the manifest and downloads every file back, verifying checksums — using the same
//! non-Tauri functions the Tauri commands call.
//!
//! Why this lives inside the crate (a `#[cfg(test)] mod`) rather than in `tests/`:
//! the crate is a lib+bin, but every module in `lib.rs` is private (`mod x;`, not
//! `pub mod`). An integration test in `tests/` only sees the public API, which here is
//! effectively empty (`run()`). All of the plumbing — `nerevar_server::{try_bind,
//! serve}`, `ServerContext`, `SyncHostState`, `sync_client::fetch::*`,
//! `instance_data::*` — is crate-private, so it is only reachable from a module
//! compiled as part of the crate.
//!
//! NOT covered: the parallel download engine (`sync_client/download.rs`). It requires a
//! Tauri `AppHandle` to emit progress events, so it cannot run here. Extend this test
//! through `download.rs` once the progress-reporter abstraction lands and decouples it
//! from `AppHandle`. This test instead downloads each file with a plain `reqwest` GET
//! against the same package-file route the download engine uses.

use std::path::Path;

use crate::instance_data::{
    build_manifest, file_checksum, load_load_order, manifest_path, scan_and_merge_load_order,
};
use crate::nerevar_server::state::ServerContext;
use crate::nerevar_server::{serve, try_bind};
use crate::sync_auth::SYNC_PASSWORD_HEADER;
use crate::sync_client::{fetch_full_manifest, fetch_manifest_summary};
use crate::sync_host::{new_shared_hosting_manifest_cache, new_shared_sync_host};

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
