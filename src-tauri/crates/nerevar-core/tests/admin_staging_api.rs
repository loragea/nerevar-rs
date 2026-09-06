//! End-to-end test of the `/admin` write routes against the real axum server.
//!
//! Same in-process pattern as `admin_api.rs` (which covers milestone 1's auth
//! story): boot the router `nerevar-host` serves on an ephemeral port over a
//! temporary instance. This one walks the whole stage-then-apply path a
//! co-admin actually takes — upload an archive, see it pending, set a load
//! order that enables it, apply — and then checks the thing that matters to
//! everyone else: that a *player* fetching `/manifest` with the sync password
//! gets the new package, with checksums that match the bytes now in `data/`.

use std::sync::Arc;

use nerevar_core::admin::{
    add_admin, save_admins, staging_dir, token_hash, AdminRecord, AdminStore, ADMIN_STORE_VERSION,
    ROLE_ADMIN, ROLE_TEST_STATUS_ONLY,
};
use nerevar_core::instance_data::{
    build_manifest, file_checksum, load_load_order, scan_and_merge_load_order,
};
use nerevar_core::nerevar_server::state::ServerContext;
use nerevar_core::nerevar_server::{serve, try_bind};
use nerevar_core::reporter::{CollectingEventSink, EventSink};
use nerevar_core::sync_auth::SYNC_PASSWORD_HEADER;
use nerevar_core::sync_host::{new_shared_hosting_manifest_cache, new_shared_sync_host};

const SYNC_PASSWORD: &str = "swordfish";
const NEW_PACKAGE: &str = "New Mod";
const NEW_PLUGIN: &str = "new.esp";
// Deliberately not a real TES3 header: the fixture only needs to be a file
// with a plugin extension, and a truncated real header makes the header
// reader fail rather than skip.
const NEW_PLUGIN_BYTES: &[u8] = b"New Mod plugin payload";

fn write(path: &std::path::Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// A zip built in the test, the way the runtime tests build their fixtures:
/// one plugin and one asset, wrapped in a top-level directory so the upload
/// path's strip rule is exercised end to end rather than only in its unit
/// tests.
fn wrapped_zip() -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let mut buffer = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
        for (name, bytes) in [
            (format!("{NEW_PACKAGE}/{NEW_PLUGIN}"), NEW_PLUGIN_BYTES),
            (
                format!("{NEW_PACKAGE}/textures/new.dds"),
                b"new texture bytes".as_slice(),
            ),
        ] {
            writer
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }
    buffer
}

