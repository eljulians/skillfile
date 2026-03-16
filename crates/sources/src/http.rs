use std::process::Command;
use std::sync::OnceLock;

use skillfile_core::error::SkillfileError;

// ---------------------------------------------------------------------------
// GitHub token discovery (cached for process lifetime)
// ---------------------------------------------------------------------------

static TOKEN_CACHE: OnceLock<Option<String>> = OnceLock::new();

/// Discover a GitHub token from environment or `gh` CLI. Cached after first call.
#[must_use]
pub fn github_token() -> Option<&'static str> {
    TOKEN_CACHE.get_or_init(discover_github_token).as_deref()
}

fn env_token(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|t| !t.is_empty())
}

fn discover_github_token() -> Option<String> {
    if let Some(token) = env_token("GITHUB_TOKEN") {
        return Some(token);
    }
    if let Some(token) = env_token("GH_TOKEN") {
        return Some(token);
    }
    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

// ---------------------------------------------------------------------------
// HttpClient trait — abstraction over HTTP GET for testability
// ---------------------------------------------------------------------------

/// Contract for HTTP GET requests used by the fetcher/resolver layer.
///
/// Implementations are responsible for:
/// - Setting standard headers (User-Agent, Authorization)
/// - Connection pooling / agent reuse
/// - Error mapping to [`SkillfileError`]
///
/// The trait has three methods covering the HTTP patterns in this codebase:
/// - `get_bytes`: raw file downloads (content from `raw.githubusercontent.com`)
/// - `get_json`: GitHub API calls that may return 4xx gracefully
/// - `post_json`: POST with JSON body (used by some registry APIs)
pub trait HttpClient: Send + Sync {
    /// GET a URL and return the response body as raw bytes.
    ///
    /// Returns `Err(SkillfileError::Network)` on HTTP errors (including 404).
    fn get_bytes(&self, url: &str) -> Result<Vec<u8>, SkillfileError>;

    /// GET a URL with `Accept: application/vnd.github.v3+json` header.
    ///
    /// Returns `Ok(None)` on 4xx client errors (used for tentative lookups
    /// like SHA resolution where a missing ref is not fatal).
    /// Returns `Err` on network/server errors.
    fn get_json(&self, url: &str) -> Result<Option<String>, SkillfileError>;

    /// POST a JSON body to a URL and return the response body as bytes.
    ///
    /// Returns `Err(SkillfileError::Network)` on HTTP or network errors.
    fn post_json(&self, url: &str, body: &str) -> Result<Vec<u8>, SkillfileError>;

    /// POST with a custom `Authorization: Bearer` header (for non-GitHub APIs).
    ///
    /// Default: ignores the token and delegates to [`post_json`](Self::post_json).
    /// Test mocks use this default; [`UreqClient`] overrides to send the header.
    ///
    /// # Note
    /// The extra `token` parameter is required by non-GitHub registry APIs (e.g.
    /// skillhub.club). `#[allow]` is intentional: this is a public trait method
    /// whose signature cannot change without a breaking API change.
    #[allow(clippy::too_many_arguments)]
    fn post_json_with_bearer(
        &self,
        url: &str,
        body: &str,
        token: &str,
    ) -> Result<Vec<u8>, SkillfileError> {
        let _ = token;
        self.post_json(url, body)
    }
}

// ---------------------------------------------------------------------------
// UreqClient — the production implementation backed by ureq
// ---------------------------------------------------------------------------

fn read_response_text(body: &mut ureq::Body, url: &str) -> Result<String, SkillfileError> {
    body.read_to_string()
        .map_err(|e| SkillfileError::Network(format!("failed to read response from {url}: {e}")))
}

/// Production HTTP client backed by `ureq::Agent`.
///
/// Automatically attaches `User-Agent` and GitHub `Authorization` headers
/// to every request. The GitHub token is discovered once from environment
/// variables or the `gh` CLI and cached for the process lifetime.
pub struct UreqClient {
    agent: ureq::Agent,
}

impl UreqClient {
    pub fn new() -> Self {
        let config = ureq::config::Config::builder()
            // Preserve Authorization header on same-host HTTPS redirects.
            // GitHub returns 301 for renamed repos (api.github.com -> api.github.com);
            // the default (Never) strips auth, causing 401 on the redirect target.
            .redirect_auth_headers(ureq::config::RedirectAuthHeaders::SameHost)
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// Build a GET request with standard headers.
    fn build_get(&self, url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let mut req = self.agent.get(url).header("User-Agent", "skillfile/1.0");
        if let Some(token) = github_token() {
            req = req.header("Authorization", &format!("Bearer {token}"));
        }
        req
    }

    /// Build a POST request with standard headers.
    fn build_post(&self, url: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let mut req = self.agent.post(url).header("User-Agent", "skillfile/1.0");
        if let Some(token) = github_token() {
            req = req.header("Authorization", &format!("Bearer {token}"));
        }
        req
    }
}

impl Default for UreqClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient for UreqClient {
    fn get_bytes(&self, url: &str) -> Result<Vec<u8>, SkillfileError> {
        let mut response = self.build_get(url).call().map_err(|e| match &e {
            ureq::Error::StatusCode(404) => SkillfileError::Network(format!(
                "HTTP 404: {url} not found — check that the path exists in the upstream repo"
            )),
            ureq::Error::StatusCode(code) => {
                SkillfileError::Network(format!("HTTP {code} fetching {url}"))
            }
            _ => SkillfileError::Network(format!("{e} fetching {url}")),
        })?;
        response.body_mut().read_to_vec().map_err(|e| {
            SkillfileError::Network(format!("failed to read response from {url}: {e}"))
        })
    }

    fn get_json(&self, url: &str) -> Result<Option<String>, SkillfileError> {
        let result = self
            .build_get(url)
            .header("Accept", "application/vnd.github.v3+json")
            .call();

        match result {
            Ok(mut response) => read_response_text(response.body_mut(), url).map(Some),
            Err(ureq::Error::StatusCode(code)) if (400..500).contains(&code) => Ok(None),
            Err(e) => Err(SkillfileError::Network(format!("{e} fetching {url}"))),
        }
    }

    fn post_json(&self, url: &str, body: &str) -> Result<Vec<u8>, SkillfileError> {
        let mut response = self
            .build_post(url)
            .header("Content-Type", "application/json")
            .send(body.as_bytes())
            .map_err(|e| match &e {
                ureq::Error::StatusCode(code) => {
                    SkillfileError::Network(format!("HTTP {code} posting to {url}"))
                }
                _ => SkillfileError::Network(format!("{e} posting to {url}")),
            })?;
        response.body_mut().read_to_vec().map_err(|e| {
            SkillfileError::Network(format!("failed to read response from {url}: {e}"))
        })
    }

    fn post_json_with_bearer(
        &self,
        url: &str,
        body: &str,
        token: &str,
    ) -> Result<Vec<u8>, SkillfileError> {
        let mut response = self
            .agent
            .post(url)
            .header("User-Agent", "skillfile/1.0")
            .header("Content-Type", "application/json")
            .header("Authorization", &format!("Bearer {token}"))
            .send(body.as_bytes())
            .map_err(|e| match &e {
                ureq::Error::StatusCode(code) => {
                    SkillfileError::Network(format!("HTTP {code} posting to {url}"))
                }
                _ => SkillfileError::Network(format!("{e} posting to {url}")),
            })?;
        response.body_mut().read_to_vec().map_err(|e| {
            SkillfileError::Network(format!("failed to read response from {url}: {e}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ureq_client_default_creates_successfully() {
        let _client = UreqClient::default();
    }
}
