//! The `/admin` route group: bearer authentication, a capability guard, and
//! the co-admin read and write routes.
//!
//! Two layers, in this order for every request under `/admin`:
//!
//! 1. [`authenticate`] — reads `Authorization: Bearer <token>`, re-reads
//!    `admins.json` from the hosted instance, and either rejects (401) or puts
//!    an [`AdminIdentity`] in the request extensions. Whatever the inner
//!    layers decide, it then writes one audit line through the `EventSink`.
//! 2. A per-route capability guard — the route names the [`Capability`] it
//!    needs; a role that does not grant it gets 403. No handler ever asks
//!    whether the caller "is an admin".
//!
//! The store is re-read per request rather than cached: `nerevar-host admin
//! add` and `admin revoke` then take effect against a running daemon with no
//! reload signal, and the file is a few hundred bytes.
//!
//! Tokens are never logged, in any branch.
//!
//! **Stage, then apply.** Every write route except `POST /admin/apply` and
//! `POST /admin/restart` edits the staging area under
//! `<data dir>/.nerevar/staging/` and nothing else (`crate::admin::staging`);
//! apply is the one call that rewrites `data/`, `load-order.json` and
//! `manifest.json`, and it re-activates hosting so the served manifest and the
//! file cache are the new ones. Restart touches no files at all: it replaces
//! the TES3MP process so the applied plugin list finally takes effect
//! (`crate::admin::restart`). Both run on a blocking thread — apply hashes the
//! whole data directory, restart waits for the game port to be released — and
//! every write route serializes on [`ServerContext::admin_write_lock`], which
//! those two take without waiting so a second concurrent one is a `409` rather
//! than a queue.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Extension, Path as AxumPath, Request, State};
use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::admin::staging::{
    available_package_names, clear_staging, data_package_names, load_pending, save_pending,
    staged_archive_path, staged_package_dir, staging_dir, validate_pending_load_order,
    validate_staged_package_name,
};
use crate::admin::{
    build_admin_status, execute_apply, instance_name_for_rebuild, load_admins, pending_changes_for,
    restart_tes3mp_server, role_grants, stage_uploaded_archive, AdminPendingChanges, ApplyPlan,
    Capability, ADMIN_APPLY_EVENT, ADMIN_REQUEST_EVENT, ADMIN_RESTART_EVENT,
};
use crate::instance_data::LoadOrder;
use crate::nerevar_server::state::ServerContext;
use crate::process_manager::ProcessRole;
use crate::reporter::emit_event;
use crate::sync_host::activate_hosting;

use super::sync::{hosting_runtime_hint, password_required, ManifestSummary};

/// Body limit for `PUT /admin/packages/{name}`, and only that route: axum's
/// 2 MiB default is right for the JSON bodies everywhere else, and wrong by
/// three orders of magnitude for a mod archive. 4 GiB is above the largest
/// Morrowind package anyone ships (Tamriel Rebuilt is ~2 GiB unpacked) and
/// still a real ceiling, so a runaway upload cannot fill the host's disk
/// unbounded.
pub const MAX_PACKAGE_UPLOAD_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// [`MAX_PACKAGE_UPLOAD_BYTES`] as axum wants it, saturating rather than
/// wrapping on a 32-bit target.
fn max_upload_body_bytes() -> usize {
    usize::try_from(MAX_PACKAGE_UPLOAD_BYTES).unwrap_or(usize::MAX)
}

/// The authenticated caller, put in the request extensions by
/// [`authenticate`] for the capability guard and the handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminIdentity {
    pub name: String,
    pub role: String,
}

/// Audit payload for one authenticated `/admin` request. Emitted after the
/// response is known, so `status` records what the caller actually got —
/// including a 403 from the capability guard.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminRequestEvent {
    admin: String,
    role: String,
    method: String,
    path: String,
    status: u16,
}

#[derive(Serialize)]
struct AdminErrorBody {
    error: String,
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(AdminErrorBody {
            error: message.into(),
        }),
    )
        .into_response()
}

