//! Resolving the address a synced instance was configured with.
//!
//! A synced instance stores one free-text `remote_host` plus a `remote_sync_port`.
//! Historically that was always a bare hostname or IP and every request was built
//! as `http://{host}:{port}`. A host behind an HTTPS reverse proxy cannot be
//! expressed that way, so the field now also accepts a full `http://` or
//! `https://` URL — optionally with an explicit port and a path prefix, for a
//! proxy that mounts the host under a subpath.
//!
//! [`base_url`] is the single place those rules live: every sync request in
//! `fetch.rs` and `download.rs` is built on what it returns, and the frontend's
//! form validation mirrors it. A `https://` address reaches either a reverse
//! proxy or a `nerevar-host` serving TLS natively (`--tls-cert`/`--tls-key`);
//! the client cannot tell the two apart and does not need to.

/// True when `host` is a full URL rather than a bare hostname or IP — i.e. when
/// the configured sync port plays no part in the address.
pub fn is_url_host(host: &str) -> bool {
    host.trim().contains("://")
}

/// Resolves a configured host address into the base URL every sync route is
/// appended to. The returned base never ends in `/`.
///
/// - A bare hostname, IP, or bracketed IPv6 literal becomes
///   `http://{host}:{port}` — the original behaviour.
/// - An `http://` or `https://` URL is used as given (trailing slashes trimmed)
///   and `port` is ignored: the URL's own port, explicit or implied by the
///   scheme, is the one that counts. A path prefix is preserved so routes append
///   after it.
/// - Anything else — another scheme, embedded whitespace, an empty string — is
///   an error describing what to enter instead.
pub fn base_url(host: &str, port: u16) -> Result<String, String> {
    let host = host.trim();
    if host.is_empty() {
        return Err(
            "Host address is empty. Enter a hostname, an IP, or a full http(s):// URL.".to_string(),
        );
    }
    if host.chars().any(char::is_whitespace) {
        return Err(format!(
            "Host address \"{host}\" contains whitespace. Enter a hostname, an IP, or a full http(s):// URL."
        ));
    }

    // The scheme is read off the untrimmed address: trimming first would turn
    // "https://" into "https:", which no longer looks like a URL at all.
    if let Some((scheme, rest)) = host.split_once("://") {
        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return Err(format!(
                "Host address \"{host}\" uses the unsupported scheme \"{scheme}\". Use http:// or https://, or just the hostname."
            ));
        }
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() {
            return Err(format!(
                "Host address \"{host}\" has no host after the scheme."
            ));
        }
        return Ok(format!("{scheme}://{rest}"));
    }

    let trimmed = host.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(
            "Host address is empty. Enter a hostname, an IP, or a full http(s):// URL.".to_string(),
        );
    }
    if trimmed.contains('/') {
        return Err(format!(
            "Host address \"{host}\" contains a path. Enter just the hostname or IP, or a full http(s):// URL."
        ));
    }
    Ok(format!("http://{trimmed}:{port}"))
}

