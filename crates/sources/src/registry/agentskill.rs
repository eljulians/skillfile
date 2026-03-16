//! agentskill.sh registry implementation.

use serde::Deserialize;

use crate::http::HttpClient;
use skillfile_core::error::SkillfileError;

use super::{urlencoded, Registry, RegistryId, SearchOptions, SearchResponse, SearchResult};

/// Base URL for the agentskill.sh search API.
const AGENTSKILL_API: &str = "https://agentskill.sh/api/agent/search";

/// The agentskill.sh registry (110K+ skills, public, no auth).
pub struct AgentskillSh;

#[derive(Deserialize)]
struct ApiResponse {
    results: Vec<ApiResult>,
    total: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiResult {
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

fn github_repo_from(owner: Option<&str>, repo: Option<&str>) -> Option<String> {
    match (owner, repo) {
        (Some(o), Some(r)) if !o.is_empty() && !r.is_empty() => Some(format!("{o}/{r}")),
        _ => None,
    }
}

fn map_api_result(r: ApiResult) -> Option<SearchResult> {
    let name = r.name?;
    let owner = r.owner.unwrap_or_default();
    let slug = r.slug.unwrap_or_else(|| format!("{owner}/{name}"));
    let source_repo = github_repo_from(r.github_owner.as_deref(), r.github_repo.as_deref())
        .or_else(|| Some(slug.clone()));
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
}

impl Registry for AgentskillSh {
    fn name(&self) -> &'static str {
        "agentskill.sh"
    }

    fn search(
        &self,
        client: &dyn HttpClient,
        query: &str,
        _opts: &SearchOptions,
    ) -> Result<SearchResponse, SkillfileError> {
        let url = format!("{AGENTSKILL_API}?q={}&limit=100", urlencoded(query));

        let bytes = client
            .get_bytes(&url)
            .map_err(|e| SkillfileError::Network(format!("agentskill.sh search failed: {e}")))?;

        let body = String::from_utf8(bytes).map_err(|e| {
            SkillfileError::Network(format!("invalid UTF-8 in agentskill.sh response: {e}"))
        })?;

        let api: ApiResponse = serde_json::from_str(&body).map_err(|e| {
            SkillfileError::Network(format!("failed to parse agentskill.sh results: {e}"))
        })?;

        let items: Vec<SearchResult> = api.results.into_iter().filter_map(map_api_result).collect();

        Ok(SearchResponse {
            total: api.total.unwrap_or(items.len()),
            items,
        })
    }
}

// ---------------------------------------------------------------------------
// Detail API — fetch GitHub coordinates for a specific skill
// ---------------------------------------------------------------------------

/// GitHub coordinates resolved from the agentskill.sh detail API.
#[derive(Debug, Clone)]
pub struct AgentskillGithubMeta {
    /// GitHub `owner/repo` (e.g. `openclaw/skills`).
    pub source_repo: String,
    /// Path to the skill file within the repo.
    pub source_path: String,
}

/// Base URL for the agentskill.sh skills detail API.
const AGENTSKILL_SKILLS_API: &str = "https://agentskill.sh/api/skills";

#[derive(Deserialize)]
struct DetailResponse {
    data: Option<Vec<DetailResult>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DetailResult {
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
    let api: DetailResponse = serde_json::from_str(&body).ok()?;

    let items = api.data?;
    let slug_lower = slug.to_ascii_lowercase();

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::test_support::MockClient;

    fn mock_response() -> String {
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

    #[test]
    fn search_parses_response() {
        let client = MockClient::new(vec![Ok(mock_response())]);
        let resp =
            super::super::search_with_client(&client, "code review", &SearchOptions::default())
                .unwrap();
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
    fn search_applies_min_score_filter() {
        let client = MockClient::new(vec![Ok(mock_response())]);
        let opts = SearchOptions {
            limit: 10,
            min_score: Some(80),
        };
        let resp = super::super::search_with_client(&client, "code review", &opts).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].name, "code-reviewer");
    }

    #[test]
    fn search_handles_missing_optional_fields() {
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
        let resp =
            super::super::search_with_client(&client, "test", &SearchOptions::default()).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].name, "minimal");
        assert_eq!(resp.items[0].owner, "");
        assert!(resp.items[0].description.is_none());
        assert!(resp.items[0].security_score.is_none());
    }

