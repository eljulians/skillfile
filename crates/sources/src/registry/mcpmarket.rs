/// mcpmarket.rs file
use crate::http::HttpClient;
use skillfile_core::error::SkillfileError;

use super::{Registry, RegistryId, SearchQuery, SearchResponse};

/// The mcpmarket.com registry (minimal implementation).
pub struct McpMarket;

impl Registry for McpMarket {
    fn name(&self) -> &'static str {
        "mcpmarket.com"
    }

    fn fetch_skill_content(
        &self,
        _client: &dyn HttpClient,
        _item: &super::SearchResult,
    ) -> Option<String> {
        None
    }

    fn search(&self, _q: &SearchQuery<'_>) -> Result<SearchResponse, SkillfileError> {
        Ok(SearchResponse {
            total: 0,
            items: vec![],
        })
    }
}
