//! skillhub.club registry implementation.

use serde::Deserialize;

use crate::http::HttpClient;
use skillfile_core::error::SkillfileError;

use super::{Registry, RegistryId, SearchOptions, SearchResponse, SearchResult};

/// Base URL for the skillhub.club search API.
const SKILLHUB_API: &str = "https://www.skillhub.club/api/v1/skills/search";

/// The skillhub.club registry (requires `SKILLHUB_API_KEY` env var).
pub struct SkillhubClub;

#[derive(Deserialize)]
struct ApiResponse {
    results: Option<Vec<ApiResult>>,
    total: Option<usize>,
}

#[derive(Deserialize)]
struct ApiResult {
    name: Option<String>,
    description: Option<String>,
    author: Option<String>,
    github_stars: Option<u32>,
    simple_score: Option<u8>,
    slug: Option<String>,
}

impl Registry for SkillhubClub {
    fn name(&self) -> &'static str {
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

        let api: ApiResponse = serde_json::from_str(&resp_body).map_err(|e| {
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

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use crate::registry::test_support::MockClient;
    use std::sync::Mutex;

    /// Serializes tests that manipulate the `SKILLHUB_API_KEY` env var.
    static SKILLHUB_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn mock_response() -> String {
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

    #[test]
    fn search_parses_response() {
        let _guard = SKILLHUB_ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("SKILLHUB_API_KEY", "test-key-123") };
        let client = MockClient::new(vec![]).with_post_responses(vec![Ok(mock_response())]);
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
        unsafe { std::env::remove_var("SKILLHUB_API_KEY") };
    }

    #[test]
    fn skips_without_api_key() {
        let _guard = SKILLHUB_ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("SKILLHUB_API_KEY") };
        let client = MockClient::new(vec![]);
        let reg = SkillhubClub;
        let resp = reg
            .search(&client, "testing", &SearchOptions::default())
            .unwrap();
        assert_eq!(resp.items.len(), 0);
        assert_eq!(resp.total, 0);
    }
}
