//! End-to-end test of the `/admin` route group against the real axum server.
//!
//! Boots the same router `nerevar-host` serves, in-process on an ephemeral
//! port, with hosting activated over a temporary instance — the pattern from
//! `sync_roundtrip.rs`. It covers the whole milestone-1 auth story from the
//! wire side: no header, a wrong token, a right token, a token that was
//! revoked while the server was running, and a role the capability table does
//! not know. It also re-checks the player-facing sync routes through the same
//! server, because `/admin` is a new layer on a router that must keep working
//! for clients that have never heard of admins.

use std::sync::Arc;

use nerevar_core::admin::{
    add_admin, load_admins, revoke_admin, save_admins, token_hash, AdminRecord, AdminStore,
    ADMIN_STORE_VERSION, ROLE_ADMIN,
};
use nerevar_core::instance_data::{build_manifest, load_load_order, scan_and_merge_load_order};
use nerevar_core::nerevar_server::state::ServerContext;
use nerevar_core::nerevar_server::{serve, try_bind};
use nerevar_core::reporter::{CollectingEventSink, EventSink};
use nerevar_core::sync_auth::SYNC_PASSWORD_HEADER;
use nerevar_core::sync_host::{new_shared_hosting_manifest_cache, new_shared_sync_host};

const SYNC_PASSWORD: &str = "swordfish";

