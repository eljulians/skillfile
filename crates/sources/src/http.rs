use std::process::Command;
use std::sync::OnceLock;

use skillfile_core::error::SkillfileError;

// ---------------------------------------------------------------------------
// GitHub token discovery (cached for process lifetime)
// ---------------------------------------------------------------------------

static TOKEN_CACHE: OnceLock<Option<String>> = OnceLock::new();

/// Token injected from the CLI config file before any command runs.
///
/// The CLI crate reads the config file and calls [`set_config_token`] once at
/// startup. This keeps the `sources` crate free of any dependency on `cli`.
static CONFIG_TOKEN: OnceLock<Option<String>> = OnceLock::new();

/// Inject a GitHub token read from the user config file.
///
/// Must be called before the first use of [`github_token`]. Subsequent calls
/// are ignored (the `OnceLock` is already set).
pub fn set_config_token(token: Option<String>) {
    let _ = CONFIG_TOKEN.set(token);
}

/// Opaque GitHub token handle.
///
/// The raw token string is **not publicly accessible**. The only way to
/// extract it is [`GithubToken::for_url`], which gates on
/// `is_github_url` — making it structurally impossible to leak the
/// token to non-GitHub domains.
pub struct GithubToken(Option<&'static str>);

impl GithubToken {
    /// Extract the token string only for GitHub domains.
    ///
    /// Returns `None` when the URL is not a GitHub domain or when no
    /// token is available. This is the **only** way to obtain the raw
    /// token value.
    #[must_use]
    pub fn for_url(&self, url: &str) -> Option<&'static str> {
        is_github_url(url).then_some(self.0).flatten()
    }
}

/// Discover a GitHub token from environment or `gh` CLI. Cached after first call.
///
/// Returns an opaque [`GithubToken`] — the raw value can only be
/// extracted via [`GithubToken::for_url`] for GitHub domains.
#[must_use]
pub fn github_token() -> GithubToken {
    GithubToken(TOKEN_CACHE.get_or_init(discover_github_token).as_deref())
}

fn env_token(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|t| !t.is_empty())
}

fn gh_cli_token() -> Option<String> {
    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!token.is_empty()).then_some(token)
}

fn discover_github_token() -> Option<String> {
    if let Some(token) = env_token("GITHUB_TOKEN") {
        return Some(token);
    }
    if let Some(token) = env_token("GH_TOKEN") {
        return Some(token);
    }
    // Config-file token injected by the CLI crate before commands run.
    if let Some(Some(token)) = CONFIG_TOKEN.get() {
        if !token.is_empty() {
            return Some(token.clone());
        }
    }
    gh_cli_token()
}

// ---------------------------------------------------------------------------
// HttpClient trait — abstraction over HTTP GET for testability
// ---------------------------------------------------------------------------

pub struct BearerPost<'a> {
    pub url: &'a str,
    pub body: &'a str,
    pub token: &'a str,
}

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
    /// skillhub.club).
    fn post_json_with_bearer(&self, req: &BearerPost<'_>) -> Result<Vec<u8>, SkillfileError> {
        self.post_json(req.url, req.body)
    }
}

// ---------------------------------------------------------------------------
// GitHub URL allowlist — tokens must never leave GitHub domains
// ---------------------------------------------------------------------------