/// The `/admin` group. Takes the context by value rather than through
/// `with_state` alone because [`authenticate`] is a `from_fn_with_state`
/// layer, which needs the state at construction time.
pub fn router(ctx: Arc<ServerContext>) -> Router {
    Router::new()
        .route(
            "/admin/status",
            get(admin_status).route_layer(middleware::from_fn_with_state(
                Capability::Status,
                require_capability,
            )),
        )
        .route(
            "/admin/load-order",
            get(get_load_order)
                .post(set_load_order)
                .route_layer(middleware::from_fn_with_state(
                    Capability::Stage,
                    require_capability,
                )),
        )
        .route(
            "/admin/discard",
            post(discard_pending).route_layer(middleware::from_fn_with_state(
                Capability::Stage,
                require_capability,
            )),
        )
        .route(
            "/admin/apply",
            post(apply).route_layer(middleware::from_fn_with_state(
                Capability::Apply,
                require_capability,
            )),
        )
        .route(
            "/admin/restart",
            post(restart).route_layer(middleware::from_fn_with_state(
                Capability::Restart,
                require_capability,
            )),
        )
        // Its own sub-router so the raised body limit covers this path and no
        // other: `DefaultBodyLimit` is a layer, and a layer on the group would
        // raise the limit for every `/admin` route at once.
        .merge(packages_router())
        .layer(middleware::from_fn_with_state(ctx.clone(), authenticate))
        .with_state(ctx)
}

fn packages_router() -> Router<Arc<ServerContext>> {
    Router::new()
        .route(
            "/admin/packages/{name}",
            put(upload_package).delete(delete_package),
        )
        .route_layer(middleware::from_fn_with_state(
            Capability::Stage,
            require_capability,
        ))
        .layer(DefaultBodyLimit::max(max_upload_body_bytes()))
}

/// `Authorization: Bearer <token>`, or `None` when the header is missing,
/// unreadable, not a bearer, or empty. The scheme is matched
/// case-insensitively (RFC 7235); the token is not.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// The hosted instance's package data directory, cloned out from under the
/// sync-host mutex. The lock is never held across an await.
///
/// The error is the ingredients of a response rather than a built one, so the
/// happy path does not carry an axum `Response` around in a `Result`.
fn hosting_data_dir(ctx: &ServerContext) -> Result<std::path::PathBuf, (StatusCode, &'static str)> {
    let host = ctx
        .sync_host
        .lock()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Sync host lock poisoned"))?;
    host.hosting_data_dir.clone().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "No instance is hosting sync",
    ))
}

/// The hosted instance, cloned out from under the sync-host mutex in one
/// take. Every write route needs all four, and the lock is never held across
/// an await.
struct Hosted {
    instance_id: String,
    instance_root: PathBuf,
    data_dir: PathBuf,
    sync_password: String,
}

fn hosted_instance(ctx: &ServerContext) -> Result<Hosted, (StatusCode, &'static str)> {
    let host = ctx
        .sync_host
        .lock()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Sync host lock poisoned"))?;

    let unavailable = (
        StatusCode::SERVICE_UNAVAILABLE,
        "No instance is hosting sync",
    );
    Ok(Hosted {
        instance_id: host.hosting_instance_id.clone().ok_or(unavailable)?,
        instance_root: host.hosting_instance_root.clone().ok_or(unavailable)?,
        data_dir: host.hosting_data_dir.clone().ok_or(unavailable)?,
        // An instance with no password hosts openly; that is a valid state,
        // not a missing one.
        sync_password: host.hosting_sync_password.clone().unwrap_or_default(),
    })
}

/// A failure from blocking admin work, carrying the status it deserves.
///
/// Core returns `String` errors, which all mean "the host could not do it"
/// (500); the cases that are the *caller's* fault — a body that is not an
/// archive, a load order naming a package that does not exist — are built
/// with [`AdminError::bad_request`] so the client is told so.
struct AdminError {
    status: StatusCode,
    message: String,
}

impl AdminError {
    fn into_response(self) -> Response {
        error_response(self.status, self.message)
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
}

impl From<String> for AdminError {
    fn from(message: String) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message,
        }
    }
}

