//! agentskill.sh registry implementation.

use serde::Deserialize;

use crate::http::HttpClient;
use skillfile_core::error::SkillfileError;

use super::scrape::urlencoded;
use super::{Registry, RegistryId, SearchQuery, SearchResponse, SearchResult};

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
    let source_repo = github_repo_from(r.github_owner.as_deref(), r.github_repo.as_deref());
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

/// Extract the JSON body from the first `__NUXT_DATA__` `<script>` tag.
fn extract_nuxt_json(html: &str) -> Option<&str> {
    let marker = r#"id="__NUXT_DATA__""#;
    let tag_start = html.find(marker)?;
    let after = &html[tag_start..];
    let tag_end = after.find('>')?;
    let content = &html[tag_start + tag_end + 1..];
    let end = content.find("</script>")?;
    Some(content[..end].trim())
}

/// Resolve the `skillMd` value from a Nuxt hydration data array.
///
/// The `__NUXT_DATA__` array contains objects whose values are numeric
/// indices into the same array. Find any object with a `"skillMd"` key,
/// read its numeric value, and index back into the array to get the
/// raw markdown string.
fn extract_skill_md(data: &[serde_json::Value]) -> Option<String> {
    let ref_idx = data
        .iter()
        .find_map(|v| v.as_object()?.get("skillMd")?.as_u64())?;
    let idx = usize::try_from(ref_idx).ok()?;
    data.get(idx)?.as_str().map(String::from)
}

impl Registry for AgentskillSh {
    fn name(&self) -> &'static str {
        "agentskill.sh"
    }

    fn fetch_skill_content(&self, client: &dyn HttpClient, item: &SearchResult) -> Option<String> {
        let bytes = client.get_bytes(&item.url).ok()?;
        let html = String::from_utf8(bytes).ok()?;
        let json_str = extract_nuxt_json(&html)?;
        let data: Vec<serde_json::Value> = serde_json::from_str(json_str).ok()?;
        extract_skill_md(&data)
    }

    fn search(&self, q: &SearchQuery<'_>) -> Result<SearchResponse, SkillfileError> {
        let (client, query) = (q.client, q.query);
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
    pub source_path: String,
}

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

/// Scrape GitHub coordinates from an agentskill.sh skill page.
///
/// Fallback for when [`fetch_agentskill_github_meta`] can't find the slug
/// in the detail API search results. The skill page embeds `githubOwner`,
/// `githubRepo`, and `githubPath` in its Nuxt hydration data.
pub fn scrape_github_meta_from_page(
    client: &dyn HttpClient,
    slug: &str,
) -> Option<AgentskillGithubMeta> {
    let url = format!("https://agentskill.sh/@{slug}");
    let bytes = client.get_bytes(&url).ok()?;
    let html = String::from_utf8(bytes).ok()?;
    let source_repo = extract_repo_from_html(&html)?;
    let source_path = extract_path_from_html(&html).unwrap_or_default();
    Some(AgentskillGithubMeta {
        source_repo,
        source_path,
    })
}

/// Parse the first `github.com/{owner}/{repo}` URL from Nuxt-rendered HTML.
fn extract_repo_from_html(html: &str) -> Option<String> {
    extract_repo_nuxt(html).or_else(|| extract_repo_plain(html))
}

/// Parse the `githubPath` from Nuxt-rendered HTML.
///
/// Looks for a Nuxt-escaped path ending in `SKILL.md`.
fn extract_path_from_html(html: &str) -> Option<String> {
    // Nuxt format: "skills\u002Fauthor\u002Fname\u002FSKILL.md"
    let marker = r"\u002FSKILL.md";
    if let Some(end) = html.find(marker) {
        let before = &html[..end];
        let quote = before.rfind('"')?;
        let raw = &html[quote + 1..end + marker.len()];
        return Some(raw.replace(r"\u002F", "/"));
    }
    None
}

