//! skillsmp.com registry implementation.

use serde::Deserialize;

use skillfile_core::error::SkillfileError;

use super::scrape::urlencoded;
use super::{Registry, RegistryId, SearchQuery, SearchResponse, SearchResult};

const SKILLSMP_API: &str = "https://skillsmp.com/api/v1/skills/search";
const API_KEY_ENV: &str = "SKILLSMP_API_KEY";
/// Largest page size the API accepts.
const MAX_LIMIT: usize = 50;

/// The skillsmp.com registry (public; `SKILLSMP_API_KEY` raises the rate limit).
pub struct SkillsmpCom;

#[derive(Deserialize)]
struct ApiResponse {
    data: ApiData,
}

#[derive(Deserialize)]
struct ApiData {
    skills: Vec<ApiSkill>,
    pagination: Option<ApiPagination>,
}

#[derive(Deserialize)]
struct ApiPagination {
    total: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiSkill {
    name: Option<String>,
    author: Option<String>,
    description: Option<String>,
    stars: Option<u32>,
    github_url: Option<String>,
    skill_url: Option<String>,
}

fn api_key() -> Option<String> {
    std::env::var(API_KEY_ENV).ok().filter(|k| !k.is_empty())
}

/// Split a GitHub URL into `owner/repo` and the in-repo path, if any.
///
/// `https://github.com/o/r/tree/main/skills/x` → `("o/r", Some("skills/x"))`.
/// The ref is assumed to be a single path segment.
fn parse_github_url(url: &str) -> Option<(String, Option<String>)> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.trim_end_matches('/').splitn(5, '/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    let path = match (parts.next(), parts.next(), parts.next()) {
        (Some("tree" | "blob"), Some(_), Some(path)) => Some(path.to_string()),
        _ => None,
    };
    Some((format!("{owner}/{repo}"), path))
}

fn map_skill(s: ApiSkill) -> Option<SearchResult> {
    let name = s.name?;
    let url = s.skill_url.or_else(|| s.github_url.clone())?;
    let (source_repo, source_path) = s
        .github_url
        .as_deref()
        .and_then(parse_github_url)
        .map_or((None, None), |(repo, path)| (Some(repo), path));
    Some(SearchResult {
        name,
        owner: s.author.unwrap_or_default(),
        description: s.description,
        security_score: None,
        stars: s.stars,
        url,
        registry: RegistryId::SkillsmpCom,
        source_repo,
        source_path,
    })
}

fn search_with_key(
    q: &SearchQuery<'_>,
    key: Option<&str>,
) -> Result<SearchResponse, SkillfileError> {
    let url = format!(
        "{SKILLSMP_API}?q={}&limit={}&sortBy=stars",
        urlencoded(q.query),
        q.opts.limit.min(MAX_LIMIT)
    );
    let bytes = match key {
        Some(key) => q.client.get_bytes_with_bearer(&url, key)?,
        None => q.client.get_bytes(&url)?,
    };
    let body = String::from_utf8(bytes).map_err(|e| {
        SkillfileError::Network(format!("invalid UTF-8 in skillsmp.com response: {e}"))
    })?;
    let api: ApiResponse = serde_json::from_str(&body).map_err(|e| {
        SkillfileError::Network(format!("failed to parse skillsmp.com results: {e}"))
    })?;
    let items: Vec<SearchResult> = api.data.skills.into_iter().filter_map(map_skill).collect();
    let total = api.data.pagination.map_or(items.len(), |p| p.total);
    Ok(SearchResponse { items, total })
}

