use reqwest::Client;

use crate::instance_data::{strip_official_game_files, NerevarManifest};
use crate::sync_auth::SYNC_PASSWORD_HEADER;

use super::host_address::{base_url, is_url_host};
use super::types::RemoteManifestSummary;

/// Formats a transport failure against the base URL the request actually used —
/// never a rebuilt one, so a URL host is reported exactly as it was reached.
///
/// `port_hint` is the configured sync port, and is `None` for a URL host: that
/// address carries its own port (or the scheme's default), so naming the ignored
/// `syncPort` field would only mislead.
fn connection_error(
    context: &str,
    base: &str,
    port_hint: Option<u16>,
    err: reqwest::Error,
) -> String {
    if err.is_connect() {
        match port_hint {
            Some(port) => format!(
                "{context} at {base}: could not connect. Ensure Nerevar is running and the sync server is listening on port {port}."
            ),
            None => format!(
                "{context} at {base}: could not connect. Ensure Nerevar is running and the reverse proxy in front of it is reachable."
            ),
        }
    } else if err.is_timeout() {
        format!("{context} at {base}: request timed out.")
    } else {
        format!("{context} at {base}: {err}")
    }
}

/// The sync port to name in a connection error, or `None` when `host` is a URL.
fn port_hint(host: &str, port: u16) -> Option<u16> {
    (!is_url_host(host)).then_some(port)
}

fn apply_sync_password(
    builder: reqwest::RequestBuilder,
    sync_password: Option<&str>,
) -> reqwest::RequestBuilder {
    match sync_password.filter(|password| !password.is_empty()) {
        Some(password) => builder.header(SYNC_PASSWORD_HEADER, password),
        None => builder,
    }
}

pub async fn ping_nerevar_server(host: &str, port: u16) -> Result<(), String> {
    probe_ping(&Client::new(), host, port)
        .await
        .map_err(ProbeError::into_message)
}

/// Why a `/ping` did not succeed, split the way the bare-host HTTPS fallback
/// in [`super::resolve`] needs it split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// Nothing answered: the connection was refused, the name did not
    /// resolve, the TLS handshake failed, or the request timed out. The only
    /// class of failure a retry at another address can fix.
    Unreachable {
        /// The single-attempt error, exactly as `ping_nerevar_server` words it.
        message: String,
        /// The innermost cause (e.g. "Connection refused (os error 111)"), for
        /// the message that names both attempts.
        cause: String,
    },
    /// Something answered, or the address itself is unusable: an HTTP status,
    /// a malformed host. Never retried elsewhere.
    Other(String),
}

impl ProbeError {
    pub fn into_message(self) -> String {
        match self {
            ProbeError::Unreachable { message, .. } | ProbeError::Other(message) => message,
        }
    }
}

/// The innermost error in `err`'s source chain — the OS or TLS reason under
/// reqwest's and hyper's wrappers.
fn root_cause(err: &(dyn std::error::Error + 'static)) -> String {
    let mut current = err;
    while let Some(source) = current.source() {
        current = source;
    }
    current.to_string()
}

/// `GET {base}/ping` with `client`, classifying a failure as [`ProbeError`].
/// The messages are the ones `ping_nerevar_server` has always returned.
pub(crate) async fn probe_ping(client: &Client, host: &str, port: u16) -> Result<(), ProbeError> {
    let base = base_url(host, port).map_err(ProbeError::Other)?;
    let url = format!("{base}/ping");
    let response = client
        .get(&url)
        .header("User-Agent", crate::USER_AGENT)
        .send()
        .await
        .map_err(|e| {
            let unreachable = e.is_connect() || e.is_timeout();
            let cause = root_cause(&e);
            let message = connection_error(
                "Failed to reach Nerevar server",
                &base,
                port_hint(host, port),
                e,
            );
            if unreachable {
                ProbeError::Unreachable { message, cause }
            } else {
                ProbeError::Other(message)
            }
        })?;

    if !response.status().is_success() {
        return Err(ProbeError::Other(format!(
            "Nerevar server ping failed (HTTP {})",
            response.status()
        )));
    }
    Ok(())
}

pub async fn fetch_manifest_summary(
    host: &str,
    port: u16,
    sync_password: Option<&str>,
) -> Result<RemoteManifestSummary, String> {
    let client = Client::new();
    let base = base_url(host, port)?;
    let url = format!("{base}/");
    let request = apply_sync_password(
        client
            .get(&url)
            .header("User-Agent", crate::USER_AGENT),
        sync_password,
    );
    let response = request.send().await.map_err(|e| {
        connection_error(
            "Failed to fetch manifest summary",
            &base,
            port_hint(host, port),
            e,
        )
    })?;

    if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        return Err(
            "No instance is hosting sync yet. On the host instance, use \"Activate Nerevar syncing\" or save & host from the data manager.".to_string(),
        );
    }

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err("Sync password required or incorrect".to_string());
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Manifest summary request failed (HTTP {status}): {body}"));
    }

    response
        .json::<RemoteManifestSummary>()
        .await
        .map_err(|e| format!("Invalid manifest summary response: {e}"))
}

