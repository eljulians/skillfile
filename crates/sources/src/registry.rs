//! Registry client for searching community skills and agents.
//!
//! Queries multiple registries (agentskill.sh, skills.sh, skillhub.club) for
//! published skills and agents. Each registry implements the [`Registry`] trait,
//! and results are aggregated into a unified [`SearchResponse`].
//!
//! # Example
//!
//! ```no_run
//! use skillfile_sources::registry::{search_all, SearchOptions};
//!
//! let results = search_all("code review", &SearchOptions::default()).unwrap();
//! for r in &results.items {
//!     println!("{} ({}): {}", r.name, r.registry.as_str(), r.description.as_deref().unwrap_or(""));
//! }
//! ```

use serde::Deserialize;

use crate::http::{HttpClient, UreqClient};
use skillfile_core::error::SkillfileError;

// ===========================================================================
// Public types
// ===========================================================================

/// Identifies which registry a search result came from.
///
/// Replaces raw strings with a closed enum so registry-specific logic
/// (colors, audit support, display names) can be matched exhaustively
/// instead of branching on stringly-typed values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum RegistryId {
    #[serde(rename = "agentskill.sh")]
    AgentskillSh,
    #[serde(rename = "skills.sh")]
    SkillsSh,
    #[serde(rename = "skillhub.club")]
    SkillhubClub,
}

impl RegistryId {
    /// String representation matching the registry's domain name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AgentskillSh => "agentskill.sh",
            Self::SkillsSh => "skills.sh",
            Self::SkillhubClub => "skillhub.club",
        }
    }

    /// Whether this registry provides per-skill security audit results
    /// (fetched from the skill's HTML page).
    pub fn has_security_audits(&self) -> bool {
        matches!(self, Self::SkillsSh)
    }
}

impl std::fmt::Display for RegistryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for RegistryId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "agentskill.sh" => Ok(Self::AgentskillSh),
            "skills.sh" => Ok(Self::SkillsSh),
            "skillhub.club" => Ok(Self::SkillhubClub),
            _ => Err(format!("unknown registry: {s}")),
        }
    }
}

/// Options for a registry search query.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Maximum number of results to return.
    pub limit: usize,
    /// Minimum security score (0-100). `None` means no filter.
    pub min_score: Option<u8>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 20,
            min_score: None,
        }
    }
}

/// A single search result from a registry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchResult {
    /// Skill/agent name.
    pub name: String,
    /// Owner (GitHub user or org).
    pub owner: String,
    /// Human-readable description.
    pub description: Option<String>,
    /// Security score (0-100).
    pub security_score: Option<u8>,
    /// GitHub stars.
    pub stars: Option<u32>,
    /// Link to the skill page.
    pub url: String,
    /// Registry that provided this result.
    pub registry: RegistryId,
    /// GitHub `owner/repo` if known from the registry metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_repo: Option<String>,
    /// Path within the GitHub repo (e.g. `skills/foo/SKILL.md`), if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
}

/// The response from a registry search.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchResponse {
    /// Matching results (up to `limit`).
    pub items: Vec<SearchResult>,
    /// Total number of matches across queried registries.
    pub total: usize,
}

// ===========================================================================
// Registry trait
// ===========================================================================

/// A searchable registry backend.
pub trait Registry: Send + Sync {
    /// Human-readable name shown in output (e.g. "agentskill.sh").
    fn name(&self) -> &str;

    /// Search this registry. Returns a unified [`SearchResponse`].
    fn search(
        &self,
        client: &dyn HttpClient,
        query: &str,
        opts: &SearchOptions,
    ) -> Result<SearchResponse, SkillfileError>;
}

/// Returns registries to query by default (public, no auth required).
pub fn all_registries() -> Vec<Box<dyn Registry>> {
    let mut regs: Vec<Box<dyn Registry>> = vec![Box::new(AgentskillSh), Box::new(SkillsSh)];
    // skillhub.club requires an API key — only include when configured.
    if std::env::var("SKILLHUB_API_KEY").is_ok_and(|k| !k.is_empty()) {
        regs.push(Box::new(SkillhubClub));
    }
    regs
}

/// Valid registry names for `--registry` flag validation.
pub const REGISTRY_NAMES: &[&str] = &["agentskill.sh", "skills.sh", "skillhub.club"];

// ===========================================================================
// agentskill.sh
// ===========================================================================

/// Base URL for the agentskill.sh search API.
const AGENTSKILL_API: &str = "https://agentskill.sh/api/agent/search";

/// The agentskill.sh registry (110K+ skills, public, no auth).
pub struct AgentskillSh;

#[derive(Deserialize)]
struct AgentskillApiResponse {
    results: Vec<AgentskillApiResult>,
    total: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentskillApiResult {
    slug: Option<String>,
    name: Option<String>,
    owner: Option<String>,
    description: Option<String>,
    security_score: Option<u8>,
    github_stars: Option<u32>,
    github_owner: Option<String>,
    github_repo: Option<String>,
    github_path: Option<String>,
}

impl Registry for AgentskillSh {
    fn name(&self) -> &str {
        "agentskill.sh"
    }

