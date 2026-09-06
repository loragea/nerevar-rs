//! Native HTTPS for the daemon: turning an operator's PEM certificate and key
//! into a rustls server that the shared axum router runs behind.
//!
//! Everything TLS lives in this crate on purpose. `nerevar-core` exposes the
//! router and a [`Transport`] trait; the rustls stack, the PEM parsing, and the
//! X.509 reading are dependencies only the daemon carries, so the desktop app
//! (which never terminates TLS) links none of it.
//!
//! Certificate provisioning is the operator's business — see
//! `docs/headless-hosting.md`. There is no hot reload: a renewed certificate
//! takes effect when the daemon restarts.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nerevar_core::nerevar_server::{state::ServerContext, ServeFuture, Transport};

use axum_server::tls_rustls::RustlsConfig;

/// A certificate this close to expiry gets a warning at startup and in
/// `--check`. Two weeks is comfortably more than certbot's renewal window (it
/// renews at 30 days), so a warning means renewal is actually stuck.
const EXPIRY_WARNING_DAYS: i64 = 14;

/// The operator's `--tls-cert` / `--tls-key` pair, validated as a pair but not
/// yet read off disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsSettings {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

impl TlsSettings {
    /// Both flags or neither. Half a pair is a configuration mistake, not a
    /// reason to silently fall back to plain HTTP on a port the operator
    /// believed was encrypted.
    pub fn from_flags(cert: Option<&Path>, key: Option<&Path>) -> Result<Option<Self>, String> {
        match (cert, key) {
            (None, None) => Ok(None),
            (Some(cert_path), Some(key_path)) => Ok(Some(TlsSettings {
                cert_path: cert_path.to_path_buf(),
                key_path: key_path.to_path_buf(),
            })),
            (Some(_), None) => Err(
                "--tls-cert was given without --tls-key. Native HTTPS needs both: the PEM \
                 certificate chain and its private key. (Environment: NEREVAR_TLS_CERT and \
                 NEREVAR_TLS_KEY.)"
                    .to_string(),
            ),
            (None, Some(_)) => Err(
                "--tls-key was given without --tls-cert. Native HTTPS needs both: the PEM \
                 certificate chain and its private key. (Environment: NEREVAR_TLS_CERT and \
                 NEREVAR_TLS_KEY.)"
                    .to_string(),
            ),
        }
    }

    /// Reads and parses both files and builds the rustls server config. The
    /// key/certificate mismatch check is rustls': `with_single_cert` refuses a
    /// key that does not go with the leaf.
    pub fn load(&self) -> Result<LoadedTls, String> {
        let certs = read_certificate_chain(&self.cert_path)?;
        let key = read_private_key(&self.key_path)?;

        let leaf = CertificateFacts::from_der(certs[0].as_ref());

        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|err| {
                format!(
                    "Certificate {} and key {} were both read but rustls rejected the pair: {err}",
                    self.cert_path.display(),
                    self.key_path.display()
                )
            })?;

        Ok(LoadedTls {
            config: Arc::new(config),
            cert_path: self.cert_path.clone(),
            leaf,
        })
    }
}

fn read_certificate_chain(
    path: &Path,
) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, String> {
    let file = std::fs::File::open(path)
        .map_err(|err| format!("Cannot read TLS certificate {}: {err}", path.display()))?;
    let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(file))
        .collect::<Result<_, _>>()
        .map_err(|err| {
            format!(
                "TLS certificate {} is not a readable PEM chain: {err}",
                path.display()
            )
        })?;
    if certs.is_empty() {
        return Err(format!(
            "TLS certificate {} contains no CERTIFICATE block. Point --tls-cert at the full \
             chain in PEM form (certbot's fullchain.pem).",
            path.display()
        ));
    }
    Ok(certs)
}

