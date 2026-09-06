//! The HTTP side of `nerevar-cli admin`: one bearer-authenticated client
//! against a host's `/admin` routes.
//!
//! The address is resolved by `nerevar_core::sync_client::base_url`, the one
//! rule the desktop app's connection form and the sync client already use, so
//! a bare `myhost` plus `--port`, `https://mw.example.org` behind a reverse
//! proxy, and `https://example.org/nerevar` under a path prefix all mean here
//! exactly what they mean there.
//!
//! Two failures are told apart on purpose. A request that never reached a
//! server names the base URL it tried, because a wrong host or port is the
//! common mistake and the message is the only clue. A request the host
//! answered with a non-2xx carries the host's own `error` string, because the
//! host knows why far better than the CLI does.
//!
//! The token goes in a header and nowhere else: not in a URL, not in a log
//! line, not in an error.

use std::path::Path;

use nerevar_core::sync_client::base_url;

/// A host's `/admin` routes, and the token to reach them with.
pub struct AdminClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

/// One answer from the host: what it said, and how it said it.
#[derive(Debug)]
pub struct Response {
    pub status: reqwest::StatusCode,
    /// The body verbatim, which is what `--json` prints.
    pub body: String,
}

impl Response {
    /// The body as JSON, or an error naming what came back instead.
    pub fn json(&self) -> Result<serde_json::Value, String> {
        if self.body.trim().is_empty() {
            return Ok(serde_json::Value::Null);
        }
        serde_json::from_str(&self.body)
            .map_err(|error| format!("The host's reply was not JSON ({error}): {}", self.body))
    }

    /// Passes a 2xx through; turns anything else into the host's own `error`
    /// message, which is what every `/admin` route puts in a failure body.
    pub fn into_success(self) -> Result<Self, String> {
        if self.status.is_success() {
            return Ok(self);
        }
        let reported = self
            .json()
            .ok()
            .and_then(|body| body.get("error")?.as_str().map(str::to_string));
        Err(match reported {
            Some(message) => message,
            None if self.body.trim().is_empty() => {
                format!("The host answered {} with an empty body.", self.status)
            }
            None => format!("The host answered {}: {}", self.status, self.body.trim()),
        })
    }
}

impl AdminClient {
    /// Resolves `host` (+ `port`, for a bare hostname) into the base URL every
    /// request is appended to.
    pub fn new(host: &str, port: u16, token: String) -> Result<Self, String> {
        Ok(Self {
            base: base_url(host, port)?,
            token,
            http: reqwest::Client::builder()
                .build()
                .map_err(|error| format!("Failed to build the HTTP client: {error}"))?,
        })
    }

    /// The address requests actually go to, for the message a connection
    /// failure prints.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub async fn get(&self, path: &str) -> Result<Response, String> {
        self.send(self.http.get(self.url(path))).await
    }

    pub async fn delete(&self, path: &str) -> Result<Response, String> {
        self.send(self.http.delete(self.url(path))).await
    }

    /// A POST with no body, which is what `apply`, `discard` and `restart`
    /// are.
    pub async fn post(&self, path: &str) -> Result<Response, String> {
        self.send(self.http.post(self.url(path))).await
    }

    pub async fn post_json(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<Response, String> {
        self.send(self.http.post(self.url(path)).json(body)).await
    }

    /// `PUT` with a file as the body, streamed off disk: a mod archive can be
    /// gigabytes, and nothing here should hold one in memory.
    pub async fn put_file(&self, path: &str, file: &Path) -> Result<Response, String> {
        let handle = tokio::fs::File::open(file)
            .await
            .map_err(|error| format!("Failed to open {}: {error}", file.display()))?;
        let length = handle
            .metadata()
            .await
            .map_err(|error| format!("Failed to read {}: {error}", file.display()))?
            .len();

        self.send(
            self.http
                .put(self.url(path))
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .header(reqwest::header::CONTENT_LENGTH, length)
                .body(reqwest::Body::from(handle)),
        )
        .await
    }

    /// `base` plus a route, with each path segment percent-encoded — a package
    /// name is a directory name and routinely holds spaces.
    fn url(&self, path: &str) -> String {
        let encoded: Vec<String> = path
            .trim_start_matches('/')
            .split('/')
            .map(urlencode)
            .collect();
        format!("{}/{}", self.base, encoded.join("/"))
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Response, String> {
        let response = request
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|error| {
                format!("Could not reach the Nerevar host at {}: {error}", self.base)
            })?;
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            format!(
                "Failed to read the host's reply from {}: {error}",
                self.base
            )
        })?;
        Ok(Response { status, body })
    }
}

/// Percent-encodes one path segment. `urlencoding::encode` escapes a space as
/// `%20` (not `+`), which is what a path needs; it is already a `nerevar-core`
/// dependency, but encoding one segment is three lines and not worth another
/// entry in this crate's manifest.
fn urlencode(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(host: &str, port: u16) -> AdminClient {
        AdminClient::new(host, port, "t".to_string()).unwrap()
    }

    #[test]
    fn a_bare_host_takes_the_port_and_a_url_host_does_not() {
        assert_eq!(
            client("myhost", 25567).url("/admin/status"),
            "http://myhost:25567/admin/status"
        );
        assert_eq!(
            client("https://mw.example.org", 25567).url("/admin/status"),
            "https://mw.example.org/admin/status"
        );
        assert_eq!(
            client("https://example.org/nerevar", 25567).url("/admin/status"),
            "https://example.org/nerevar/admin/status"
        );
    }

    #[test]
    fn a_package_name_is_percent_encoded_into_the_path() {
        assert_eq!(
            client("myhost", 1).url("/admin/packages/Better Bodies"),
            "http://myhost:1/admin/packages/Better%20Bodies"
        );
        assert_eq!(
            client("myhost", 1).url("/admin/packages/Tamriel_Rebuilt-1.0"),
            "http://myhost:1/admin/packages/Tamriel_Rebuilt-1.0"
        );
    }

    #[test]
    fn a_bad_host_address_fails_before_any_request() {
        assert!(AdminClient::new("ftp://mw.example.org", 1, "t".into()).is_err());
        assert!(AdminClient::new("", 1, "t".into()).is_err());
    }

    #[test]
    fn a_non_2xx_reports_the_hosts_own_error_string() {
        let response = Response {
            status: reqwest::StatusCode::CONFLICT,
            body: r#"{"error":"Another admin change is in progress"}"#.to_string(),
        };
        assert_eq!(
            response.into_success().unwrap_err(),
            "Another admin change is in progress"
        );
    }

    #[test]
    fn a_non_2xx_with_no_error_field_still_says_what_happened() {
        let response = Response {
            status: reqwest::StatusCode::BAD_GATEWAY,
            body: "<html>proxy</html>".to_string(),
        };
        let error = response.into_success().unwrap_err();
        assert!(error.contains("502"), "{error}");
        assert!(error.contains("proxy"), "{error}");
    }
}
