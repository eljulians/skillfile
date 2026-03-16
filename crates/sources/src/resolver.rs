use skillfile_core::error::SkillfileError;

use crate::http::HttpClient;

// Re-export so existing callers (`use crate::resolver::github_token`) keep working.
pub use crate::http::github_token;

/// Perform an HTTP GET and return the response body as bytes.
pub fn http_get(client: &dyn HttpClient, url: &str) -> Result<Vec<u8>, SkillfileError> {
    client.get_bytes(url)
}

/// Check whether a GitHub repository has been renamed by comparing the
/// API-returned `full_name` against the requested `owner_repo`.
///
/// When ureq follows a 301 redirect (renamed repo), the final response
/// from `/repos/{old}` contains the new repo metadata. If `full_name`
/// differs from what we asked for, the repo was renamed.
///
/// Returns `Some(new_full_name)` if renamed, `None` otherwise.
fn check_repo_renamed(client: &dyn HttpClient, owner_repo: &str) -> Option<String> {
    let url = format!("https://api.github.com/repos/{owner_repo}");
    let text = client.get_json(&url).ok()??;
    let data: serde_json::Value = serde_json::from_str(&text).ok()?;
    let full_name = data["full_name"].as_str()?;
    // Case-insensitive comparison: GitHub normalises casing on rename.
    if full_name.eq_ignore_ascii_case(owner_repo) {
        None
    } else {
        Some(full_name.to_string())
    }
}

/// Try to resolve a git ref to a commit SHA. Returns `None` on 4xx.
fn try_resolve_sha(
    client: &dyn HttpClient,
    owner_repo: &str,
    ref_: &str,
) -> Result<Option<String>, SkillfileError> {
    let url = format!("https://api.github.com/repos/{owner_repo}/commits/{ref_}");
    let Some(text) = client.get_json(&url)? else {
        return Ok(None);
    };
    let data: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        SkillfileError::Network(format!(
            "invalid JSON in SHA response for {owner_repo}@{ref_}: {e}"
        ))
    })?;

    Ok(data["sha"].as_str().map(std::string::ToString::to_string))
}

/// Resolve a branch/tag/SHA ref to a full commit SHA via GitHub API.
///
/// When ref is `main` and the repo uses `master`, falls back automatically.
pub fn resolve_github_sha(
    client: &dyn HttpClient,
    owner_repo: &str,
    ref_: &str,
) -> Result<String, SkillfileError> {
    if let Some(sha) = try_resolve_sha(client, owner_repo, ref_)? {
        return Ok(sha);
    }
    // Fall back: main <-> master
    let fallback = match ref_ {
        "main" => Some("master"),
        "master" => Some("main"),
        _ => None,
    };
    if let Some(fb) = fallback {
        if let Some(sha) = try_resolve_sha(client, owner_repo, fb)? {
            return Ok(sha);
        }
    }
    // Before giving up, check if the repo was renamed. ureq follows 301
    // redirects transparently, so /repos/{old_name} may return the new
    // repo's metadata with a different full_name.
    if let Some(new_name) = check_repo_renamed(client, owner_repo) {
        return Err(SkillfileError::Network(format!(
            "repository '{owner_repo}' has been renamed to '{new_name}'.\n  \
             Update the owner/repo in your Skillfile to the new name."
        )));
    }
    Err(SkillfileError::Network(format!(
        "could not resolve {owner_repo}@{ref_} -- check that the repository exists and the ref is valid"
    )))
}

/// Reference to a GitHub repo at a specific commit, bundling client + coordinates.
pub struct GithubFetch<'a> {
    pub client: &'a dyn HttpClient,
    pub owner_repo: &'a str,
    pub ref_: &'a str,
}

/// Fetch raw file bytes from `raw.githubusercontent.com`.
pub fn fetch_github_file(
    gh: &GithubFetch<'_>,
    path_in_repo: &str,
) -> Result<Vec<u8>, SkillfileError> {
    let effective_path = if path_in_repo == "." {
        "SKILL.md"
    } else {
        path_in_repo
    };
    let url = format!(
        "https://raw.githubusercontent.com/{}/{}/{}",
        gh.owner_repo, gh.ref_, effective_path
    );
    http_get(gh.client, &url)
}

// ---------------------------------------------------------------------------
// list_repo_skill_entries — discover skill entry paths in a repo
// ---------------------------------------------------------------------------

/// Filenames (lowercase) to exclude when listing candidate skill files.
const REPO_META_FILES: &[&str] = &[
    "readme.md",
    "changelog.md",
    "license.md",
    "contributing.md",
    "code_of_conduct.md",
    "security.md",
];

fn is_repo_meta_file(path: &str) -> bool {
    let filename = path.rsplit('/').next().unwrap_or(path);
    REPO_META_FILES
        .iter()
        .any(|m| m.eq_ignore_ascii_case(filename))
        || path.to_ascii_lowercase().starts_with(".github/")
}

/// Convert raw `.md` file paths into deduplicated Skillfile entry paths.
///
/// Follows the Skillfile convention:
/// Map a root-level filename to its Skillfile entry path.
///
/// `SKILL.md` (case-insensitive) becomes `"."`, everything else stays as-is.
fn root_entry_path(filename: &str) -> String {
    if filename.eq_ignore_ascii_case("SKILL.md") {
        ".".to_string()
    } else {
        filename.to_string()
    }
}

/// Determine the entry path when a directory contains exactly one `.md` file.
///
/// If the file is named `SKILL.md` (case-insensitive), the directory itself
/// is the entry. Otherwise the file path is the entry.
fn single_file_dir_entry(dir: &str, file_path: &str) -> String {
    let filename = file_path.rsplit('/').next().unwrap_or(file_path);
    if filename.eq_ignore_ascii_case("SKILL.md") {
        dir.to_string()
    } else {
        file_path.to_string()
    }
}

