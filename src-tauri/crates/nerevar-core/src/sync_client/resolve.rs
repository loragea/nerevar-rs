//! Finding the address a host actually answers on, from what the player typed.
//!
//! A bare hostname or IP means `http://{host}:{syncPort}` (see
//! [`super::host_address::base_url`]). A host behind an HTTPS reverse proxy
//! does not listen there, so a player who types only its name would get a
//! connection error. [`resolve_reachable_host`] therefore pings the bare form
//! first and, only when nothing answered at all (a connect-class failure:
//! refused, unresolvable, TLS handshake, timeout), pings `https://{host}` once
//! — the scheme's default port, no sync port. Whichever answered is the
//! address to keep, so every later request goes straight to it.
//!
//! An HTTP status from the first attempt — a 401, a 404, a 502 — means
//! something is listening there, and is reported as-is. A full URL is taken
//! at its word and never retried in either direction.

use std::future::Future;
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::data::InstanceConfig;

use super::fetch::{fetch_manifest_summary, probe_ping, ProbeError};
use super::host_address::{base_url, is_url_host};
use super::types::RemoteManifestSummary;

/// How long a probe waits for a TCP (and TLS) connection before treating the
/// address as unreachable. Bounded so a firewall that silently drops the sync
/// port delays the HTTPS retry by seconds, not the OS's minutes-long default.
const PROBE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A host address that answered, plus the summary it served.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ResolvedAddress {
    /// The address to store and use from now on: the one that was typed, or
    /// `https://{host}` when only the HTTPS retry answered.
    pub host: String,
    pub summary: RemoteManifestSummary,
}

/// The HTTPS address to retry a bare host at, or `None` for a URL host, which
/// is never retried.
pub fn https_fallback(host: &str) -> Option<String> {
    if is_url_host(host) {
        return None;
    }
    Some(format!("https://{}", host.trim().trim_end_matches('/')))
}

/// Pings `host` (bare hosts first on `port`, then over HTTPS) and returns the
/// address that answered. See the module docs for when the retry happens.
pub async fn resolve_reachable_host(host: &str, port: u16) -> Result<String, String> {
    let client = Client::builder()
        .connect_timeout(PROBE_CONNECT_TIMEOUT)
        .build()
        .map_err(|e| format!("Failed to build the HTTP client: {e}"))?;
    resolve_host_with(host, port, |candidate| {
        let client = client.clone();
        async move { probe_ping(&client, &candidate, port).await }
    })
    .await
}

/// [`resolve_reachable_host`], then the manifest summary from the address it
/// found — the first contact the join and New Connection flows make.
pub async fn resolve_reachable_address(
    host: &str,
    port: u16,
    sync_password: Option<&str>,
) -> Result<ResolvedAddress, String> {
    let host = resolve_reachable_host(host, port).await?;
    let summary = fetch_manifest_summary(&host, port, sync_password).await?;
    Ok(ResolvedAddress { host, summary })
}

/// Applies the join flow's rule to a synced instance's *stored* host before a
/// sync: resolves `remote_host` on `remote_sync_port` and, when a different
/// address answered (a bare host now only reachable at `https://{host}`),
/// writes it into `instance.remote_host`.
///
/// Returns whether the host changed, so the caller saves the instance its own
/// way. Fails with the resolver's error when nothing answers in either form,
/// so the sync stops there rather than going on with an address that did not
/// answer. An instance missing its host or port is left alone (`Ok(false)`):
/// the sync itself reports what is missing.
pub async fn refresh_instance_host(instance: &mut InstanceConfig) -> Result<bool, String> {
    refresh_instance_host_with(instance, |host, port| async move {
        resolve_reachable_host(&host, port).await
    })
    .await
}

/// [`refresh_instance_host`] with the resolver injected, for tests.
async fn refresh_instance_host_with<R, F>(
    instance: &mut InstanceConfig,
    resolve: R,
) -> Result<bool, String>
where
    R: FnOnce(String, u16) -> F,
    F: Future<Output = Result<String, String>>,
{
    let (Some(host), Some(port)) = (instance.remote_host.clone(), instance.remote_sync_port) else {
        return Ok(false);
    };
    let resolved = resolve(host.clone(), port).await?;
    if resolved == host {
        return Ok(false);
    }
    log::info!("{host} did not answer on port {port}; saving {resolved} as its address");
    instance.remote_host = Some(resolved);
    Ok(true)
}

