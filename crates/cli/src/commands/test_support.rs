//! Shared test utilities for command tests: TUI rendering and a GitHub double.
//!
//! `#[cfg(test)]`-gated module — only compiled during `cargo test`.

use ratatui::backend::TestBackend;
use ratatui::layout::Position;
use ratatui::Terminal;
use skillfile_core::error::SkillfileError;
use skillfile_sources::http::HttpClient;

/// A GitHub repository whose file contents are pinned per commit SHA.
///
/// Serves the two endpoints that fetching "at a SHA" goes through: raw file
/// downloads and the recursive Trees API. Unknown files are a 404 and an
/// unknown SHA has no tree, mirroring GitHub.
pub struct FakeGithub {
    owner_repo: &'static str,
    files: Vec<(String, String, String)>,
}

impl FakeGithub {
    pub fn new(owner_repo: &'static str) -> Self {
        Self {
            owner_repo,
            files: Vec::new(),
        }
    }

    /// Repository contents at commit `sha`, as `(path, content)` pairs.
    pub fn at(mut self, sha: &str, files: &[(&str, &str)]) -> Self {
        self.files.extend(
            files
                .iter()
                .map(|(path, content)| (sha.to_string(), path.to_string(), content.to_string())),
        );
        self
    }
}

impl HttpClient for FakeGithub {
    fn get_bytes(&self, url: &str) -> Result<Vec<u8>, SkillfileError> {
        let raw_prefix = format!("https://raw.githubusercontent.com/{}/", self.owner_repo);
        url.strip_prefix(&raw_prefix)
            .and_then(|sha_path| {
                self.files
                    .iter()
                    .find(|(sha, path, _)| sha_path == format!("{sha}/{path}"))
            })
            .map(|(_, _, content)| content.clone().into_bytes())
            .ok_or_else(|| SkillfileError::Network(format!("HTTP 404 fetching {url}")))
    }

    fn get_json(&self, url: &str) -> Result<Option<String>, SkillfileError> {
        let tree_prefix = format!(
            "https://api.github.com/repos/{}/git/trees/",
            self.owner_repo
        );
        let Some(sha) = url
            .strip_prefix(&tree_prefix)
            .and_then(|rest| rest.strip_suffix("?recursive=1"))
        else {
            return Ok(None);
        };
        let blobs: Vec<String> = self
            .files
            .iter()
            .filter(|(file_sha, _, _)| file_sha == sha)
            .map(|(_, path, _)| format!(r#"{{"path": "{path}", "type": "blob"}}"#))
            .collect();
        if blobs.is_empty() {
            return Ok(None);
        }
        Ok(Some(format!(r#"{{"tree": [{}]}}"#, blobs.join(", "))))
    }

    fn post_json(&self, url: &str, _body: &str) -> Result<Vec<u8>, SkillfileError> {
        panic!("unexpected POST in test: {url}");
    }
}

/// Standard terminal dimensions for snapshot tests.
pub const TERM_WIDTH: u16 = 80;
pub const TERM_HEIGHT: u16 = 24;

/// Extract text from a single buffer row, trimming trailing whitespace.
fn row_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| {
            buf.cell(Position::new(x, y))
                .map_or(" ", ratatui::buffer::Cell::symbol)
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Render the terminal buffer to text — one trimmed line per row.
fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
    (0..buf.area.height)
        .map(|y| row_text(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a TUI frame via `draw_fn` and return the buffer as text.
///
/// Creates a `TestBackend` terminal at the standard 80x24 size,
/// draws one frame, and extracts the buffer content. This eliminates
/// the 4-line boilerplate from every render test.
pub fn render_to_text(draw_fn: impl FnOnce(&mut ratatui::Frame)) -> String {
    let backend = TestBackend::new(TERM_WIDTH, TERM_HEIGHT);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw_fn(f)).unwrap();
    buffer_text(terminal.backend().buffer())
}