/// The address to hand to the TES3MP client for the *game* connection.
///
/// TES3MP talks UDP to the game port and cannot go through an HTTPS proxy, so a
/// URL host contributes only its hostname here — the proxy's name, which is the
/// best available guess at where the game server listens. A bare host is passed
/// through unchanged. Errors for exactly the inputs [`base_url`] rejects.
pub fn game_host(host: &str) -> Result<String, String> {
    let base = base_url(host, 0)?;
    let trimmed = host.trim().trim_end_matches('/');
    if !is_url_host(host) {
        return Ok(trimmed.to_string());
    }

    let (_, rest) = base
        .split_once("://")
        .expect("base_url always returns a scheme-qualified URL");
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, after)| after);
    let host_only = if let Some(bracket_end) = authority.rfind(']') {
        &authority[..=bracket_end]
    } else if let Some(colon) = authority.rfind(':') {
        &authority[..colon]
    } else {
        authority
    };

    if host_only.is_empty() {
        return Err(format!(
            "Host address \"{host}\" has no host after the scheme."
        ));
    }
    Ok(host_only.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- bare hosts: unchanged behaviour, the port is appended -------------------

    #[test]
    fn bare_hostname_gets_scheme_and_port() {
        assert_eq!(base_url("myhost", 25567).unwrap(), "http://myhost:25567");
    }

    #[test]
    fn bare_ipv4_gets_scheme_and_port() {
        assert_eq!(
            base_url("192.168.1.5", 25567).unwrap(),
            "http://192.168.1.5:25567"
        );
    }

    #[test]
    fn bracketed_ipv6_literal_gets_scheme_and_port() {
        assert_eq!(base_url("[::1]", 25567).unwrap(), "http://[::1]:25567");
        assert_eq!(
            base_url("[2001:db8::1]", 8080).unwrap(),
            "http://[2001:db8::1]:8080"
        );
    }

    #[test]
    fn bare_host_is_trimmed_of_surrounding_space_and_trailing_slashes() {
        assert_eq!(
            base_url("  myhost  ", 25567).unwrap(),
            "http://myhost:25567"
        );
        assert_eq!(base_url("myhost/", 25567).unwrap(), "http://myhost:25567");
        assert_eq!(base_url("myhost///", 25567).unwrap(), "http://myhost:25567");
    }

    // ---- URL hosts: used as given, port ignored ---------------------------------

    #[test]
    fn https_url_is_used_as_given_and_port_is_ignored() {
        assert_eq!(
            base_url("https://mw.example.org", 25567).unwrap(),
            "https://mw.example.org"
        );
        assert_eq!(
            base_url("https://mw.example.org", 1).unwrap(),
            "https://mw.example.org"
        );
    }

    #[test]
    fn http_url_is_used_as_given() {
        assert_eq!(
            base_url("http://127.0.0.1:25567", 9999).unwrap(),
            "http://127.0.0.1:25567"
        );
    }

    #[test]
    fn explicit_port_on_a_url_host_survives() {
        assert_eq!(
            base_url("https://mw.example.org:8443", 25567).unwrap(),
            "https://mw.example.org:8443"
        );
    }

    #[test]
    fn url_host_keeps_a_path_prefix() {
        assert_eq!(
            base_url("https://example.org/nerevar", 25567).unwrap(),
            "https://example.org/nerevar"
        );
        assert_eq!(
            base_url("https://example.org/nerevar/", 25567).unwrap(),
            "https://example.org/nerevar"
        );
        assert_eq!(
            base_url("https://example.org:8443/nerevar/sync", 25567).unwrap(),
            "https://example.org:8443/nerevar/sync"
        );
    }

    #[test]
    fn url_host_with_ipv6_literal_survives() {
        assert_eq!(
            base_url("http://[::1]:25567", 1).unwrap(),
            "http://[::1]:25567"
        );
    }

    #[test]
    fn url_scheme_matching_is_case_insensitive() {
        assert_eq!(
            base_url("HTTPS://mw.example.org", 25567).unwrap(),
            "HTTPS://mw.example.org"
        );
    }

    #[test]
    fn routes_append_cleanly_to_every_base() {
        for (host, port, expected) in [
            ("myhost", 25567u16, "http://myhost:25567/manifest"),
            (
                "https://mw.example.org",
                25567,
                "https://mw.example.org/manifest",
            ),
            (
                "https://example.org/nerevar/",
                25567,
                "https://example.org/nerevar/manifest",
            ),
        ] {
            let base = base_url(host, port).unwrap();
            assert_eq!(format!("{base}/manifest"), expected);
        }
    }

    // ---- rejections --------------------------------------------------------------

    #[test]
    fn empty_host_is_rejected() {
        assert!(base_url("", 25567).is_err());
        assert!(base_url("   ", 25567).is_err());
        assert!(base_url("///", 25567).is_err());
    }

    #[test]
    fn whitespace_inside_the_host_is_rejected() {
        let err = base_url("my host", 25567).unwrap_err();
        assert!(err.contains("whitespace"), "unexpected message: {err}");
        assert!(base_url("https://mw.example.org/a b", 25567).is_err());
        assert!(base_url("my\thost", 25567).is_err());
    }

    #[test]
    fn an_unsupported_scheme_is_rejected() {
        for host in [
            "ftp://mw.example.org",
            "ws://mw.example.org",
            "file:///tmp/x",
        ] {
            let err = base_url(host, 25567).unwrap_err();
            assert!(
                err.contains("unsupported scheme"),
                "unexpected message for {host}: {err}"
            );
        }
    }

    #[test]
    fn a_scheme_with_no_host_is_rejected() {
        assert!(base_url("https://", 25567).is_err());
        assert!(base_url("http://", 25567).is_err());
    }

    #[test]
    fn a_bare_host_with_a_path_is_rejected() {
        let err = base_url("myhost/nerevar", 25567).unwrap_err();
        assert!(err.contains("path"), "unexpected message: {err}");
    }

    // ---- is_url_host ---------------------------------------------------------------

    #[test]
    fn is_url_host_distinguishes_the_two_forms() {
        assert!(is_url_host("https://mw.example.org"));
        assert!(is_url_host("  http://127.0.0.1:25567/  "));
        assert!(!is_url_host("myhost"));
        assert!(!is_url_host("[::1]"));
        assert!(!is_url_host(""));
    }

    // ---- game_host -----------------------------------------------------------------

    #[test]
    fn game_host_passes_bare_hosts_through() {
        assert_eq!(game_host("myhost").unwrap(), "myhost");
        assert_eq!(game_host(" 192.168.1.5/ ").unwrap(), "192.168.1.5");
        assert_eq!(game_host("[::1]").unwrap(), "[::1]");
    }

    #[test]
    fn game_host_strips_scheme_port_and_path_from_a_url() {
        assert_eq!(
            game_host("https://mw.example.org").unwrap(),
            "mw.example.org"
        );
        assert_eq!(
            game_host("https://mw.example.org:8443/nerevar").unwrap(),
            "mw.example.org"
        );
        assert_eq!(game_host("http://[::1]:25567").unwrap(), "[::1]");
        assert_eq!(game_host("http://127.0.0.1:25567").unwrap(), "127.0.0.1");
    }

    #[test]
    fn game_host_rejects_what_base_url_rejects() {
        assert!(game_host("").is_err());
        assert!(game_host("ftp://mw.example.org").is_err());
        assert!(game_host("my host").is_err());
    }
}