/// The decision behind [`resolve_reachable_host`], with the ping injected so
/// it can be tested without a TLS server. `probe` is called with the host
/// address to try (bare or `https://…`).
pub async fn resolve_host_with<P, F>(host: &str, port: u16, probe: P) -> Result<String, String>
where
    P: Fn(String) -> F,
    F: Future<Output = Result<(), ProbeError>>,
{
    let first_base = base_url(host, port)?;
    let first_cause = match probe(host.to_string()).await {
        Ok(()) => return Ok(host.to_string()),
        Err(ProbeError::Other(message)) => return Err(message),
        Err(ProbeError::Unreachable { message, cause }) => match https_fallback(host) {
            Some(_) => cause,
            None => return Err(message),
        },
    };

    let fallback = https_fallback(host).expect("checked above");
    match probe(fallback.clone()).await {
        Ok(()) => {
            log::info!("{first_base} did not answer; using {fallback}");
            Ok(fallback)
        }
        // Something answered over HTTPS — its own error says more than a
        // two-address "could not connect" would.
        Err(ProbeError::Other(message)) => Err(message),
        Err(ProbeError::Unreachable { cause, .. }) => Err(format!(
            "Failed to reach Nerevar server: could not connect to {first_base} ({first_cause}) or {fallback} ({cause}). \
             Ensure Nerevar is running and the sync server is listening on port {port}. \
             If the host sits behind a reverse proxy, enter its full URL instead (https://…, with any port or path it uses).",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    /// Every address a test prober was asked to ping, in order.
    type Calls = Arc<Mutex<Vec<String>>>;
    type ProbeFuture = Pin<Box<dyn Future<Output = Result<(), ProbeError>>>>;

    fn unreachable(cause: &str) -> ProbeError {
        ProbeError::Unreachable {
            message: format!("single-attempt error ({cause})"),
            cause: cause.to_string(),
        }
    }

    /// A prober answering from `script` by address, recording every call.
    fn scripted(
        script: Vec<(&'static str, Result<(), ProbeError>)>,
    ) -> (
        Calls,
        impl Fn(String) -> std::future::Ready<Result<(), ProbeError>>,
    ) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let probe = move |candidate: String| {
            recorded.lock().unwrap().push(candidate.clone());
            let answer = script
                .iter()
                .find(|(address, _)| *address == candidate)
                .map(|(_, answer)| answer.clone())
                .unwrap_or_else(|| panic!("unexpected probe of {candidate}"));
            std::future::ready(answer)
        };
        (calls, probe)
    }

    // ---- the decision, with a scripted prober ------------------------------------

    #[tokio::test]
    async fn a_bare_host_that_cannot_connect_falls_back_to_https() {
        let (calls, probe) = scripted(vec![
            ("mw.example.org", Err(unreachable("Connection refused"))),
            ("https://mw.example.org", Ok(())),
        ]);
        let resolved = resolve_host_with("mw.example.org", 25567, probe).await;
        assert_eq!(resolved.unwrap(), "https://mw.example.org");
        assert_eq!(
            *calls.lock().unwrap(),
            ["mw.example.org", "https://mw.example.org"]
        );
    }

    #[tokio::test]
    async fn a_bare_host_that_answers_is_kept_as_typed() {
        let (calls, probe) = scripted(vec![("192.168.1.5", Ok(()))]);
        let resolved = resolve_host_with("192.168.1.5", 25567, probe).await;
        assert_eq!(resolved.unwrap(), "192.168.1.5");
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_url_host_never_falls_back_in_either_direction() {
        for host in ["http://mw.example.org:25567", "https://mw.example.org"] {
            let (calls, probe) = scripted(vec![(host, Err(unreachable("Connection refused")))]);
            let err = resolve_host_with(host, 25567, probe).await.unwrap_err();
            assert_eq!(err, "single-attempt error (Connection refused)");
            assert_eq!(*calls.lock().unwrap(), [host]);
        }
    }

    #[tokio::test]
    async fn an_http_status_on_the_first_attempt_does_not_fall_back() {
        let (calls, probe) = scripted(vec![(
            "mw.example.org",
            Err(ProbeError::Other(
                "Nerevar server ping failed (HTTP 401 Unauthorized)".to_string(),
            )),
        )]);
        let err = resolve_host_with("mw.example.org", 25567, probe)
            .await
            .unwrap_err();
        assert_eq!(err, "Nerevar server ping failed (HTTP 401 Unauthorized)");
        assert_eq!(*calls.lock().unwrap(), ["mw.example.org"]);
    }

    #[tokio::test]
    async fn both_attempts_failing_names_both_and_suggests_the_full_url() {
        let (_, probe) = scripted(vec![
            ("mw.example.org", Err(unreachable("Connection refused"))),
            ("https://mw.example.org", Err(unreachable("dns error"))),
        ]);
        let err = resolve_host_with("mw.example.org", 25567, probe)
            .await
            .unwrap_err();
        assert!(
            err.contains("could not connect to http://mw.example.org:25567 (Connection refused) or https://mw.example.org (dns error)"),
            "unexpected message: {err}"
        );
        assert!(err.contains("full URL"), "unexpected message: {err}");
    }

    #[tokio::test]
    async fn an_http_status_from_the_https_retry_is_reported_as_is() {
        let (_, probe) = scripted(vec![
            ("mw.example.org", Err(unreachable("Connection refused"))),
            (
                "https://mw.example.org",
                Err(ProbeError::Other(
                    "Nerevar server ping failed (HTTP 502 Bad Gateway)".to_string(),
                )),
            ),
        ]);
        let err = resolve_host_with("mw.example.org", 25567, probe)
            .await
            .unwrap_err();
        assert_eq!(err, "Nerevar server ping failed (HTTP 502 Bad Gateway)");
    }

    #[tokio::test]
    async fn an_unusable_address_fails_before_any_probe() {
        let (calls, probe) = scripted(vec![]);
        assert!(resolve_host_with("my host", 25567, probe).await.is_err());
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn https_fallback_is_only_offered_for_bare_hosts() {
        assert_eq!(
            https_fallback(" mw.example.org/ ").as_deref(),
            Some("https://mw.example.org")
        );
        assert_eq!(https_fallback("[::1]").as_deref(), Some("https://[::1]"));
        assert_eq!(https_fallback("http://mw.example.org"), None);
        assert_eq!(https_fallback("https://mw.example.org"), None);
    }

    // ---- refreshing a stored instance's host --------------------------------------

    fn synced_instance(host: Option<&str>, port: Option<u16>) -> InstanceConfig {
        InstanceConfig {
            id: "synced".into(),
            name: "Synced".into(),
            description: String::new(),
            path: String::new(),
            data_dir: String::new(),
            release_id: None,
            runtime: None,
            runtime_hint: None,
            remote_host: host.map(str::to_string),
            remote_sync_port: port,
            last_synced_at: None,
            tes3mp_server_port: None,
            sync_password: None,
        }
    }

    /// Runs [`refresh_instance_host_with`] through the real decision
    /// ([`resolve_host_with`]) with a scripted prober.
    async fn refresh_scripted(
        instance: &mut InstanceConfig,
        script: Vec<(&'static str, Result<(), ProbeError>)>,
    ) -> (Calls, Result<bool, String>) {
        let (calls, probe) = scripted(script);
        let result = refresh_instance_host_with(instance, |host, port| async move {
            resolve_host_with(&host, port, probe).await
        })
        .await;
        (calls, result)
    }

    #[tokio::test]
    async fn a_stored_bare_host_only_reachable_over_https_is_rewritten() {
        let mut instance = synced_instance(Some("mw.example.org"), Some(25567));
        let (_, result) = refresh_scripted(
            &mut instance,
            vec![
                ("mw.example.org", Err(unreachable("Connection refused"))),
                ("https://mw.example.org", Ok(())),
            ],
        )
        .await;
        assert_eq!(result, Ok(true));
        assert_eq!(
            instance.remote_host.as_deref(),
            Some("https://mw.example.org")
        );
        assert_eq!(instance.remote_sync_port, Some(25567));
    }

    #[tokio::test]
    async fn a_stored_host_that_answers_is_left_unchanged() {
        for host in ["192.168.1.5", "https://mw.example.org"] {
            let mut instance = synced_instance(Some(host), Some(25567));
            let (calls, result) = refresh_scripted(&mut instance, vec![(host, Ok(()))]).await;
            assert_eq!(result, Ok(false));
            assert_eq!(instance.remote_host.as_deref(), Some(host));
            assert_eq!(*calls.lock().unwrap(), [host]);
        }
    }

    #[tokio::test]
    async fn a_stored_host_that_answers_nowhere_fails_and_is_left_unchanged() {
        let mut instance = synced_instance(Some("mw.example.org"), Some(25567));
        let (_, result) = refresh_scripted(
            &mut instance,
            vec![
                ("mw.example.org", Err(unreachable("Connection refused"))),
                ("https://mw.example.org", Err(unreachable("dns error"))),
            ],
        )
        .await;
        let err = result.unwrap_err();
        assert!(
            err.contains("could not connect to http://mw.example.org:25567"),
            "unexpected message: {err}"
        );
        assert_eq!(instance.remote_host.as_deref(), Some("mw.example.org"));
    }

    #[tokio::test]
    async fn an_instance_without_a_host_or_port_is_not_probed() {
        for (host, port) in [(None, Some(25567)), (Some("mw.example.org"), None)] {
            let mut instance = synced_instance(host, port);
            let (calls, result) = refresh_scripted(&mut instance, vec![]).await;
            assert_eq!(result, Ok(false));
            assert_eq!(instance.remote_host.as_deref(), host);
            assert!(calls.lock().unwrap().is_empty());
        }
    }

    // ---- the real ping against local servers --------------------------------------

    /// Serves `router` on an ephemeral loopback port.
    async fn serve(router: axum::Router) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        port
    }

    /// A loopback port nothing listens on.
    async fn closed_port() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    }

    /// The real ping, recording what was asked for. The HTTPS candidate is
    /// pointed at `https_stand_in` instead, since there is no TLS server here:
    /// what is under test is the classification of real reqwest failures.
    fn real_probe(port: u16, https_stand_in: String) -> (Calls, impl Fn(String) -> ProbeFuture) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let probe = move |candidate: String| {
            recorded.lock().unwrap().push(candidate.clone());
            let target = if candidate.starts_with("https://") {
                https_stand_in.clone()
            } else {
                candidate
            };
            Box::pin(async move { probe_ping(&Client::new(), &target, port).await }) as ProbeFuture
        };
        (calls, probe)
    }

    #[tokio::test]
    async fn a_refused_connection_falls_back_and_both_failing_names_both() {
        let port = closed_port().await;
        let stand_in = format!("http://127.0.0.1:{}", closed_port().await);
        let (calls, probe) = real_probe(port, stand_in);
        let err = resolve_host_with("127.0.0.1", port, probe)
            .await
            .unwrap_err();
        assert_eq!(*calls.lock().unwrap(), ["127.0.0.1", "https://127.0.0.1"]);
        assert!(
            err.contains(&format!("could not connect to http://127.0.0.1:{port} (")),
            "unexpected message: {err}"
        );
        assert!(
            err.contains(") or https://127.0.0.1 ("),
            "unexpected message: {err}"
        );
    }

    #[tokio::test]
    async fn a_refused_connection_falls_back_to_an_answering_https_address() {
        let port = closed_port().await;
        let answering =
            serve(axum::Router::new().route("/ping", axum::routing::get(|| async { "pong" })))
                .await;
        let (_, probe) = real_probe(port, format!("http://127.0.0.1:{answering}"));
        let resolved = resolve_host_with("127.0.0.1", port, probe).await.unwrap();
        assert_eq!(resolved, "https://127.0.0.1");
    }

    #[tokio::test]
    async fn a_real_401_does_not_fall_back() {
        let port = serve(axum::Router::new().route(
            "/ping",
            axum::routing::get(|| async { axum::http::StatusCode::UNAUTHORIZED }),
        ))
        .await;
        let (calls, probe) = real_probe(port, "unused".to_string());
        let err = resolve_host_with("127.0.0.1", port, probe)
            .await
            .unwrap_err();
        assert_eq!(err, "Nerevar server ping failed (HTTP 401 Unauthorized)");
        assert_eq!(*calls.lock().unwrap(), ["127.0.0.1"]);
    }

    #[tokio::test]
    async fn resolve_reachable_host_keeps_a_bare_host_that_answers() {
        let port =
            serve(axum::Router::new().route("/ping", axum::routing::get(|| async { "pong" })))
                .await;
        assert_eq!(
            resolve_reachable_host("127.0.0.1", port).await.unwrap(),
            "127.0.0.1"
        );
    }
}
