//! End-to-end test of `nerevar-cli admin` against the real host.
//!
//! Boots the same axum router `nerevar-host` serves, in-process on an
//! ephemeral port over a temporary instance — the pattern
//! `nerevar-core`'s `tests/admin_api.rs` and `tests/admin_staging_api.rs` use
//! — and then drives it through this crate's own command functions rather
//! than through `curl`. So what it checks is the CLI's half: the routes it
//! picks, the load-order edit it computes, the name it derives from an
//! archive, and the errors it surfaces from the host.
//!
//! The walk is the one `docs/headless-hosting.md` documents: status, upload,
//! enable, apply — and then a restart, which this host refuses with a 409
//! because no process manager is wired into its context (the desktop app and
//! a `--sync-only` daemon are in the same position).

use std::io::Write;

use nerevar_cli::admin::client::AdminClient;
use nerevar_cli::cli::{AdminAction, AdminOptions};
use nerevar_core::admin::{add_admin, ROLE_ADMIN};
use nerevar_core::instance_data::{build_manifest, load_load_order, scan_and_merge_load_order};
use nerevar_core::nerevar_server::state::ServerContext;
use nerevar_core::nerevar_server::{serve, try_bind};
use nerevar_core::sync_host::{new_shared_hosting_manifest_cache, new_shared_sync_host};

const INSTANCE_ID: &str = "cli-instance";
const INSTANCE_NAME: &str = "CLI Instance";
const NEW_PACKAGE: &str = "New Mod";

