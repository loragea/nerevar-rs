//! End-to-end tests of `POST /admin/restart` against the real axum server.
//!
//! Same in-process pattern as `admin_api.rs` and `admin_staging_api.rs`: boot
//! the router `nerevar-host` serves on an ephemeral port over a temporary
//! instance. The difference is that this one needs a *game server* to restart,
//! so the fixture ships a fake `tes3mp-server` — a shell script that sleeps —
//! in the place the real executable would be, and lets the ordinary launch
//! path find and spawn it. That makes the restart genuine: the same stop, the
//! same relaunch, the same process manager the daemon uses.
//!
//! The fake child is a shell script, so the full restart is `cfg(unix)`. The
//! two refusal paths — no game server to restart, and a role without the
//! `restart` capability — need no child and run everywhere.

use std::sync::Arc;

use nerevar_core::admin::{
    add_admin, save_admins, token_hash, AdminRecord, AdminStore, ADMIN_STORE_VERSION, ROLE_ADMIN,
    ROLE_TEST_STATUS_ONLY,
};
use nerevar_core::instance_data::{build_manifest, load_load_order, scan_and_merge_load_order};
use nerevar_core::nerevar_server::state::ServerContext;
use nerevar_core::nerevar_server::{serve, try_bind};
use nerevar_core::reporter::{CollectingEventSink, EventSink};
use nerevar_core::sync_host::{new_shared_hosting_manifest_cache, new_shared_sync_host};

const SYNC_PASSWORD: &str = "swordfish";
const INSTANCE_ID: &str = "restart-instance";