impl Registry for SkillsmpCom {
    fn name(&self) -> &'static str {
        "skillsmp.com"
    }

    fn search(&self, q: &SearchQuery<'_>) -> Result<SearchResponse, SkillfileError> {
        search_with_key(q, api_key().as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::test_support::MockClient;
    use crate::registry::SearchOptions;

    const RESPONSE: &str = r#"{
        "success": true,
        "data": {
            "skills": [
                {
                    "id": "acme-terraform-deploy",
                    "name": "terraform-deploy",
                    "author": "acme",
                    "description": "Deploy infrastructure with Terraform",
                    "stars": 42,
                    "updatedAt": 1700000000,
                    "githubUrl": "https://github.com/acme/infra-skills/tree/main/skills/terraform-deploy",
                    "skillUrl": "https://skillsmp.com/skills/acme-terraform-deploy"
                },
                {
                    "name": "root-skill",
                    "author": "bob",
                    "githubUrl": "https://github.com/bob/root-skill"
                },
                { "author": "nameless" }
            ],
            "pagination": { "total": 3, "page": 1, "totalPages": 1 }
        }
    }"#;

    fn run(
        client: &MockClient,
        query: &str,
        limit: usize,
    ) -> Result<SearchResponse, SkillfileError> {
        let opts = SearchOptions {
            limit,
            ..SearchOptions::default()
        };
        search_with_key(
            &SearchQuery {
                client,
                query,
                opts: &opts,
            },
            Some("sk_live_test"),
        )
    }

    #[test]
    fn search_maps_skills_and_drops_nameless() {
        let client = MockClient::new(vec![Ok(RESPONSE.to_string())]);
        let resp = run(&client, "terraform", 20).unwrap();

        let full = &resp.items[0];
        assert_eq!(full.name, "terraform-deploy");
        assert_eq!(full.owner, "acme");
        assert_eq!(
            full.description.as_deref(),
            Some("Deploy infrastructure with Terraform")
        );
        assert_eq!(full.stars, Some(42));
        assert_eq!(full.security_score, None);
        assert_eq!(
            full.url,
            "https://skillsmp.com/skills/acme-terraform-deploy"
        );
        assert_eq!(full.registry, RegistryId::SkillsmpCom);
        assert_eq!(full.source_repo.as_deref(), Some("acme/infra-skills"));
        assert_eq!(full.source_path.as_deref(), Some("skills/terraform-deploy"));
    }

    #[test]
    fn search_falls_back_to_github_url_for_sparse_skill() {
        let client = MockClient::new(vec![Ok(RESPONSE.to_string())]);
        let resp = run(&client, "terraform", 20).unwrap();

        assert_eq!(resp.total, 3);
        assert_eq!(resp.items.len(), 2);
        let sparse = &resp.items[1];
        assert_eq!(sparse.url, "https://github.com/bob/root-skill");
        assert_eq!(sparse.source_repo.as_deref(), Some("bob/root-skill"));
        assert_eq!(sparse.source_path, None);
        assert_eq!(sparse.description, None);
    }

    #[test]
    fn search_sends_bearer_token_and_caps_limit() {
        let client = MockClient::new(vec![Ok(RESPONSE.to_string())]);
        run(&client, "code review", 100).unwrap();

        assert_eq!(
            client.bearer_gets(),
            vec![(
                "https://skillsmp.com/api/v1/skills/search?q=code+review&limit=50&sortBy=stars"
                    .to_string(),
                "sk_live_test".to_string(),
            )]
        );
    }

    #[test]
    fn search_without_key_sends_no_bearer() {
        let client = MockClient::new(vec![Ok(RESPONSE.to_string())]);
        let opts = SearchOptions::default();
        let query = SearchQuery {
            client: &client,
            query: "terraform",
            opts: &opts,
        };
        let resp = search_with_key(&query, None).unwrap();

        assert_eq!(resp.items.len(), 2);
        assert!(client.bearer_gets().is_empty());
    }

    #[test]
    fn search_rejects_missing_envelope() {
        let client = MockClient::new(vec![Ok(r#"{"error": "nope"}"#.to_string())]);
        let result = run(&client, "x", 20);

        assert!(matches!(
            result,
            Err(SkillfileError::Network(msg)) if msg.contains("skillsmp.com")
        ));
    }

    #[test]
    fn parse_github_url_cases() {
        assert_eq!(
            parse_github_url("https://github.com/o/r/tree/main/skills/x"),
            Some(("o/r".to_string(), Some("skills/x".to_string())))
        );
        assert_eq!(
            parse_github_url("https://github.com/o/r/blob/main/a/SKILL.md"),
            Some(("o/r".to_string(), Some("a/SKILL.md".to_string())))
        );
        assert_eq!(
            parse_github_url("https://github.com/o/r/"),
            Some(("o/r".to_string(), None))
        );
        assert_eq!(
            parse_github_url("https://github.com/o/r/tree/main"),
            Some(("o/r".to_string(), None))
        );
        assert_eq!(parse_github_url("https://github.com/o"), None);
        assert_eq!(parse_github_url("https://gitlab.com/o/r"), None);
    }
}