fn extract_repo_nuxt(html: &str) -> Option<String> {
    let marker = r"github.com\u002F";
    let sep = r"\u002F";
    let pos = html.find(marker)?;
    let after = &html[pos + marker.len()..];
    let owner_end = after.find(sep)?;
    let owner = &after[..owner_end];
    let after_owner = &after[owner_end + sep.len()..];
    let repo_end = after_owner.find(['"', '\\'])?;
    let repo = &after_owner[..repo_end];
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn extract_repo_plain(html: &str) -> Option<String> {
    let marker = "github.com/";
    for (i, _) in html.match_indices(marker) {
        let after = &html[i + marker.len()..];
        let Some(owner_end) = after.find('/') else {
            continue;
        };
        let owner = &after[..owner_end];
        let after_owner = &after[owner_end + 1..];
        let Some(repo_end) =
            after_owner.find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_' && c != '.')
        else {
            continue;
        };
        let repo = &after_owner[..repo_end];
        if !owner.is_empty() && !repo.is_empty() && owner != "avatars" {
            return Some(format!("{owner}/{repo}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::test_support::MockClient;
    use crate::registry::SearchOptions;

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
        // No GitHub coordinates in the mock → source_repo must be None.
        // The slug is in the URL, not source_repo.
        assert!(resp.items[0].source_repo.is_none());
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

    // -- Slug-as-source_repo regression tests ----------------------------------

    /// Regression: when the search API returns a result WITHOUT explicit GitHub
    /// coordinates (`githubOwner`, `githubRepo`, `githubPath` are all null),
    /// `source_repo` must NOT be set to the registry slug.
    ///
    /// The slug (e.g. `openclaw/k8s`) is a registry identifier, not a GitHub
    /// `owner/repo`. Treating it as one causes the Tree API to fail downstream,
    /// leading to a confusing "Path in repo:" prompt instead of a clear error.
    /// Regression: slug must NOT leak into `source_repo` when GitHub
    /// coordinates are absent from the search API response.
    #[test]
    fn map_result_without_github_coords_does_not_use_slug_as_source_repo() {
        let json = r#"{
            "results": [{
                "slug": "openclaw/k8s-config-gen",
                "name": "k8s-config-gen",
                "owner": "openclaw",
                "description": "Kubernetes config generator",
                "securityScore": 80,
                "githubStars": 100
            }],
            "total": 1
        }"#;
        let client = MockClient::new(vec![Ok(json.to_string())]);
        let resp =
            super::super::search_with_client(&client, "k8s", &SearchOptions::default()).unwrap();

        assert_eq!(resp.items.len(), 1);
        assert!(
            resp.items[0].source_repo.is_none(),
            "source_repo should be None when GitHub coords are missing, \
             got {:?} (slug leaked into source_repo)",
            resp.items[0].source_repo
        );
    }

    // -- Page scrape tests -------------------------------------------------------

    #[test]
    fn extract_repo_from_nuxt_escaped_html() {
        let html = r#"some stuff "https:\u002F\u002Fgithub.com\u002Fopenclaw\u002Fskills" more"#;
        assert_eq!(
            extract_repo_from_html(html).as_deref(),
            Some("openclaw/skills")
        );
    }

    #[test]
    fn extract_repo_from_plain_html() {
        let html = r#"<a href="https://github.com/openclaw/skills/tree/main">repo</a>"#;
        assert_eq!(
            extract_repo_from_html(html).as_deref(),
            Some("openclaw/skills")
        );
    }

    #[test]
    fn extract_repo_skips_avatar_urls() {
        let html =
            "https://avatars.githubusercontent.com/u/12345 https://github.com/real/repo stuff";
        assert_eq!(extract_repo_from_html(html).as_deref(), Some("real/repo"));
    }

    #[test]
    fn extract_repo_returns_none_for_no_github_url() {
        let html = "<html><body>no github links here</body></html>";
        assert!(extract_repo_from_html(html).is_none());
    }

    #[test]
    fn extract_repo_handles_hyphenated_names() {
        let html = r#""https:\u002F\u002Fgithub.com\u002Falphaonedev\u002Fopenclaw-graph""#;
        assert_eq!(
            extract_repo_from_html(html).as_deref(),
            Some("alphaonedev/openclaw-graph")
        );
    }

    #[test]
    fn extract_repo_plain_skips_malformed_first_match() {
        let html = "github.com/broken https://github.com/real/repo end";
        assert_eq!(extract_repo_from_html(html).as_deref(), Some("real/repo"));
    }

    #[test]
    fn extract_path_from_nuxt_html() {
        let html = r#"stuff "skills\u002Fivangdavila\u002Fk8s\u002FSKILL.md" more"#;
        assert_eq!(
            extract_path_from_html(html).as_deref(),
            Some("skills/ivangdavila/k8s/SKILL.md")
        );
    }

    #[test]
    fn extract_path_returns_none_when_missing() {
        let html = "<html>no skill path here</html>";
        assert!(extract_path_from_html(html).is_none());
    }

    #[test]
    fn scrape_page_returns_full_meta_from_mock_html() {
        let html = r#"<html>"https:\u002F\u002Fgithub.com\u002Fopenclaw\u002Fskills" and "skills\u002Fivangdavila\u002Fk8s\u002FSKILL.md"</html>"#;
        let client = MockClient::new(vec![Ok(html.to_string())]);
        let meta = scrape_github_meta_from_page(&client, "openclaw/k8s");
        let meta = meta.expect("should return meta");
        assert_eq!(meta.source_repo, "openclaw/skills");
        assert_eq!(meta.source_path, "skills/ivangdavila/k8s/SKILL.md");
    }

    #[test]
    fn scrape_page_returns_repo_only_when_no_path() {
        let html = r#"<html>"https:\u002F\u002Fgithub.com\u002Fopenclaw\u002Fskills"</html>"#;
        let client = MockClient::new(vec![Ok(html.to_string())]);
        let meta = scrape_github_meta_from_page(&client, "openclaw/k8s");
        let meta = meta.expect("should return meta with empty path");
        assert_eq!(meta.source_repo, "openclaw/skills");
        assert!(meta.source_path.is_empty());
    }

    #[test]
    fn scrape_page_returns_none_on_network_error() {
        let client = MockClient::new(vec![Err("connection refused".to_string())]);
        let meta = scrape_github_meta_from_page(&client, "openclaw/k8s");
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

    // -- fetch_skill_content tests (Nuxt __NUXT_DATA__ scraping) ---------------

    fn make_nuxt_html(nuxt_data: &serde_json::Value) -> String {
        format!(
            r#"<html><head><script id="__NUXT_DATA__" type="application/json">{nuxt_data}</script></head></html>"#
        )
    }

    fn make_search_result(url: &str) -> SearchResult {
        SearchResult {
            name: "test-skill".into(),
            owner: "test".into(),
            description: None,
            security_score: None,
            stars: None,
            url: url.into(),
            registry: RegistryId::AgentskillSh,
            source_repo: None,
            source_path: None,
        }
    }

    #[test]
    fn fetch_skill_content_extracts_from_nuxt_payload() {
        let md = "---\nname: test-skill\n---\n# Test Skill\nBody.";
        // Real format: an object has {"skillMd": N} where data[N] is the markdown.
        let data = serde_json::json!(["padding", {"skillMd": 2}, md]);
        let html = make_nuxt_html(&data);
        let client = MockClient::new(vec![Ok(html)]);
        let item = make_search_result("https://agentskill.sh/@test/test-skill");
        let result = AgentskillSh.fetch_skill_content(&client, &item);
        assert_eq!(result.as_deref(), Some(md));
    }

    #[test]
    fn fetch_skill_content_returns_none_on_missing_payload() {
        let html = "<html><body>No Nuxt data</body></html>";
        let client = MockClient::new(vec![Ok(html.into())]);
        let item = make_search_result("https://agentskill.sh/@test/x");
        assert!(AgentskillSh.fetch_skill_content(&client, &item).is_none());
    }

    #[test]
    fn fetch_skill_content_returns_none_on_network_error() {
        let client = MockClient::new(vec![Err("refused".into())]);
        let item = make_search_result("https://agentskill.sh/@test/x");
        assert!(AgentskillSh.fetch_skill_content(&client, &item).is_none());
    }

    #[test]
    fn fetch_skill_content_returns_none_on_missing_skill_md_key() {
        let data = serde_json::json!(["no", "skillMd_key", "here"]);
        let html = make_nuxt_html(&data);
        let client = MockClient::new(vec![Ok(html)]);
        let item = make_search_result("https://agentskill.sh/@test/x");
        assert!(AgentskillSh.fetch_skill_content(&client, &item).is_none());
    }

    #[test]
    fn fetch_skill_content_returns_none_on_invalid_json() {
        let html = r#"<html><script id="__NUXT_DATA__">not json</script></html>"#;
        let client = MockClient::new(vec![Ok(html.into())]);
        let item = make_search_result("https://agentskill.sh/@test/x");
        assert!(AgentskillSh.fetch_skill_content(&client, &item).is_none());
    }

    #[test]
    fn extract_nuxt_json_handles_attributes_order() {
        let html = r#"<script type="application/json" id="__NUXT_DATA__">["a","b"]</script>"#;
        let json = extract_nuxt_json(html);
        assert_eq!(json, Some(r#"["a","b"]"#));
    }

    #[test]
    fn extract_skill_md_returns_none_when_ref_out_of_bounds() {
        let data = vec![serde_json::json!({"skillMd": 999})];
        assert!(extract_skill_md(&data).is_none());
    }
}