fn write(path: &std::path::Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// A hosted instance on disk: one package, a TES3MP tree, a built manifest.
/// Returns `(instance_root, data_dir)`.
fn fixture(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "nerevar-admin-restart-{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let instance_root = root.join("instance");
    let data_dir = instance_root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    write(&data_dir.join("ModA/plugin.esp"), b"ModA plugin payload");

    let tes3mp_dir = instance_root.join("tes3mp");
    write(
        &tes3mp_dir.join("tes3mp-server-default.cfg"),
        b"[General]\nhostname = Restart Test\nport = 25565\npassword = swordfish\n",
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
        INSTANCE_ID,
        "Restart Instance",
        &instance_root,
        &data_dir,
        &load_order,
        &mut no_progress,
    )
    .expect("build manifest");

    (instance_root, data_dir)
}

/// Points a fresh sync host at the fixture, the way `activate_hosting` leaves
/// it.
fn hosting(
    instance_root: &std::path::Path,
    data_dir: &std::path::Path,
) -> nerevar_core::sync_host::SharedSyncHost {
    let sync_host = new_shared_sync_host();
    {
        let mut host = sync_host.lock().unwrap();
        host.hosting_instance_id = Some(INSTANCE_ID.to_string());
        host.hosting_data_dir = Some(data_dir.to_path_buf());
        host.hosting_instance_root = Some(instance_root.to_path_buf());
        host.hosting_sync_password = Some(SYNC_PASSWORD.to_string());
    }
    sync_host
}

async fn get_status(client: &reqwest::Client, base: &str, token: &str) -> serde_json::Value {
    client
        .get(format!("{base}/admin/status"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// Adds a second admin whose role grants only `status`, so the capability
/// guard's 403 branch has something to reject.
fn add_narrow_admin(data_dir: &std::path::Path) -> String {
    let token = "b".repeat(64);
    let mut store = nerevar_core::admin::load_admins(data_dir).expect("store");
    store.admins.push(AdminRecord {
        name: "vic".to_string(),
        role: ROLE_TEST_STATUS_ONLY.to_string(),
        token_sha256: token_hash(&token),
        created_at: "2026-01-01T00:00:00+00:00".to_string(),
    });
    save_admins(
        data_dir,
        &AdminStore {
            version: ADMIN_STORE_VERSION,
            admins: store.admins,
        },
    )
    .expect("save admins");
    token
}

/// An embedder with no process manager at all — the desktop app's server —
/// has nothing to restart, and says so with a `409` rather than a `500`.
/// A role without the `restart` capability never reaches that question.
#[tokio::test]
async fn restart_without_a_game_server_is_a_conflict() {
    let (instance_root, data_dir) = fixture("no-manager");
    let ctx = ServerContext::builder(
        hosting(&instance_root, &data_dir),
        new_shared_hosting_manifest_cache(),
    )
    .shared();

    let listener = try_bind(0).await.expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    let server = tokio::spawn(async move {
        let _ = serve(listener, ctx).await;
    });

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    let token = add_admin(&data_dir, "ada", ROLE_ADMIN)
        .expect("add admin")
        .token;

    let response = client
        .post(format!("{base}/admin/restart"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("restart")),
        "the 409 must explain itself, got {body}"
    );

    // ---- A role without `restart` is refused by the guard, before any of that ---
    let narrow = add_narrow_admin(&data_dir);
    let response = client
        .post(format!("{base}/admin/restart"))
        .bearer_auth(&narrow)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::FORBIDDEN,
        "a role granting only `status` must not restart the game server"
    );
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("restart")),
        "the 403 must name the capability, got {body}"
    );
    // That role can still read status, so this really is the guard and not a
    // broken token.
    let status = get_status(&client, &base, &narrow).await;
    assert_eq!(status["hosting"], serde_json::json!(true));
    assert_eq!(status["tes3mpServerRunning"], serde_json::Value::Null);
    assert_eq!(status["tes3mpServerStartedAt"], serde_json::Value::Null);

    server.abort();
    let _ = std::fs::remove_dir_all(instance_root.parent().unwrap());
}

/// A `--sync-only` daemon has a process manager but never launched a game
/// server. It must refuse rather than start one nobody asked for.
#[tokio::test]
async fn restart_on_a_sync_only_host_is_a_conflict() {
    use nerevar_core::process_manager::ProcessManager;

    let (instance_root, data_dir) = fixture("sync-only");
    let manager = Arc::new(ProcessManager::new());
    let ctx = ServerContext::builder(
        hosting(&instance_root, &data_dir),
        new_shared_hosting_manifest_cache(),
    )
    .with_process_manager(manager.clone())
    .shared();
    assert!(
        !ctx.server_restart.is_supervising(),
        "nothing has been launched, so nothing is supervised"
    );

    let listener = try_bind(0).await.expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    let server_ctx = ctx.clone();
    let server = tokio::spawn(async move {
        let _ = serve(listener, server_ctx).await;
    });

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    let token = add_admin(&data_dir, "ada", ROLE_ADMIN)
        .expect("add admin")
        .token;

    let response = client
        .post(format!("{base}/admin/restart"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("sync-only")),
        "the 409 must point at --sync-only, got {body}"
    );
    assert!(
        !manager
            .is_running(
                INSTANCE_ID,
                nerevar_core::process_manager::ProcessRole::Server
            )
            .unwrap(),
        "a refused restart must not have launched anything"
    );
    assert_eq!(ctx.server_restart.completed_restarts(), 0);

    server.abort();
    let _ = std::fs::remove_dir_all(instance_root.parent().unwrap());
}

/// The whole route, against a real child: stop the running fake server, start
/// a new one, clear the stale-plugin-list flag, and report the new pid.
#[cfg(unix)]
#[tokio::test]
async fn restart_replaces_the_running_game_server_and_clears_the_stale_flag() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use nerevar_core::process_manager::{launch_tes3mp_server, ProcessManager, ProcessRole};

    let (instance_root, data_dir) = fixture("replaces");

    // The fake TES3MP dedicated server, where the real one would be: a script
    // that stays up until it is killed. `launch_tes3mp_server` finds it by
    // name, exactly as it finds the real executable.
    let fake_server = instance_root.join("tes3mp/tes3mp-server");
    write(&fake_server, b"#!/bin/sh\nexec sleep 600\n");
    std::fs::set_permissions(
        &fake_server,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .unwrap();

    let audit = Arc::new(CollectingEventSink::default());
    let manager = Arc::new(ProcessManager::new());
    let ctx = ServerContext::builder(
        hosting(&instance_root, &data_dir),
        new_shared_hosting_manifest_cache(),
    )
    .with_sink(audit.clone() as Arc<dyn EventSink>)
    .with_process_manager(manager.clone())
    .shared();

    // Boot the game server the way the daemon does at startup, then say so.
    launch_tes3mp_server(
        audit.clone() as Arc<dyn EventSink>,
        manager.clone(),
        INSTANCE_ID,
        &instance_root,
        &data_dir,
    )
    .expect("launch the fake TES3MP server");
    ctx.server_restart.mark_supervising();

    let original_pid = manager
        .pid(INSTANCE_ID, ProcessRole::Server)
        .unwrap()
        .expect("a pid for the launched child");
    assert!(manager
        .is_running(INSTANCE_ID, ProcessRole::Server)
        .unwrap());

    // An apply just landed and the running server is behind it.
    ctx.tes3mp_plugin_list_stale.store(true, Ordering::Relaxed);

    let listener = try_bind(0).await.expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    let server_ctx = ctx.clone();
    let server = tokio::spawn(async move {
        let _ = serve(listener, server_ctx).await;
    });

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    let token = add_admin(&data_dir, "ada", ROLE_ADMIN)
        .expect("add admin")
        .token;

    let status = get_status(&client, &base, &token).await;
    assert_eq!(status["tes3mpServerRunning"], serde_json::json!(true));
    assert_eq!(status["tes3mpPluginListStale"], serde_json::json!(true));
    let started_before = status["tes3mpServerStartedAt"]
        .as_str()
        .expect("a start time for a running server")
        .to_string();

    // ---- A restart while another admin write holds the lock is a 409 -----------
    let held = ctx.admin_write_lock.lock().await;
    let busy = client
        .post(format!("{base}/admin/restart"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(busy.status(), reqwest::StatusCode::CONFLICT);
    drop(held);

    // ---- The restart itself ------------------------------------------------------
    let response = client
        .post(format!("{base}/admin/restart"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let response_status = response.status();
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(response_status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["restarted"], serde_json::json!(true));
    assert_eq!(body["wasRunning"], serde_json::json!(true));
    let new_pid = body["pid"].as_u64().expect("a new pid") as u32;
    assert_ne!(
        new_pid, original_pid,
        "the restart must have replaced the child, not reported the old one"
    );
    assert!(body["startedAt"].as_str().is_some());

    // The manager tracks the new child, and the old one is gone.
    assert_eq!(
        manager.pid(INSTANCE_ID, ProcessRole::Server).unwrap(),
        Some(new_pid)
    );
    assert!(manager
        .is_running(INSTANCE_ID, ProcessRole::Server)
        .unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::path::Path::new(&format!("/proc/{original_pid}")).exists() {
        assert!(
            Instant::now() < deadline,
            "the old fake server ({original_pid}) is still alive after the restart"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // ---- Status reflects it ------------------------------------------------------
    let status = get_status(&client, &base, &token).await;
    assert_eq!(status["tes3mpServerRunning"], serde_json::json!(true));
    assert_eq!(
        status["tes3mpPluginListStale"],
        serde_json::json!(false),
        "the new process read the applied plugin list, so nothing is stale"
    );
    let started_after = status["tes3mpServerStartedAt"]
        .as_str()
        .expect("a start time for the new server");
    assert!(
        started_after >= started_before.as_str(),
        "the start time must move forward across a restart: {started_before} -> {started_after}"
    );

    // ---- The restart is in the audit trail --------------------------------------
    let events = audit.events();
    let (_, restart_event) = events
        .iter()
        .find(|(name, _)| *name == nerevar_core::admin::ADMIN_RESTART_EVENT)
        .expect("an admin-restart event");
    assert_eq!(restart_event["admin"], serde_json::json!("ada"));
    assert_eq!(restart_event["pid"], serde_json::json!(new_pid));
    assert_eq!(restart_event["wasRunning"], serde_json::json!(true));

    // ---- The window closed, so a later death is a death again -------------------
    assert!(!ctx.server_restart.restart_in_flight());
    assert_eq!(ctx.server_restart.completed_restarts(), 1);
    assert!(ctx
        .server_restart
        .is_unrequested_death(ctx.server_restart.mark()));

    manager
        .stop_blocking(None, INSTANCE_ID, ProcessRole::Server)
        .expect("stop the fake server");
    server.abort();
    let _ = std::fs::remove_dir_all(instance_root.parent().unwrap());
}