    fn search(
        &self,
        client: &dyn HttpClient,
        query: &str,
        _opts: &SearchOptions,
    ) -> Result<SearchResponse, SkillfileError> {
        // Don't pass limit to the API — post_process applies it globally
        // after merging results from all registries.
        let url = format!("{AGENTSKILL_API}?q={}&limit=100", urlencoded(query));

        let bytes = client
            .get_bytes(&url)
            .map_err(|e| SkillfileError::Network(format!("agentskill.sh search failed: {e}")))?;

        let body = String::from_utf8(bytes).map_err(|e| {
            SkillfileError::Network(format!("invalid UTF-8 in agentskill.sh response: {e}"))
        })?;

        let api: AgentskillApiResponse = serde_json::from_str(&body).map_err(|e| {
            SkillfileError::Network(format!("failed to parse agentskill.sh results: {e}"))
        })?;

        let items: Vec<SearchResult> = api
            .results
            .into_iter()
            .filter_map(|r| {
                let name = r.name?;
                let owner = r.owner.unwrap_or_default();
                let slug = r.slug.unwrap_or_else(|| format!("{owner}/{name}"));

                // Prefer actual GitHub coordinates over the registry slug.
                let source_repo = match (&r.github_owner, &r.github_repo) {
                    (Some(o), Some(repo)) if !o.is_empty() && !repo.is_empty() => {
                        Some(format!("{o}/{repo}"))
                    }
                    _ => Some(slug.clone()),
                };

                Some(SearchResult {
                    url: format!("https://agentskill.sh/@{slug}"),
                    source_repo,
                    source_path: r.github_path,
                    name,
                    owner,
                    description: r.description,
                    security_score: r.security_score,
                    stars: r.github_stars,
                    registry: RegistryId::AgentskillSh,
                })
            })
            .collect();

        Ok(SearchResponse {
            total: api.total.unwrap_or(items.len()),
            items,
        })
    }
}

// ===========================================================================
// skills.sh
// ===========================================================================

/// Base URL for the skills.sh search API.
const SKILLSSH_API: &str = "https://skills.sh/api/search";

/// The skills.sh registry (public, no auth, minimal fields).
pub struct SkillsSh;

#[derive(Deserialize)]
struct SkillsShApiResponse {
    skills: Option<Vec<SkillsShApiResult>>,
    count: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillsShApiResult {
    /// Full identifier: `owner/repo/skillId`.
    id: Option<String>,
    name: Option<String>,
    installs: Option<u32>,
    source: Option<String>,
}

impl Registry for SkillsSh {
    fn name(&self) -> &str {
        "skills.sh"
    }

    fn search(
        &self,
        client: &dyn HttpClient,
        query: &str,
        _opts: &SearchOptions,
    ) -> Result<SearchResponse, SkillfileError> {
        let url = format!("{SKILLSSH_API}?q={}", urlencoded(query));

        let bytes = client
            .get_bytes(&url)
            .map_err(|e| SkillfileError::Network(format!("skills.sh search failed: {e}")))?;

        let body = String::from_utf8(bytes).map_err(|e| {
            SkillfileError::Network(format!("invalid UTF-8 in skills.sh response: {e}"))
        })?;

        let api: SkillsShApiResponse = serde_json::from_str(&body).map_err(|e| {
            SkillfileError::Network(format!("failed to parse skills.sh results: {e}"))
        })?;

        let results = api.skills.unwrap_or_default();
        let items: Vec<SearchResult> = results
            .into_iter()
            .filter_map(|r| {
                let name = r.name?;
                // skills.sh `source` field is `owner/repo` (GitHub coordinates)
                let source_repo = r.source.clone();
                let owner = source_repo
                    .as_deref()
                    .and_then(|s| s.split('/').next())
                    .unwrap_or("")
                    .to_string();
                // URL uses the `id` field (owner/repo/skillId) when available.
                let url = match &r.id {
                    Some(id) => format!("https://skills.sh/{id}"),
                    None => format!("https://skills.sh/skills/{name}"),
                };
                Some(SearchResult {
                    name,
                    owner,
                    description: None, // skills.sh doesn't return descriptions
                    security_score: None,
                    stars: r.installs,
                    url,
                    registry: RegistryId::SkillsSh,
                    source_repo,
                    source_path: None,
                })
            })
            .collect();

        Ok(SearchResponse {
            total: api.count.unwrap_or(items.len()),
            items,
        })
    }
}

// ===========================================================================
// skillhub.club
// ===========================================================================

/// Base URL for the skillhub.club search API.
const SKILLHUB_API: &str = "https://www.skillhub.club/api/v1/skills/search";

/// The skillhub.club registry (requires `SKILLHUB_API_KEY` env var).
pub struct SkillhubClub;

#[derive(Deserialize)]
struct SkillhubApiResponse {
    results: Option<Vec<SkillhubApiResult>>,
    total: Option<usize>,
}

#[derive(Deserialize)]
struct SkillhubApiResult {
    name: Option<String>,
    description: Option<String>,
    author: Option<String>,
    github_stars: Option<u32>,
    simple_score: Option<u8>,
    slug: Option<String>,
}

impl Registry for SkillhubClub {
    fn name(&self) -> &str {
        "skillhub.club"
    }

