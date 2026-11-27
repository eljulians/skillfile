//! Incremental, multi-registry search.
//!
//! A [`SearchSession`] remembers where each registry left off, so callers can
//! pull results one page at a time: the TUI loads more as the user scrolls,
//! and [`SearchSession::collect`] pulls pages until a result limit is met.
//!
//! The session holds no HTTP client. Each [`SearchSession::next_page`] call
//! takes one, which keeps the session `Send` and cheap to move into a worker
//! thread.

use std::collections::HashSet;

use crate::http::HttpClient;
use skillfile_core::error::SkillfileError;

use super::{
    all_registries, registry_for, sort_by_popularity, Registry, RegistryId, RegistryPage,
    SearchQuery, SearchResponse, SearchResult,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct SessionOptions {
    /// Search only this registry. `None` searches the default set.
    pub registry: Option<RegistryId>,
    /// Minimum security score (0-100). `None` means no filter.
    pub min_score: Option<u8>,
    /// Stop after this many results in total. `None` means no cap.
    pub max_results: Option<usize>,
}

/// One `next_page` worth of new results.
///
/// `failures` lists registries that failed during this call. They are not
/// retried, so a broken registry can't cause a stream of repeated requests,
/// and the remaining registries keep paging.
#[derive(Debug, Default)]
pub struct SearchBatch {
    pub items: Vec<SearchResult>,
    pub failures: Vec<(RegistryId, SkillfileError)>,
}

struct RegistryCursor {
    registry: Box<dyn Registry>,
    next_page: u32,
    exhausted: bool,
    total: usize,
}

impl RegistryCursor {
    fn advance(&mut self, page: &RegistryPage) {
        self.next_page += 1;
        self.exhausted = !page.has_more;
        self.total = page
            .total_hint
            .unwrap_or(self.total + page.items.len())
            .max(self.total + page.items.len());
    }

    /// Apply one fetch outcome. A failure retires the cursor so it is not
    /// retried on later pages.
    fn absorb(
        &mut self,
        result: Result<RegistryPage, SkillfileError>,
    ) -> Result<Vec<SearchResult>, (RegistryId, SkillfileError)> {
        match result {
            Ok(page) => {
                self.advance(&page);
                Ok(page.items)
            }
            Err(e) => {
                self.exhausted = true;
                Err((self.registry.id(), e))
            }
        }
    }
}

pub struct SearchSession {
    query: String,
    min_score: Option<u8>,
    max_results: Option<usize>,
    cursors: Vec<RegistryCursor>,
    /// URLs already returned. Pages can overlap when a registry's index
    /// changes between requests.
    seen: HashSet<String>,
    loaded: usize,
}

impl SearchSession {
    pub fn new(query: &str, opts: &SessionOptions) -> Self {
        let registries = match opts.registry {
            Some(id) => vec![registry_for(id)],
            None => all_registries(),
        };
        Self::with_registries(query, registries, opts)
    }

    pub(crate) fn with_registries(
        query: &str,
        registries: Vec<Box<dyn Registry>>,
        opts: &SessionOptions,
    ) -> Self {
        let cursors = registries
            .into_iter()
            .map(|registry| RegistryCursor {
                registry,
                next_page: 1,
                exhausted: false,
                total: 0,
            })
            .collect();
        Self {
            query: query.to_string(),
            min_score: opts.min_score,
            max_results: opts.max_results,
            cursors,
            seen: HashSet::new(),
            loaded: 0,
        }
    }

    /// Whether another `next_page` call could return results.
    pub fn has_more(&self) -> bool {
        !self.reached_max() && self.cursors.iter().any(|c| !c.exhausted)
    }

    /// Best-known total match count across registries. Some registries only
    /// report a lower bound, so treat this as "at least".
    pub fn total_hint(&self) -> usize {
        let reported: usize = self.cursors.iter().map(|c| c.total).sum();
        reported.max(self.loaded)
    }

    /// Fetch the next page from every registry that still has results.
    ///
    /// Each batch is sorted by popularity and then appended. Earlier batches
    /// are never re-sorted, so rows already on screen don't move.
    ///
    /// # Errors
    ///
    /// Fails only when every registry asked in this call fails. Cursors are
    /// then left untouched, so the same call can be retried. With a single
    /// registry, that registry's own error is returned.
    pub fn next_page(&mut self, client: &dyn HttpClient) -> Result<SearchBatch, SkillfileError> {
        if !self.has_more() {
            return Ok(SearchBatch::default());
        }
        let mut fetched = self.fetch_active(client);
        if fetched.iter().all(|(_, result)| result.is_err()) {
            return Err(all_failed_error(&mut fetched));
        }

        let mut batch = SearchBatch::default();
        let mut raw = Vec::new();
        for (idx, result) in fetched {
            match self.cursors[idx].absorb(result) {
                Ok(items) => raw.extend(items),
                Err(failure) => batch.failures.push(failure),
            }
        }
        batch.items = self.admit(raw);
        Ok(batch)
    }

    /// Pull pages until at least `limit` results are gathered or nothing is
    /// left, then sort everything by popularity and cut it to `limit`.
    ///
    /// Registry failures are reported on stderr as warnings. A failure after
    /// the first page stops paging and returns what was gathered so far.
    ///
    /// # Errors
    ///
    /// Returns the first page's error when no registry responds.
    pub fn collect(
        &mut self,
        client: &dyn HttpClient,
        limit: usize,
    ) -> Result<SearchResponse, SkillfileError> {
        let mut items = Vec::new();
        let mut first_page = true;
        while items.len() < limit && self.has_more() {
            match self.next_page_for_collect(client, first_page)? {
                Some(batch) => items.extend(batch.items),
                None => break,
            }
            first_page = false;
        }
        sort_by_popularity(&mut items);
        let has_more = self.has_more() || items.len() > limit;
        items.truncate(limit);
        Ok(SearchResponse {
            total: self.total_hint().max(items.len()),
            has_more,
            items,
        })
    }

    /// One `collect` step: warns about partial failures, propagates a
    /// first-page error, and turns a later error into `None` (stop paging).
    fn next_page_for_collect(
        &mut self,
        client: &dyn HttpClient,
        first_page: bool,
    ) -> Result<Option<SearchBatch>, SkillfileError> {
        match self.next_page(client) {
            Ok(batch) => {
                warn_failures(&batch.failures);
                Ok(Some(batch))
            }
            Err(e) if first_page => Err(e),
            Err(e) => {
                eprintln!("warning: search stopped early: {e}");
                Ok(None)
            }
        }
    }

    fn fetch_active(
        &self,
        client: &dyn HttpClient,
    ) -> Vec<(usize, Result<RegistryPage, SkillfileError>)> {
        self.cursors
            .iter()
            .enumerate()
            .filter(|(_, cursor)| !cursor.exhausted)
            .map(|(idx, cursor)| {
                let query = SearchQuery {
                    client,
                    query: &self.query,
                    page: cursor.next_page,
                };
                (idx, cursor.registry.search_page(&query))
            })
            .collect()
    }

    /// Apply the score filter, drop duplicates, sort, and enforce `max_results`.
    fn admit(&mut self, mut items: Vec<SearchResult>) -> Vec<SearchResult> {
        if let Some(min) = self.min_score {
            items.retain(|r| r.security_score.unwrap_or(0) >= min);
        }
        items.retain(|r| self.seen.insert(r.url.clone()));
        sort_by_popularity(&mut items);
        if let Some(max) = self.max_results {
            items.truncate(max.saturating_sub(self.loaded));
        }
        self.loaded += items.len();
        items
    }

    fn reached_max(&self) -> bool {
        self.max_results.is_some_and(|max| self.loaded >= max)
    }
}

fn warn_failures(failures: &[(RegistryId, SkillfileError)]) {
    for (registry, e) in failures {
        eprintln!("warning: {registry} search failed: {e}");
    }
}

/// With one registry, surface its own error; with several, the aggregate.
fn all_failed_error(
    fetched: &mut Vec<(usize, Result<RegistryPage, SkillfileError>)>,
) -> SkillfileError {
    if fetched.len() == 1 {
        if let Some((_, Err(e))) = fetched.pop() {
            return e;
        }
    }
    SkillfileError::Network("all registry searches failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::test_support::MockClient;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Registry double that serves canned pages, so these tests don't depend
    /// on any real registry's JSON format or on HTTP.
    struct FakeRegistry {
        id: RegistryId,
        pages: Mutex<VecDeque<Result<RegistryPage, String>>>,
        requested: std::sync::Arc<Mutex<Vec<u32>>>,
    }

    impl Registry for FakeRegistry {
        fn id(&self) -> RegistryId {
            self.id
        }

        fn search_page(&self, q: &SearchQuery<'_>) -> Result<RegistryPage, SkillfileError> {
            self.requested.lock().unwrap().push(q.page);
            self.pages
                .lock()
                .unwrap()
                .pop_front()
                .expect("FakeRegistry: no more pages")
                .map_err(SkillfileError::Network)
        }
    }

    fn fake(
        id: RegistryId,
        pages: Vec<Result<RegistryPage, String>>,
    ) -> (Box<dyn Registry>, std::sync::Arc<Mutex<Vec<u32>>>) {
        let requested = std::sync::Arc::new(Mutex::new(Vec::new()));
        let reg = FakeRegistry {
            id,
            pages: Mutex::new(pages.into()),
            requested: requested.clone(),
        };
        (Box::new(reg), requested)
    }

    fn item(name: &str, stars: u32) -> SearchResult {
        SearchResult {
            name: name.to_string(),
            owner: String::new(),
            description: None,
            security_score: None,
            stars: Some(stars),
            url: format!("https://example.com/{name}"),
            registry: RegistryId::AgentskillSh,
            source_repo: None,
            source_path: None,
        }
    }

    #[allow(clippy::unnecessary_wraps)] // keeps fixture lists uniform with `Err` entries
    fn page(items: Vec<SearchResult>, has_more: bool) -> Result<RegistryPage, String> {
        Ok(RegistryPage {
            items,
            has_more,
            total_hint: None,
        })
    }

    fn names(items: &[SearchResult]) -> Vec<&str> {
        items.iter().map(|r| r.name.as_str()).collect()
    }

    fn session(registries: Vec<Box<dyn Registry>>, opts: SessionOptions) -> SearchSession {
        SearchSession::with_registries("q", registries, &opts)
    }

    fn no_http() -> MockClient {
        MockClient::new(vec![])
    }

    #[test]
    fn pages_until_registry_reports_no_more() {
        let (reg, requested) = fake(
            RegistryId::AgentskillSh,
            vec![
                page(vec![item("a", 1)], true),
                page(vec![item("b", 1)], false),
            ],
        );
        let mut s = session(vec![reg], SessionOptions::default());
        let client = no_http();

        assert_eq!(names(&s.next_page(&client).unwrap().items), ["a"]);
        assert!(s.has_more());
        assert_eq!(names(&s.next_page(&client).unwrap().items), ["b"]);
        assert!(!s.has_more());
        assert!(s.next_page(&client).unwrap().items.is_empty());
        assert_eq!(*requested.lock().unwrap(), [1, 2]);
    }

    #[test]
    fn exhausted_registries_are_not_asked_again() {
        let (paged, paged_requests) = fake(
            RegistryId::AgentskillSh,
            vec![
                page(vec![item("a", 1)], true),
                page(vec![item("b", 1)], false),
            ],
        );
        let (single, single_requests) =
            fake(RegistryId::SkillsSh, vec![page(vec![item("s", 1)], false)]);
        let mut s = session(vec![paged, single], SessionOptions::default());
        let client = no_http();

        s.next_page(&client).unwrap();
        s.next_page(&client).unwrap();

        assert_eq!(*paged_requests.lock().unwrap(), [1, 2]);
        assert_eq!(*single_requests.lock().unwrap(), [1]);
    }

    #[test]
    fn each_batch_is_sorted_by_popularity() {
        let (reg, _) = fake(
            RegistryId::AgentskillSh,
            vec![page(vec![item("low", 1), item("high", 9)], false)],
        );
        let mut s = session(vec![reg], SessionOptions::default());
        let batch = s.next_page(&no_http()).unwrap();
        assert_eq!(names(&batch.items), ["high", "low"]);
    }

    #[test]
    fn duplicates_across_pages_are_dropped() {
        let (reg, _) = fake(
            RegistryId::AgentskillSh,
            vec![
                page(vec![item("a", 1), item("b", 1)], true),
                page(vec![item("b", 1), item("c", 1)], false),
            ],
        );
        let mut s = session(vec![reg], SessionOptions::default());
        let client = no_http();
        s.next_page(&client).unwrap();
        assert_eq!(names(&s.next_page(&client).unwrap().items), ["c"]);
    }

    #[test]
    fn min_score_filters_each_batch() {
        let mut trusted = item("trusted", 1);
        trusted.security_score = Some(90);
        let mut risky = item("risky", 1);
        risky.security_score = Some(10);
        let (reg, _) = fake(
            RegistryId::AgentskillSh,
            vec![page(vec![trusted, risky, item("unscored", 1)], false)],
        );
        let opts = SessionOptions {
            min_score: Some(50),
            ..SessionOptions::default()
        };
        let mut s = session(vec![reg], opts);
        assert_eq!(names(&s.next_page(&no_http()).unwrap().items), ["trusted"]);
    }

    #[test]
    fn max_results_truncates_and_stops_paging() {
        let (reg, requested) = fake(
            RegistryId::AgentskillSh,
            vec![page(vec![item("a", 3), item("b", 2), item("c", 1)], true)],
        );
        let opts = SessionOptions {
            max_results: Some(2),
            ..SessionOptions::default()
        };
        let mut s = session(vec![reg], opts);
        let client = no_http();

        assert_eq!(names(&s.next_page(&client).unwrap().items), ["a", "b"]);
        assert!(!s.has_more());
        assert!(s.next_page(&client).unwrap().items.is_empty());
        assert_eq!(*requested.lock().unwrap(), [1]);
    }

    #[test]
    fn one_failing_registry_is_reported_and_retired() {
        let (broken, broken_requests) =
            fake(RegistryId::AgentskillSh, vec![Err("boom".to_string())]);
        let (ok, _) = fake(RegistryId::SkillsSh, vec![page(vec![item("s", 1)], false)]);
        let mut s = session(vec![broken, ok], SessionOptions::default());
        let client = no_http();

        let batch = s.next_page(&client).unwrap();

        assert_eq!(names(&batch.items), ["s"]);
        assert_eq!(batch.failures.len(), 1);
        assert_eq!(batch.failures[0].0, RegistryId::AgentskillSh);
        assert!(!s.has_more());
        assert_eq!(*broken_requests.lock().unwrap(), [1]);
    }

    #[test]
    fn all_failing_registries_error_and_keep_cursors() {
        let (a, a_requests) = fake(
            RegistryId::AgentskillSh,
            vec![Err("a down".into()), page(vec![item("a", 1)], false)],
        );
        let (b, _) = fake(
            RegistryId::SkillsSh,
            vec![Err("b down".into()), page(vec![], false)],
        );
        let mut s = session(vec![a, b], SessionOptions::default());
        let client = no_http();

        let err = s.next_page(&client).unwrap_err();
        assert_eq!(err.to_string(), "all registry searches failed");
        assert!(s.has_more());

        // Retrying asks for the same page again.
        assert_eq!(names(&s.next_page(&client).unwrap().items), ["a"]);
        assert_eq!(*a_requests.lock().unwrap(), [1, 1]);
    }

    #[test]
    fn single_registry_failure_surfaces_its_own_error() {
        let (reg, _) = fake(RegistryId::SkillsSh, vec![Err("connection refused".into())]);
        let mut s = session(vec![reg], SessionOptions::default());
        let err = s.next_page(&no_http()).unwrap_err();
        assert_eq!(err.to_string(), "connection refused");
    }

    #[test]
    fn total_hint_uses_reported_totals_and_never_undercounts() {
        let (reg, _) = fake(
            RegistryId::AgentskillSh,
            vec![Ok(RegistryPage {
                items: vec![item("a", 1)],
                has_more: true,
                total_hint: Some(101),
            })],
        );
        let (unreported, _) = fake(
            RegistryId::SkillsSh,
            vec![page(vec![item("s", 1), item("t", 1)], false)],
        );
        let mut s = session(vec![reg, unreported], SessionOptions::default());
        s.next_page(&no_http()).unwrap();
        assert_eq!(s.total_hint(), 103);
    }

    #[test]
    fn collect_pages_until_limit_then_sorts_globally() {
        let (reg, requested) = fake(
            RegistryId::AgentskillSh,
            vec![
                page(vec![item("a", 1), item("b", 2)], true),
                page(vec![item("c", 3), item("d", 4)], true),
            ],
        );
        let mut s = session(vec![reg], SessionOptions::default());

        let resp = s.collect(&no_http(), 3).unwrap();

        assert_eq!(names(&resp.items), ["d", "c", "b"]);
        assert!(resp.has_more);
        assert_eq!(*requested.lock().unwrap(), [1, 2]);
    }

    #[test]
    fn collect_stops_when_exhausted() {
        let (reg, _) = fake(
            RegistryId::AgentskillSh,
            vec![page(vec![item("a", 1)], false)],
        );
        let mut s = session(vec![reg], SessionOptions::default());
        let resp = s.collect(&no_http(), 10).unwrap();
        assert_eq!(names(&resp.items), ["a"]);
        assert!(!resp.has_more);
        assert_eq!(resp.total, 1);
    }

    #[test]
    fn collect_reports_more_when_truncating() {
        let (reg, _) = fake(
            RegistryId::AgentskillSh,
            vec![page(vec![item("a", 2), item("b", 1)], false)],
        );
        let mut s = session(vec![reg], SessionOptions::default());
        let resp = s.collect(&no_http(), 1).unwrap();
        assert_eq!(names(&resp.items), ["a"]);
        assert!(resp.has_more);
    }

    #[test]
    fn collect_returns_partial_results_when_a_later_page_fails() {
        let (reg, _) = fake(
            RegistryId::AgentskillSh,
            vec![page(vec![item("a", 1)], true), Err("down".into())],
        );
        let mut s = session(vec![reg], SessionOptions::default());
        let resp = s.collect(&no_http(), 10).unwrap();
        assert_eq!(names(&resp.items), ["a"]);
    }

    #[test]
    fn collect_propagates_first_page_failure() {
        let (reg, _) = fake(RegistryId::AgentskillSh, vec![Err("down".into())]);
        let mut s = session(vec![reg], SessionOptions::default());
        assert!(s.collect(&no_http(), 10).is_err());
    }
}
