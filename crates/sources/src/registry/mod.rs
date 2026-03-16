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

pub mod agentskill;
mod skillhub;
mod skillssh;

use crate::http::{HttpClient, UreqClient};
use skillfile_core::error::SkillfileError;

// Re-export registry implementations for `all_registries()` and `search_registry_with_client()`.
use agentskill::AgentskillSh;
use skillhub::SkillhubClub;
use skillssh::SkillsSh;

// Re-export the detail API from the agentskill module.
pub use agentskill::{fetch_agentskill_github_meta, AgentskillGithubMeta};

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
    ///
    /// # Note
    /// The four parameters (`self`, `client`, `query`, `opts`) are all semantically
    /// distinct and cannot be collapsed without a breaking trait change.
    #[allow(clippy::too_many_arguments)]
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
///
/// # Note
/// The four parameters are all semantically required; bundling them would
/// complicate the public API at every call site.
#[allow(clippy::too_many_arguments)]
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

/// Percent-encode a single character that requires escaping in a URL query component.
fn percent_encode_char(c: char, out: &mut String) {
    use std::fmt::Write;
    for byte in c.to_string().as_bytes() {
        let _ = write!(out, "%{byte:02X}");
    }
}

/// Minimal URL encoding for query parameters.
pub(crate) fn urlencoded(s: &str) -> String {
    let s = s.trim();
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ' ' => out.push('+'),
            '&' | '=' | '?' | '#' | '+' | '%' => percent_encode_char(c, &mut out),
            _ => out.push(c),
        }
    }
    out
}

// ===========================================================================
// Shared test support
// ===========================================================================

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use crate::http::HttpClient;
    use skillfile_core::error::SkillfileError;

    /// Sequential mock client: returns responses in FIFO order.
    ///
    /// Each call to `get_bytes` pops the next response. An `Err` variant
    /// simulates a network failure.
    pub(crate) struct MockClient {
        responses: Mutex<VecDeque<Result<String, String>>>,
        post_responses: Mutex<VecDeque<Result<String, String>>>,
    }

    impl MockClient {
        pub fn new(responses: Vec<Result<String, String>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                post_responses: Mutex::new(VecDeque::new()),
            }
        }

        pub fn with_post_responses(mut self, post_responses: Vec<Result<String, String>>) -> Self {
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
}

// ===========================================================================
// Tests — aggregation, sorting, utilities
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use test_support::MockClient;

    /// Serializes tests that manipulate the `SKILLHUB_API_KEY` env var.
    static SKILLHUB_ENV_LOCK: Mutex<()> = Mutex::new(());

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
        assert_eq!(urlencoded("docker\n"), "docker");
        assert_eq!(urlencoded("  docker  "), "docker");
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
            "results": [
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

        assert_eq!(resp.items.len(), 2);
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
}