fn write(path: &std::path::Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

#[tokio::test]
async fn admin_routes_authenticate_by_bearer_token() {
    // ---- FIXTURE: a hosted instance with one package and one plugin --------------
    let root = std::env::temp_dir().join(format!("nerevar-admin-api-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let instance_root = root.join("instance");
    let data_dir = instance_root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    write(&data_dir.join("ModA/plugin.esp"), b"ModA plugin payload");
    write(&data_dir.join("ModA/textures/a.dds"), b"ModA texture bytes");

    let tes3mp_dir = instance_root.join("tes3mp");
    write(
        &tes3mp_dir.join("tes3mp-server-default.cfg"),
        b"[General]\nhostname = Admin Test\nport = 25565\npassword = \n",
    );
    std::fs::create_dir_all(tes3mp_dir.join("server/data")).unwrap();
    write(
        &tes3mp_dir.join("server/scripts/config.lua"),
        b"config = {}\n",
    );

    let mut no_progress = None;
    scan_and_merge_load_order(&data_dir, &mut no_progress).expect("scan");
    let load_order = load_load_order(&data_dir).expect("load order");
    let manifest = build_manifest(
        "admin-instance",
        "Admin Instance",
        &instance_root,
        &data_dir,
        &load_order,
        &mut no_progress,
    )
    .expect("build manifest");
    assert_eq!(manifest.packages.len(), 1);

    // ---- HOST: the real server, with an audit sink the test can read -------------
    let sync_host = new_shared_sync_host();
    {
        let mut host = sync_host.lock().unwrap();
        host.hosting_instance_id = Some("admin-instance".to_string());
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
    let server = tokio::spawn(async move {
        let _ = serve(listener, ctx).await;
    });

    let base = format!("http://127.0.0.1:{port}");
    let status_url = format!("{base}/admin/status");
    let client = reqwest::Client::new();

    // ---- No admins exist yet: any token is wrong --------------------------------
    let response = client
        .get(&status_url)
        .bearer_auth("0".repeat(64))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a token on a host with no admins must be rejected"
    );

    // ---- Create one, the way `nerevar-host admin add` does ----------------------
    let created = add_admin(&data_dir, "ada", ROLE_ADMIN).expect("add admin");

    // No Authorization header at all -> 401 with a JSON error body.
    let response = client.get(&status_url).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = response.json().await.expect("a JSON error body");
    assert!(
        body["error"].as_str().is_some_and(|e| !e.is_empty()),
        "401 body must carry an error message, got {body}"
    );

    // A well-formed but wrong token -> 401.
    let wrong = client
        .get(&status_url)
        .bearer_auth("f".repeat(64))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A non-bearer scheme carrying the right token -> 401.
    let basic = client
        .get(&status_url)
        .header("Authorization", format!("Basic {}", created.token))
        .send()
        .await
        .unwrap();
    assert_eq!(basic.status(), reqwest::StatusCode::UNAUTHORIZED);

    // ---- The real token -> 200 with the documented shape ------------------------
    let ok = client
        .get(&status_url)
        .bearer_auth(&created.token)
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), reqwest::StatusCode::OK);
    let status: serde_json::Value = ok.json().await.expect("status body");

    assert_eq!(status["hosting"], serde_json::json!(true));
    assert_eq!(status["instanceId"], serde_json::json!("admin-instance"));
    assert_eq!(status["instanceName"], serde_json::json!("Admin Instance"));
    assert_eq!(status["loadOrder"]["entryCount"], serde_json::json!(1));
    assert_eq!(
        status["loadOrder"]["enabledPluginCount"],
        serde_json::json!(1)
    );
    assert_eq!(
        status["manifest"]["generatedAt"],
        serde_json::json!(manifest.generated_at)
    );
    assert!(status["manifest"]["ageSeconds"]
        .as_i64()
        .is_some_and(|a| a >= 0));
    // No process manager in this context, so the daemon-only fact is unknown.
    assert_eq!(status["tes3mpServerRunning"], serde_json::Value::Null);
    // Nothing has been staged on this host, so the pending set is a present
    // null rather than an empty object (`admin_staging_api.rs` covers the
    // populated shape).
    assert!(status.as_object().unwrap().contains_key("pendingChanges"));
    assert_eq!(status["pendingChanges"], serde_json::Value::Null);
    // Nothing has been applied either, so no running server is behind.
    assert_eq!(status["tes3mpPluginListStale"], serde_json::json!(false));

    // ---- The audit line reached the sink, without the token ---------------------
    let events = audit.events();
    let admin_events: Vec<_> = events
        .iter()
        .filter(|(name, _)| *name == nerevar_core::admin::ADMIN_REQUEST_EVENT)
        .collect();
    assert_eq!(
        admin_events.len(),
        1,
        "only the authenticated request is audited, got {events:?}"
    );
    let (_, payload) = admin_events[0];
    assert_eq!(payload["admin"], serde_json::json!("ada"));
    assert_eq!(payload["role"], serde_json::json!(ROLE_ADMIN));
    assert_eq!(payload["method"], serde_json::json!("GET"));
    assert_eq!(payload["path"], serde_json::json!("/admin/status"));
    assert_eq!(payload["status"], serde_json::json!(200));
    let rendered = serde_json::to_string(payload).unwrap();
    assert!(
        !rendered.contains(&created.token),
        "the audit line must never carry the token"
    );

    // ---- Revoking takes effect against the running server -----------------------
    assert!(revoke_admin(&data_dir, "ada").expect("revoke"));
    let revoked = client
        .get(&status_url)
        .bearer_auth(&created.token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        revoked.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a revoked token must stop working without restarting the server"
    );

    // ---- A role the capability table does not know authenticates nothing --------
    let stranded = "a".repeat(64);
    save_admins(
        &data_dir,
        &AdminStore {
            version: ADMIN_STORE_VERSION,
            admins: vec![AdminRecord {
                name: "contrib".to_string(),
                role: "contributor".to_string(),
                token_sha256: token_hash(&stranded),
                created_at: "2026-01-01T00:00:00+00:00".to_string(),
            }],
        },
    )
    .expect("save a hand-edited store");
    assert_eq!(load_admins(&data_dir).unwrap().admins.len(), 1);
    let unknown_role = client
        .get(&status_url)
        .bearer_auth(&stranded)
        .send()
        .await
        .unwrap();
    assert_eq!(
        unknown_role.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "an unknown role must fail closed"
    );

    // ---- The sync routes are untouched by any of this ---------------------------
    let no_password = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(no_password.status(), reqwest::StatusCode::UNAUTHORIZED);

    let with_password = client
        .get(format!("{base}/"))
        .header(SYNC_PASSWORD_HEADER, SYNC_PASSWORD)
        .send()
        .await
        .unwrap();
    assert_eq!(
        with_password.status(),
        reqwest::StatusCode::OK,
        "the sync password must still work"
    );
    let summary: serde_json::Value = with_password.json().await.unwrap();
    assert_eq!(summary["packageCount"], serde_json::json!(1));

    // A sync password is not an admin credential, and an admin token is not a
    // sync password: the two secrets stay separate.
    let sync_as_admin = client
        .get(&status_url)
        .header(SYNC_PASSWORD_HEADER, SYNC_PASSWORD)
        .send()
        .await
        .unwrap();
    assert_eq!(sync_as_admin.status(), reqwest::StatusCode::UNAUTHORIZED);

    let admin_as_sync = client
        .get(format!("{base}/"))
        .header(SYNC_PASSWORD_HEADER, &stranded)
        .send()
        .await
        .unwrap();
    assert_eq!(admin_as_sync.status(), reqwest::StatusCode::UNAUTHORIZED);

    // `/health` needs neither.
    let health = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(health.status(), reqwest::StatusCode::OK);

    server.abort();
    let _ = std::fs::remove_dir_all(&root);
}
