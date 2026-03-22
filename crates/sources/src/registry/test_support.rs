//! Test mock infrastructure for registry tests.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::http::HttpClient;
use skillfile_core::error::SkillfileError;

/// Sequential mock client: returns responses in FIFO order.
///
/// Each call to `get_bytes` pops the next response. An `Err` variant
/// simulates a network failure.
pub(crate) struct MockClient {
    responses: Mutex<VecDeque<Result<String, String>>>,
    post_responses: Mutex<VecDeque<Result<String, String>>>,
}

impl MockClient {
    pub fn new(responses: Vec<Result<String, String>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            post_responses: Mutex::new(VecDeque::new()),
        }
    }

    pub fn with_post_responses(mut self, post_responses: Vec<Result<String, String>>) -> Self {
        self.post_responses = Mutex::new(post_responses.into());
        self
    }
}

impl HttpClient for MockClient {
    fn get_bytes(&self, _url: &str) -> Result<Vec<u8>, SkillfileError> {
        let resp = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("MockClient: no more responses");
        match resp {
            Ok(body) => Ok(body.into_bytes()),
            Err(msg) => Err(SkillfileError::Network(msg)),
        }
    }

    fn get_json(&self, _url: &str) -> Result<Option<String>, SkillfileError> {
        unimplemented!("registry tests don't use get_json")
    }

    fn post_json(&self, _url: &str, _body: &str) -> Result<Vec<u8>, SkillfileError> {
        let resp = self
            .post_responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("MockClient: no more post responses");
        match resp {
            Ok(body) => Ok(body.into_bytes()),
            Err(msg) => Err(SkillfileError::Network(msg)),
        }
    }
}