/// Returns `true` if `url` targets a GitHub domain that should receive the
/// GitHub `Authorization` header.
///
/// Only exact host matches are accepted — subdomain tricks like
/// `api.github.com.evil.com` are rejected.
fn is_github_url(url: &str) -> bool {
    // Accept both https:// and http:// schemes. In practice only HTTPS URLs
    // are constructed, but accepting HTTP is fail-safe: the token is attached
    // only if the *host* matches, and ureq will negotiate TLS regardless.
    let host = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .and_then(|s| s.split('/').next())
        .unwrap_or("");
    matches!(host, "api.github.com" | "raw.githubusercontent.com")
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
/// Attaches `User-Agent` to every request. GitHub `Authorization` header
/// is only sent to GitHub domains (`api.github.com`,
/// `raw.githubusercontent.com`) — never to third-party registries.
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

    fn build_get(&self, url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let mut req = self.agent.get(url).header("User-Agent", "skillfile/1.0");
        if let Some(token) = github_token().for_url(url) {
            req = req.header("Authorization", &format!("Bearer {token}"));
        }
        req
    }

    fn build_post(&self, url: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let mut req = self.agent.post(url).header("User-Agent", "skillfile/1.0");
        if let Some(token) = github_token().for_url(url) {
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
            // 404/422 = ref or repo doesn't exist (tentative lookup, not fatal).
            // 403 = rate-limited or forbidden; 401 = bad token — surface these.
            Err(ureq::Error::StatusCode(code)) if code == 404 || code == 422 => Ok(None),
            Err(ureq::Error::StatusCode(403)) => Err(SkillfileError::Network(format!(
                "HTTP 403 fetching {url} — you may be rate-limited. \
                 Set GITHUB_TOKEN or run `gh auth login` to authenticate."
            ))),
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

    fn post_json_with_bearer(&self, req: &BearerPost<'_>) -> Result<Vec<u8>, SkillfileError> {
        let (url, token) = (req.url, req.token);
        let mut response = self
            .agent
            .post(url)
            .header("User-Agent", "skillfile/1.0")
            .header("Content-Type", "application/json")
            .header("Authorization", &format!("Bearer {token}"))
            .send(req.body.as_bytes())
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

    /// Verify that `set_config_token` populates `CONFIG_TOKEN`.
    ///
    /// `OnceLock` can only be written once per process; this test confirms the
    /// happy-path write succeeds (or that a prior write is already present).
    #[test]
    fn set_config_token_populates_cache() {
        set_config_token(Some("test-token-abc".to_string()));
        // Either we just set it, or a previous test already set it.
        // Either way the lock must be initialised.
        assert!(CONFIG_TOKEN.get().is_some());
    }

    // -- GithubToken newtype tests -----------------------------------------------
    //
    // Test the opaque wrapper that makes it structurally impossible to
    // extract the raw token without providing a GitHub URL.

    #[test]
    fn github_token_type_for_url_rejects_registries() {
        let token = GithubToken(Some("ghp_secret"));
        assert!(token.for_url("https://agentskill.sh/api/search").is_none());
        assert!(token.for_url("https://skills.sh/api/search").is_none());
        assert!(token
            .for_url("https://www.skillhub.club/api/v1/skills/search")
            .is_none());
    }

    #[test]
    fn github_token_type_for_url_allows_github() {
        let token = GithubToken(Some("ghp_secret"));
        assert_eq!(
            token.for_url("https://api.github.com/repos/o/r"),
            Some("ghp_secret")
        );
        assert_eq!(
            token.for_url("https://raw.githubusercontent.com/o/r/HEAD/f"),
            Some("ghp_secret")
        );
    }

    #[test]
    fn github_token_type_for_url_returns_none_without_token() {
        let token = GithubToken(None);
        assert!(token.for_url("https://api.github.com/repos/o/r").is_none());
    }

    // -- is_github_url tests (token leakage prevention) -----------------------

    #[test]
    fn github_api_url_is_github() {
        assert!(is_github_url("https://api.github.com/repos/owner/repo"));
    }

    #[test]
    fn github_raw_url_is_github() {
        assert!(is_github_url(
            "https://raw.githubusercontent.com/owner/repo/main/file.md"
        ));
    }

    #[test]
    fn github_api_root_is_github() {
        assert!(is_github_url("https://api.github.com/"));
    }

    #[test]
    fn agentskill_url_is_not_github() {
        assert!(!is_github_url(
            "https://agentskill.sh/api/agent/search?q=test"
        ));
    }

    #[test]
    fn skillssh_url_is_not_github() {
        assert!(!is_github_url("https://skills.sh/api/search?q=test"));
    }

    #[test]
    fn skillhub_url_is_not_github() {
        assert!(!is_github_url(
            "https://www.skillhub.club/api/v1/skills/search"
        ));
    }

    #[test]
    fn spoofed_github_subdomain_is_not_github() {
        assert!(!is_github_url("https://api.github.com.evil.com/repos"));
    }

    #[test]
    fn spoofed_raw_subdomain_is_not_github() {
        assert!(!is_github_url(
            "https://raw.githubusercontent.com.evil.com/file"
        ));
    }

    #[test]
    fn empty_url_is_not_github() {
        assert!(!is_github_url(""));
    }

    #[test]
    fn bare_domain_is_not_github() {
        assert!(!is_github_url("api.github.com/repos"));
    }

    #[test]
    fn http_github_url_is_github() {
        assert!(is_github_url("http://api.github.com/repos/owner/repo"));
    }
}
