//! Test mock infrastructure for registry tests.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::http::HttpClient;
use skillfile_core::error::SkillfileError;

/// Sequential mock client: returns responses in FIFO order.
///
/// Each call to `get_bytes` pops the next response. An `Err` variant
/// simulates a network failure. Every requested URL is recorded so tests can
/// assert on query parameters such as `page=`.
pub(crate) struct MockClient {
    responses: Mutex<VecDeque<Result<String, String>>>,
    post_responses: Mutex<VecDeque<Result<String, String>>>,
    urls: Mutex<Vec<String>>,
}

impl MockClient {
    pub fn new(responses: Vec<Result<String, String>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            post_responses: Mutex::new(VecDeque::new()),
            urls: Mutex::new(Vec::new()),
        }
    }

    pub fn with_post_responses(mut self, post_responses: Vec<Result<String, String>>) -> Self {
        self.post_responses = Mutex::new(post_responses.into());
        self
    }

    pub fn requested_urls(&self) -> Vec<String> {
        self.urls.lock().unwrap().clone()
    }
}

fn pop(queue: &Mutex<VecDeque<Result<String, String>>>) -> Result<Vec<u8>, SkillfileError> {
    let resp = queue
        .lock()
        .unwrap()
        .pop_front()
        .expect("MockClient: no more responses");
    match resp {
        Ok(body) => Ok(body.into_bytes()),
        Err(msg) => Err(SkillfileError::Network(msg)),
    }
}

impl HttpClient for MockClient {
    fn get_bytes(&self, url: &str) -> Result<Vec<u8>, SkillfileError> {
        self.urls.lock().unwrap().push(url.to_string());
        pop(&self.responses)
    }

    fn get_json(&self, _url: &str) -> Result<Option<String>, SkillfileError> {
        unimplemented!("registry tests don't use get_json")
    }

    fn post_json(&self, url: &str, _body: &str) -> Result<Vec<u8>, SkillfileError> {
        self.urls.lock().unwrap().push(url.to_string());
        pop(&self.post_responses)
    }
}