    fn search(
        &self,
        client: &dyn HttpClient,
        query: &str,
        _opts: &SearchOptions,
    ) -> Result<SearchResponse, SkillfileError> {
        // Gracefully skip if no API key is configured
        let api_key = match std::env::var("SKILLHUB_API_KEY") {
            Ok(key) if !key.is_empty() => key,
            _ => {
                return Ok(SearchResponse {
                    items: vec![],
                    total: 0,
                });
            }
        };

        let body = serde_json::json!({
            "query": query,
            "limit": 100,
        })
        .to_string();

        let bytes = client
            .post_json_with_bearer(SKILLHUB_API, &body, &api_key)
            .map_err(|e| SkillfileError::Network(format!("skillhub.club search failed: {e}")))?;

        let resp_body = String::from_utf8(bytes).map_err(|e| {
            SkillfileError::Network(format!("invalid UTF-8 in skillhub.club response: {e}"))
        })?;

        let api: SkillhubApiResponse = serde_json::from_str(&resp_body).map_err(|e| {
            SkillfileError::Network(format!("failed to parse skillhub.club results: {e}"))
        })?;

        let results = api.results.unwrap_or_default();
        let items: Vec<SearchResult> = results
            .into_iter()
            .filter_map(|r| {
                let name = r.name?;
                let slug = r.slug.unwrap_or_else(|| name.clone());
                Some(SearchResult {
                    url: format!("https://www.skillhub.club/skills/{slug}"),
                    owner: r.author.unwrap_or_default(),
                    description: r.description,
                    security_score: r.simple_score,
                    stars: r.github_stars,
                    name,
                    registry: RegistryId::SkillhubClub,
                    source_repo: None,
                    source_path: None,
                })
            })
            .collect();

        Ok(SearchResponse {
            total: api.total.unwrap_or(items.len()),
            items,
        })
    }
}

// ===========================================================================
// Public search functions
// ===========================================================================

/// Search all registries and aggregate results.
///
/// Iterates over all registries, collects results (skipping registries that
/// fail with a warning), applies `min_score` filter, and returns combined
/// results.
pub fn search_all(query: &str, opts: &SearchOptions) -> Result<SearchResponse, SkillfileError> {
    let client = UreqClient::new();
    search_all_with_client(&client, query, opts)
}

/// Search all registries using an injected HTTP client (for testing).
pub fn search_all_with_client(
    client: &dyn HttpClient,
    query: &str,
    opts: &SearchOptions,
) -> Result<SearchResponse, SkillfileError> {
    let registries = all_registries();
    let mut all_items = Vec::new();
    let mut total = 0;

    for reg in &registries {
        match reg.search(client, query, opts) {
            Ok(resp) => {
                total += resp.total;
                all_items.extend(resp.items);
            }
            Err(e) => {
                eprintln!("warning: {} search failed: {e}", reg.name());
            }
        }
    }

    let mut resp = SearchResponse {
        items: all_items,
        total,
    };
    post_process(&mut resp, opts);

    Ok(resp)
}

/// Search a single registry by name.
///
/// Returns an error if the registry name is not recognized.
pub fn search_registry(
    registry_name: &str,
    query: &str,
    opts: &SearchOptions,
) -> Result<SearchResponse, SkillfileError> {
    let client = UreqClient::new();
    search_registry_with_client(&client, registry_name, query, opts)
}

/// Search a single registry by name using an injected HTTP client (for testing).
pub fn search_registry_with_client(
    client: &dyn HttpClient,
    registry_name: &str,
    query: &str,
    opts: &SearchOptions,
) -> Result<SearchResponse, SkillfileError> {
    let reg: Box<dyn Registry> = match registry_name {
        "agentskill.sh" => Box::new(AgentskillSh),
        "skills.sh" => Box::new(SkillsSh),
        "skillhub.club" => Box::new(SkillhubClub),
        _ => {
            return Err(SkillfileError::Manifest(format!(
                "unknown registry '{registry_name}'. Valid registries: {}",
                REGISTRY_NAMES.join(", ")
            )));
        }
    };

    let mut resp = reg.search(client, query, opts)?;
    post_process(&mut resp, opts);

    Ok(resp)
}

/// Backward-compatible entry point — searches agentskill.sh only.
pub fn search(query: &str, opts: &SearchOptions) -> Result<SearchResponse, SkillfileError> {
    let client = UreqClient::new();
    search_with_client(&client, query, opts)
}

/// Search agentskill.sh using an injected HTTP client (for testing).
pub fn search_with_client(
    client: &dyn HttpClient,
    query: &str,
    opts: &SearchOptions,
) -> Result<SearchResponse, SkillfileError> {
    let reg = AgentskillSh;
    let mut resp = reg.search(client, query, opts)?;
    post_process(&mut resp, opts);

    Ok(resp)
}

// ===========================================================================
// agentskill.sh detail API — fetch GitHub coordinates for a specific skill
// ===========================================================================

/// GitHub coordinates resolved from the agentskill.sh detail API.
#[derive(Debug, Clone)]
pub struct AgentskillGithubMeta {
    /// GitHub `owner/repo` (e.g. `openclaw/skills`).
    pub source_repo: String,
    /// Path to the skill file within the repo (e.g. `skills/arnarsson/fzf-fuzzy-finder/SKILL.md`).
    pub source_path: String,
}

/// Base URL for the agentskill.sh skills detail API.
const AGENTSKILL_SKILLS_API: &str = "https://agentskill.sh/api/skills";

#[derive(Deserialize)]
struct AgentskillDetailResponse {
    data: Option<Vec<AgentskillDetailResult>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentskillDetailResult {
    slug: Option<String>,
    github_owner: Option<String>,
    github_repo: Option<String>,
    github_path: Option<String>,
}

/// Fetch GitHub coordinates for an agentskill.sh skill by querying the detail API.
///
/// The search API (`/api/agent/search`) only returns a registry slug, not the
/// actual GitHub coordinates. The detail API (`/api/skills`) returns
/// `githubOwner`, `githubRepo`, and `githubPath`.
///
/// Queries by `skill_name`, then matches on `slug` to find the right entry.
/// Returns `None` on network failure or if no matching entry is found.
pub fn fetch_agentskill_github_meta(
    client: &dyn HttpClient,
    slug: &str,
    skill_name: &str,
) -> Option<AgentskillGithubMeta> {
    let url = format!(
        "{AGENTSKILL_SKILLS_API}?q={}&limit=5",
        urlencoded(skill_name)
    );

    let bytes = client.get_bytes(&url).ok()?;
    let body = String::from_utf8(bytes).ok()?;
    let api: AgentskillDetailResponse = serde_json::from_str(&body).ok()?;

    let items = api.data?;
    let slug_lower = slug.to_ascii_lowercase();

    // Find the entry whose slug matches.
    for item in items {
        let item_slug = item.slug.as_deref().unwrap_or("");
        if item_slug.to_ascii_lowercase() == slug_lower {
            let owner = item.github_owner.filter(|s| !s.is_empty())?;
            let repo = item.github_repo.filter(|s| !s.is_empty())?;
            let path = item.github_path.filter(|s| !s.is_empty())?;
            return Some(AgentskillGithubMeta {
                source_repo: format!("{owner}/{repo}"),
                source_path: path,
            });
        }
    }

    None
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Apply post-processing to search results: filter by `min_score`, sort by
/// popularity, and truncate to `limit`.
///
/// Every public search function (`search_all`, `search_registry`, `search`)
/// pipes its raw results through this helper so behavior is consistent.
fn post_process(resp: &mut SearchResponse, opts: &SearchOptions) {
    if let Some(min) = opts.min_score {
        resp.items.retain(|r| r.security_score.unwrap_or(0) >= min);
    }
    sort_by_popularity(&mut resp.items);
    resp.items.truncate(opts.limit);
}

/// Sort results by popularity (descending), then by security score (descending).
///
/// Each registry maps its own popularity metric (GitHub stars, install count,
/// etc.) into the common `stars` field. This function sorts on that normalized
/// value so the most popular results appear first regardless of registry.
/// Items without a popularity signal sink to the bottom.
fn sort_by_popularity(items: &mut [SearchResult]) {
    items.sort_by(|a, b| {
        let pop = b.stars.unwrap_or(0).cmp(&a.stars.unwrap_or(0));
        if pop != std::cmp::Ordering::Equal {
            return pop;
        }
        b.security_score
            .unwrap_or(0)
            .cmp(&a.security_score.unwrap_or(0))
    });
}

/// Minimal URL encoding for query parameters.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ' ' => out.push('+'),
            '&' | '=' | '?' | '#' | '+' | '%' => {
                for byte in c.to_string().as_bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
            _ => out.push(c),
        }
    }
    out
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Serializes tests that manipulate the `SKILLHUB_API_KEY` env var.
    ///
    /// `std::env::set_var`/`remove_var` affect process-global state. Without
    /// serialization, concurrent tests that set and remove the same var race
    /// against each other, causing flaky failures.
    static SKILLHUB_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Sequential mock client: returns responses in FIFO order.
    ///
    /// Each call to `get_bytes` pops the next response. An `Err` variant
    /// simulates a network failure.
    pub(crate) struct MockClient {
        responses: Mutex<VecDeque<Result<String, String>>>,
        post_responses: Mutex<VecDeque<Result<String, String>>>,
    }

