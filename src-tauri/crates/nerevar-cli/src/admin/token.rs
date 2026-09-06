//! Where the co-admin token comes from, and in what order.
//!
//! Three sources, most explicit first: `--token`, then `--token-file`, then
//! the `NEREVAR_ADMIN_TOKEN` environment variable. A source that is present
//! but blank counts as absent, so an unset-but-exported `NEREVAR_ADMIN_TOKEN=`
//! falls through to the error rather than sending an empty bearer header the
//! host would only reject.
//!
//! The token is never printed, and never appears in an error message: the
//! failures here name the *source*, not its contents.

use std::path::{Path, PathBuf};

/// The environment variable the host's docs tell a co-admin to export.
pub const TOKEN_ENV_VAR: &str = "NEREVAR_ADMIN_TOKEN";

/// Which of the three sources a run will read its token from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenSource {
    /// `--token <token>`, carrying the token itself.
    Argument(String),
    /// `--token-file <path>`.
    File(PathBuf),
    /// `NEREVAR_ADMIN_TOKEN`, carrying its value.
    Environment(String),
}

/// Picks the source, without touching the filesystem. Pure, so the precedence
/// is testable on its own.
pub fn choose_token_source(
    argument: Option<&str>,
    file: Option<&Path>,
    environment: Option<&str>,
) -> Result<TokenSource, String> {
    if let Some(token) = argument.filter(|token| !token.trim().is_empty()) {
        return Ok(TokenSource::Argument(token.trim().to_string()));
    }
    if let Some(path) = file {
        return Ok(TokenSource::File(path.to_path_buf()));
    }
    if let Some(token) = environment.filter(|token| !token.trim().is_empty()) {
        return Ok(TokenSource::Environment(token.trim().to_string()));
    }
    Err(format!(
        "No admin token. Pass --token, or --token-file <path>, or export {TOKEN_ENV_VAR}."
    ))
}

/// The token itself: [`choose_token_source`] plus the one read it may need.
///
/// A token file is read whole and trimmed, so the file
/// `nerevar-host admin add ada > ada.token` writes — token plus a newline —
/// works unedited.
pub fn resolve_token(
    argument: Option<&str>,
    file: Option<&Path>,
    environment: Option<&str>,
) -> Result<String, String> {
    match choose_token_source(argument, file, environment)? {
        TokenSource::Argument(token) | TokenSource::Environment(token) => Ok(token),
        TokenSource::File(path) => {
            let contents = std::fs::read_to_string(&path)
                .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
            let token = contents.trim();
            if token.is_empty() {
                return Err(format!("{} is empty — no token in it.", path.display()));
            }
            Ok(token.to_string())
        }
    }
}

/// The environment source as the CLI reads it, so a caller never has to
/// remember the variable's name.
pub fn token_from_environment() -> Option<String> {
    std::env::var(TOKEN_ENV_VAR).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_argument_wins_over_the_file_and_the_environment() {
        let source = choose_token_source(
            Some("from-argument"),
            Some(Path::new("/tmp/ada.token")),
            Some("from-environment"),
        )
        .unwrap();
        assert_eq!(source, TokenSource::Argument("from-argument".to_string()));
    }

    #[test]
    fn the_file_wins_over_the_environment() {
        let source =
            choose_token_source(None, Some(Path::new("/tmp/ada.token")), Some("from-env")).unwrap();
        assert_eq!(source, TokenSource::File(PathBuf::from("/tmp/ada.token")));
    }

    #[test]
    fn the_environment_is_the_last_resort() {
        let source = choose_token_source(None, None, Some("from-environment")).unwrap();
        assert_eq!(
            source,
            TokenSource::Environment("from-environment".to_string())
        );
    }

    #[test]
    fn a_blank_argument_or_variable_counts_as_absent() {
        assert_eq!(
            choose_token_source(Some("   "), None, Some("from-environment")).unwrap(),
            TokenSource::Environment("from-environment".to_string())
        );
        assert!(choose_token_source(Some(""), None, Some("  ")).is_err());
        assert!(choose_token_source(None, None, None).is_err());
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_off_every_source() {
        assert_eq!(
            choose_token_source(Some("  abc\n"), None, None).unwrap(),
            TokenSource::Argument("abc".to_string())
        );
        assert_eq!(
            choose_token_source(None, None, Some("\tabc ")).unwrap(),
            TokenSource::Environment("abc".to_string())
        );
    }

    #[test]
    fn no_source_at_all_names_all_three_and_carries_no_secret() {
        let error = choose_token_source(None, None, None).unwrap_err();
        assert!(error.contains("--token"), "unexpected message: {error}");
        assert!(
            error.contains("--token-file"),
            "unexpected message: {error}"
        );
        assert!(error.contains(TOKEN_ENV_VAR), "unexpected message: {error}");
    }

    #[test]
    fn a_token_file_is_read_whole_and_trimmed() {
        let path = std::env::temp_dir().join(format!(
            "nerevar-cli-token-{}-{}.token",
            std::process::id(),
            line!()
        ));
        std::fs::write(&path, "  0123456789abcdef\n").unwrap();
        assert_eq!(
            resolve_token(None, Some(&path), Some("from-environment")).unwrap(),
            "0123456789abcdef"
        );

        std::fs::write(&path, "\n\n").unwrap();
        let error = resolve_token(None, Some(&path), None).unwrap_err();
        assert!(error.contains("empty"), "unexpected message: {error}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_token_file_is_an_error_rather_than_a_fallthrough() {
        let missing = std::env::temp_dir().join("nerevar-cli-no-such-file.token");
        let _ = std::fs::remove_file(&missing);
        assert!(resolve_token(None, Some(&missing), Some("from-environment")).is_err());
    }
}