fn write(path: &std::path::Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// A zip holding one plugin and one asset under a top-level directory, the
/// shape a mod archive actually has.
fn wrapped_zip(path: &std::path::Path) {
    let mut buffer = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
        for (name, bytes) in [
            (
                format!("{NEW_PACKAGE}/new.esp"),
                b"New Mod plugin payload".as_slice(),
            ),
            (
                format!("{NEW_PACKAGE}/textures/new.dds"),
                b"new texture bytes".as_slice(),
            ),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }
    write(path, &buffer);
}

/// Runs one command and returns what a person would have seen on stdout.
async fn run(
    client: &AdminClient,
    options: &AdminOptions,
    action: AdminAction,
) -> Result<String, String> {
    let mut out = Vec::new();
    nerevar_cli::admin::run(client, options, &action, &mut out).await?;
    Ok(String::from_utf8(out).expect("command output is UTF-8"))
}

#[tokio::test]
async fn the_cli_walks_a_package_from_upload_to_applied() {
    // ---- FIXTURE: a hosted instance with one package already in data/ ------------
    let root = std::env::temp_dir().join(format!(
        "nerevar-cli-admin-{}-{}",
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
        b"[General]\nhostname = CLI Test\nport = 25565\npassword = swordfish\n",
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
        INSTANCE_NAME,
        &instance_root,
        &data_dir,
        &load_order,
        &mut no_progress,
    )
    .expect("build manifest");

    // ---- HOST: no process manager, which is why restart below is a 409 ----------
    let sync_host = new_shared_sync_host();
    {
        let mut host = sync_host.lock().unwrap();
        host.hosting_instance_id = Some(INSTANCE_ID.to_string());
        host.hosting_data_dir = Some(data_dir.clone());
        host.hosting_instance_root = Some(instance_root.clone());
        host.hosting_sync_password = Some("swordfish".to_string());
    }
    let ctx =
        ServerContext::builder(sync_host.clone(), new_shared_hosting_manifest_cache()).shared();

    let listener = try_bind(0).await.expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    let server = tokio::spawn(async move {
        let _ = serve(listener, ctx).await;
    });

    let created = add_admin(&data_dir, "ada", ROLE_ADMIN).expect("add admin");
    let client = AdminClient::new("127.0.0.1", port, created.token.clone()).expect("client");
    assert_eq!(client.base_url(), format!("http://127.0.0.1:{port}"));
    let options = AdminOptions::default();
    let json_options = AdminOptions {
        json: true,
        ..AdminOptions::default()
    };

    // ---- STATUS: before anything is staged --------------------------------------
    let text = run(&client, &options, AdminAction::Status).await.unwrap();
    assert!(text.contains(INSTANCE_NAME), "{text}");
    assert!(text.contains(INSTANCE_ID), "{text}");
    assert!(text.contains("1 package, 1 plugin enabled"), "{text}");
    assert!(text.contains("nothing staged"), "{text}");
    assert!(
        text.contains("supervises no game server"),
        "a context with no process manager cannot say: {text}"
    );

    // `--json` is the same request, printed as the host sent it.
    let raw = run(&client, &json_options, AdminAction::Status)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("--json emits the raw body");
    assert_eq!(parsed["instanceId"], serde_json::json!(INSTANCE_ID));
    assert_eq!(parsed["pendingChanges"], serde_json::Value::Null);

    // ---- UPLOAD: the package name comes from the archive's file name ------------
    let archive = root.join(format!("{NEW_PACKAGE}.zip"));
    wrapped_zip(&archive);
    let text = run(
        &client,
        &options,
        AdminAction::Upload {
            archive: archive.clone(),
            name: None,
        },
    )
    .await
    .unwrap();
    assert!(
        text.contains(&format!("Staged \"{NEW_PACKAGE}\"")),
        "{text}"
    );
    assert!(text.contains("new.esp"), "{text}");
    assert!(text.contains("mod,"), "{text}");
    assert!(
        !text.contains("already in data/"),
        "the host has never had this package: {text}"
    );

    let text = run(&client, &options, AdminAction::Status).await.unwrap();
    assert!(text.contains("1 upload"), "{text}");
    assert!(text.contains("staged by ada"), "{text}");

    // ---- ENABLE: a staged package has no load-order entry until an apply --------
    let text = run(
        &client,
        &options,
        AdminAction::Enable {
            name: NEW_PACKAGE.to_string(),
        },
    )
    .await
    .unwrap();
    assert!(
        text.contains(&format!("Enabled \"{NEW_PACKAGE}\"")),
        "{text}"
    );
    assert!(
        text.contains("a load order"),
        "the edit is staged too: {text}"
    );

    // ---- APPLY -------------------------------------------------------------------
    let text = run(&client, &options, AdminAction::Apply).await.unwrap();
    assert!(text.contains("Applied."), "{text}");
    assert!(text.contains("2 packages"), "{text}");
    assert!(text.contains(INSTANCE_NAME), "{text}");

    assert!(
        data_dir.join(NEW_PACKAGE).join("new.esp").is_file(),
        "apply moves the staged tree into data/"
    );
    let applied = load_load_order(&data_dir).expect("load order after apply");
    let entry = applied
        .entries
        .iter()
        .find(|entry| entry.relative_dir == NEW_PACKAGE)
        .expect("the enabled package survived the apply's rescan");
    assert!(entry.enabled);

    let text = run(&client, &options, AdminAction::Status).await.unwrap();
    assert!(text.contains("2 packages, 2 plugins enabled"), "{text}");
    assert!(text.contains("nothing staged"), "{text}");

    // ---- RESTART: refused, because this host supervises no game server ----------
    let error = run(&client, &options, AdminAction::Restart { yes: true })
        .await
        .expect_err("a host with no process manager has nothing to restart");
    assert!(
        error.contains("nothing to restart"),
        "the host's own 409 message must come through: {error}"
    );

    server.abort();
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_wrong_token_and_an_unreachable_host_fail_differently() {
    let root = std::env::temp_dir().join(format!(
        "nerevar-cli-admin-errors-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let data_dir = root.join("instance/data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let sync_host = new_shared_sync_host();
    {
        let mut host = sync_host.lock().unwrap();
        host.hosting_instance_id = Some(INSTANCE_ID.to_string());
        host.hosting_data_dir = Some(data_dir.clone());
        host.hosting_instance_root = Some(root.join("instance"));
    }
    let ctx = ServerContext::builder(sync_host, new_shared_hosting_manifest_cache()).shared();
    let listener = try_bind(0).await.expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    let server = tokio::spawn(async move {
        let _ = serve(listener, ctx).await;
    });

    add_admin(&data_dir, "ada", ROLE_ADMIN).expect("add admin");
    let options = AdminOptions::default();

    // A token the store does not know: the host's 401 message, not a panic.
    let wrong = AdminClient::new("127.0.0.1", port, "f".repeat(64)).unwrap();
    let error = run(&wrong, &options, AdminAction::Status)
        .await
        .expect_err("an unrecognised token is refused");
    assert!(error.to_lowercase().contains("token"), "{error}");
    assert!(
        !error.contains(&"f".repeat(64)),
        "an error must never echo the token: {error}"
    );

    server.abort();

    // Nothing listening any more: the message names the address it tried, so
    // a wrong --host or --port is diagnosable from the one line.
    let dead = AdminClient::new("127.0.0.1", port, "irrelevant".to_string()).unwrap();
    let error = run(&dead, &options, AdminAction::Status)
        .await
        .expect_err("a connection failure is an error, not an empty status");
    assert!(
        error.contains(&format!("http://127.0.0.1:{port}")),
        "{error}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