    impl MockClient {
        fn new(responses: Vec<Result<String, String>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                post_responses: Mutex::new(VecDeque::new()),
            }
        }

        fn with_post_responses(mut self, post_responses: Vec<Result<String, String>>) -> Self {
            self.post_responses = Mutex::new(post_responses.into());
            self
        }
    }

    impl HttpClient for MockClient {
        fn get_bytes(&self, _url: &str) -> Result<Vec<u8>, SkillfileError> {
            let resp = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("MockClient: no more responses");
            match resp {
                Ok(body) => Ok(body.into_bytes()),
                Err(msg) => Err(SkillfileError::Network(msg)),
            }
        }

        fn get_json(&self, _url: &str) -> Result<Option<String>, SkillfileError> {
            unimplemented!("registry tests don't use get_json")
        }

        fn post_json(&self, _url: &str, _body: &str) -> Result<Vec<u8>, SkillfileError> {
            let resp = self
                .post_responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("MockClient: no more post responses");
            match resp {
                Ok(body) => Ok(body.into_bytes()),
                Err(msg) => Err(SkillfileError::Network(msg)),
            }
        }
    }

    // -- agentskill.sh mock data ------------------------------------------------

    fn agentskill_mock_response() -> String {
        r#"{
            "results": [
                {
                    "slug": "alice/code-reviewer",
                    "name": "code-reviewer",
                    "owner": "alice",
                    "description": "Review code changes",
                    "securityScore": 92,
                    "githubStars": 150
                },
                {
                    "slug": "bob/pr-review",
                    "name": "pr-review",
                    "owner": "bob",
                    "description": "Automated PR reviews",
                    "securityScore": 65,
                    "githubStars": 30
                }
            ],
            "total": 2,
            "hasMore": false,
            "totalExact": true
        }"#
        .to_string()
    }

    // -- skills.sh mock data ----------------------------------------------------