/// Runs blocking filesystem work off the runtime thread, folding a panicked
/// or cancelled task into the same error type the work itself returns.
async fn blocking<T, F>(work: F) -> Result<T, AdminError>
where
    F: FnOnce() -> Result<T, AdminError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(error) => Err(format!("Admin task failed: {error}").into()),
    }
}

/// The body every staging write answers with: the whole pending set as
/// `GET /admin/status` would report it, so a caller sees the result of its
/// change without a second request.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingChangesResponse {
    pending_changes: Option<AdminPendingChanges>,
}

fn pending_response(data_dir: &Path) -> Response {
    Json(PendingChangesResponse {
        pending_changes: pending_changes_for(data_dir),
    })
    .into_response()
}

async fn authenticate(
    State(ctx): State<Arc<ServerContext>>,
    mut request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();

    let Some(token) = bearer_token(request.headers()) else {
        log::warn!("{method} {path}: rejected, no admin bearer token");
        return error_response(
            StatusCode::UNAUTHORIZED,
            "An admin bearer token is required: Authorization: Bearer <token>",
        );
    };

    let data_dir = match hosting_data_dir(&ctx) {
        Ok(dir) => dir,
        Err((status, message)) => return error_response(status, message),
    };

    let store = match load_admins(&data_dir) {
        Ok(store) => store,
        Err(error) => {
            log::error!("{method} {path}: cannot read the admin store: {error}");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The admin store could not be read",
            );
        }
    };

    let Some(identity) = store.authenticate(&token).map(|record| AdminIdentity {
        name: record.name.clone(),
        role: record.role.clone(),
    }) else {
        // Never the token, not even truncated.
        log::warn!("{method} {path}: rejected, admin token not recognised");
        return error_response(StatusCode::UNAUTHORIZED, "Unrecognised admin token");
    };

    request.extensions_mut().insert(identity.clone());
    let response = next.run(request).await;

    emit_event(
        &*ctx.sink,
        ADMIN_REQUEST_EVENT,
        &AdminRequestEvent {
            admin: identity.name,
            role: identity.role,
            method: method.to_string(),
            path,
            status: response.status().as_u16(),
        },
    );

    response
}

/// Per-route guard: the caller authenticated, but does their role grant this?
async fn require_capability(
    State(required): State<Capability>,
    request: Request,
    next: Next,
) -> Response {
    let Some(identity) = request.extensions().get::<AdminIdentity>().cloned() else {
        // Unreachable while `authenticate` wraps this layer; a 500 rather than
        // an open door if that wiring is ever changed.
        log::error!("capability guard ran without an authenticated identity");
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Admin identity missing from the request",
        );
    };

    if !role_grants(&identity.role, required) {
        return error_response(
            StatusCode::FORBIDDEN,
            format!(
                "Role \"{}\" does not grant the \"{required}\" capability",
                identity.role
            ),
        );
    }

    next.run(request).await
}

async fn admin_status(State(ctx): State<Arc<ServerContext>>) -> Response {
    let (instance_id, data_dir) = match ctx.sync_host.lock() {
        Ok(host) => (
            host.hosting_instance_id.clone(),
            host.hosting_data_dir.clone(),
        ),
        Err(_) => {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "Sync host lock poisoned")
        }
    };

    // Only the embedder that owns the TES3MP child can answer these; a lock
    // error there is "cannot tell", which is what `null` means.
    let supervised = match (ctx.process_manager.as_ref(), instance_id.as_deref()) {
        (Some(manager), Some(id)) => Some((manager, id)),
        _ => None,
    };
    let tes3mp_server_running =
        supervised.and_then(|(manager, id)| manager.is_running(id, ProcessRole::Server).ok());
    // Only meaningful for a server that is up: the manager keeps an exited
    // child's entry around until its watcher reaps it, and a launch time for a
    // process that is gone would read as uptime it does not have.
    let tes3mp_server_started_at = supervised
        .filter(|_| tes3mp_server_running == Some(true))
        .and_then(|(manager, id)| manager.started_at(id, ProcessRole::Server).ok().flatten())
        .map(|started| started.to_rfc3339());

    Json(build_admin_status(
        instance_id,
        data_dir.as_deref(),
        tes3mp_server_running,
        tes3mp_server_started_at,
        ctx.tes3mp_plugin_list_stale.load(Ordering::Relaxed),
    ))
    .into_response()
}

