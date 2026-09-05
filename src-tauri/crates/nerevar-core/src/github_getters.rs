//! Reading the GitHub releases API.
//!
//! Fetching only: what to do with a release — which asset is this platform's
//! TES3MP runtime, downloading and extracting it — lives in `runtime`.

use crate::data::GithubReleaseResponse;
use crate::runtime::{normalize_repo, DEFAULT_TES3MP_REPO};
use log::{error, info};
use reqwest::Client;

/// Lists the releases of a TES3MP repository — `repo` when the caller named
/// one, [`DEFAULT_TES3MP_REPO`] otherwise.
///
/// The repository is a caller-supplied string (the runtime picker's repo
/// field, an instance's `RuntimeSource`), so it is normalised and validated
/// here rather than pasted into a URL: `owner/name` is the only shape that
/// reaches the API.
pub async fn get_all_releases(repo: Option<&str>) -> Result<Vec<GithubReleaseResponse>, String> {
    let repo = match repo.map(str::trim).filter(|repo| !repo.is_empty()) {
        Some(repo) => normalize_repo(repo)?,
        None => DEFAULT_TES3MP_REPO.to_string(),
    };
    fetch_github_releases(&repo, "Tes3MP").await
}

// Takes `repo` as a parameter (rather than reaching for the app crate's
// `app_update::NEREVAR_REPO` constant, as before the leaf-layer split) so
// this module has no dependency back on app-side code; nerevar-core can't
// depend on the `nerevar` crate that depends on it.
pub async fn get_nerevar_releases(repo: &str) -> Result<Vec<GithubReleaseResponse>, String> {
    fetch_github_releases(repo, "Nerevar").await
}

pub(crate) async fn fetch_github_releases(
    repo: &str,
    label: &str,
) -> Result<Vec<GithubReleaseResponse>, String> {
    info!("Attempting to fetch all {label} releases from GitHub...");
    let client = Client::new();
    let url = format!("https://api.github.com/repos/{repo}/releases");
    let user_agent = format!("Nerevar-{}", env!("CARGO_PKG_VERSION"));

    let response = match client
        .get(url)
        .header("User-Agent", user_agent)
        .send()
        .await
    {
        Ok(res) => res,
        Err(e) => {
            error!("fetch_github_releases request failed: {e}");
            return Err(e.to_string());
        }
    };

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        error!("fetch_github_releases HTTP 404 for {repo}");
        // Nerevar sends no GitHub credentials, so a private repository is
        // indistinguishable from one that does not exist — say both.
        return Err(format!(
            "GitHub repository {repo} not found or private. Nerevar does not sign in to GitHub, so private repositories cannot be used."
        ));
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        error!("fetch_github_releases HTTP {status}: {body}");
        return Err(format!("GitHub API returned HTTP {status}: {body}"));
    }

    match response.json::<Vec<GithubReleaseResponse>>().await {
        Ok(releases) => {
            info!("Successfully fetched all {label} releases from GitHub");
            Ok(releases)
        }
        Err(e) => {
            error!("fetch_github_releases JSON parsing failed: {e}");
            Err(e.to_string())
        }
    }
}