    fn skillssh_mock_response() -> String {
        r#"{
            "query": "docker",
            "searchType": "fuzzy",
            "skills": [
                {
                    "id": "dockerfan/docker-helper/docker-helper",
                    "skillId": "docker-helper",
                    "name": "docker-helper",
                    "installs": 500,
                    "source": "dockerfan/docker-helper"
                },
                {
                    "id": "k8suser/k8s-deploy/k8s-deploy",
                    "skillId": "k8s-deploy",
                    "name": "k8s-deploy",
                    "installs": 200,
                    "source": "k8suser/k8s-deploy"
                }
            ],
            "count": 2,
            "duration_ms": 35
        }"#
        .to_string()
    }

    // -- skillhub.club mock data ------------------------------------------------

    fn skillhub_mock_response() -> String {
        r#"{
            "results": [
                {
                    "name": "testing-pro",
                    "description": "Advanced testing utilities",
                    "author": "testmaster",
                    "github_stars": 75,
                    "simple_score": 88,
                    "slug": "testing-pro"
                }
            ],
            "total": 1
        }"#
        .to_string()
    }

    // -- agentskill.sh tests ----------------------------------------------------

    #[test]
    fn agentskill_search_parses_response() {
        let client = MockClient::new(vec![Ok(agentskill_mock_response())]);
        let resp = search_with_client(&client, "code review", &SearchOptions::default()).unwrap();
        assert_eq!(resp.items.len(), 2);
        assert_eq!(resp.total, 2);
        assert_eq!(resp.items[0].name, "code-reviewer");
        assert_eq!(resp.items[0].owner, "alice");
        assert_eq!(resp.items[0].security_score, Some(92));
        assert_eq!(resp.items[0].stars, Some(150));
        assert!(resp.items[0].url.contains("agentskill.sh"));
        assert_eq!(resp.items[0].registry, RegistryId::AgentskillSh);
    }

    #[test]
    fn agentskill_search_applies_min_score_filter() {
        let client = MockClient::new(vec![Ok(agentskill_mock_response())]);
        let opts = SearchOptions {
            limit: 10,
            min_score: Some(80),
        };
        let resp = search_with_client(&client, "code review", &opts).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].name, "code-reviewer");
    }

    #[test]
    fn agentskill_search_handles_missing_optional_fields() {
        let json = r#"{
            "results": [
                {
                    "slug": "alice/minimal",
                    "name": "minimal",
                    "owner": null,
                    "description": null,
                    "securityScore": null,
                    "githubStars": null
                }
            ],
            "total": 1
        }"#;
        let client = MockClient::new(vec![Ok(json.to_string())]);
        let resp = search_with_client(&client, "test", &SearchOptions::default()).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].name, "minimal");
        assert_eq!(resp.items[0].owner, "");
        assert!(resp.items[0].description.is_none());
        assert!(resp.items[0].security_score.is_none());
    }

    #[test]
    fn agentskill_search_skips_results_without_name() {
        let json = r#"{
            "results": [
                {"slug": "x/y", "name": null, "owner": "x"},
                {"slug": "a/b", "name": "valid", "owner": "a"}
            ],
            "total": 2
        }"#;
        let client = MockClient::new(vec![Ok(json.to_string())]);
        let resp = search_with_client(&client, "test", &SearchOptions::default()).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].name, "valid");
    }

    #[test]
    fn agentskill_search_returns_error_on_network_failure() {
        let client = MockClient::new(vec![Err("connection refused".to_string())]);
        let result = search_with_client(&client, "test", &SearchOptions::default());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("search failed"), "got: {err}");
    }

    #[test]
    fn agentskill_search_returns_error_on_malformed_json() {
        let client = MockClient::new(vec![Ok("not json".to_string())]);
        let result = search_with_client(&client, "test", &SearchOptions::default());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to parse"), "got: {err}");
    }

    #[test]
    fn agentskill_search_constructs_url_from_slug() {
        let client = MockClient::new(vec![Ok(agentskill_mock_response())]);
        let resp = search_with_client(&client, "test", &SearchOptions::default()).unwrap();
        assert_eq!(
            resp.items[0].url,
            "https://agentskill.sh/@alice/code-reviewer"
        );
        // Without githubOwner/githubRepo, source_repo falls back to slug.
        assert_eq!(
            resp.items[0].source_repo.as_deref(),
            Some("alice/code-reviewer")
        );
        // No githubPath in mock → source_path is None.
        assert!(resp.items[0].source_path.is_none());
    }

    #[test]
    fn agentskill_search_uses_github_coordinates_when_present() {
        // When the API returns githubOwner/githubRepo/githubPath, those
        // should be used instead of the slug (which is a registry path).
        let json = r#"{
            "results": [{
                "slug": "openclaw/fzf-fuzzy-finder",
                "name": "fzf-fuzzy-finder",
                "owner": "openclaw",
                "description": "Fuzzy finder skill",
                "securityScore": 80,
                "githubStars": 2218,
                "githubOwner": "openclaw",
                "githubRepo": "skills",
                "githubPath": "skills/arnarsson/fzf-fuzzy-finder/SKILL.md"
            }],
            "total": 1
        }"#
        .to_string();
        let client = MockClient::new(vec![Ok(json)]);
        let resp = search_with_client(&client, "fzf", &SearchOptions::default()).unwrap();

        assert_eq!(resp.items.len(), 1);
        // source_repo should be the actual GitHub owner/repo, not the slug.
        assert_eq!(
            resp.items[0].source_repo.as_deref(),
            Some("openclaw/skills")
        );
        // source_path should carry the exact file path from the API.
        assert_eq!(
            resp.items[0].source_path.as_deref(),
            Some("skills/arnarsson/fzf-fuzzy-finder/SKILL.md")
        );
        // URL still uses the slug.
        assert_eq!(
            resp.items[0].url,
            "https://agentskill.sh/@openclaw/fzf-fuzzy-finder"
        );
    }

    // -- skills.sh tests --------------------------------------------------------

    #[test]
    fn skillssh_search_parses_response() {
        let client = MockClient::new(vec![Ok(skillssh_mock_response())]);
        let reg = SkillsSh;
        let resp = reg
            .search(&client, "docker", &SearchOptions::default())
            .unwrap();
        assert_eq!(resp.items.len(), 2);
        assert_eq!(resp.total, 2);
        assert_eq!(resp.items[0].name, "docker-helper");
        assert_eq!(resp.items[0].owner, "dockerfan");
        assert!(resp.items[0].description.is_none());
        assert_eq!(resp.items[0].stars, Some(500));
        assert_eq!(
            resp.items[0].url,
            "https://skills.sh/dockerfan/docker-helper/docker-helper"
        );
        assert_eq!(resp.items[0].registry, RegistryId::SkillsSh);
        assert_eq!(
            resp.items[0].source_repo.as_deref(),
            Some("dockerfan/docker-helper")
        );
    }

    #[test]
    fn skillssh_search_returns_all_results() {
        let client = MockClient::new(vec![Ok(skillssh_mock_response())]);
        let reg = SkillsSh;
        let opts = SearchOptions {
            limit: 1,
            min_score: None,
        };
        // Per-registry search returns all results; limit is applied globally by post_process.
        let resp = reg.search(&client, "docker", &opts).unwrap();
        assert_eq!(resp.items.len(), 2);
    }

    #[test]
    fn skillssh_search_handles_empty_results() {
        let json = r#"{"skills": [], "count": 0}"#;
        let client = MockClient::new(vec![Ok(json.to_string())]);
        let reg = SkillsSh;
        let resp = reg
            .search(&client, "nonexistent", &SearchOptions::default())
            .unwrap();
        assert_eq!(resp.items.len(), 0);
        assert_eq!(resp.total, 0);
    }

    // -- skillhub.club tests ----------------------------------------------------

    #[test]
    fn skillhub_search_parses_response() {
        let _guard = SKILLHUB_ENV_LOCK.lock().unwrap();
        std::env::set_var("SKILLHUB_API_KEY", "test-key-123");
        let client =
            MockClient::new(vec![]).with_post_responses(vec![Ok(skillhub_mock_response())]);
        let reg = SkillhubClub;
        let resp = reg
            .search(&client, "testing", &SearchOptions::default())
            .unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].name, "testing-pro");
        assert_eq!(resp.items[0].owner, "testmaster");
        assert_eq!(
            resp.items[0].description.as_deref(),
            Some("Advanced testing utilities")
        );
        assert_eq!(resp.items[0].security_score, Some(88));
        assert_eq!(resp.items[0].stars, Some(75));
        assert!(resp.items[0].url.contains("skillhub.club"));
        assert_eq!(resp.items[0].registry, RegistryId::SkillhubClub);
        std::env::remove_var("SKILLHUB_API_KEY");
    }

    #[test]
    fn skillhub_skips_without_api_key() {
        let _guard = SKILLHUB_ENV_LOCK.lock().unwrap();
        std::env::remove_var("SKILLHUB_API_KEY");
        let client = MockClient::new(vec![]);
        let reg = SkillhubClub;
        let resp = reg
            .search(&client, "testing", &SearchOptions::default())
            .unwrap();
        assert_eq!(resp.items.len(), 0);
        assert_eq!(resp.total, 0);
    }

    // -- Aggregation tests ------------------------------------------------------

    #[test]
    fn search_all_aggregates_results() {
        // Mock: agentskill returns 2, skills.sh returns 2
        // skillhub.club skipped (no API key)
        let _guard = SKILLHUB_ENV_LOCK.lock().unwrap();
        std::env::remove_var("SKILLHUB_API_KEY");
        let client = MockClient::new(vec![
            Ok(agentskill_mock_response()),
            Ok(skillssh_mock_response()),
        ]);
        let resp = search_all_with_client(&client, "test", &SearchOptions::default()).unwrap();
        assert_eq!(resp.items.len(), 4);
        // Verify items from both registries are present
        let registries: Vec<RegistryId> = resp.items.iter().map(|r| r.registry).collect();
        assert!(registries.contains(&RegistryId::AgentskillSh));
        assert!(registries.contains(&RegistryId::SkillsSh));
    }

    #[test]
    fn search_all_skips_failed_registry() {
        // agentskill fails, skills.sh succeeds
        let _guard = SKILLHUB_ENV_LOCK.lock().unwrap();
        std::env::remove_var("SKILLHUB_API_KEY");
        let client = MockClient::new(vec![
            Err("connection refused".to_string()),
            Ok(skillssh_mock_response()),
        ]);
        let resp = search_all_with_client(&client, "test", &SearchOptions::default()).unwrap();
        assert_eq!(resp.items.len(), 2);
        assert_eq!(resp.items[0].registry, RegistryId::SkillsSh);
    }

    #[test]
    fn search_all_applies_min_score_filter() {
        let _guard = SKILLHUB_ENV_LOCK.lock().unwrap();
        std::env::remove_var("SKILLHUB_API_KEY");
        let client = MockClient::new(vec![
            Ok(agentskill_mock_response()),
            Ok(skillssh_mock_response()),
        ]);
        let opts = SearchOptions {
            limit: 10,
            min_score: Some(80),
        };
        let resp = search_all_with_client(&client, "test", &opts).unwrap();
        // Only agentskill's "code-reviewer" (score 92) passes the filter.
        // skills.sh items have no score, so they're filtered out.
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].name, "code-reviewer");
    }

    #[test]
    fn search_registry_filters_by_name() {
        let client = MockClient::new(vec![Ok(skillssh_mock_response())]);
        let resp =
            search_registry_with_client(&client, "skills.sh", "docker", &SearchOptions::default())
                .unwrap();
        assert_eq!(resp.items.len(), 2);
        assert!(resp
            .items
            .iter()
            .all(|r| r.registry == RegistryId::SkillsSh));
    }

    #[test]
    fn search_registry_rejects_unknown_name() {
        let client = MockClient::new(vec![]);
        let result = search_registry_with_client(
            &client,
            "nonexistent.io",
            "test",
            &SearchOptions::default(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown registry"), "got: {err}");
    }

    #[test]
    fn search_result_includes_registry_field() {
        let client = MockClient::new(vec![Ok(agentskill_mock_response())]);
        let resp = search_with_client(&client, "test", &SearchOptions::default()).unwrap();
        for item in &resp.items {
            assert_eq!(item.registry, RegistryId::AgentskillSh);
        }
    }

    // -- Utility tests ----------------------------------------------------------

    #[test]
    fn urlencoded_encodes_spaces_and_specials() {
        assert_eq!(urlencoded("code review"), "code+review");
        assert_eq!(urlencoded("a&b"), "a%26b");
        assert_eq!(urlencoded("q=1"), "q%3D1");
        assert_eq!(urlencoded("hello"), "hello");
        // Multi-byte characters pass through (non-ASCII is not percent-encoded).
        assert_eq!(urlencoded("代码审查"), "代码审查");
    }

    #[test]
    fn default_search_options() {
        let opts = SearchOptions::default();
        assert_eq!(opts.limit, 20);
        assert!(opts.min_score.is_none());
    }

    #[test]
    fn all_registries_default_excludes_skillhub() {
        // Without SKILLHUB_API_KEY, only agentskill.sh and skills.sh are returned.
        let regs = all_registries();
        assert!(regs.len() >= 2);
        assert_eq!(regs[0].name(), "agentskill.sh");
        assert_eq!(regs[1].name(), "skills.sh");
    }

    #[test]
    fn registry_names_covers_all_known() {
        assert_eq!(
            REGISTRY_NAMES,
            &["agentskill.sh", "skills.sh", "skillhub.club"]
        );
    }

    // -- Sorting tests ----------------------------------------------------------

    #[test]
    fn sort_by_popularity_orders_by_stars_desc() {
        let mut items = vec![
            SearchResult {
                name: "low".into(),
                stars: Some(10),
                ..make_result("low")
            },
            SearchResult {
                name: "high".into(),
                stars: Some(500),
                ..make_result("high")
            },
            SearchResult {
                name: "mid".into(),
                stars: Some(100),
                ..make_result("mid")
            },
        ];
        sort_by_popularity(&mut items);
        assert_eq!(items[0].name, "high");
        assert_eq!(items[1].name, "mid");
        assert_eq!(items[2].name, "low");
    }

    #[test]
    fn sort_by_popularity_uses_score_as_tiebreaker() {
        let mut items = vec![
            SearchResult {
                name: "low-score".into(),
                stars: Some(100),
                security_score: Some(50),
                ..make_result("low-score")
            },
            SearchResult {
                name: "high-score".into(),
                stars: Some(100),
                security_score: Some(95),
                ..make_result("high-score")
            },
        ];
        sort_by_popularity(&mut items);
        assert_eq!(items[0].name, "high-score");
        assert_eq!(items[1].name, "low-score");
    }

    #[test]
    fn sort_by_popularity_none_stars_sort_last() {
        let mut items = vec![
            SearchResult {
                name: "no-stars".into(),
                stars: None,
                ..make_result("no-stars")
            },
            SearchResult {
                name: "has-stars".into(),
                stars: Some(1),
                ..make_result("has-stars")
            },
        ];
        sort_by_popularity(&mut items);
        assert_eq!(items[0].name, "has-stars");
        assert_eq!(items[1].name, "no-stars");
    }

    #[test]
    fn search_all_returns_sorted_results() {
        let _guard = SKILLHUB_ENV_LOCK.lock().unwrap();
        std::env::remove_var("SKILLHUB_API_KEY");
        let client = MockClient::new(vec![
            Ok(agentskill_mock_response()),
            Ok(skillssh_mock_response()),
        ]);
        let resp = search_all_with_client(&client, "test", &SearchOptions::default()).unwrap();
        // Expected order: docker-helper(500) > k8s-deploy(200) > code-reviewer(150) > pr-review(30)
        assert_eq!(resp.items[0].name, "docker-helper");
        assert_eq!(resp.items[1].name, "k8s-deploy");
        assert_eq!(resp.items[2].name, "code-reviewer");
        assert_eq!(resp.items[3].name, "pr-review");
    }

    #[test]
    fn search_with_client_sorts_results() {
        // Verify the backward-compat entry point also sorts by popularity.
        let json = r#"{
            "results": [
                {"name": "aaa-low", "owner": "a", "githubStars": 10},
                {"name": "bbb-high", "owner": "b", "githubStars": 500}
            ],
            "total": 2
        }"#;
        let client = MockClient::new(vec![Ok(json.to_string())]);
        let resp = search_with_client(&client, "test", &SearchOptions::default()).unwrap();
        // bbb-high (500 stars) should come before aaa-low (10 stars)
        assert_eq!(resp.items[0].name, "bbb-high");
        assert_eq!(resp.items[1].name, "aaa-low");
    }

    #[test]
    fn post_process_filters_and_sorts() {
        let mut resp = SearchResponse {
            total: 3,
            items: vec![
                SearchResult {
                    name: "low-score-low-stars".into(),
                    security_score: Some(30),
                    stars: Some(10),
                    ..make_result("low-score-low-stars")
                },
                SearchResult {
                    name: "high-score-high-stars".into(),
                    security_score: Some(90),
                    stars: Some(500),
                    ..make_result("high-score-high-stars")
                },
                SearchResult {
                    name: "mid-score-mid-stars".into(),
                    security_score: Some(60),
                    stars: Some(100),
                    ..make_result("mid-score-mid-stars")
                },
            ],
        };
        let opts = SearchOptions {
            min_score: Some(50),
            ..Default::default()
        };
        post_process(&mut resp, &opts);

        // low-score-low-stars (score 30) should be filtered out
        assert_eq!(resp.items.len(), 2);
        // Remaining sorted by stars descending
        assert_eq!(resp.items[0].name, "high-score-high-stars");
        assert_eq!(resp.items[1].name, "mid-score-mid-stars");
    }

    #[test]
    fn post_process_no_filter_only_sorts() {
        let mut resp = SearchResponse {
            total: 2,
            items: vec![
                SearchResult {
                    name: "few".into(),
                    stars: Some(5),
                    ..make_result("few")
                },
                SearchResult {
                    name: "many".into(),
                    stars: Some(999),
                    ..make_result("many")
                },
            ],
        };
        post_process(&mut resp, &SearchOptions::default());
        assert_eq!(resp.items[0].name, "many");
        assert_eq!(resp.items[1].name, "few");
    }

    #[test]
    fn post_process_truncates_to_limit() {
        let mut resp = SearchResponse {
            total: 5,
            items: (0..5)
                .map(|i| SearchResult {
                    name: format!("item-{i}"),
                    stars: Some(100 - i),
                    ..make_result(&format!("item-{i}"))
                })
                .collect(),
        };
        let opts = SearchOptions {
            limit: 3,
            ..Default::default()
        };
        post_process(&mut resp, &opts);
        assert_eq!(resp.items.len(), 3);
        // Sorted descending: item-0(100), item-1(99), item-2(98)
        assert_eq!(resp.items[0].name, "item-0");
        assert_eq!(resp.items[2].name, "item-2");
    }

    /// Helper to create a minimal `SearchResult` for sorting tests.
    fn make_result(name: &str) -> SearchResult {
        SearchResult {
            name: name.to_string(),
            owner: String::new(),
            description: None,
            security_score: None,
            stars: None,
            url: String::new(),
            registry: RegistryId::AgentskillSh,
            source_repo: None,
            source_path: None,
        }
    }

    // -- fetch_agentskill_github_meta tests -------------------------------------

    fn agentskill_detail_mock(slug: &str, owner: &str, repo: &str, path: &str) -> String {
        format!(
            r#"{{"data": [{{"slug": "{slug}", "githubOwner": "{owner}", "githubRepo": "{repo}", "githubPath": "{path}"}}]}}"#
        )
    }

    #[test]
    fn fetch_github_meta_returns_coordinates() {
        let json = agentskill_detail_mock(
            "openclaw/fzf-fuzzy-finder",
            "openclaw",
            "skills",
            "skills/arnarsson/fzf-fuzzy-finder/SKILL.md",
        );
        let client = MockClient::new(vec![Ok(json)]);
        let meta =
            fetch_agentskill_github_meta(&client, "openclaw/fzf-fuzzy-finder", "fzf-fuzzy-finder");
        let meta = meta.expect("should return meta");
        assert_eq!(meta.source_repo, "openclaw/skills");
        assert_eq!(
            meta.source_path,
            "skills/arnarsson/fzf-fuzzy-finder/SKILL.md"
        );
    }

    #[test]
    fn fetch_github_meta_case_insensitive_slug() {
        let json = agentskill_detail_mock(
            "OpenClaw/FZF-Fuzzy-Finder",
            "openclaw",
            "skills",
            "skills/arnarsson/fzf-fuzzy-finder/SKILL.md",
        );
        let client = MockClient::new(vec![Ok(json)]);
        let meta =
            fetch_agentskill_github_meta(&client, "openclaw/fzf-fuzzy-finder", "fzf-fuzzy-finder");
        assert!(meta.is_some());
    }

    #[test]
    fn fetch_github_meta_no_match_returns_none() {
        let json = agentskill_detail_mock("other/skill", "other", "repo", "skill.md");
        let client = MockClient::new(vec![Ok(json)]);
        let meta =
            fetch_agentskill_github_meta(&client, "openclaw/fzf-fuzzy-finder", "fzf-fuzzy-finder");
        assert!(meta.is_none());
    }

    #[test]
    fn fetch_github_meta_empty_data_returns_none() {
        let json = r#"{"data": []}"#.to_string();
        let client = MockClient::new(vec![Ok(json)]);
        let meta =
            fetch_agentskill_github_meta(&client, "openclaw/fzf-fuzzy-finder", "fzf-fuzzy-finder");
        assert!(meta.is_none());
    }

    #[test]
    fn fetch_github_meta_network_error_returns_none() {
        let client = MockClient::new(vec![Err("connection refused".to_string())]);
        let meta =
            fetch_agentskill_github_meta(&client, "openclaw/fzf-fuzzy-finder", "fzf-fuzzy-finder");
        assert!(meta.is_none());
    }

    #[test]
    fn fetch_github_meta_malformed_json_returns_none() {
        let client = MockClient::new(vec![Ok("not json".to_string())]);
        let meta =
            fetch_agentskill_github_meta(&client, "openclaw/fzf-fuzzy-finder", "fzf-fuzzy-finder");
        assert!(meta.is_none());
    }

    #[test]
    fn fetch_github_meta_missing_github_fields_returns_none() {
        // Slug matches but githubOwner/githubRepo/githubPath are missing.
        let json = r#"{"data": [{"slug": "openclaw/fzf-fuzzy-finder"}]}"#.to_string();
        let client = MockClient::new(vec![Ok(json)]);
        let meta =
            fetch_agentskill_github_meta(&client, "openclaw/fzf-fuzzy-finder", "fzf-fuzzy-finder");
        assert!(meta.is_none());
    }

    #[test]
    fn fetch_github_meta_empty_github_fields_returns_none() {
        let json = r#"{"data": [{"slug": "openclaw/fzf-fuzzy-finder", "githubOwner": "", "githubRepo": "", "githubPath": ""}]}"#.to_string();
        let client = MockClient::new(vec![Ok(json)]);
        let meta =
            fetch_agentskill_github_meta(&client, "openclaw/fzf-fuzzy-finder", "fzf-fuzzy-finder");
        assert!(meta.is_none());
    }

    #[test]
    fn fetch_github_meta_picks_matching_slug_from_multiple() {
        // Multiple results, only the second matches the slug.
        let json = r#"{"data": [
            {"slug": "other/fzf", "githubOwner": "other", "githubRepo": "repo", "githubPath": "fzf.md"},
            {"slug": "openclaw/fzf-fuzzy-finder", "githubOwner": "openclaw", "githubRepo": "skills", "githubPath": "skills/arnarsson/fzf-fuzzy-finder/SKILL.md"}
        ]}"#.to_string();
        let client = MockClient::new(vec![Ok(json)]);
        let meta =
            fetch_agentskill_github_meta(&client, "openclaw/fzf-fuzzy-finder", "fzf-fuzzy-finder");
        let meta = meta.expect("should match second entry");
        assert_eq!(meta.source_repo, "openclaw/skills");
        assert_eq!(
            meta.source_path,
            "skills/arnarsson/fzf-fuzzy-finder/SKILL.md"
        );
    }
}