/// `PUT /admin/packages/{name}` — stage an archive as a package.
///
/// The body is the archive itself, raw: no multipart, so a client is one
/// `curl --upload-file` and a server-side upload is one streamed write. It
/// lands in `staging/<name>.part`, is extracted into `staging/<name>/`, and is
/// recorded in `pending.json`; `data/` is untouched until apply. A name that
/// already exists in `data/` is a replace-at-apply, which the pending set
/// records and `GET /admin/status` reports.
///
/// A failed upload leaves nothing behind — no `.part`, no half-extracted
/// tree, no record.
async fn upload_package(
    State(ctx): State<Arc<ServerContext>>,
    Extension(identity): Extension<AdminIdentity>,
    AxumPath(name): AxumPath<String>,
    body: Body,
) -> Response {
    let name = match validate_staged_package_name(&name) {
        Ok(name) => name,
        Err(reason) => return error_response(StatusCode::BAD_REQUEST, reason),
    };

    let hosted = match hosted_instance(&ctx) {
        Ok(hosted) => hosted,
        Err((status, message)) => return error_response(status, message),
    };

    // Serialized with every other admin write: two uploads landing at once
    // would otherwise read and write `pending.json` over each other.
    let _guard = ctx.admin_write_lock.lock().await;

    let staging = staging_dir(&hosted.data_dir);
    if let Err(error) = tokio::fs::create_dir_all(&staging).await {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create {}: {error}", staging.display()),
        );
    }

    let archive = staged_archive_path(&hosted.data_dir, &name);
    let archive_bytes = match stream_body_to_file(body, &archive).await {
        Ok(bytes) => bytes,
        Err(reason) => {
            let _ = tokio::fs::remove_file(&archive).await;
            return error_response(StatusCode::BAD_REQUEST, reason);
        }
    };

    let data_dir = hosted.data_dir.clone();
    let staged_by = identity.name.clone();
    let staged_name = name.clone();
    let staged = blocking(move || -> Result<_, AdminError> {
        let replaces_existing = data_package_names(&data_dir)?
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&staged_name));
        // A body that is not a readable archive is the client's mistake.
        let staged = stage_uploaded_archive(
            &data_dir,
            &staged_name,
            archive_bytes,
            replaces_existing,
            &staged_by,
        )
        .map_err(AdminError::bad_request)?;
        let mut pending = load_pending(&data_dir)?;
        pending.upsert_staged(staged.clone());
        save_pending(&data_dir, &pending)?;
        Ok(staged)
    })
    .await;

    match staged {
        Ok(staged) => Json(staged).into_response(),
        // Extraction refuses a body that is not an archive; that is the
        // client's mistake, not the host's.
        Err(error) => {
            let _ = tokio::fs::remove_file(&archive).await;
            error.into_response()
        }
    }
}

/// Streams a request body to `path`, returning the bytes written. The caller
/// removes the file on failure.
async fn stream_body_to_file(body: Body, path: &Path) -> Result<u64, String> {
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|error| format!("Failed to create {}: {error}", path.display()))?;

    let mut stream = body.into_data_stream();
    let mut written = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("Failed to read the upload body: {error}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("Failed to write {}: {error}", path.display()))?;
        written = written.saturating_add(chunk.len() as u64);
    }
    file.flush()
        .await
        .map_err(|error| format!("Failed to flush {}: {error}", path.display()))?;

    Ok(written)
}