    #[test]
    fn search_skips_results_without_name() {
        let json = r#"{
            "results": [
                {"slug": "x/y", "name": null, "owner": "x"},
                {"slug": "a/b", "name": "valid", "owner": "a"}
            ],
            "total": 2
        }"#;
        let client = MockClient::new(vec![Ok(json.to_string())]);
        let resp =
            super::super::search_with_client(&client, "test", &SearchOptions::default()).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].name, "valid");
    }

    #[test]
    fn search_returns_error_on_network_failure() {
        let client = MockClient::new(vec![Err("connection refused".to_string())]);
        let result = super::super::search_with_client(&client, "test", &SearchOptions::default());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("search failed"), "got: {err}");
    }

    #[test]
    fn search_returns_error_on_malformed_json() {
        let client = MockClient::new(vec![Ok("not json".to_string())]);
        let result = super::super::search_with_client(&client, "test", &SearchOptions::default());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to parse"), "got: {err}");
    }

    #[test]
    fn search_constructs_url_from_slug() {
        let client = MockClient::new(vec![Ok(mock_response())]);
        let resp =
            super::super::search_with_client(&client, "test", &SearchOptions::default()).unwrap();
        assert_eq!(
            resp.items[0].url,
            "https://agentskill.sh/@alice/code-reviewer"
        );
        assert_eq!(
            resp.items[0].source_repo.as_deref(),
            Some("alice/code-reviewer")
        );
        assert!(resp.items[0].source_path.is_none());
    }

    #[test]
    fn search_uses_github_coordinates_when_present() {
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
        let resp =
            super::super::search_with_client(&client, "fzf", &SearchOptions::default()).unwrap();

        assert_eq!(resp.items.len(), 1);
        assert_eq!(
            resp.items[0].source_repo.as_deref(),
            Some("openclaw/skills")
        );
        assert_eq!(
            resp.items[0].source_path.as_deref(),
            Some("skills/arnarsson/fzf-fuzzy-finder/SKILL.md")
        );
        assert_eq!(
            resp.items[0].url,
            "https://agentskill.sh/@openclaw/fzf-fuzzy-finder"
        );
    }

    // -- Detail API tests -------------------------------------------------------

    struct DetailMockParams<'a> {
        slug: &'a str,
        owner: &'a str,
        repo: &'a str,
        path: &'a str,
    }

    fn detail_mock(p: &DetailMockParams<'_>) -> String {
        let (slug, owner, repo, path) = (p.slug, p.owner, p.repo, p.path);
        format!(
            r#"{{"data": [{{"slug": "{slug}", "githubOwner": "{owner}", "githubRepo": "{repo}", "githubPath": "{path}"}}]}}"#
        )
    }

    #[test]
    fn fetch_github_meta_returns_coordinates() {
        let json = detail_mock(&DetailMockParams {
            slug: "openclaw/fzf-fuzzy-finder",
            owner: "openclaw",
            repo: "skills",
            path: "skills/arnarsson/fzf-fuzzy-finder/SKILL.md",
        });
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
        let json = detail_mock(&DetailMockParams {
            slug: "OpenClaw/FZF-Fuzzy-Finder",
            owner: "openclaw",
            repo: "skills",
            path: "skills/arnarsson/fzf-fuzzy-finder/SKILL.md",
        });
        let client = MockClient::new(vec![Ok(json)]);
        let meta =
            fetch_agentskill_github_meta(&client, "openclaw/fzf-fuzzy-finder", "fzf-fuzzy-finder");
        assert!(meta.is_some());
    }

    #[test]
    fn fetch_github_meta_no_match_returns_none() {
        let json = detail_mock(&DetailMockParams {
            slug: "other/skill",
            owner: "other",
            repo: "repo",
            path: "skill.md",
        });
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