/// - `SKILL.md` at repo root → `.`
/// - `dir/SKILL.md` (or multiple `.md` files in a dir) → `dir` (directory entry)
/// - `path/to/file.md` (only `.md` file in its dir) → `path/to/file.md` (single file)
fn collapse_to_entries(md_files: &[String]) -> Vec<String> {
    use std::collections::BTreeMap;

    // Group files by their parent directory.
    let mut dirs: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for path in md_files {
        if let Some(pos) = path.rfind('/') {
            let dir = &path[..pos];
            dirs.entry(dir).or_default().push(path);
        } else {
            // Root-level file.
            dirs.entry("").or_default().push(path);
        }
    }

    let mut entries = Vec::new();
    for (dir, files) in &dirs {
        if dir.is_empty() {
            // Root level: a root SKILL.md becomes "."; other files stay as-is.
            entries.extend(files.iter().map(|f| root_entry_path(f)));
        } else if files.len() > 1 {
            // Multiple .md files in one dir → directory entry.
            entries.push(dir.to_string());
        } else if files.len() == 1 {
            entries.push(single_file_dir_entry(dir, files[0]));
        }
    }
    entries
}

/// Discover skill entry paths in a GitHub repo.
///
/// Returns Skillfile-ready paths: `.` for root SKILL.md, directory paths for
/// multi-file skills, and individual `.md` paths for single-file skills.
/// Excludes repo metadata (README, CHANGELOG, etc.).
///
/// Tries the Tree API with "main", falls back to "master". Returns an empty
/// vec on any failure (graceful degradation for interactive flows).
pub fn list_repo_skill_entries(client: &dyn HttpClient, owner_repo: &str) -> Vec<String> {
    list_md_files_with_ref(client, owner_repo, "main")
        .or_else(|| list_md_files_with_ref(client, owner_repo, "master"))
        .map(|files| collapse_to_entries(&files))
        .unwrap_or_default()
}

/// Try to list `.md` files for a specific ref. Returns `None` on failure.
fn list_md_files_with_ref(
    client: &dyn HttpClient,
    owner_repo: &str,
    ref_: &str,
) -> Option<Vec<String>> {
    let url = format!("https://api.github.com/repos/{owner_repo}/git/trees/{ref_}?recursive=1");
    let text = client.get_json(&url).ok()??;
    let data: serde_json::Value = serde_json::from_str(&text).ok()?;
    let empty = Vec::new();
    let tree = data["tree"].as_array().unwrap_or(&empty);

    let files: Vec<String> = tree
        .iter()
        .filter_map(|item| {
            if item["type"].as_str() != Some("blob") {
                return None;
            }
            let path = item["path"].as_str()?;
            if !path.to_ascii_lowercase().ends_with(".md") {
                return None;
            }
            if is_repo_meta_file(path) {
                return None;
            }
            Some(path.to_string())
        })
        .collect();

    Some(files)
}

/// A file entry from a GitHub directory listing.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub relative_path: String,
    pub download_url: String,
}

/// List all files under `base_path` using the Git Trees API.
pub fn list_github_dir_recursive(
    gh: &GithubFetch<'_>,
    base_path: &str,
) -> Result<Vec<DirEntry>, SkillfileError> {
    let url = format!(
        "https://api.github.com/repos/{}/git/trees/{}?recursive=1",
        gh.owner_repo, gh.ref_
    );
    let text = gh.client.get_json(&url)?.ok_or_else(|| {
        SkillfileError::Network(format!(
            "failed to list directory {}/{base_path}: 4xx error",
            gh.owner_repo
        ))
    })?;
    let data: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| SkillfileError::Network(format!("invalid tree JSON: {e}")))?;

    let prefix = format!("{}/", base_path.trim_end_matches('/'));
    let empty = Vec::new();
    let tree = data["tree"].as_array().unwrap_or(&empty);

    let entries = tree
        .iter()
        .filter(|item| {
            item["type"].as_str() == Some("blob")
                && item["path"]
                    .as_str()
                    .is_some_and(|p| p.starts_with(&prefix))
        })
        .filter_map(|item| {
            let path = item["path"].as_str()?;
            let relative_path = path.strip_prefix(&prefix)?.to_string();
            let download_url = format!(
                "https://raw.githubusercontent.com/{}/{}/{}",
                gh.owner_repo, gh.ref_, path
            );
            Some(DirEntry {
                relative_path,
                download_url,
            })
        })
        .collect();

    Ok(entries)
}

/// Attempt to decode bytes as UTF-8 text. Returns `Err(original_bytes)` for binary.
pub fn decode_safe(raw: Vec<u8>) -> Result<String, Vec<u8>> {
    String::from_utf8(raw).map_err(std::string::FromUtf8Error::into_bytes)
}

/// File content: either decoded text or raw binary bytes.
#[derive(Debug, Clone)]
pub enum FileContent {
    Text(String),
    Binary(Vec<u8>),
}

impl FileContent {
    pub fn from_bytes(raw: Vec<u8>) -> Self {
        match decode_safe(raw) {
            Ok(text) => FileContent::Text(text),
            Err(bytes) => FileContent::Binary(bytes),
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            FileContent::Text(s) => s.as_bytes(),
            FileContent::Binary(b) => b,
        }
    }
}

/// Download a single file and return `(relative_path, content)`.
///
/// Extracted to reduce nesting inside the `thread::scope` closure in
/// [`fetch_files_parallel`].
fn download_one(
    client: &dyn HttpClient,
    url: &str,
    rel: &str,
) -> Result<(String, FileContent), SkillfileError> {
    let bytes = http_get(client, url)?;
    Ok((rel.to_string(), FileContent::from_bytes(bytes)))
}