/// `DELETE /admin/packages/{name}`.
///
/// A staged package is simply dropped from staging — the upload never
/// happened. A package in `data/` is marked for removal, and apply is what
/// deletes it. A name that is neither is a `404`.
async fn delete_package(
    State(ctx): State<Arc<ServerContext>>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    let name = match validate_staged_package_name(&name) {
        Ok(name) => name,
        Err(reason) => return error_response(StatusCode::BAD_REQUEST, reason),
    };

    let hosted = match hosted_instance(&ctx) {
        Ok(hosted) => hosted,
        Err((status, message)) => return error_response(status, message),
    };

    let _guard = ctx.admin_write_lock.lock().await;

    let data_dir = hosted.data_dir.clone();
    let target = name.clone();
    let found = blocking(move || -> Result<bool, AdminError> {
        let mut pending = load_pending(&data_dir)?;
        if pending.remove_staged(&target) {
            let staged = staged_package_dir(&data_dir, &target);
            if let Err(error) = std::fs::remove_dir_all(&staged) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(format!("Failed to delete {}: {error}", staged.display()).into());
                }
            }
            save_pending(&data_dir, &pending)?;
            return Ok(true);
        }

        let Some(existing) = data_package_names(&data_dir)?
            .into_iter()
            .find(|existing| existing.eq_ignore_ascii_case(&target))
        else {
            return Ok(false);
        };
        pending.mark_removal(&existing);
        save_pending(&data_dir, &pending)?;
        Ok(true)
    })
    .await;

    match found {
        Ok(true) => pending_response(&hosted.data_dir),
        Ok(false) => error_response(
            StatusCode::NOT_FOUND,
            format!("No package \"{name}\" in the data directory or in staging"),
        ),
        Err(error) => error.into_response(),
    }
}

/// What `GET /admin/load-order` serves: the order on disk, and the pending
/// one if a `POST /admin/load-order` is waiting. The read half of the write
/// route — an admin with no shell has nowhere else to get the document they
/// are about to edit.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadOrderView {
    current: LoadOrder,
    pending: Option<LoadOrder>,
}

