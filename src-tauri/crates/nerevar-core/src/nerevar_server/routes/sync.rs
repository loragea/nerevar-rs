use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tokio::fs::File;
use tokio_util::io::ReaderStream;

use crate::instance_data::{load_manifest, manifest_path};
use crate::instance_setup::{instance_tes3mp_dir, read_tes3mp_server_settings};
use crate::nerevar_server::state::ServerContext;
use crate::runtime::RuntimeSource;
use crate::sync_auth::{sync_password_matches, SYNC_PASSWORD_HEADER};
use crate::sync_host::get_package_file_path;
use crate::sync_paths::normalize_manifest_file_path;

/// What `GET /` serves, and what `POST /admin/apply` answers with so an admin
/// sees exactly the summary a client would fetch next.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ManifestSummary {
    pub instance_id: String,
    pub instance_name: String,
    pub total_download_bytes: u64,
    pub package_count: u32,
    pub tes3mp_server_port: u16,
    pub password_required: bool,
    pub packages: Vec<ManifestPackageSummary>,
    /// The runtime the operator advertises, when there is one. Omitted
    /// entirely otherwise, which is also what a host older than the field
    /// sends — a client reads both as "no suggestion".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_hint: Option<RuntimeSource>,
}

impl ManifestSummary {
    /// The summary for `manifest`, given the two facts that live outside it.
    pub(super) fn of(
        manifest: &crate::instance_data::NerevarManifest,
        password_required: bool,
        runtime_hint: Option<RuntimeSource>,
    ) -> Self {
        let packages: Vec<ManifestPackageSummary> = manifest
            .packages
            .iter()
            .map(|p| ManifestPackageSummary {
                id: p.id.clone(),
                name: p.name.clone(),
                relative_dir: p.relative_dir.clone(),
                total_size_bytes: p.total_size_bytes,
                file_count: p.file_count,
            })
            .collect();

        Self {
            instance_id: manifest.instance_id.clone(),
            instance_name: manifest.instance_name.clone(),
            total_download_bytes: manifest.total_download_bytes,
            package_count: packages.len() as u32,
            tes3mp_server_port: manifest.tes3mp_server_port,
            password_required,
            packages,
            runtime_hint,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ManifestPackageSummary {
    pub id: String,
    pub name: String,
    pub relative_dir: String,
    pub total_size_bytes: u64,
    pub file_count: u32,
}

pub fn router() -> Router<Arc<ServerContext>> {
    Router::new()
        .route("/", get(root_manifest_summary))
        .route("/manifest", get(get_full_manifest))
        .route("/download", post(acknowledge_download))
        .route(
            "/packages/{package_id}/files/{*file_path}",
            get(serve_package_file),
        )
}

async fn hosting_data_dir(
    state: &ServerContext,
) -> Result<std::path::PathBuf, (StatusCode, String)> {
    let host = state
        .sync_host
        .lock()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Lock poisoned".to_string()))?;

    let data_dir = host
        .hosting_data_dir
        .clone()
        .ok_or((
            StatusCode::SERVICE_UNAVAILABLE,
            "No instance is hosting sync".to_string(),
        ))?;

    Ok(data_dir)
}

fn verify_sync_password(
    state: &ServerContext,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, String)> {
    let expected = {
        let host = state
            .sync_host
            .lock()
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Lock poisoned".to_string()))?;

        if let Some(password) = host.hosting_sync_password.as_ref() {
            password.clone()
        } else {
            let instance_root = host.hosting_instance_root.clone().ok_or((
                StatusCode::SERVICE_UNAVAILABLE,
                "No instance is hosting sync".to_string(),
            ))?;
            drop(host);

            let tes3mp_dir = instance_tes3mp_dir(&instance_root);
            read_tes3mp_server_settings(&tes3mp_dir)
                .map_err(|error| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to read TES3MP server password: {error}"),
                    )
                })?
                .password
        }
    };

    let provided = headers
        .get(SYNC_PASSWORD_HEADER)
        .and_then(|value| value.to_str().ok());

    if sync_password_matches(&expected, provided) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            "Sync password required or incorrect".to_string(),
        ))
    }
}

