//! Background page loading for the search TUI.
//!
//! The [`SearchSession`] moves into a worker thread for each page request and
//! comes back over a channel together with the result, so the UI thread never
//! blocks on the network and no locking is needed. At most one request is in
//! flight at a time.

use std::sync::mpsc;

use skillfile_core::error::SkillfileError;
use skillfile_sources::http::UreqClient;
use skillfile_sources::registry::{RegistryId, SearchBatch, SearchResult, SearchSession};

type PageReply = (Box<SearchSession>, Result<SearchBatch, SkillfileError>);

enum PagerState {
    Idle(Box<SearchSession>),
    Loading,
    Exhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagerStatus {
    Loading,
    MoreAvailable,
    AllLoaded,
}

pub struct Pager {
    state: PagerState,
    tx: mpsc::Sender<PageReply>,
    rx: mpsc::Receiver<PageReply>,
    last_error: Option<String>,
}

impl Pager {
    /// `None` (or a session with nothing left) means every result is already
    /// loaded.
    pub fn new(session: Option<SearchSession>) -> Self {
        let state = match session {
            Some(session) if session.has_more() => PagerState::Idle(Box::new(session)),
            _ => PagerState::Exhausted,
        };
        let (tx, rx) = mpsc::channel();
        Self {
            state,
            tx,
            rx,
            last_error: None,
        }
    }

    /// Start fetching the next page in the background. Does nothing while a
    /// request is in flight or once everything is loaded.
    pub fn request(&mut self) {
        match std::mem::replace(&mut self.state, PagerState::Loading) {
            PagerState::Idle(session) => spawn_page_fetch(session, self.tx.clone()),
            other => self.state = other,
        }
    }

    /// Collect a finished page, if one has arrived.
    ///
    /// A failed request puts the session back in the idle state, so the next
    /// `request` retries the same page.
    pub fn poll(&mut self) -> Option<Vec<SearchResult>> {
        let (session, result) = self.rx.try_recv().ok()?;
        self.state = if session.has_more() {
            PagerState::Idle(session)
        } else {
            PagerState::Exhausted
        };
        let Ok(batch) = result else {
            self.last_error = Some("load failed, scroll to retry".to_string());
            return None;
        };
        self.last_error = failures_message(&batch.failures);
        Some(batch.items)
    }

    pub fn status(&self) -> PagerStatus {
        match self.state {
            PagerState::Idle(_) => PagerStatus::MoreAvailable,
            PagerState::Loading => PagerStatus::Loading,
            PagerState::Exhausted => PagerStatus::AllLoaded,
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// A pager that looks like it has a request in flight.
    #[cfg(test)]
    pub fn loading() -> Self {
        let mut pager = Self::new(None);
        pager.state = PagerState::Loading;
        pager
    }
}

fn spawn_page_fetch(mut session: Box<SearchSession>, tx: mpsc::Sender<PageReply>) {
    std::thread::spawn(move || {
        let result = session.next_page(&UreqClient::new());
        let _ = tx.send((session, result));
    });
}

fn failures_message(failures: &[(RegistryId, SkillfileError)]) -> Option<String> {
    if failures.is_empty() {
        return None;
    }
    let names: Vec<&str> = failures.iter().map(|(id, _)| id.as_str()).collect();
    Some(format!("{} unavailable", names.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use skillfile_sources::registry::SessionOptions;

    fn unstarted_session() -> SearchSession {
        let opts = SessionOptions {
            registry: Some(RegistryId::SkillsSh),
            ..SessionOptions::default()
        };
        SearchSession::new("docker", &opts)
    }

    #[test]
    fn without_session_everything_is_loaded() {
        let pager = Pager::new(None);
        assert_eq!(pager.status(), PagerStatus::AllLoaded);
    }

    #[test]
    fn fresh_session_has_more() {
        let pager = Pager::new(Some(unstarted_session()));
        assert_eq!(pager.status(), PagerStatus::MoreAvailable);
    }

    #[test]
    fn request_when_exhausted_is_a_no_op() {
        let mut pager = Pager::new(None);
        pager.request();
        assert_eq!(pager.status(), PagerStatus::AllLoaded);
        assert!(pager.poll().is_none());
    }

    #[test]
    fn request_while_loading_is_a_no_op() {
        let mut pager = Pager::loading();
        pager.request();
        assert_eq!(pager.status(), PagerStatus::Loading);
    }

    #[test]
    fn poll_without_reply_keeps_state() {
        let mut pager = Pager::loading();
        assert!(pager.poll().is_none());
        assert_eq!(pager.status(), PagerStatus::Loading);
    }

    #[test]
    fn failed_reply_returns_session_to_idle_with_error() {
        let mut pager = Pager::loading();
        pager
            .tx
            .send((
                Box::new(unstarted_session()),
                Err(SkillfileError::Network("down".into())),
            ))
            .unwrap();

        assert!(pager.poll().is_none());
        assert_eq!(pager.status(), PagerStatus::MoreAvailable);
        assert_eq!(pager.last_error(), Some("load failed, scroll to retry"));
    }

    #[test]
    fn partial_failures_are_reported_by_registry() {
        let mut pager = Pager::loading();
        let batch = SearchBatch {
            items: Vec::new(),
            failures: vec![(
                RegistryId::AgentskillSh,
                SkillfileError::Network("down".into()),
            )],
        };
        pager
            .tx
            .send((Box::new(unstarted_session()), Ok(batch)))
            .unwrap();

        let items = pager.poll().expect("a successful reply yields a batch");
        assert!(items.is_empty());
        assert_eq!(pager.last_error(), Some("agentskill.sh unavailable"));
    }
}
