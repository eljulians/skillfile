//! skills.sh registry implementation.

use serde::Deserialize;

use skillfile_core::error::SkillfileError;

use super::{urlencoded, Registry, RegistryId, SearchQuery, SearchResponse, SearchResult};

/// Base URL for the skills.sh search API.
const SKILLSSH_API: &str = "https://skills.sh/api/search";

/// The skills.sh registry (public, no auth, minimal fields).
pub struct SkillsSh;

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

impl Registry for SkillsSh {
    fn name(&self) -> &'static str {
        "skills.sh"
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
}
