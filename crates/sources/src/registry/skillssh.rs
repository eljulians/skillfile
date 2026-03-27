//! skills.sh registry implementation.

use serde::Deserialize;

use crate::http::HttpClient;
use skillfile_core::error::SkillfileError;

use super::scrape::{html_to_markdown, json_string_end, urlencoded};
use super::{Registry, RegistryId, SearchQuery, SearchResponse, SearchResult};

const SKILLSSH_API: &str = "https://skills.sh/api/search";

/// The skills.sh registry (public, no auth, minimal fields).
pub struct SkillsSh;

const GITHUB_RAW: &str = "https://raw.githubusercontent.com";

#[derive(Deserialize)]
struct ApiResponse {
    skills: Option<Vec<ApiResult>>,
    count: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiResult {
    /// Full identifier: `owner/repo/skillId`.
    id: Option<String>,
    name: Option<String>,
    installs: Option<u32>,
    source: Option<String>,
}

/// Candidate paths for locating SKILL.md in a GitHub repository.
///
/// Most skills.sh entries follow the `skills/{name}/SKILL.md` convention.
/// Falls back to `{name}/SKILL.md` and then root `SKILL.md`.
fn skill_md_urls(repo: &str, name: &str) -> [String; 3] {
    [
        format!("{GITHUB_RAW}/{repo}/HEAD/skills/{name}/SKILL.md"),
        format!("{GITHUB_RAW}/{repo}/HEAD/{name}/SKILL.md"),
        format!("{GITHUB_RAW}/{repo}/HEAD/SKILL.md"),
    ]
}

/// Returns true if `prefix` looks like an RSC flight data header (`22:T46a`).
fn looks_like_rsc_prefix(prefix: &str) -> bool {
    let Some(colon) = prefix.find(':') else {
        return false;
    };
    let (id, rest) = (&prefix[..colon], &prefix[colon + 1..]);
    if id.is_empty() || rest.is_empty() {
        return false;
    }
    id.bytes().all(|b| b.is_ascii_digit())
        && rest.as_bytes()[0].is_ascii_alphabetic()
        && rest[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Strip RSC flight data header from a decoded chunk.
///
/// RSC chunks have the format `{id}:{type}{hex_size},{content}`.
/// Example: `22:T46a,<h1>Kubernetes...` → `<h1>Kubernetes...`
fn strip_rsc_header(s: &str) -> &str {
    let Some(comma) = s.find(',') else { return s };
    let prefix = &s[..comma];
    if prefix.len() < 20 && looks_like_rsc_prefix(prefix) {
        &s[comma + 1..]
    } else {
        s
    }
}

/// Extract rendered skill content from a skills.sh Next.js RSC page.
///
/// The page streams content via `self.__next_f.push([1, "..."])` chunks.
/// The main content chunk contains the SKILL.md rendered as HTML. We
/// convert it to approximate markdown so the TUI preview can style it.
fn extract_rsc_content(html: &str) -> Option<String> {
    let prefix = "self.__next_f.push([1,";
    let mut pos = 0;
    while let Some(offset) = html[pos..].find(prefix) {
        let json_start = pos + offset + prefix.len();
        pos = json_start + 1;
        let Some(end) = json_string_end(&html[json_start..]) else {
            continue;
        };
        let Ok(decoded) = serde_json::from_str::<String>(&html[json_start..json_start + end])
        else {
            continue;
        };
        let content = strip_rsc_header(&decoded);
        if !content.contains("<h1>") && !content.contains("<h2>") {
            continue;
        }
        let text = html_to_markdown(content);
        if text.len() > 50 {
            return Some(text);
        }
    }
    None
}

/// Fetch skill content by scraping the skills.sh page directly.
///
/// Fallback when raw GitHub paths fail (repo uses non-standard layout).
fn scrape_skill_page(client: &dyn HttpClient, url: &str) -> Option<String> {
    let bytes = client.get_bytes(url).ok()?;
    let html = String::from_utf8(bytes).ok()?;
    extract_rsc_content(&html)
}

fn map_api_result(r: ApiResult) -> Option<SearchResult> {
    let name = r.name?;
    // skills.sh `source` field is `owner/repo` (GitHub coordinates)
    let source_repo = r.source;
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
}

impl Registry for SkillsSh {
    fn name(&self) -> &'static str {
        "skills.sh"
    }

    fn fetch_skill_content(&self, client: &dyn HttpClient, item: &SearchResult) -> Option<String> {
        // Fast path: try raw GitHub URLs (gives raw markdown).
        if let Some(md) = item.source_repo.as_deref().and_then(|repo| {
            skill_md_urls(repo, &item.name)
                .iter()
                .find_map(|url| String::from_utf8(client.get_bytes(url).ok()?).ok())
        }) {
            return Some(md);
        }
        // Fallback: scrape rendered HTML from the skills.sh page.
        scrape_skill_page(client, &item.url)
    }

    fn search(&self, q: &SearchQuery<'_>) -> Result<SearchResponse, SkillfileError> {
        let (client, query) = (q.client, q.query);
        let url = format!("{SKILLSSH_API}?q={}", urlencoded(query));

        let bytes = client
            .get_bytes(&url)
            .map_err(|e| SkillfileError::Network(format!("skills.sh search failed: {e}")))?;

        let body = String::from_utf8(bytes).map_err(|e| {
            SkillfileError::Network(format!("invalid UTF-8 in skills.sh response: {e}"))
        })?;

        let api: ApiResponse = serde_json::from_str(&body).map_err(|e| {
            SkillfileError::Network(format!("failed to parse skills.sh results: {e}"))
        })?;

        let results = api.skills.unwrap_or_default();
        let items: Vec<SearchResult> = results.into_iter().filter_map(map_api_result).collect();

        Ok(SearchResponse {
            total: api.count.unwrap_or(items.len()),
            items,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::test_support::MockClient;
    use crate::registry::{SearchOptions, SearchQuery};

    fn mock_response() -> String {
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

    #[test]
    fn search_parses_response() {
        let client = MockClient::new(vec![Ok(mock_response())]);
        let reg = SkillsSh;
        let resp = reg
            .search(&SearchQuery {
                client: &client,
                query: "docker",
                opts: &SearchOptions::default(),
            })
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
    fn search_returns_all_results() {
        let client = MockClient::new(vec![Ok(mock_response())]);
        let reg = SkillsSh;
        let opts = SearchOptions {
            limit: 1,
            min_score: None,
        };
        // Per-registry search returns all results; limit is applied globally by post_process.
        let resp = reg
            .search(&SearchQuery {
                client: &client,
                query: "docker",
                opts: &opts,
            })
            .unwrap();
        assert_eq!(resp.items.len(), 2);
    }

    #[test]
    fn search_handles_empty_results() {
        let json = r#"{"skills": [], "count": 0}"#;
        let client = MockClient::new(vec![Ok(json.to_string())]);
        let reg = SkillsSh;
        let resp = reg
            .search(&SearchQuery {
                client: &client,
                query: "nonexistent",
                opts: &SearchOptions::default(),
            })
            .unwrap();
        assert_eq!(resp.items.len(), 0);
        assert_eq!(resp.total, 0);
    }

    // -- fetch_skill_content tests (GitHub raw fetch with fallback) -------------

    fn make_search_result(name: &str, repo: Option<&str>) -> SearchResult {
        SearchResult {
            name: name.into(),
            owner: String::new(),
            description: None,
            security_score: None,
            stars: None,
            url: format!("https://skills.sh/owner/{name}/{name}"),
            registry: RegistryId::SkillsSh,
            source_repo: repo.map(String::from),
            source_path: None,
        }
    }

    #[test]
    fn fetch_skill_content_from_github_raw() {
        let md = "---\nname: docker-helper\n---\n# Docker Helper";
        let client = MockClient::new(vec![Ok(md.into())]);
        let item = make_search_result("docker-helper", Some("dockerfan/docker-helper"));
        let result = SkillsSh.fetch_skill_content(&client, &item);
        assert_eq!(result.as_deref(), Some(md));
    }

    #[test]
    fn fetch_skill_content_fallback_paths() {
        let md = "---\nname: test\n---";
        let client = MockClient::new(vec![
            Err("404".into()), // skills/{name}/SKILL.md
            Ok(md.into()),     // {name}/SKILL.md
        ]);
        let item = make_search_result("test-skill", Some("owner/repo"));
        let result = SkillsSh.fetch_skill_content(&client, &item);
        assert_eq!(result.as_deref(), Some(md));
    }

    #[test]
    fn fetch_skill_content_falls_through_to_root() {
        let md = "# Root SKILL.md";
        let client = MockClient::new(vec![
            Err("404".into()), // skills/{name}/SKILL.md
            Err("404".into()), // {name}/SKILL.md
            Ok(md.into()),     // SKILL.md (root)
        ]);
        let item = make_search_result("mono", Some("owner/mono-repo"));
        let result = SkillsSh.fetch_skill_content(&client, &item);
        assert_eq!(result.as_deref(), Some(md));
    }

    #[test]
    fn fetch_skill_content_without_source_repo_tries_page_scrape() {
        // No source_repo → skip GitHub raw paths, attempt page scrape.
        let client = MockClient::new(vec![Err("404".into())]); // page scrape fails
        let item = make_search_result("orphan", None);
        assert!(SkillsSh.fetch_skill_content(&client, &item).is_none());
    }

    #[test]
    fn fetch_skill_content_returns_none_when_all_paths_fail() {
        let client = MockClient::new(vec![
            Err("404".into()), // skills/{name}/SKILL.md
            Err("404".into()), // {name}/SKILL.md
            Err("404".into()), // SKILL.md
            Err("404".into()), // page scrape
        ]);
        let item = make_search_result("gone", Some("owner/repo"));
        assert!(SkillsSh.fetch_skill_content(&client, &item).is_none());
    }

    #[test]
    fn fetch_skill_content_falls_back_to_page_scrape() {
        let rsc_page = r#"<html><script>self.__next_f.push([1,"\u003ch1\u003eKubernetes Operations\u003c/h1\u003e\n\u003cp\u003eExpert knowledge for Kubernetes cluster management, deployment, and troubleshooting.\u003c/p\u003e"])</script></html>"#;
        let client = MockClient::new(vec![
            Err("404".into()),   // skills/{name}/SKILL.md
            Err("404".into()),   // {name}/SKILL.md
            Err("404".into()),   // SKILL.md
            Ok(rsc_page.into()), // page scrape succeeds
        ]);
        let item = make_search_result("k8s-ops", Some("owner/repo"));
        let result = SkillsSh.fetch_skill_content(&client, &item);
        let text = result.expect("should extract from page");
        assert!(
            text.contains("Kubernetes Operations"),
            "missing title: {text}"
        );
        assert!(text.contains("deployment"), "missing body: {text}");
    }

    // -- RSC extraction unit tests ---------------------------------------------

    #[test]
    fn extract_rsc_content_parses_html_chunk() {
        let html = r#"self.__next_f.push([1,"\u003ch1\u003eKubernetes Operations\u003c/h1\u003e\n\u003cp\u003eExpert knowledge for Kubernetes cluster management and troubleshooting.\u003c/p\u003e"])"#;
        let result = extract_rsc_content(html).expect("should parse");
        assert!(result.contains("Kubernetes Operations"));
        assert!(result.contains("Expert knowledge"));
        assert!(!result.contains("<h1>"), "tags should be stripped");
    }

    #[test]
    fn extract_rsc_content_skips_non_content_chunks() {
        let html = r#"self.__next_f.push([1,"$Sreact.fragment"])self.__next_f.push([1,"\u003ch2\u003eReal Content\u003c/h2\u003e\n\u003cp\u003eThis is the actual skill description with enough detail to pass the length check.\u003c/p\u003e"])"#;
        let result = extract_rsc_content(html).expect("should find content chunk");
        assert!(result.contains("Real Content"));
    }

    #[test]
    fn extract_rsc_content_returns_none_without_html() {
        let html = r#"self.__next_f.push([1,"just text no tags"])"#;
        assert!(extract_rsc_content(html).is_none());
    }

    #[test]
    fn strip_rsc_header_removes_prefix() {
        assert_eq!(strip_rsc_header("22:T46a,<h1>Title</h1>"), "<h1>Title</h1>");
    }

    #[test]
    fn strip_rsc_header_preserves_plain_text() {
        assert_eq!(strip_rsc_header("no prefix here"), "no prefix here");
    }

    #[test]
    fn strip_rsc_header_preserves_non_rsc_comma() {
        assert_eq!(strip_rsc_header("hello, world"), "hello, world");
    }

    #[test]
    fn extract_rsc_strips_flight_header() {
        let html = r#"self.__next_f.push([1,"22:T46a,\u003ch1\u003eKubernetes\u003c/h1\u003e\n\u003cp\u003eExpert knowledge for managing clusters and deployments.\u003c/p\u003e"])"#;
        let result = extract_rsc_content(html).expect("should parse");
        assert!(
            !result.starts_with("22:T"),
            "RSC header should be stripped: {result}"
        );
        assert!(result.contains("Kubernetes"), "missing content: {result}");
    }

    #[test]
    fn extract_rsc_preserves_markdown_structure() {
        let html = r#"self.__next_f.push([1,"\u003ch1\u003eKubernetes\u003c/h1\u003e\u003ch2\u003eQuick Start\u003c/h2\u003e\u003cul\u003e\u003cli\u003eFirst step\u003c/li\u003e\u003cli\u003eSecond step\u003c/li\u003e\u003c/ul\u003e"])"#;
        let result = extract_rsc_content(html).expect("should parse");
        assert!(result.contains("# Kubernetes"), "missing h1: {result}");
        assert!(result.contains("## Quick Start"), "missing h2: {result}");
        assert!(result.contains("- First step"), "missing list: {result}");
    }
}