pub(super) fn password_required(state: &ServerContext) -> Result<bool, (StatusCode, String)> {
    let expected = {
        let host = state
            .sync_host
            .lock()
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Lock poisoned".to_string()))?;

        if let Some(password) = host.hosting_sync_password.as_ref() {
            password.clone()
        } else {
            let instance_root = host.hosting_instance_root.clone().ok_or((
                StatusCode::SERVICE_UNAVAILABLE,
                "No instance is hosting sync".to_string(),
            ))?;
            drop(host);

            let tes3mp_dir = instance_tes3mp_dir(&instance_root);
            read_tes3mp_server_settings(&tes3mp_dir)
                .map_err(|error| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to read TES3MP server password: {error}"),
                    )
                })?
                .password
        }
    };

    Ok(crate::sync_auth::sync_password_required(&expected))
}

/// The runtime hint the hosted instance advertises, snapshotted into the
/// hosting state when hosting started (the request path has no config to
/// read).
pub(super) fn hosting_runtime_hint(
    state: &ServerContext,
) -> Result<Option<RuntimeSource>, (StatusCode, String)> {
    let host = state.sync_host.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Lock poisoned".to_string(),
        )
    })?;
    Ok(host.hosting_runtime_hint.clone())
}

async fn root_manifest_summary(
    State(state): State<Arc<ServerContext>>,
    headers: HeaderMap,
) -> Result<Json<ManifestSummary>, (StatusCode, String)> {
    verify_sync_password(&state, &headers)?;
    let data_dir = hosting_data_dir(&state).await?;
    let manifest = load_manifest(&data_dir).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let password_required = password_required(&state)?;
    let runtime_hint = hosting_runtime_hint(&state)?;

    Ok(Json(ManifestSummary::of(
        &manifest,
        password_required,
        runtime_hint,
    )))
}

async fn get_full_manifest(
    State(state): State<Arc<ServerContext>>,
    headers: HeaderMap,
) -> Result<Json<crate::instance_data::NerevarManifest>, (StatusCode, String)> {
    verify_sync_password(&state, &headers)?;
    let data_dir = hosting_data_dir(&state).await?;
    let manifest = load_manifest(&data_dir).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    Ok(Json(manifest))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadAck {
    message: String,
    manifest_path: String,
}

async fn acknowledge_download(
    State(state): State<Arc<ServerContext>>,
    headers: HeaderMap,
) -> Result<Json<DownloadAck>, (StatusCode, String)> {
    verify_sync_password(&state, &headers)?;
    let data_dir = hosting_data_dir(&state).await?;
    let path = manifest_path(&data_dir);
    Ok(Json(DownloadAck {
        message: "Download acknowledged. Fetch files per package from /packages/{id}/files/...".to_string(),
        manifest_path: path.to_string_lossy().into_owned(),
    }))
}

async fn serve_package_file(
    State(state): State<Arc<ServerContext>>,
    headers: HeaderMap,
    AxumPath((package_id, file_path)): AxumPath<(String, String)>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    verify_sync_password(&state, &headers)?;
    let data_dir = hosting_data_dir(&state).await?;

    let relative = normalize_manifest_file_path(&file_path).map_err(|reason| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid file path for package {package_id}: {reason}"),
        )
    })?;

    let resolved = get_package_file_path(&state.manifest_cache, &data_dir, &package_id, &relative)
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;

    let Some((data_root, relative_path)) = resolved else {
        return Err((StatusCode::NOT_FOUND, "File not in manifest".to_string()));
    };

    let full_path = Path::new(&data_root).join(relative_path);
    let file = File::open(&full_path)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, format!("Failed to read file: {e}")))?;

    let stream = ReaderStream::new(file);
    Ok(Body::from_stream(stream))
}
