//! Registry client for searching community skills and agents.
//!
//! Queries multiple registries (agentskill.sh, skills.sh, skillhub.club) for
//! published skills and agents. Each registry implements the [`Registry`] trait
//! and serves results one page at a time. [`SearchSession`] pages through all
//! of them incrementally; the `search_*` functions collect a fixed number of
//! results into a [`SearchResponse`].
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

pub mod agentskill;
mod scrape;
mod session;
mod skillhub;
mod skillssh;

pub use session::{SearchBatch, SearchSession, SessionOptions};

#[cfg(test)]
pub(crate) mod test_support;

use crate::http::{HttpClient, UreqClient};
use skillfile_core::error::SkillfileError;

// Re-export registry implementations for `all_registries()` and `search_registry_with_client()`.
use agentskill::AgentskillSh;
use skillhub::SkillhubClub;
use skillssh::SkillsSh;

// Re-export the detail API from the agentskill module.
pub use agentskill::{
    fetch_agentskill_github_meta, scrape_github_meta_from_page, AgentskillGithubMeta,
};

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

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchResult {
    pub name: String,
    pub owner: String,
    pub description: Option<String>,
    pub security_score: Option<u8>,
    pub stars: Option<u32>,
    pub url: String,
    pub registry: RegistryId,
    /// GitHub `owner/repo` if known from the registry metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_repo: Option<String>,
    /// Path within the GitHub repo (e.g. `skills/foo/SKILL.md`), if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchResponse {
    /// Matching results (up to `limit`).
    pub items: Vec<SearchResult>,
    /// Total number of matches across queried registries. Some registries
    /// only report a lower bound.
    pub total: usize,
    /// Whether more results exist beyond `items`.
    pub has_more: bool,
}

// ===========================================================================
// Registry trait
// ===========================================================================

pub(crate) struct SearchQuery<'a> {
    pub client: &'a dyn HttpClient,
    pub query: &'a str,
    /// 1-based page number.
    pub page: u32,
}

/// One page of results from a single registry.
#[derive(Debug, Default)]
pub(crate) struct RegistryPage {
    pub items: Vec<SearchResult>,
    /// Whether requesting `page + 1` can return more results.
    pub has_more: bool,
    /// The registry's reported total match count, if any.
    pub total_hint: Option<usize>,
}

pub(crate) trait Registry: Send + Sync {
    fn id(&self) -> RegistryId;

    /// Registries without server-side paging return everything on page 1
    /// with `has_more: false`.
    fn search_page(&self, q: &SearchQuery<'_>) -> Result<RegistryPage, SkillfileError>;

    /// Each registry extracts the content from its own data source:
    /// agentskill.sh scrapes Nuxt hydration data, skills.sh fetches from
    /// `raw.githubusercontent.com`. Default returns `None` (not supported).
    fn fetch_skill_content(
        &self,
        _client: &dyn HttpClient,
        _item: &SearchResult,
    ) -> Option<String> {
        None
    }
}

/// Returns registries to query by default (public, no auth required).
pub(crate) fn all_registries() -> Vec<Box<dyn Registry>> {
    let mut regs: Vec<Box<dyn Registry>> = vec![Box::new(AgentskillSh), Box::new(SkillsSh)];
    // skillhub.club requires an API key — only include when configured.
    if std::env::var("SKILLHUB_API_KEY").is_ok_and(|k| !k.is_empty()) {
        regs.push(Box::new(SkillhubClub));
    }
    regs
}

pub(crate) fn registry_for(id: RegistryId) -> Box<dyn Registry> {
    match id {
        RegistryId::AgentskillSh => Box::new(AgentskillSh),
        RegistryId::SkillsSh => Box::new(SkillsSh),
        RegistryId::SkillhubClub => Box::new(SkillhubClub),
    }
}

/// Valid registry names for `--registry` flag validation.
pub const REGISTRY_NAMES: &[&str] = &["agentskill.sh", "skills.sh", "skillhub.club"];

/// Parse a `--registry` value.
///
/// # Errors
///
/// Returns `SkillfileError::Manifest` listing the valid names when
/// `name` is not a known registry.
pub fn parse_registry_name(name: &str) -> Result<RegistryId, SkillfileError> {
    name.parse().map_err(|_| {
        SkillfileError::Manifest(format!(
            "unknown registry '{name}'. Valid registries: {}",
            REGISTRY_NAMES.join(", ")
        ))
    })
}

// ===========================================================================
// Public search functions
// ===========================================================================

/// Searches all default registries, paging until `opts.limit` results are
/// gathered, and returns them sorted by popularity. Individual registry
/// failures are skipped with a warning, but the search fails if no registry
/// responds.
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
    SearchSession::new(query, &session_options(opts, None)).collect(client, opts.limit)
}