async fn get_load_order(State(ctx): State<Arc<ServerContext>>) -> Response {
    let hosted = match hosted_instance(&ctx) {
        Ok(hosted) => hosted,
        Err((status, message)) => return error_response(status, message),
    };

    let data_dir = hosted.data_dir.clone();
    match blocking(move || -> Result<LoadOrderView, AdminError> {
        let (current, pending) = crate::admin::current_and_pending_load_order(&data_dir)?;
        Ok(LoadOrderView { current, pending })
    })
    .await
    {
        Ok(view) => Json(view).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `POST /admin/load-order` — stage a load order.
///
/// The body is exactly the document the desktop app saves and the daemon
/// reads, so the rules are the same file's rules; on top of them, an entry
/// may name a staged package but not a package that will exist nowhere after
/// apply. Nothing is written to `load-order.json` until apply.
async fn set_load_order(
    State(ctx): State<Arc<ServerContext>>,
    Json(load_order): Json<LoadOrder>,
) -> Response {
    let hosted = match hosted_instance(&ctx) {
        Ok(hosted) => hosted,
        Err((status, message)) => return error_response(status, message),
    };

    let _guard = ctx.admin_write_lock.lock().await;

    let data_dir = hosted.data_dir.clone();
    let stored = blocking(move || -> Result<(), AdminError> {
        let mut pending = load_pending(&data_dir)?;
        let available = available_package_names(&data_dir, &pending)?;
        validate_pending_load_order(&load_order, &available).map_err(AdminError::bad_request)?;
        pending.load_order = Some(load_order);
        save_pending(&data_dir, &pending)?;
        Ok(())
    })
    .await;

    match stored {
        Ok(()) => pending_response(&hosted.data_dir),
        Err(error) => error.into_response(),
    }
}

/// `POST /admin/discard` — throw the whole pending set away.
async fn discard_pending(State(ctx): State<Arc<ServerContext>>) -> Response {
    let hosted = match hosted_instance(&ctx) {
        Ok(hosted) => hosted,
        Err((status, message)) => return error_response(status, message),
    };

    let _guard = ctx.admin_write_lock.lock().await;

    let data_dir = hosted.data_dir.clone();
    match blocking(move || Ok(clear_staging(&data_dir)?)).await {
        Ok(()) => pending_response(&hosted.data_dir),
        Err(error) => error.into_response(),
    }
}

/// Payload of the one event an apply emits, so the daemon's journal records
/// what changed and — the part an operator has to act on — that the running
/// TES3MP server is now behind the manifest.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminApplyEvent {
    admin: String,
    installed: Vec<String>,
    removed: Vec<String>,
    wrote_load_order: bool,
    package_count: u32,
    tes3mp_plugin_list_stale: bool,
}

/// `POST /admin/apply` — the approval step.
///
/// Serialized behind the server's admin write lock, taken without waiting so
/// a second apply is a `409` instead of a queue. The body of the work runs on
/// a blocking thread (a manifest rebuild hashes every enabled package), and
/// hosting is re-activated afterwards so the manifest cache is dropped and
/// clients are served the new one.
///
/// Applying with nothing pending is a `200` that still rebuilds — the way to
/// regenerate a manifest after editing files on the host by hand.
async fn apply(
    State(ctx): State<Arc<ServerContext>>,
    Extension(identity): Extension<AdminIdentity>,
) -> Response {
    let Ok(_guard) = ctx.admin_write_lock.try_lock() else {
        return error_response(
            StatusCode::CONFLICT,
            "Another admin change is already being applied; try again when it finishes",
        );
    };

    let hosted = match hosted_instance(&ctx) {
        Ok(hosted) => hosted,
        Err((status, message)) => return error_response(status, message),
    };
    let runtime_hint = match hosting_runtime_hint(&ctx) {
        Ok(hint) => hint,
        Err((status, message)) => return error_response(status, message),
    };

    let instance_id = hosted.instance_id.clone();
    let instance_root = hosted.instance_root.clone();
    let data_dir = hosted.data_dir.clone();
    let outcome = blocking(move || {
        let instance_name = instance_name_for_rebuild(&data_dir, &instance_id);
        Ok(execute_apply(
            &instance_id,
            &instance_name,
            &instance_root,
            &data_dir,
        )?)
    })
    .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => return error.into_response(),
    };

    // The manifest on disk is new, so the cache indexed by the old one has to
    // go; `activate_hosting` is the same call the GUI and the daemon make and
    // it clears the cache as part of pointing hosting at the data directory.
    if let Err(error) = activate_hosting(
        &ctx.sync_host,
        &ctx.manifest_cache,
        hosted.instance_id.clone(),
        hosted.data_dir.clone(),
        hosted.instance_root.clone(),
        hosted.sync_password.clone(),
        runtime_hint.clone(),
        ctx.sink.clone(),
    ) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Applied, but re-activating hosting failed: {error}"),
        );
    }

    // Apply deliberately does not restart TES3MP, which reads its plugin list
    // once at start. A server that is positively not running cannot be stale;
    // anything else (running, or an embedder that cannot tell) is.
    let server_running = ctx.process_manager.as_ref().and_then(|manager| {
        manager
            .is_running(&hosted.instance_id, ProcessRole::Server)
            .ok()
    });
    let stale = server_running != Some(false);
    if stale {
        ctx.tes3mp_plugin_list_stale.store(true, Ordering::Relaxed);
        log::warn!(
            "Applied {} staged package(s) and {} removal(s) for \"{}\": the running TES3MP \
             server still enforces the old plugin list until it is restarted",
            outcome.plan.installs.len(),
            outcome.plan.removals.len(),
            hosted.instance_id
        );
    }

    emit_event(
        &*ctx.sink,
        ADMIN_APPLY_EVENT,
        &AdminApplyEvent {
            admin: identity.name,
            installed: plan_names(&outcome.plan),
            removed: outcome.plan.removals.clone(),
            wrote_load_order: outcome.plan.writes_load_order,
            package_count: outcome.manifest.packages.len() as u32,
            tes3mp_plugin_list_stale: stale,
        },
    );

    let password_required = match password_required(&ctx) {
        Ok(required) => required,
        Err((status, message)) => return error_response(status, message),
    };

    Json(ManifestSummary::of(
        &outcome.manifest,
        password_required,
        runtime_hint,
    ))
    .into_response()
}

