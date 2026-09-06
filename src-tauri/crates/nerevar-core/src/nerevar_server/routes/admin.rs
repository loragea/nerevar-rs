//! The `/admin` route group: bearer authentication, a capability guard, and
//! `GET /admin/status`.
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

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::admin::{build_admin_status, load_admins, role_grants, Capability, ADMIN_REQUEST_EVENT};
use crate::nerevar_server::state::ServerContext;
use crate::process_manager::ProcessRole;
use crate::reporter::emit_event;

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
        .layer(middleware::from_fn_with_state(ctx.clone(), authenticate))
        .with_state(ctx)
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

    // Only the embedder that owns the TES3MP child can answer this; a lock
    // error there is "cannot tell", which is what `null` means.
    let tes3mp_server_running = match (ctx.process_manager.as_ref(), instance_id.as_deref()) {
        (Some(manager), Some(id)) => manager.is_running(id, ProcessRole::Server).ok(),
        _ => None,
    };

    Json(build_admin_status(
        instance_id,
        data_dir.as_deref(),
        tes3mp_server_running,
    ))
    .into_response()
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