/// Search a single registry by name.
///
/// Returns an error if the registry name is not recognized.
pub fn search_registry(
    registry_name: &str,
    query: &str,
    opts: &SearchOptions,
) -> Result<SearchResponse, SkillfileError> {
    let registry = parse_registry_name(registry_name)?;
    SearchSession::new(query, &session_options(opts, Some(registry)))
        .collect(&UreqClient::new(), opts.limit)
}

/// Fetch raw SKILL.md content for a search result.
///
/// Dispatches to the correct registry's content fetcher based on
/// `item.registry`. Returns `None` if the registry doesn't support
/// content fetching or if the fetch fails.
pub fn fetch_skill_content_for(item: &SearchResult) -> Option<String> {
    registry_for(item.registry).fetch_skill_content(&UreqClient::new(), item)
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
    let session_opts = session_options(opts, Some(RegistryId::AgentskillSh));
    SearchSession::new(query, &session_opts).collect(client, opts.limit)
}

// ===========================================================================
// Helpers
// ===========================================================================

/// `opts.limit` is enforced by `SearchSession::collect`, not as a session cap.
fn session_options(opts: &SearchOptions, registry: Option<RegistryId>) -> SessionOptions {
    SessionOptions {
        registry,
        min_score: opts.min_score,
        max_results: None,
    }
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

// ===========================================================================
// Tests — aggregation, sorting, utilities
// ===========================================================================

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use test_support::MockClient;

    /// Serializes tests that manipulate the `SKILLHUB_API_KEY` env var.
    static SKILLHUB_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn agentskill_mock_response() -> String {
        r#"{
            "data": [
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

    // -- Aggregation tests ------------------------------------------------------

    #[test]
    fn search_all_aggregates_results() {
        let _guard = SKILLHUB_ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("SKILLHUB_API_KEY") };
        let client = MockClient::new(vec![
            Ok(agentskill_mock_response()),
            Ok(skillssh_mock_response()),
        ]);
        let resp = search_all_with_client(&client, "test", &SearchOptions::default()).unwrap();
        assert_eq!(resp.items.len(), 4);
        let registries: Vec<RegistryId> = resp.items.iter().map(|r| r.registry).collect();
        assert!(registries.contains(&RegistryId::AgentskillSh));
        assert!(registries.contains(&RegistryId::SkillsSh));
    }

    #[test]
    fn search_all_skips_failed_registry() {
        let _guard = SKILLHUB_ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("SKILLHUB_API_KEY") };
        let client = MockClient::new(vec![
            Err("connection refused".to_string()),
            Ok(skillssh_mock_response()),
        ]);
        let resp = search_all_with_client(&client, "test", &SearchOptions::default()).unwrap();
        assert_eq!(resp.items.len(), 2);
        assert_eq!(resp.items[0].registry, RegistryId::SkillsSh);
    }

    #[test]
    fn search_all_fails_when_every_registry_fails() {
        let _guard = SKILLHUB_ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("SKILLHUB_API_KEY") };
        let client = MockClient::new(vec![
            Err("agentskill unavailable".to_string()),
            Err("skills unavailable".to_string()),
        ]);

        let result = search_all_with_client(&client, "test", &SearchOptions::default());

        assert!(matches!(
            result,
            Err(SkillfileError::Network(message)) if message == "all registry searches failed"
        ));
    }

    #[test]
    fn search_all_applies_min_score_filter() {
        let _guard = SKILLHUB_ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("SKILLHUB_API_KEY") };
        let client = MockClient::new(vec![
            Ok(agentskill_mock_response()),
            Ok(skillssh_mock_response()),
        ]);
        let opts = SearchOptions {
            limit: 10,
            min_score: Some(80),
        };
        let resp = search_all_with_client(&client, "test", &opts).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].name, "code-reviewer");
    }

    #[test]
    fn session_with_registry_queries_only_that_registry() {
        let client = MockClient::new(vec![Ok(skillssh_mock_response())]);
        let opts = SessionOptions {
            registry: Some(RegistryId::SkillsSh),
            ..SessionOptions::default()
        };
        let resp = SearchSession::new("docker", &opts)
            .collect(&client, 20)
            .unwrap();
        assert_eq!(resp.items.len(), 2);
        assert!(resp
            .items
            .iter()
            .all(|r| r.registry == RegistryId::SkillsSh));
        assert_eq!(client.requested_urls().len(), 1);
    }

    #[test]
    fn parse_registry_name_rejects_unknown_name() {
        let err = parse_registry_name("nonexistent.io")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown registry"), "got: {err}");
        assert!(err.contains("skills.sh"), "got: {err}");
    }

    #[test]
    fn parse_registry_name_accepts_known_names() {
        for name in REGISTRY_NAMES {
            assert_eq!(parse_registry_name(name).unwrap().as_str(), *name);
        }
    }

    #[test]
    fn search_with_client_pages_until_limit() {
        let page1 = r#"{"data": [
            {"slug": "a/one", "name": "one", "owner": "a", "githubStars": 1},
            {"slug": "a/two", "name": "two", "owner": "a", "githubStars": 2}
        ], "total": 3, "hasMore": true}"#;
        let page2 = r#"{"data": [
            {"slug": "a/three", "name": "three", "owner": "a", "githubStars": 3}
        ], "total": 3, "hasMore": false}"#;
        let client = MockClient::new(vec![Ok(page1.into()), Ok(page2.into())]);
        let opts = SearchOptions {
            limit: 3,
            min_score: None,
        };

        let resp = search_with_client(&client, "x", &opts).unwrap();

        let names: Vec<&str> = resp.items.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["three", "two", "one"]);
        assert!(!resp.has_more);
        let urls = client.requested_urls();
        assert!(urls[0].ends_with("&page=1"), "got: {urls:?}");
        assert!(urls[1].ends_with("&page=2"), "got: {urls:?}");
    }

    #[test]
    fn search_with_client_stops_at_limit_without_extra_pages() {
        let client = MockClient::new(vec![Ok(agentskill_mock_response())]);
        let opts = SearchOptions {
            limit: 1,
            min_score: None,
        };
        let resp = search_with_client(&client, "x", &opts).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert!(resp.has_more);
        assert_eq!(client.requested_urls().len(), 1);
    }

    #[test]
    fn search_result_includes_registry_field() {
        let client = MockClient::new(vec![Ok(agentskill_mock_response())]);
        let resp = search_with_client(&client, "test", &SearchOptions::default()).unwrap();
        for item in &resp.items {
            assert_eq!(item.registry, RegistryId::AgentskillSh);
        }
    }

    #[test]
    fn default_search_options() {
        let opts = SearchOptions::default();
        assert_eq!(opts.limit, 20);
        assert!(opts.min_score.is_none());
    }

    #[test]
    fn all_registries_default_excludes_skillhub() {
        let regs = all_registries();
        assert!(regs.len() >= 2);
        assert_eq!(regs[0].id(), RegistryId::AgentskillSh);
        assert_eq!(regs[1].id(), RegistryId::SkillsSh);
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
        unsafe { std::env::remove_var("SKILLHUB_API_KEY") };
        let client = MockClient::new(vec![
            Ok(agentskill_mock_response()),
            Ok(skillssh_mock_response()),
        ]);
        let resp = search_all_with_client(&client, "test", &SearchOptions::default()).unwrap();
        assert_eq!(resp.items[0].name, "docker-helper");
        assert_eq!(resp.items[1].name, "k8s-deploy");
        assert_eq!(resp.items[2].name, "code-reviewer");
        assert_eq!(resp.items[3].name, "pr-review");
    }

    #[test]
    fn search_with_client_sorts_results() {
        let json = r#"{
            "data": [
                {"name": "aaa-low", "owner": "a", "githubStars": 10},
                {"name": "bbb-high", "owner": "b", "githubStars": 500}
            ],
            "total": 2
        }"#;
        let client = MockClient::new(vec![Ok(json.to_string())]);
        let resp = search_with_client(&client, "test", &SearchOptions::default()).unwrap();
        assert_eq!(resp.items[0].name, "bbb-high");
        assert_eq!(resp.items[1].name, "aaa-low");
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
}