fn read_private_key(path: &Path) -> Result<rustls::pki_types::PrivateKeyDer<'static>, String> {
    let file = std::fs::File::open(path)
        .map_err(|err| format!("Cannot read TLS private key {}: {err}", path.display()))?;
    rustls_pemfile::private_key(&mut BufReader::new(file))
        .map_err(|err| {
            format!(
                "TLS private key {} is not a readable PEM key: {err}",
                path.display()
            )
        })?
        .ok_or_else(|| {
            format!(
                "TLS private key {} contains no PRIVATE KEY block. --tls-key takes a PKCS#8 \
                 (\"BEGIN PRIVATE KEY\") or RSA (\"BEGIN RSA PRIVATE KEY\") key in PEM form.",
                path.display()
            )
        })
}

/// What the leaf certificate says about itself. Every field is optional: a
/// certificate rustls accepts but this cannot describe still serves traffic,
/// so a parse failure here costs a log line, never a startup.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CertificateFacts {
    pub subject: Option<String>,
    pub names: Vec<String>,
    /// Human-readable not-after, as printed by the X.509 parser.
    pub not_after: Option<String>,
    /// Whole days until not-after; negative once the certificate has expired.
    pub days_remaining: Option<i64>,
}

impl CertificateFacts {
    fn from_der(der: &[u8]) -> Self {
        let Ok((_, cert)) = x509_parser::parse_x509_certificate(der) else {
            return CertificateFacts::default();
        };

        let names = cert
            .tbs_certificate
            .subject_alternative_name()
            .ok()
            .flatten()
            .map(|san| {
                san.value
                    .general_names
                    .iter()
                    .filter_map(|name| match name {
                        x509_parser::extensions::GeneralName::DNSName(dns) => {
                            Some((*dns).to_string())
                        }
                        x509_parser::extensions::GeneralName::IPAddress(bytes) => format_ip(bytes),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let validity = cert.validity();
        CertificateFacts {
            subject: Some(cert.subject().to_string()),
            names,
            not_after: Some(validity.not_after.to_string()),
            days_remaining: Some(
                validity
                    .time_to_expiration()
                    .map(|d| d.whole_days())
                    .unwrap_or(-1),
            ),
        }
    }

    /// `subject / SAN names / not-after` folded into the one line `--check` and
    /// the startup path both print.
    pub fn describe(&self) -> String {
        match (&self.subject, &self.not_after) {
            (Some(subject), Some(not_after)) => {
                let names = if self.names.is_empty() {
                    "no SAN names".to_string()
                } else {
                    self.names.join(", ")
                };
                format!("{subject} ({names}), not after {not_after}")
            }
            // x509-parser could not read the leaf; rustls still accepted it.
            _ => "certificate and key parse and match (details unreadable)".to_string(),
        }
    }

    /// `Some(days)` when the certificate expires within the warning window,
    /// including when it already has.
    pub fn expiring_soon(&self) -> Option<i64> {
        self.days_remaining
            .filter(|days| *days <= EXPIRY_WARNING_DAYS)
    }
}

fn format_ip(bytes: &[u8]) -> Option<String> {
    match bytes.len() {
        4 => {
            let octets: [u8; 4] = bytes.try_into().ok()?;
            Some(std::net::Ipv4Addr::from(octets).to_string())
        }
        16 => {
            let octets: [u8; 16] = bytes.try_into().ok()?;
            Some(std::net::Ipv6Addr::from(octets).to_string())
        }
        _ => None,
    }
}

/// A parsed, rustls-accepted certificate and key, ready to serve.
#[derive(Debug)]
pub struct LoadedTls {
    pub config: Arc<rustls::ServerConfig>,
    pub cert_path: PathBuf,
    pub leaf: CertificateFacts,
}

impl LoadedTls {
    /// Logs the near-expiry warning, if any. There is no hot reload, so a
    /// renewed certificate needs a restart — which is exactly what this warns
    /// an operator to schedule.
    pub fn warn_if_expiring_soon(&self) {
        if let Some(days) = self.leaf.expiring_soon() {
            if days < 0 {
                log::warn!(
                    "TLS certificate {} has already expired; clients will refuse to connect. \
                     Renew it and restart the daemon.",
                    self.cert_path.display()
                );
            } else {
                log::warn!(
                    "TLS certificate {} expires in {days} day(s). Renewing does not take effect \
                     until the daemon restarts.",
                    self.cert_path.display()
                );
            }
        }
    }

    pub fn into_transport(self) -> TlsTransport {
        TlsTransport {
            config: RustlsConfig::from_config(self.config),
            cert_path: self.cert_path,
        }
    }
}

/// Serves the shared router over rustls. `axum-server` supplies the accept
/// loop; the router, the state, and the port binding are core's, unchanged.
pub struct TlsTransport {
    config: RustlsConfig,
    cert_path: PathBuf,
}

impl Transport for TlsTransport {
    fn serve(&self, listener: tokio::net::TcpListener, ctx: Arc<ServerContext>) -> ServeFuture {
        let app = nerevar_core::nerevar_server::router(ctx);
        let config = self.config.clone();
        let cert_path = self.cert_path.clone();
        Box::pin(async move {
            let addr = listener.local_addr().map_err(|err| err.to_string())?;
            // axum-server takes the std listener back; tokio hands it over
            // still in nonblocking mode, which is what it re-registers.
            let std_listener = listener
                .into_std()
                .map_err(|err| format!("Cannot hand the sync listener to the TLS server: {err}"))?;
            log::info!(
                "NEREVAR SERVER: serving HTTPS on {addr} with certificate {}",
                cert_path.display()
            );
            axum_server::from_tcp_rustls(std_listener, config)
                .map_err(|err| format!("Cannot start the TLS server on {addr}: {err}"))?
                .serve(app.into_make_service())
                .await
                .map_err(|err| err.to_string())
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A temp directory that removes itself, matching `config.rs`'s test
    /// helper — nothing in the workspace pulls in a tempdir crate.
    pub(crate) struct Scratch(pub(crate) PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn scratch(label: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-host-tls-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// Writes a self-signed certificate and key for `127.0.0.1`/`localhost`
    /// into `dir` and returns `(settings, certificate PEM)`.
    pub(crate) fn self_signed_pair(dir: &Path) -> (TlsSettings, String) {
        let generated = rcgen::generate_simple_self_signed(vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
        ])
        .expect("rcgen should produce a self-signed pair");
        let cert_pem = generated.cert.pem();
        let key_pem = generated.signing_key.serialize_pem();

        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        std::fs::write(&cert_path, &cert_pem).unwrap();
        std::fs::write(&key_path, key_pem).unwrap();

        (
            TlsSettings {
                cert_path,
                key_path,
            },
            cert_pem,
        )
    }

    /// Half a pair is refused. Falling back to plain HTTP on a port the
    /// operator believes is encrypted would be the worst possible reading of
    /// a typo.
    #[test]
    fn one_tls_flag_without_the_other_is_an_error() {
        let cert = PathBuf::from("/etc/nerevar/fullchain.pem");
        let key = PathBuf::from("/etc/nerevar/privkey.pem");

        assert_eq!(TlsSettings::from_flags(None, None), Ok(None));

        let both = TlsSettings::from_flags(Some(&cert), Some(&key)).expect("a full pair is fine");
        assert_eq!(
            both,
            Some(TlsSettings {
                cert_path: cert.clone(),
                key_path: key.clone(),
            })
        );

        let cert_only = TlsSettings::from_flags(Some(&cert), None).expect_err("half a pair");
        assert!(cert_only.contains("--tls-key"), "{cert_only}");

        let key_only = TlsSettings::from_flags(None, Some(&key)).expect_err("half a pair");
        assert!(key_only.contains("--tls-cert"), "{key_only}");
    }

    /// A generated PEM pair loads, rustls accepts it as a matching pair, and
    /// the X.509 read reports the names and a sane expiry.
    #[test]
    fn a_generated_certificate_and_key_load_as_a_matching_pair() {
        let dir = scratch("load");
        let (settings, _) = self_signed_pair(&dir.0);

        let loaded = settings.load().expect("the generated pair should load");
        assert_eq!(loaded.cert_path, settings.cert_path);
        assert!(
            loaded.leaf.names.iter().any(|n| n == "localhost"),
            "SAN names were {:?}",
            loaded.leaf.names
        );
        assert!(
            loaded.leaf.names.iter().any(|n| n == "127.0.0.1"),
            "SAN names were {:?}",
            loaded.leaf.names
        );
        assert!(loaded.leaf.not_after.is_some());
        // rcgen's default validity is years out, so nothing should warn.
        assert!(loaded.leaf.expiring_soon().is_none());
        assert!(loaded.leaf.describe().contains("not after"));
    }

    /// A key that belongs to a different certificate is rustls' error to
    /// raise, and the daemon turns it into a config failure rather than
    /// starting.
    #[test]
    fn a_key_that_does_not_match_the_certificate_is_rejected() {
        let dir = scratch("mismatch");
        let (settings, _) = self_signed_pair(&dir.0);
        let other = scratch("mismatch-other");
        let (other_settings, _) = self_signed_pair(&other.0);

        let mixed = TlsSettings {
            cert_path: settings.cert_path.clone(),
            key_path: other_settings.key_path.clone(),
        };
        let err = mixed.load().expect_err("a mismatched pair must not load");
        assert!(err.contains("rustls rejected the pair"), "{err}");
    }

    /// A missing file is a plain config error, named so an operator can fix
    /// it from the journal line alone.
    #[test]
    fn a_missing_certificate_file_is_a_config_error() {
        let dir = scratch("missing");
        let settings = TlsSettings {
            cert_path: dir.0.join("nope.pem"),
            key_path: dir.0.join("nope.key"),
        };
        let err = settings.load().expect_err("nothing to load");
        assert!(err.contains("Cannot read TLS certificate"), "{err}");
    }

    /// The whole serve path, end to end: core binds the port and builds the
    /// router, this crate's transport wraps it in rustls, and a client that
    /// trusts the generated certificate gets `/health`. The plain-HTTP half of
    /// the assertion is the ruling that matters — with TLS on there is no
    /// cleartext fallback on that port.
    #[tokio::test]
    async fn the_tls_transport_serves_health_over_https_and_nothing_over_http() {
        use nerevar_core::nerevar_server::state::ServerContext;
        use nerevar_core::sync_host::{new_shared_hosting_manifest_cache, new_shared_sync_host};

        let dir = scratch("serve");
        let (settings, cert_pem) = self_signed_pair(&dir.0);
        let transport = settings.load().expect("load").into_transport();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral loopback port");
        let port = listener.local_addr().unwrap().port();

        let ctx =
            ServerContext::builder(new_shared_sync_host(), new_shared_hosting_manifest_cache())
                .shared();
        let server = tokio::spawn(transport.serve(listener, ctx));

        let client = reqwest::Client::builder()
            .add_root_certificate(
                reqwest::Certificate::from_pem(cert_pem.as_bytes()).expect("test certificate"),
            )
            .build()
            .expect("client");

        // The acceptor needs a moment to be listening; retry rather than sleep
        // a fixed amount.
        let mut body = None;
        for _ in 0..50 {
            match client
                .get(format!("https://127.0.0.1:{port}/health"))
                .send()
                .await
            {
                Ok(response) => {
                    assert!(response.status().is_success(), "{:?}", response.status());
                    body = Some(response.text().await.expect("body"));
                    break;
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        assert_eq!(body.as_deref(), Some("ok"), "HTTPS /health should answer");

        let cleartext = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await;
        assert!(
            cleartext.is_err(),
            "a plain HTTP request to the TLS port must not succeed: {cleartext:?}"
        );

        server.abort();
    }
}