/// Payload of the event a restart emits, next to the audit line every
/// `/admin` request already writes.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminRestartEvent {
    admin: String,
    pid: Option<u32>,
    started_at: Option<String>,
    /// Whether there was a live server to stop, or the restart started one
    /// that had already gone away.
    was_running: bool,
}

/// `POST /admin/restart` — stop the TES3MP dedicated server and start it again
/// now.
///
/// The lever apply deliberately does not pull: TES3MP reads its plugin list
/// once, at start, so an applied mod list only reaches players' sessions after
/// this. It kicks everyone connected, which is why the admin chooses the
/// moment.
///
/// Only the embedder that launched the game server can restart it, so a `409`
/// — not a `500` — is the answer on the desktop app and on a `--sync-only`
/// daemon, and nothing is attempted in either case. Serialized behind the same
/// write lock as apply, taken without waiting: a restart queued behind a
/// five-minute rebuild is never what the caller meant.
async fn restart(
    State(ctx): State<Arc<ServerContext>>,
    Extension(identity): Extension<AdminIdentity>,
) -> Response {
    let Some(manager) = ctx.process_manager.clone() else {
        return error_response(
            StatusCode::CONFLICT,
            "This host does not supervise a TES3MP dedicated server, so there is nothing to \
             restart",
        );
    };
    if !ctx.server_restart.is_supervising() {
        return error_response(
            StatusCode::CONFLICT,
            "This host is not running a TES3MP dedicated server (started with --sync-only), so \
             there is nothing to restart",
        );
    }

    let Ok(_guard) = ctx.admin_write_lock.try_lock() else {
        return error_response(
            StatusCode::CONFLICT,
            "Another admin change is in progress; try again when it finishes",
        );
    };

    let hosted = match hosted_instance(&ctx) {
        Ok(hosted) => hosted,
        Err((status, message)) => return error_response(status, message),
    };

    // Blocking: the stop reaps the process group so the game port is free
    // before the relaunch binds it.
    let sink = ctx.sink.clone();
    let restart_state = ctx.server_restart.clone();
    let instance_id = hosted.instance_id.clone();
    let instance_root = hosted.instance_root.clone();
    let data_dir = hosted.data_dir.clone();
    let outcome = blocking(move || {
        Ok(restart_tes3mp_server(
            sink,
            &manager,
            &restart_state,
            &instance_id,
            &instance_root,
            &data_dir,
        )?)
    })
    .await;

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => return error.into_response(),
    };

    // The new process read the plugin list the last apply wrote, so whatever
    // was stale about the old one is settled.
    ctx.tes3mp_plugin_list_stale.store(false, Ordering::Relaxed);
    log::info!(
        "TES3MP server restarted by admin {} for instance \"{}\"{}",
        identity.name,
        hosted.instance_id,
        match outcome.pid {
            Some(pid) => format!(" (pid {pid})"),
            None => String::new(),
        }
    );

    emit_event(
        &*ctx.sink,
        ADMIN_RESTART_EVENT,
        &AdminRestartEvent {
            admin: identity.name,
            pid: outcome.pid,
            started_at: outcome.started_at.clone(),
            was_running: outcome.was_running,
        },
    );

    Json(outcome).into_response()
}

fn plan_names(plan: &ApplyPlan) -> Vec<String> {
    plan.installs
        .iter()
        .map(|install| install.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(value: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        map
    }

    #[test]
    fn a_bearer_header_yields_its_token() {
        assert_eq!(
            bearer_token(&headers("Bearer abc123")).as_deref(),
            Some("abc123")
        );
        assert_eq!(
            bearer_token(&headers("bearer abc123")).as_deref(),
            Some("abc123")
        );
        assert_eq!(
            bearer_token(&headers("BEARER  abc123 ")).as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn anything_that_is_not_a_non_empty_bearer_token_yields_none() {
        assert!(bearer_token(&HeaderMap::new()).is_none());
        assert!(bearer_token(&headers("Basic abc123")).is_none());
        assert!(bearer_token(&headers("Bearer")).is_none());
        assert!(bearer_token(&headers("Bearer ")).is_none());
        assert!(bearer_token(&headers("abc123")).is_none());
    }
}