#[tokio::test]
async fn a_co_admin_stages_a_package_and_applies_it() {
    // ---- FIXTURE: a hosted instance with one package already in data/ ------------
    let root = std::env::temp_dir().join(format!(
        "nerevar-admin-staging-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let instance_root = root.join("instance");
    let data_dir = instance_root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    write(&data_dir.join("ModA/plugin.esp"), b"ModA plugin payload");
    write(&data_dir.join("ModA/textures/a.dds"), b"ModA texture bytes");

    let tes3mp_dir = instance_root.join("tes3mp");
    write(
        &tes3mp_dir.join("tes3mp-server-default.cfg"),
        b"[General]\nhostname = Staging Test\nport = 25565\npassword = swordfish\n",
    );
    std::fs::create_dir_all(tes3mp_dir.join("server/data")).unwrap();
    write(
        &tes3mp_dir.join("server/scripts/config.lua"),
        b"config = {}\n",
    );

    let mut no_progress = None;
    scan_and_merge_load_order(&data_dir, &mut no_progress).expect("scan");
    let load_order = load_load_order(&data_dir).expect("load order");
    build_manifest(
        "staging-instance",
        "Staging Instance",
        &instance_root,
        &data_dir,
        &load_order,
        &mut no_progress,
    )
    .expect("build manifest");

    // ---- HOST -------------------------------------------------------------------
    let sync_host = new_shared_sync_host();
    {
        let mut host = sync_host.lock().unwrap();
        host.hosting_instance_id = Some("staging-instance".to_string());
        host.hosting_data_dir = Some(data_dir.clone());
        host.hosting_instance_root = Some(instance_root.clone());
        host.hosting_sync_password = Some(SYNC_PASSWORD.to_string());
    }
    let audit = Arc::new(CollectingEventSink::default());
    let ctx = ServerContext::builder(sync_host.clone(), new_shared_hosting_manifest_cache())
        .with_sink(audit.clone() as Arc<dyn EventSink>)
        .shared();

    let listener = try_bind(0).await.expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    let server_ctx = ctx.clone();
    let server = tokio::spawn(async move {
        let _ = serve(listener, server_ctx).await;
    });

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    let admin = add_admin(&data_dir, "ada", ROLE_ADMIN).expect("add admin");
    let token = admin.token.clone();

    // ---- UPLOAD -----------------------------------------------------------------
    let archive = wrapped_zip();
    let archive_len = archive.len() as u64;
    let uploaded = client
        .put(format!("{base}/admin/packages/{NEW_PACKAGE}"))
        .bearer_auth(&token)
        .body(archive)
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), reqwest::StatusCode::OK);
    let staged: serde_json::Value = uploaded.json().await.unwrap();
    assert_eq!(staged["name"], serde_json::json!(NEW_PACKAGE));
    assert_eq!(staged["kind"], serde_json::json!("mod"));
    assert_eq!(staged["plugins"], serde_json::json!([NEW_PLUGIN]));
    assert_eq!(staged["replacesExisting"], serde_json::json!(false));
    assert_eq!(staged["archiveBytes"], serde_json::json!(archive_len));
    assert_eq!(staged["stagedBy"], serde_json::json!("ada"));

    // The wrapper directory came off, and none of this reached data/ yet.
    let staged_dir = staging_dir(&data_dir).join(NEW_PACKAGE);
    assert!(staged_dir.join(NEW_PLUGIN).is_file());
    assert!(!data_dir.join(NEW_PACKAGE).exists());

    // ---- STATUS sees it as pending ----------------------------------------------
    let status: serde_json::Value = get_status(&client, &base, &token).await;
    let pending = &status["pendingChanges"];
    assert_eq!(pending["staged"][0]["name"], serde_json::json!(NEW_PACKAGE));
    assert_eq!(
        pending["staged"][0]["replacesExisting"],
        serde_json::json!(false)
    );
    assert_eq!(pending["removals"], serde_json::json!([]));
    assert_eq!(pending["hasLoadOrder"], serde_json::json!(false));
    assert_eq!(status["tes3mpPluginListStale"], serde_json::json!(false));
    // The load order on disk still knows only the package that was there.
    assert_eq!(status["loadOrder"]["entryCount"], serde_json::json!(1));

    // ---- A LOAD ORDER that enables the staged package ---------------------------
    let view: serde_json::Value = client
        .get(format!("{base}/admin/load-order"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(view["pending"], serde_json::Value::Null);
    let mut proposed = view["current"].clone();
    proposed["entries"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "new-mod-entry",
            "name": NEW_PACKAGE,
            "kind": "mod",
            "relativeDir": NEW_PACKAGE,
            "enabled": true,
            "priority": 20,
            "plugins": [{ "file": NEW_PLUGIN, "enabled": true }],
        }));

    // A package that will exist nowhere after apply is refused.
    let mut bogus = proposed.clone();
    bogus["entries"].as_array_mut().unwrap()[1]["relativeDir"] =
        serde_json::json!("Package That Does Not Exist");
    let refused = post_load_order(&client, &base, &token, &bogus).await;
    assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST);

    let accepted = post_load_order(&client, &base, &token, &proposed).await;
    assert_eq!(accepted.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = accepted.json().await.unwrap();
    assert_eq!(
        body["pendingChanges"]["hasLoadOrder"],
        serde_json::json!(true)
    );
    // Still staged only: load-order.json is untouched until apply.
    let status: serde_json::Value = get_status(&client, &base, &token).await;
    assert_eq!(status["loadOrder"]["entryCount"], serde_json::json!(1));

    // ---- APPLY ------------------------------------------------------------------
    // A second apply while one is in flight is a 409, not a queue. Holding the
    // server's own write lock is the deterministic way to be "in flight": a
    // real second request takes exactly this branch.
    let held = ctx.admin_write_lock.lock().await;
    let busy = client
        .post(format!("{base}/admin/apply"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(busy.status(), reqwest::StatusCode::CONFLICT);
    drop(held);

    let applied = client
        .post(format!("{base}/admin/apply"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let applied_status = applied.status();
    let summary: serde_json::Value = applied.json().await.unwrap();
    // The body is in the message: an apply that fails says why, and that is
    // the one thing worth seeing when this test breaks.
    assert_eq!(applied_status, reqwest::StatusCode::OK, "{summary}");
    assert_eq!(summary["packageCount"], serde_json::json!(2));
    assert_eq!(summary["passwordRequired"], serde_json::json!(true));

    // ---- The filesystem is what the plan said ------------------------------------
    assert!(data_dir.join(NEW_PACKAGE).join(NEW_PLUGIN).is_file());
    assert!(
        !staging_dir(&data_dir).exists(),
        "apply clears the staging area"
    );
    let status: serde_json::Value = get_status(&client, &base, &token).await;
    assert_eq!(status["pendingChanges"], serde_json::Value::Null);
    assert_eq!(status["loadOrder"]["entryCount"], serde_json::json!(2));
    assert_eq!(
        status["loadOrder"]["enabledPluginCount"],
        serde_json::json!(2)
    );
    // No process manager in this context, so the daemon-only fact is unknown
    // and apply must assume a running server is now behind the manifest.
    assert_eq!(status["tes3mpPluginListStale"], serde_json::json!(true));

    // ---- A PLAYER gets the new package, with checksums that match disk -----------
    let manifest: serde_json::Value = client
        .get(format!("{base}/manifest"))
        .header(SYNC_PASSWORD_HEADER, SYNC_PASSWORD)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let packages = manifest["packages"].as_array().expect("packages");
    assert_eq!(packages.len(), 2);
    let new_package = packages
        .iter()
        .find(|package| package["name"] == serde_json::json!(NEW_PACKAGE))
        .expect("the applied package must be in the served manifest");
    assert_eq!(new_package["kind"], serde_json::json!("mod"));
    assert_eq!(
        new_package["plugins"],
        serde_json::json!([{ "file": NEW_PLUGIN, "enabled": true }])
    );

    let plugin_entry = new_package["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|file| file["path"] == serde_json::json!(NEW_PLUGIN))
        .expect("the plugin must be listed as a file");
    assert_eq!(
        plugin_entry["checksum"],
        serde_json::json!(file_checksum(&data_dir.join(NEW_PACKAGE).join(NEW_PLUGIN)).unwrap()),
        "the served checksum must be of the bytes now on disk"
    );
    assert_eq!(
        plugin_entry["size"],
        serde_json::json!(NEW_PLUGIN_BYTES.len() as u64)
    );

    // The apply left an audit trail naming what it did.
    let events = audit.events();
    let (_, apply_event) = events
        .iter()
        .find(|(name, _)| *name == nerevar_core::admin::ADMIN_APPLY_EVENT)
        .expect("an admin-apply event");
    assert_eq!(apply_event["admin"], serde_json::json!("ada"));
    assert_eq!(apply_event["installed"], serde_json::json!([NEW_PACKAGE]));
    assert_eq!(apply_event["wroteLoadOrder"], serde_json::json!(true));

    // ---- Applying with nothing pending is a 200 that rebuilds -------------------
    let regenerated = client
        .post(format!("{base}/admin/apply"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(regenerated.status(), reqwest::StatusCode::OK);
    let summary: serde_json::Value = regenerated.json().await.unwrap();
    assert_eq!(summary["packageCount"], serde_json::json!(2));

    // ---- Delete marks an installed package, discard throws the mark away --------
    let marked = client
        .delete(format!("{base}/admin/packages/ModA"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(marked.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = marked.json().await.unwrap();
    assert_eq!(
        body["pendingChanges"]["removals"],
        serde_json::json!(["ModA"])
    );
    assert!(
        data_dir.join("ModA").is_dir(),
        "a mark is not a delete; apply is"
    );

    let missing = client
        .delete(format!("{base}/admin/packages/Never Existed"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let discarded = client
        .post(format!("{base}/admin/discard"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(discarded.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = discarded.json().await.unwrap();
    assert_eq!(body["pendingChanges"], serde_json::Value::Null);
    assert!(data_dir.join("ModA").is_dir());

    // ---- A name that is not one plain path segment is a 400 ---------------------
    for bad in [".hidden", "tes3mp", "data"] {
        let response = client
            .put(format!("{base}/admin/packages/{bad}"))
            .bearer_auth(&token)
            .body(wrapped_zip())
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{bad} should be refused"
        );
        assert!(!data_dir.join(bad).exists() || bad == "tes3mp");
    }

    // A body that is not an archive is the caller's mistake, and leaves
    // nothing staged.
    let junk = client
        .put(format!("{base}/admin/packages/Junk"))
        .bearer_auth(&token)
        .body(b"not an archive".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(junk.status(), reqwest::StatusCode::BAD_REQUEST);
    let status: serde_json::Value = get_status(&client, &base, &token).await;
    assert_eq!(status["pendingChanges"], serde_json::Value::Null);

    // ---- A role without `stage` is refused by the guard, not by the handler -----
    let narrow_token = "b".repeat(64);
    let mut store = nerevar_core::admin::load_admins(&data_dir).expect("store");
    store.admins.push(AdminRecord {
        name: "vic".to_string(),
        role: ROLE_TEST_STATUS_ONLY.to_string(),
        token_sha256: token_hash(&narrow_token),
        created_at: "2026-01-01T00:00:00+00:00".to_string(),
    });
    save_admins(
        &data_dir,
        &AdminStore {
            version: ADMIN_STORE_VERSION,
            admins: store.admins,
        },
    )
    .expect("save store");

    // It can read.
    let readable = client
        .get(format!("{base}/admin/status"))
        .bearer_auth(&narrow_token)
        .send()
        .await
        .unwrap();
    assert_eq!(readable.status(), reqwest::StatusCode::OK);

    // It cannot stage, in any of the ways staging is spelled.
    let upload = client
        .put(format!("{base}/admin/packages/Anything"))
        .bearer_auth(&narrow_token)
        .body(wrapped_zip())
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), reqwest::StatusCode::FORBIDDEN);

    let remove = client
        .delete(format!("{base}/admin/packages/ModA"))
        .bearer_auth(&narrow_token)
        .send()
        .await
        .unwrap();
    assert_eq!(remove.status(), reqwest::StatusCode::FORBIDDEN);

    let order = post_load_order(&client, &base, &narrow_token, &proposed).await;
    assert_eq!(order.status(), reqwest::StatusCode::FORBIDDEN);

    let discard = client
        .post(format!("{base}/admin/discard"))
        .bearer_auth(&narrow_token)
        .send()
        .await
        .unwrap();
    assert_eq!(discard.status(), reqwest::StatusCode::FORBIDDEN);

    // Nor apply, which is its own capability.
    let apply = client
        .post(format!("{base}/admin/apply"))
        .bearer_auth(&narrow_token)
        .send()
        .await
        .unwrap();
    assert_eq!(apply.status(), reqwest::StatusCode::FORBIDDEN);

    // A 403 is still audited, with the name of whoever was refused.
    let events = audit.events();
    let forbidden = events
        .iter()
        .filter(|(name, _)| *name == nerevar_core::admin::ADMIN_REQUEST_EVENT)
        .filter(|(_, payload)| payload["status"] == serde_json::json!(403))
        .count();
    assert_eq!(forbidden, 5, "every refused route is audited");

    server.abort();
    let _ = std::fs::remove_dir_all(&root);
}

async fn get_status(client: &reqwest::Client, base: &str, token: &str) -> serde_json::Value {
    let response = client
        .get(format!("{base}/admin/status"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json().await.unwrap()
}

async fn post_load_order(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    body: &serde_json::Value,
) -> reqwest::Response {
    client
        .post(format!("{base}/admin/load-order"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap()
}