/// Fetch multiple files in parallel using threads.
pub fn fetch_files_parallel(
    client: &dyn HttpClient,
    files: &[DirEntry],
) -> Result<Vec<(String, FileContent)>, SkillfileError> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    if files.len() == 1 {
        let bytes = http_get(client, &files[0].download_url)?;
        return Ok(vec![(
            files[0].relative_path.clone(),
            FileContent::from_bytes(bytes),
        )]);
    }

    // Parallel fetch using scoped threads (client is &dyn HttpClient: Send + Sync)
    let results: Vec<Result<(String, FileContent), SkillfileError>> = std::thread::scope(|s| {
        let handles: Vec<_> = files
            .iter()
            .map(|entry| {
                s.spawn(|| download_one(client, &entry.download_url, &entry.relative_path))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("download thread panicked"))
            .collect()
    });

    results.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // -----------------------------------------------------------------------
    // MockClient — in-memory HttpClient for unit tests
    // -----------------------------------------------------------------------

    struct MockClient {
        bytes_responses: HashMap<String, Result<Vec<u8>, SkillfileError>>,
        json_responses: HashMap<String, Result<Option<String>, SkillfileError>>,
    }

    impl MockClient {
        fn new() -> Self {
            Self {
                bytes_responses: HashMap::new(),
                json_responses: HashMap::new(),
            }
        }

        fn add_bytes(&mut self, url: &str, data: Vec<u8>) {
            self.bytes_responses.insert(url.to_string(), Ok(data));
        }

        fn add_bytes_err(&mut self, url: &str, msg: &str) {
            self.bytes_responses.insert(
                url.to_string(),
                Err(SkillfileError::Network(msg.to_string())),
            );
        }

        fn add_json(&mut self, url: &str, json: &str) {
            self.json_responses
                .insert(url.to_string(), Ok(Some(json.to_string())));
        }

        fn add_json_none(&mut self, url: &str) {
            self.json_responses.insert(url.to_string(), Ok(None));
        }

        fn add_json_err(&mut self, url: &str, msg: &str) {
            self.json_responses.insert(
                url.to_string(),
                Err(SkillfileError::Network(msg.to_string())),
            );
        }
    }

    impl HttpClient for MockClient {
        fn get_bytes(&self, url: &str) -> Result<Vec<u8>, SkillfileError> {
            match self.bytes_responses.get(url) {
                Some(Ok(data)) => Ok(data.clone()),
                Some(Err(e)) => Err(SkillfileError::Network(e.to_string())),
                None => Err(SkillfileError::Network(format!(
                    "MockClient: no bytes stub for {url}"
                ))),
            }
        }

        fn get_json(&self, url: &str) -> Result<Option<String>, SkillfileError> {
            match self.json_responses.get(url) {
                Some(Ok(v)) => Ok(v.clone()),
                Some(Err(e)) => Err(SkillfileError::Network(e.to_string())),
                None => Err(SkillfileError::Network(format!(
                    "MockClient: no json stub for {url}"
                ))),
            }
        }

        fn post_json(&self, url: &str, _body: &str) -> Result<Vec<u8>, SkillfileError> {
            Err(SkillfileError::Network(format!(
                "MockClient: no post_json stub for {url}"
            )))
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn sha_json(sha: &str) -> String {
        format!(r#"{{"sha": "{sha}"}}"#)
    }

    fn commit_url(owner_repo: &str, ref_: &str) -> String {
        format!("https://api.github.com/repos/{owner_repo}/commits/{ref_}")
    }

    fn tree_url(owner_repo: &str, ref_: &str) -> String {
        format!("https://api.github.com/repos/{owner_repo}/git/trees/{ref_}?recursive=1")
    }

    // -----------------------------------------------------------------------
    // decode_safe (existing coverage retained)
    // -----------------------------------------------------------------------

    #[test]
    fn decode_safe_utf8() {
        assert_eq!(
            decode_safe(b"hello world".to_vec()),
            Ok("hello world".to_string())
        );
    }

    #[test]
    fn decode_safe_binary() {
        let binary = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00];
        assert!(decode_safe(binary.clone()).is_err());
        assert_eq!(decode_safe(binary.clone()).unwrap_err(), binary);
    }

    #[test]
    fn file_content_from_bytes_text() {
        let fc = FileContent::from_bytes(b"# Hello".to_vec());
        assert!(matches!(fc, FileContent::Text(_)));
        assert_eq!(fc.as_bytes(), b"# Hello");
    }

    #[test]
    fn file_content_from_bytes_binary() {
        let raw = vec![0x89, 0x50, 0x4E, 0x47];
        let fc = FileContent::from_bytes(raw.clone());
        assert!(matches!(fc, FileContent::Binary(_)));
        assert_eq!(fc.as_bytes(), &raw);
    }

    // -----------------------------------------------------------------------
    // http_get — delegates to client.get_bytes
    // -----------------------------------------------------------------------

    #[test]
    fn http_get_returns_bytes_from_client() {
        let mut client = MockClient::new();
        client.add_bytes("https://example.com/file.md", b"content here".to_vec());

        let result = http_get(&client, "https://example.com/file.md").unwrap();
        assert_eq!(result, b"content here");
    }

    #[test]
    fn http_get_propagates_error() {
        let mut client = MockClient::new();
        client.add_bytes_err("https://example.com/missing.md", "HTTP 404");

        let err = http_get(&client, "https://example.com/missing.md").unwrap_err();
        assert!(err.to_string().contains("HTTP 404"));
    }

    // -----------------------------------------------------------------------
    // try_resolve_sha — private function exercised via resolve_github_sha
    // -----------------------------------------------------------------------

    #[test]
    fn try_resolve_sha_extracts_sha_from_json() {
        // try_resolve_sha is private; exercise it through resolve_github_sha.
        let mut client = MockClient::new();
        let url = commit_url("owner/repo", "main");
        client.add_json(&url, &sha_json("deadbeef1234567890abcdef"));

        let sha = resolve_github_sha(&client, "owner/repo", "main").unwrap();
        assert_eq!(sha, "deadbeef1234567890abcdef");
    }

    #[test]
    fn try_resolve_sha_returns_none_on_4xx() {
        // When the primary ref returns None (4xx) AND the fallback also returns None,
        // resolve_github_sha should produce a Network error.
        let mut client = MockClient::new();
        let primary = commit_url("owner/repo", "v99");
        client.add_json_none(&primary);
        // No fallback registered — "v99" has no main/master alias, so fallback is None.

        let err = resolve_github_sha(&client, "owner/repo", "v99").unwrap_err();
        assert!(err.to_string().contains("could not resolve owner/repo@v99"));
    }

    #[test]
    fn try_resolve_sha_propagates_network_error() {
        let mut client = MockClient::new();
        let url = commit_url("owner/repo", "main");
        client.add_json_err(&url, "connection refused");

        let err = resolve_github_sha(&client, "owner/repo", "main").unwrap_err();
        assert!(err.to_string().contains("connection refused"));
    }

    // -----------------------------------------------------------------------
    // resolve_github_sha — fallback logic
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_github_sha_happy_path() {
        let mut client = MockClient::new();
        let url = commit_url("myorg/myrepo", "main");
        client.add_json(&url, &sha_json("aabbccddeeff00112233445566778899aabbccdd"));

        let sha = resolve_github_sha(&client, "myorg/myrepo", "main").unwrap();
        assert_eq!(sha, "aabbccddeeff00112233445566778899aabbccdd");
    }

    #[test]
    fn resolve_github_sha_main_falls_back_to_master() {
        // Primary "main" returns 4xx (None); fallback "master" succeeds.
        let mut client = MockClient::new();
        client.add_json_none(&commit_url("org/repo", "main"));
        client.add_json(
            &commit_url("org/repo", "master"),
            &sha_json("cafebabe000000000000"),
        );

        let sha = resolve_github_sha(&client, "org/repo", "main").unwrap();
        assert_eq!(sha, "cafebabe000000000000");
    }

    #[test]
    fn resolve_github_sha_master_falls_back_to_main() {
        // Primary "master" returns 4xx (None); fallback "main" succeeds.
        let mut client = MockClient::new();
        client.add_json_none(&commit_url("org/repo", "master"));
        client.add_json(
            &commit_url("org/repo", "main"),
            &sha_json("1234abcd5678ef90"),
        );

        let sha = resolve_github_sha(&client, "org/repo", "master").unwrap();
        assert_eq!(sha, "1234abcd5678ef90");
    }

    #[test]
    fn resolve_github_sha_fails_when_both_branches_absent() {
        // Both "main" and "master" return 4xx — should error.
        let mut client = MockClient::new();
        client.add_json_none(&commit_url("org/repo", "main"));
        client.add_json_none(&commit_url("org/repo", "master"));

        let err = resolve_github_sha(&client, "org/repo", "main").unwrap_err();
        assert!(err.to_string().contains("could not resolve org/repo@main"));
    }

    #[test]
    fn resolve_github_sha_non_main_ref_no_fallback() {
        // A ref like "v1.2.3" that isn't "main"/"master" has no fallback.
        let mut client = MockClient::new();
        client.add_json_none(&commit_url("org/repo", "v1.2.3"));

        let err = resolve_github_sha(&client, "org/repo", "v1.2.3").unwrap_err();
        assert!(err
            .to_string()
            .contains("could not resolve org/repo@v1.2.3"));
    }

    #[test]
    fn resolve_github_sha_invalid_json_returns_error() {
        // JSON that parses successfully but has no "sha" field returns None from
        // try_resolve_sha, propagating through to the "could not resolve" error.
        let mut client = MockClient::new();
        client.add_json(
            &commit_url("org/repo", "main"),
            r#"{"message": "Not Found"}"#,
        );
        // No "sha" key in JSON → try_resolve_sha returns Ok(None).
        client.add_json_none(&commit_url("org/repo", "master"));

        let err = resolve_github_sha(&client, "org/repo", "main").unwrap_err();
        assert!(err.to_string().contains("could not resolve org/repo@main"));
    }

    // -----------------------------------------------------------------------
    // check_repo_renamed — detects renamed repos via full_name mismatch
    // -----------------------------------------------------------------------

    fn repo_url(owner_repo: &str) -> String {
        format!("https://api.github.com/repos/{owner_repo}")
    }

    #[test]
    fn check_repo_renamed_detects_rename() {
        let mut client = MockClient::new();
        client.add_json(
            &repo_url("old-owner/repo"),
            r#"{"full_name": "new-owner/repo"}"#,
        );

        assert_eq!(
            check_repo_renamed(&client, "old-owner/repo"),
            Some("new-owner/repo".to_string())
        );
    }

    #[test]
    fn check_repo_renamed_same_name_returns_none() {
        let mut client = MockClient::new();
        client.add_json(&repo_url("owner/repo"), r#"{"full_name": "owner/repo"}"#);

        assert_eq!(check_repo_renamed(&client, "owner/repo"), None);
    }

    #[test]
    fn check_repo_renamed_case_insensitive() {
        let mut client = MockClient::new();
        client.add_json(&repo_url("Owner/Repo"), r#"{"full_name": "owner/repo"}"#);

        assert_eq!(check_repo_renamed(&client, "Owner/Repo"), None);
    }

    #[test]
    fn check_repo_renamed_returns_none_on_4xx() {
        let mut client = MockClient::new();
        client.add_json_none(&repo_url("gone/repo"));

        assert_eq!(check_repo_renamed(&client, "gone/repo"), None);
    }

    #[test]
    fn check_repo_renamed_returns_none_on_network_error() {
        let mut client = MockClient::new();
        client.add_json_err(&repo_url("err/repo"), "connection refused");

        assert_eq!(check_repo_renamed(&client, "err/repo"), None);
    }

    // -----------------------------------------------------------------------
    // resolve_github_sha — renamed repo detection (end-to-end)
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_github_sha_renamed_repo_shows_new_name() {
        // SHA resolution fails (4xx on both main and master), but
        // /repos/{old_name} returns the new name via redirect.
        let mut client = MockClient::new();
        client.add_json_none(&commit_url("old-owner/repo", "main"));
        client.add_json_none(&commit_url("old-owner/repo", "master"));
        client.add_json(
            &repo_url("old-owner/repo"),
            r#"{"full_name": "new-owner/repo"}"#,
        );

        let err = resolve_github_sha(&client, "old-owner/repo", "main").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("renamed to 'new-owner/repo'"),
            "should include new name: {msg}"
        );
        assert!(
            msg.contains("old-owner/repo"),
            "should include old name: {msg}"
        );
    }

    #[test]
    fn resolve_github_sha_rename_check_fails_falls_back() {
        // SHA resolution fails, and the rename check also fails (4xx).
        // Should fall back to the generic "could not resolve" message.
        let mut client = MockClient::new();
        client.add_json_none(&commit_url("old-owner/repo", "main"));
        client.add_json_none(&commit_url("old-owner/repo", "master"));
        client.add_json_none(&repo_url("old-owner/repo"));

        let err = resolve_github_sha(&client, "old-owner/repo", "main").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("could not resolve"),
            "should use generic fallback: {msg}"
        );
    }

    #[test]
    fn resolve_github_sha_malformed_json_returns_error() {
        let mut client = MockClient::new();
        client.add_json(&commit_url("org/repo", "main"), "not json {{{{");

        let err = resolve_github_sha(&client, "org/repo", "main").unwrap_err();
        assert!(err.to_string().contains("invalid JSON in SHA response"));
    }

    // -----------------------------------------------------------------------
    // fetch_github_file
    // -----------------------------------------------------------------------

    #[test]
    fn fetch_github_file_basic() {
        let sha = "abc123";
        let url = format!("https://raw.githubusercontent.com/owner/repo/{sha}/skills/git.md");
        let mut client = MockClient::new();
        client.add_bytes(&url, b"# Git skill".to_vec());

        let gh = GithubFetch {
            client: &client,
            owner_repo: "owner/repo",
            ref_: sha,
        };
        let result = fetch_github_file(&gh, "skills/git.md").unwrap();
        assert_eq!(result, b"# Git skill");
    }

    #[test]
    fn fetch_github_file_dot_path_becomes_skill_md() {
        // path_in_repo == "." must be rewritten to "SKILL.md".
        let sha = "def456";
        let url = format!("https://raw.githubusercontent.com/org/repo/{sha}/SKILL.md");
        let mut client = MockClient::new();
        client.add_bytes(&url, b"# Root skill".to_vec());

        let gh = GithubFetch {
            client: &client,
            owner_repo: "org/repo",
            ref_: sha,
        };
        let result = fetch_github_file(&gh, ".").unwrap();
        assert_eq!(result, b"# Root skill");
    }

    #[test]
    fn fetch_github_file_propagates_error() {
        let sha = "fff000";
        let url = format!("https://raw.githubusercontent.com/org/repo/{sha}/missing.md");
        let mut client = MockClient::new();
        client.add_bytes_err(&url, "HTTP 404: not found");

        let gh = GithubFetch {
            client: &client,
            owner_repo: "org/repo",
            ref_: sha,
        };
        let err = fetch_github_file(&gh, "missing.md").unwrap_err();
        assert!(err.to_string().contains("HTTP 404: not found"));
    }

    // -----------------------------------------------------------------------
    // list_github_dir_recursive
    // -----------------------------------------------------------------------

    fn tree_json(entries: &[(&str, &str)]) -> String {
        let items: Vec<String> = entries
            .iter()
            .map(|(path, kind)| format!(r#"{{"path": "{path}", "type": "{kind}"}}"#))
            .collect();
        format!(r#"{{"tree": [{}]}}"#, items.join(", "))
    }

    #[test]
    fn list_github_dir_recursive_returns_blobs_under_prefix() {
        let owner_repo = "org/repo";
        let ref_ = "main";
        let url = tree_url(owner_repo, ref_);

        let json = tree_json(&[
            ("agents/dir/file1.md", "blob"),
            ("agents/dir/file2.md", "blob"),
            ("agents/dir/sub", "tree"), // tree entries must be excluded
            ("agents/other/file.md", "blob"), // different prefix — excluded
            ("readme.md", "blob"),      // no prefix at all — excluded
        ]);

        let mut client = MockClient::new();
        client.add_json(&url, &json);

        let gh = GithubFetch {
            client: &client,
            owner_repo,
            ref_,
        };
        let entries = list_github_dir_recursive(&gh, "agents/dir").unwrap();

        assert_eq!(entries.len(), 2);

        let relative_paths: Vec<&str> = entries.iter().map(|e| e.relative_path.as_str()).collect();
        assert!(relative_paths.contains(&"file1.md"));
        assert!(relative_paths.contains(&"file2.md"));
    }

    #[test]
    fn list_github_dir_recursive_download_urls_are_correct() {
        let owner_repo = "myorg/myrepo";
        let ref_ = "abc123sha";
        let url = tree_url(owner_repo, ref_);

        let json = tree_json(&[("skills/python/SKILL.md", "blob")]);

        let mut client = MockClient::new();
        client.add_json(&url, &json);

        let gh = GithubFetch {
            client: &client,
            owner_repo,
            ref_,
        };
        let entries = list_github_dir_recursive(&gh, "skills/python").unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].relative_path, "SKILL.md");
        assert_eq!(
            entries[0].download_url,
            format!("https://raw.githubusercontent.com/{owner_repo}/{ref_}/skills/python/SKILL.md")
        );
    }

    #[test]
    fn list_github_dir_recursive_filters_out_tree_nodes() {
        let owner_repo = "org/repo";
        let ref_ = "main";
        let url = tree_url(owner_repo, ref_);

        let json = tree_json(&[
            ("agents/data-analyst", "tree"),
            ("agents/data-analyst/agent.md", "blob"),
        ]);

        let mut client = MockClient::new();
        client.add_json(&url, &json);

        let gh = GithubFetch {
            client: &client,
            owner_repo,
            ref_,
        };
        let entries = list_github_dir_recursive(&gh, "agents/data-analyst").unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].relative_path, "agent.md");
    }

    #[test]
    fn list_github_dir_recursive_empty_tree() {
        let owner_repo = "org/repo";
        let ref_ = "main";
        let url = tree_url(owner_repo, ref_);

        let mut client = MockClient::new();
        client.add_json(&url, r#"{"tree": []}"#);

        let gh = GithubFetch {
            client: &client,
            owner_repo,
            ref_,
        };
        let entries = list_github_dir_recursive(&gh, "agents/dir").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn list_github_dir_recursive_4xx_returns_error() {
        let owner_repo = "org/repo";
        let ref_ = "main";
        let url = tree_url(owner_repo, ref_);

        let mut client = MockClient::new();
        client.add_json_none(&url);

        let gh = GithubFetch {
            client: &client,
            owner_repo,
            ref_,
        };
        let err = list_github_dir_recursive(&gh, "agents/dir").unwrap_err();
        assert!(err.to_string().contains("failed to list directory"));
    }

    #[test]
    fn list_github_dir_recursive_malformed_json_returns_error() {
        let owner_repo = "org/repo";
        let ref_ = "main";
        let url = tree_url(owner_repo, ref_);

        let mut client = MockClient::new();
        client.add_json(&url, "not valid json");

        let gh = GithubFetch {
            client: &client,
            owner_repo,
            ref_,
        };
        let err = list_github_dir_recursive(&gh, "agents/dir").unwrap_err();
        assert!(err.to_string().contains("invalid tree JSON"));
    }

    // -----------------------------------------------------------------------
    // fetch_files_parallel
    // -----------------------------------------------------------------------

    fn make_dir_entry(relative_path: &str, download_url: &str) -> DirEntry {
        DirEntry {
            relative_path: relative_path.to_string(),
            download_url: download_url.to_string(),
        }
    }

    #[test]
    fn fetch_files_parallel_empty_list_returns_empty() {
        let client = MockClient::new();
        let result = fetch_files_parallel(&client, &[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn fetch_files_parallel_single_file() {
        let url = "https://raw.githubusercontent.com/org/repo/abc/file.md";
        let mut client = MockClient::new();
        client.add_bytes(url, b"# Single file content".to_vec());

        let files = vec![make_dir_entry("file.md", url)];
        let result = fetch_files_parallel(&client, &files).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "file.md");
        assert_eq!(result[0].1.as_bytes(), b"# Single file content");
    }

    #[test]
    fn fetch_files_parallel_multiple_files() {
        let url1 = "https://raw.githubusercontent.com/org/repo/abc/file1.md";
        let url2 = "https://raw.githubusercontent.com/org/repo/abc/file2.md";
        let url3 = "https://raw.githubusercontent.com/org/repo/abc/file3.md";

        let mut client = MockClient::new();
        client.add_bytes(url1, b"content one".to_vec());
        client.add_bytes(url2, b"content two".to_vec());
        client.add_bytes(url3, b"content three".to_vec());

        let files = vec![
            make_dir_entry("file1.md", url1),
            make_dir_entry("file2.md", url2),
            make_dir_entry("file3.md", url3),
        ];
        let mut result = fetch_files_parallel(&client, &files).unwrap();

        // Sort for deterministic assertion (parallel order not guaranteed).
        result.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, "file1.md");
        assert_eq!(result[0].1.as_bytes(), b"content one");
        assert_eq!(result[1].0, "file2.md");
        assert_eq!(result[1].1.as_bytes(), b"content two");
        assert_eq!(result[2].0, "file3.md");
        assert_eq!(result[2].1.as_bytes(), b"content three");
    }

    #[test]
    fn fetch_files_parallel_single_file_text_variant() {
        // Verify TextContent classification works end-to-end through fetch_files_parallel.
        let url = "https://raw.githubusercontent.com/org/repo/abc/skill.md";
        let mut client = MockClient::new();
        client.add_bytes(url, b"# My Skill\n\nDoes things.".to_vec());

        let files = vec![make_dir_entry("skill.md", url)];
        let result = fetch_files_parallel(&client, &files).unwrap();

        assert_eq!(result.len(), 1);
        assert!(matches!(result[0].1, FileContent::Text(_)));
    }

    #[test]
    fn fetch_files_parallel_single_file_binary_variant() {
        let url = "https://raw.githubusercontent.com/org/repo/abc/image.png";
        let mut client = MockClient::new();
        // PNG magic bytes — not valid UTF-8
        client.add_bytes(url, vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

        let files = vec![make_dir_entry("image.png", url)];
        let result = fetch_files_parallel(&client, &files).unwrap();

        assert_eq!(result.len(), 1);
        assert!(matches!(result[0].1, FileContent::Binary(_)));
    }

    #[test]
    fn fetch_files_parallel_error_propagates_for_single_file() {
        let url = "https://raw.githubusercontent.com/org/repo/abc/missing.md";
        let mut client = MockClient::new();
        client.add_bytes_err(url, "HTTP 404: missing.md not found");

        let files = vec![make_dir_entry("missing.md", url)];
        let err = fetch_files_parallel(&client, &files).unwrap_err();
        assert!(err.to_string().contains("HTTP 404"));
    }

    #[test]
    fn fetch_files_parallel_error_propagates_for_multiple_files() {
        let url1 = "https://raw.githubusercontent.com/org/repo/abc/ok.md";
        let url2 = "https://raw.githubusercontent.com/org/repo/abc/bad.md";
        let url3 = "https://raw.githubusercontent.com/org/repo/abc/also_ok.md";

        let mut client = MockClient::new();
        client.add_bytes(url1, b"ok".to_vec());
        client.add_bytes_err(url2, "HTTP 500: server error");
        client.add_bytes(url3, b"also ok".to_vec());

        let files = vec![
            make_dir_entry("ok.md", url1),
            make_dir_entry("bad.md", url2),
            make_dir_entry("also_ok.md", url3),
        ];
        let err = fetch_files_parallel(&client, &files).unwrap_err();
        assert!(err.to_string().contains("HTTP 500"));
    }

    // -----------------------------------------------------------------------
    // is_repo_meta_file
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // is_repo_meta_file — comprehensive edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn is_repo_meta_file_readme_variants() {
        assert!(is_repo_meta_file("README.md"));
        assert!(is_repo_meta_file("readme.md"));
        assert!(is_repo_meta_file("Readme.md"));
        assert!(is_repo_meta_file("ReadMe.md"));
    }

    #[test]
    fn is_repo_meta_file_nested_readme() {
        // README.md in a subdirectory is still metadata.
        assert!(is_repo_meta_file("docs/README.md"));
        assert!(is_repo_meta_file("deep/nested/path/README.md"));
    }

    #[test]
    fn is_repo_meta_file_changelog() {
        assert!(is_repo_meta_file("CHANGELOG.md"));
        assert!(is_repo_meta_file("changelog.md"));
    }

    #[test]
    fn is_repo_meta_file_license() {
        assert!(is_repo_meta_file("LICENSE.md"));
        assert!(is_repo_meta_file("License.md"));
    }

    #[test]
    fn is_repo_meta_file_contributing() {
        assert!(is_repo_meta_file("CONTRIBUTING.md"));
        assert!(is_repo_meta_file("Contributing.md"));
    }

    #[test]
    fn is_repo_meta_file_code_of_conduct() {
        assert!(is_repo_meta_file("CODE_OF_CONDUCT.md"));
        assert!(is_repo_meta_file("Code_Of_Conduct.md"));
    }

    #[test]
    fn is_repo_meta_file_security() {
        assert!(is_repo_meta_file("SECURITY.md"));
        assert!(is_repo_meta_file("security.md"));
    }

    #[test]
    fn is_repo_meta_file_dotgithub_paths() {
        assert!(is_repo_meta_file(".github/ISSUE_TEMPLATE.md"));
        assert!(is_repo_meta_file(".github/pull_request_template.md"));
        assert!(is_repo_meta_file(".github/FUNDING.md"));
        // Case-insensitive .github prefix
        assert!(is_repo_meta_file(".GitHub/something.md"));
        assert!(is_repo_meta_file(".GITHUB/FOO.md"));
    }

    #[test]
    fn is_repo_meta_file_regular_skill_files() {
        assert!(!is_repo_meta_file("SKILL.md"));
        assert!(!is_repo_meta_file("skills/git.md"));
        assert!(!is_repo_meta_file("agents/code-reviewer.md"));
        assert!(!is_repo_meta_file("docs/tutorial.md"));
    }

    #[test]
    fn is_repo_meta_file_similar_but_not_matching() {
        // Filenames that look like metadata but aren't exact matches.
        assert!(!is_repo_meta_file("README.md.bak"));
        assert!(!is_repo_meta_file("MY-README.md"));
        assert!(!is_repo_meta_file("README-old.md"));
        assert!(!is_repo_meta_file("READMEX.md"));
        assert!(!is_repo_meta_file("XREADME.md"));
    }

    #[test]
    fn is_repo_meta_file_empty_string() {
        assert!(!is_repo_meta_file(""));
    }

    #[test]
    fn is_repo_meta_file_just_md_extension() {
        // A file named just ".md" is not metadata.
        assert!(!is_repo_meta_file(".md"));
    }

    #[test]
    fn is_repo_meta_file_github_prefix_not_nested() {
        // ".github" as a standalone file (unlikely but should handle).
        // It starts with ".github/" only if there's a slash.
        assert!(!is_repo_meta_file(".github"));
    }

    // -----------------------------------------------------------------------
    // collapse_to_entries — pure logic tests
    // -----------------------------------------------------------------------

    fn strs(s: &[&str]) -> Vec<String> {
        s.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn collapse_root_skill_md_becomes_dot() {
        let files = strs(&["SKILL.md"]);
        assert_eq!(collapse_to_entries(&files), vec!["."]);
    }

    #[test]
    fn collapse_root_skill_md_case_insensitive() {
        let files = strs(&["skill.md"]);
        assert_eq!(collapse_to_entries(&files), vec!["."]);
    }

    #[test]
    fn collapse_root_non_skill_stays_as_file() {
        let files = strs(&["my-agent.md"]);
        assert_eq!(collapse_to_entries(&files), vec!["my-agent.md"]);
    }

    #[test]
    fn collapse_dir_with_skill_md_becomes_dir_entry() {
        let files = strs(&["skills/docker/SKILL.md"]);
        assert_eq!(collapse_to_entries(&files), vec!["skills/docker"]);
    }

    #[test]
    fn collapse_dir_with_multiple_files_becomes_dir_entry() {
        let files = strs(&[
            "skills/k8s/SKILL.md",
            "skills/k8s/references/helm.md",
            "skills/k8s/references/config.md",
        ]);
        let entries = collapse_to_entries(&files);
        // "skills/k8s" has SKILL.md → dir entry.
        // "skills/k8s/references" has 2 files → dir entry.
        assert!(entries.contains(&"skills/k8s".to_string()));
        assert!(entries.contains(&"skills/k8s/references".to_string()));
    }

    #[test]
    fn collapse_single_file_in_dir_not_skill_stays_as_file() {
        let files = strs(&["agents/code-reviewer.md"]);
        assert_eq!(collapse_to_entries(&files), vec!["agents/code-reviewer.md"]);
    }

    #[test]
    fn collapse_mixed_root_and_nested() {
        let files = strs(&[
            "SKILL.md",
            "skills/git.md",
            "skills/docker/SKILL.md",
            "skills/docker/compose.md",
        ]);
        let entries = collapse_to_entries(&files);
        assert!(entries.contains(&".".to_string()));
        assert!(entries.contains(&"skills/git.md".to_string()));
        assert!(entries.contains(&"skills/docker".to_string()));
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn collapse_multiple_root_files() {
        let files = strs(&["SKILL.md", "other.md"]);
        let entries = collapse_to_entries(&files);
        assert!(entries.contains(&".".to_string()));
        assert!(entries.contains(&"other.md".to_string()));
    }

    #[test]
    fn collapse_empty_returns_empty() {
        assert!(collapse_to_entries(&[]).is_empty());
    }

    #[test]
    fn collapse_multi_skill_repo() {
        // Typical multi-skill repo like jeffallan/claude-skills.
        let files = strs(&[
            "skills/kubernetes-specialist/SKILL.md",
            "skills/kubernetes-specialist/references/helm.md",
            "skills/kubernetes-specialist/references/config.md",
            "skills/docker-helper/SKILL.md",
            "skills/python-pro/SKILL.md",
            "skills/python-pro/examples.md",
        ]);
        let entries = collapse_to_entries(&files);
        assert!(entries.contains(&"skills/kubernetes-specialist".to_string()));
        assert!(entries.contains(&"skills/kubernetes-specialist/references".to_string()));
        assert!(entries.contains(&"skills/docker-helper".to_string()));
        assert!(entries.contains(&"skills/python-pro".to_string()));
    }

    // -----------------------------------------------------------------------
    // list_repo_skill_entries — end-to-end with mocked HTTP
    // -----------------------------------------------------------------------

    #[test]
    fn skill_entries_filters_and_collapses() {
        let mut client = MockClient::new();
        let url = tree_url("org/repo", "main");
        let json = tree_json(&[
            ("SKILL.md", "blob"),
            ("skills/git.md", "blob"),
            ("README.md", "blob"),
            (".github/ISSUE_TEMPLATE.md", "blob"),
            ("src/main.rs", "blob"),
            ("agents", "tree"),
        ]);
        client.add_json(&url, &json);

        let entries = list_repo_skill_entries(&client, "org/repo");
        assert!(entries.contains(&".".to_string()));
        assert!(entries.contains(&"skills/git.md".to_string()));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn skill_entries_falls_back_to_master() {
        let mut client = MockClient::new();
        client.add_json_none(&tree_url("org/repo", "main"));
        let json = tree_json(&[("agent.md", "blob")]);
        client.add_json(&tree_url("org/repo", "master"), &json);

        let entries = list_repo_skill_entries(&client, "org/repo");
        assert_eq!(entries, vec!["agent.md"]);
    }

    #[test]
    fn skill_entries_main_succeeds_does_not_try_master() {
        let mut client = MockClient::new();
        let json = tree_json(&[("SKILL.md", "blob")]);
        client.add_json(&tree_url("org/repo", "main"), &json);

        let entries = list_repo_skill_entries(&client, "org/repo");
        assert_eq!(entries, vec!["."]);
    }

    #[test]
    fn skill_entries_returns_empty_on_total_failure() {
        let mut client = MockClient::new();
        client.add_json_none(&tree_url("org/repo", "main"));
        client.add_json_none(&tree_url("org/repo", "master"));
        assert!(list_repo_skill_entries(&client, "org/repo").is_empty());
    }

    #[test]
    fn skill_entries_returns_empty_on_network_error() {
        let mut client = MockClient::new();
        client.add_json_err(&tree_url("org/repo", "main"), "connection refused");
        client.add_json_err(&tree_url("org/repo", "master"), "connection refused");
        assert!(list_repo_skill_entries(&client, "org/repo").is_empty());
    }

    #[test]
    fn skill_entries_no_md_files_returns_empty() {
        let mut client = MockClient::new();
        let json = tree_json(&[("src/main.rs", "blob"), ("Cargo.toml", "blob")]);
        client.add_json(&tree_url("org/repo", "main"), &json);
        assert!(list_repo_skill_entries(&client, "org/repo").is_empty());
    }

    #[test]
    fn skill_entries_only_metadata_returns_empty() {
        let mut client = MockClient::new();
        let json = tree_json(&[
            ("README.md", "blob"),
            ("CHANGELOG.md", "blob"),
            (".github/ISSUE_TEMPLATE.md", "blob"),
        ]);
        client.add_json(&tree_url("org/repo", "main"), &json);
        assert!(list_repo_skill_entries(&client, "org/repo").is_empty());
    }

    #[test]
    fn skill_entries_dir_skill_collapses() {
        // Multi-file skill in a directory should become a single dir entry.
        let mut client = MockClient::new();
        let json = tree_json(&[
            ("skills/k8s/SKILL.md", "blob"),
            ("skills/k8s/references/helm.md", "blob"),
            ("skills/k8s/references/config.md", "blob"),
        ]);
        client.add_json(&tree_url("org/repo", "main"), &json);

        let entries = list_repo_skill_entries(&client, "org/repo");
        assert!(entries.contains(&"skills/k8s".to_string()));
        assert!(entries.contains(&"skills/k8s/references".to_string()));
    }

    #[test]
    fn skill_entries_malformed_json_returns_empty() {
        let mut client = MockClient::new();
        client.add_json(&tree_url("org/repo", "main"), "not valid json {{{");
        client.add_json_none(&tree_url("org/repo", "master"));
        assert!(list_repo_skill_entries(&client, "org/repo").is_empty());
    }

    #[test]
    fn skill_entries_missing_tree_key_returns_empty() {
        let mut client = MockClient::new();
        client.add_json(
            &tree_url("org/repo", "main"),
            r#"{"sha": "abc123", "url": "..."}"#,
        );
        assert!(list_repo_skill_entries(&client, "org/repo").is_empty());
    }

    #[test]
    fn skill_entries_empty_tree_array() {
        let mut client = MockClient::new();
        client.add_json(&tree_url("org/repo", "main"), r#"{"tree": []}"#);
        assert!(list_repo_skill_entries(&client, "org/repo").is_empty());
    }

    #[test]
    fn skill_entries_tree_entries_missing_fields() {
        let mut client = MockClient::new();
        let json = r#"{"tree": [
            {"path": "good.md", "type": "blob"},
            {"type": "blob"},
            {"path": "also-good.md"},
            {"path": "fine.md", "type": "blob"}
        ]}"#;
        client.add_json(&tree_url("org/repo", "main"), json);

        let entries = list_repo_skill_entries(&client, "org/repo");
        assert!(entries.contains(&"good.md".to_string()));
        assert!(entries.contains(&"fine.md".to_string()));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn skill_entries_mixed_single_and_dir() {
        let mut client = MockClient::new();
        let json = tree_json(&[
            ("skills/git.md", "blob"),
            ("skills/docker/SKILL.md", "blob"),
            ("agents/reviewer.md", "blob"),
        ]);
        client.add_json(&tree_url("org/repo", "main"), &json);

        let entries = list_repo_skill_entries(&client, "org/repo");
        assert!(entries.contains(&"skills/git.md".to_string()));
        assert!(entries.contains(&"skills/docker".to_string()));
        assert!(entries.contains(&"agents/reviewer.md".to_string()));
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn skill_entries_single_skill_at_root() {
        let mut client = MockClient::new();
        let json = tree_json(&[
            ("SKILL.md", "blob"),
            ("README.md", "blob"),
            ("LICENSE", "blob"),
        ]);
        client.add_json(&tree_url("org/repo", "main"), &json);

        let entries = list_repo_skill_entries(&client, "org/repo");
        assert_eq!(entries, vec!["."]);
    }

    #[test]
    fn skill_entries_main_error_master_error_both_graceful() {
        let mut client = MockClient::new();
        client.add_json_err(&tree_url("org/repo", "main"), "timeout");
        client.add_json(&tree_url("org/repo", "master"), "{{broken}}");
        assert!(list_repo_skill_entries(&client, "org/repo").is_empty());
    }

    #[test]
    fn skill_entries_main_empty_does_not_fallback() {
        let mut client = MockClient::new();
        client.add_json(&tree_url("org/repo", "main"), r#"{"tree": []}"#);
        assert!(list_repo_skill_entries(&client, "org/repo").is_empty());
    }
}