pub async fn fetch_full_manifest(
    host: &str,
    port: u16,
    sync_password: Option<&str>,
) -> Result<NerevarManifest, String> {
    let client = Client::new();
    let base = base_url(host, port)?;
    let url = format!("{base}/manifest");
    let request = apply_sync_password(
        client
            .get(&url)
            .header("User-Agent", crate::USER_AGENT),
        sync_password,
    );
    let response = request.send().await.map_err(|e| {
        connection_error("Failed to fetch manifest", &base, port_hint(host, port), e)
    })?;

    if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        return Err(
            "No instance is hosting sync yet. On the host instance, use \"Activate Nerevar syncing\" or save & host from the data manager.".to_string(),
        );
    }

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err("Sync password required or incorrect".to_string());
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Manifest request failed (HTTP {status}): {body}"));
    }

    let mut manifest = response
        .json::<NerevarManifest>()
        .await
        .map_err(|e| format!("Invalid manifest response: {e}"))?;
    // A host never lists official game data; one built before that rule, or
    // a hostile one, might. Every caller downloads, persists and validates
    // against this manifest, so dropping the files here means they are never
    // fetched, while the rest of the sync goes ahead.
    strip_official_game_files(&mut manifest);
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host that lists `Morrowind.bsa` as a package file: the client's view
    /// of the manifest no longer has it, so nothing downstream downloads it.
    #[tokio::test]
    async fn a_fetched_manifest_never_lists_official_game_data() {
        let body = serde_json::json!({
            "version": 1,
            "instanceId": "x",
            "instanceName": "x",
            "generatedAt": "now",
            "packages": [{
                "id": "pkg",
                "name": "Hostile",
                "kind": "mod",
                "relativeDir": "Hostile",
                "priority": 10,
                "treeChecksum": "sha256:tree",
                "totalSizeBytes": 101,
                "fileCount": 2,
                "files": [
                    { "path": "mod.esp", "size": 1, "checksum": "sha256:a" },
                    { "path": "Morrowind.bsa", "size": 100, "checksum": "sha256:b" }
                ],
                "plugins": [{ "file": "mod.esp", "enabled": true }]
            }],
            "resolved": {
                "encoding": "win1252",
                "dataPaths": [],
                "content": ["Morrowind.esm", "mod.esp"],
                "archives": ["Morrowind.bsa"]
            },
            "totalDownloadBytes": 101
        });
        let router = axum::Router::new().route(
            "/manifest",
            axum::routing::get(move || {
                let body = body.clone();
                async move { axum::Json(body) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let manifest = fetch_full_manifest("127.0.0.1", port, None).await.unwrap();
        let package = &manifest.packages[0];
        let paths: Vec<&str> = package.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["mod.esp"]);
        assert_eq!(package.file_count, 1);
        assert_eq!(manifest.total_download_bytes, 1);
        assert!(manifest.resolved.archives.is_empty());
    }
}
