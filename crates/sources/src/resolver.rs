use skillfile_core::error::SkillfileError;

use crate::http::HttpClient;

pub fn http_get(client: &dyn HttpClient, url: &str) -> Result<Vec<u8>, SkillfileError> {
    client.get_bytes(url)
}

/// Percent-encode a file path for use in a raw.githubusercontent.com URL.
///
/// GitHub's Tree API returns paths with raw Unicode and spaces, but
/// `raw.githubusercontent.com` requires them percent-encoded. We encode
/// each path segment individually so `/` separators are preserved.
fn encode_url_path(path: &str) -> String {
    path.split('/')
        .map(encode_path_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// Percent-encode a single path segment (RFC 3986 unreserved characters
/// pass through, everything else becomes %XX).
fn encode_path_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        match byte {
            // unreserved: ALPHA / DIGIT / "-" / "." / "_" / "~"
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            _ => {
                out.push('%');
                out.push(char::from(HEX[(*byte >> 4) as usize]));
                out.push(char::from(HEX[(*byte & 0x0F) as usize]));
            }
        }
    }
    out
}

const HEX: [u8; 16] = *b"0123456789ABCDEF";

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

    Ok(data["sha"].as_str().map(ToString::to_string))
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
///
/// SKILL.md markers act as boundary detectors: a directory containing SKILL.md
/// "claims" all descendant `.md` files, preventing them from producing separate
/// entries. This handles arbitrarily nested repos (e.g. `skills/alice/python-pro/`
/// with `SKILL.md` + `resources/playbook.md` produces ONE entry, not two).
fn collapse_to_entries(md_files: &[String]) -> Vec<String> {
    let skill_roots = find_skill_roots(md_files);
    let unclaimed = find_unclaimed_files(md_files, &skill_roots);

    let mut entries: Vec<String> = skill_roots.iter().map(ToString::to_string).collect();
    entries.extend(collapse_by_heuristic(&unclaimed));
    entries
}

fn find_skill_roots(md_files: &[String]) -> std::collections::BTreeSet<&str> {
    let mut roots = std::collections::BTreeSet::new();
    for path in md_files {
        let filename = path.rsplit('/').next().unwrap_or(path);
        let is_nested_marker = filename.eq_ignore_ascii_case("SKILL.md") && path.contains('/');
        if is_nested_marker {
            // unwrap safe: we checked contains('/')
            let pos = path.rfind('/').unwrap();
            roots.insert(&path[..pos]);
        }
    }
    roots
}

/// Return `true` if `path` is a descendant of (or the marker file at) any skill root.
fn is_claimed_by_root(path: &str, skill_roots: &std::collections::BTreeSet<&str>) -> bool {
    skill_roots.iter().any(|root| {
        path.starts_with(root) && path.as_bytes().get(root.len()).copied() == Some(b'/')
    })
}

/// Collect files not claimed by any SKILL.md root (and not markers themselves).
fn find_unclaimed_files<'a>(
    md_files: &'a [String],
    skill_roots: &std::collections::BTreeSet<&str>,
) -> Vec<&'a str> {
    md_files
        .iter()
        .filter(|path| {
            // Root-level files are never claimed.
            if !path.contains('/') {
                return true;
            }
            // SKILL.md at a skill root is claimed (the root is the entry).
            let filename = path.rsplit('/').next().unwrap_or(path);
            if filename.eq_ignore_ascii_case("SKILL.md") {
                let is_root_marker = path
                    .rfind('/')
                    .is_some_and(|pos| skill_roots.contains(&path[..pos]));
                return !is_root_marker;
            }
            !is_claimed_by_root(path, skill_roots)
        })
        .map(String::as_str)
        .collect()
}

fn collapse_by_heuristic(unclaimed: &[&str]) -> Vec<String> {
    use std::collections::BTreeMap;

    let mut dirs: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for path in unclaimed {
        if let Some(pos) = path.rfind('/') {
            dirs.entry(&path[..pos]).or_default().push(path);
        } else {
            dirs.entry("").or_default().push(path);
        }
    }

    let mut entries = Vec::new();
    for (dir, files) in &dirs {
        if dir.is_empty() {
            entries.extend(files.iter().map(|f| root_entry_path(f)));
        } else if files.len() > 1 {
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

/// Discover skill entry paths under a specific directory in a GitHub repo.
///
/// Like [`list_repo_skill_entries`] but scoped to `base_path`. Pass `"."` to
/// search the entire repo (equivalent to `list_repo_skill_entries`).
///
/// The returned paths are repo-relative (e.g. `skills/browser`, not `browser`).
pub fn list_repo_skill_entries_under(
    client: &dyn HttpClient,
    owner_repo: &str,
    base_path: &str,
) -> Vec<String> {
    let all_files = list_md_files_with_ref(client, owner_repo, "main")
        .or_else(|| list_md_files_with_ref(client, owner_repo, "master"));

    let Some(files) = all_files else {
        return Vec::new();
    };

    if base_path == "." {
        return collapse_to_entries(&files);
    }

    let prefix = base_path.trim_end_matches('/');
    let filtered: Vec<String> = files
        .into_iter()
        .filter(|p| p.starts_with(prefix) && p.as_bytes().get(prefix.len()).copied() == Some(b'/'))
        .collect();

    collapse_to_entries(&filtered)
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

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub relative_path: String,
    pub download_url: String,
}

/// List all files under `base_path` using the Git Trees API.
pub(crate) fn list_github_dir_recursive(
    gh: &GithubFetch<'_>,
    base_path: &str,
) -> Result<Vec<DirEntry>, SkillfileError> {
    let entries = list_dir_via_tree(gh, base_path)?;
    if !entries.is_empty() {
        return Ok(entries);
    }
    // Tree API returned nothing for this prefix. For massive repos the
    // recursive tree is truncated (~7000 entries) and the prefix may fall
    // beyond the cutoff. Fall back to the Contents API which lists a
    // specific directory without truncation.
    list_dir_via_contents(gh, base_path)
}

/// List directory files via the recursive Tree API (fast, but truncates on huge repos).
fn list_dir_via_tree(
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
            let encoded_path = encode_url_path(path);
            let download_url = format!(
                "https://raw.githubusercontent.com/{}/{}/{}",
                gh.owner_repo, gh.ref_, encoded_path
            );
            Some(DirEntry {
                relative_path,
                download_url,
            })
        })
        .collect();

    Ok(entries)
}

/// List directory files via the Contents API (slower, but works for any repo size).
fn list_dir_via_contents(
    gh: &GithubFetch<'_>,
    base_path: &str,
) -> Result<Vec<DirEntry>, SkillfileError> {
    let encoded = encode_url_path(base_path);
    let url = format!(
        "https://api.github.com/repos/{}/contents/{}?ref={}",
        gh.owner_repo, encoded, gh.ref_
    );
    let Some(text) = gh.client.get_json(&url)? else {
        return Ok(Vec::new());
    };
    let items: Vec<serde_json::Value> = serde_json::from_str(&text)
        .map_err(|e| SkillfileError::Network(format!("invalid contents JSON: {e}")))?;

    Ok(items
        .iter()
        .filter(|item| item["type"].as_str() == Some("file"))
        .filter_map(|item| {
            let name = item["name"].as_str()?;
            let download_url = item["download_url"].as_str()?.to_string();
            Some(DirEntry {
                relative_path: name.to_string(),
                download_url,
            })
        })
        .collect())
}

/// Attempt to decode bytes as UTF-8 text. Returns `Err(original_bytes)` for binary.
pub fn decode_safe(raw: Vec<u8>) -> Result<String, Vec<u8>> {
    String::from_utf8(raw).map_err(std::string::FromUtf8Error::into_bytes)
}

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

/// Maximum number of files to download concurrently per batch.
const DOWNLOAD_BATCH_SIZE: usize = 50;

/// Download a batch of files in parallel using `thread::scope`.
fn fetch_batch(
    client: &dyn HttpClient,
    chunk: &[DirEntry],
) -> Vec<Result<(String, FileContent), SkillfileError>> {
    std::thread::scope(|s| {
        let handles: Vec<_> = chunk
            .iter()
            .map(|entry| {
                s.spawn(|| download_one(client, &entry.download_url, &entry.relative_path))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("download thread panicked"))
            .collect()
    })
}

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

    // Parallel fetch in batches to avoid exhausting file descriptors.
    // Each thread opens a TCP connection + TLS + local file handle; repos with
    // hundreds of files (aiskillstore/marketplace has 400+) exceed the default
    // ulimit -n (typically 1024) if all fetched at once.
    let mut out = Vec::with_capacity(files.len());

    for chunk in files.chunks(DOWNLOAD_BATCH_SIZE) {
        let batch = fetch_batch(client, chunk);
        for result in batch {
            out.push(result?);
        }
    }

    Ok(out)
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
    fn list_github_dir_recursive_empty_tree_falls_back_to_contents() {
        let owner_repo = "org/repo";
        let ref_ = "main";

        let mut client = MockClient::new();
        // Tree API returns empty (simulates truncation where prefix isn't found).
        client.add_json(&tree_url(owner_repo, ref_), r#"{"tree": []}"#);
        // Contents API returns the directory listing.
        let contents_url =
            format!("https://api.github.com/repos/{owner_repo}/contents/agents/dir?ref={ref_}");
        client.add_json(
            &contents_url,
            r#"[
                {"name": "SKILL.md", "type": "file", "download_url": "https://raw.githubusercontent.com/org/repo/main/agents/dir/SKILL.md"},
                {"name": "sub", "type": "dir", "download_url": null}
            ]"#,
        );

        let gh = GithubFetch {
            client: &client,
            owner_repo,
            ref_,
        };
        let entries = list_github_dir_recursive(&gh, "agents/dir").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].relative_path, "SKILL.md");
    }

    #[test]
    fn list_github_dir_recursive_empty_tree_and_empty_contents() {
        let owner_repo = "org/repo";
        let ref_ = "main";

        let mut client = MockClient::new();
        client.add_json(&tree_url(owner_repo, ref_), r#"{"tree": []}"#);
        let contents_url =
            format!("https://api.github.com/repos/{owner_repo}/contents/agents/dir?ref={ref_}");
        // Contents API also returns nothing (directory doesn't exist or is empty).
        client.add_json_none(&contents_url);

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
    // encode_url_path
    // -----------------------------------------------------------------------

    #[test]
    fn encode_url_path_ascii_unchanged() {
        assert_eq!(
            encode_url_path("skills/browser/SKILL.md"),
            "skills/browser/SKILL.md"
        );
    }

    #[test]
    fn encode_url_path_spaces_encoded() {
        assert_eq!(
            encode_url_path("skills/my skill/SKILL.md"),
            "skills/my%20skill/SKILL.md"
        );
    }

    #[test]
    fn encode_url_path_chinese_characters_encoded() {
        // The real-world case: aiskillstore/marketplace has Chinese filenames
        let input = "skills/telegram-dev/references/Telegram_Bot_按钮  和键盘实现模板.md";
        let encoded = encode_url_path(input);
        assert!(
            !encoded.contains('按'),
            "Chinese chars should be percent-encoded"
        );
        assert!(!encoded.contains(' '), "spaces should be percent-encoded");
        assert!(encoded.starts_with("skills/telegram-dev/references/"));
    }

    #[test]
    fn encode_url_path_preserves_slashes() {
        assert_eq!(encode_url_path("a/b/c"), "a/b/c");
    }

    #[test]
    fn encode_url_path_tilde_and_dash_unchanged() {
        assert_eq!(encode_url_path("my-skill/v~1"), "my-skill/v~1");
    }

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
        // "skills/k8s" has SKILL.md → dir entry; descendants are claimed.
        assert!(entries.contains(&"skills/k8s".to_string()));
        assert_eq!(entries.len(), 1, "references/ must be absorbed by k8s root");
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
        assert!(entries.contains(&"skills/docker-helper".to_string()));
        assert!(entries.contains(&"skills/python-pro".to_string()));
        assert_eq!(
            entries.len(),
            3,
            "descendants must be absorbed by SKILL.md roots"
        );
    }

    #[test]
    fn collapse_depth2_skill_with_resources() {
        // openclaw-style: skills/alice/python-pro/SKILL.md + resources subdir
        let files = strs(&[
            "skills/alice/python-pro/SKILL.md",
            "skills/alice/python-pro/resources/playbook.md",
        ]);
        let entries = collapse_to_entries(&files);
        assert_eq!(entries, vec!["skills/alice/python-pro"]);
    }

    #[test]
    fn collapse_depth2_multiple_authors() {
        // openclaw-style tree with multiple authors
        let files = strs(&[
            "skills/alice/python-pro/SKILL.md",
            "skills/alice/python-pro/resources/playbook.md",
            "skills/bob/docker-helper/SKILL.md",
            "skills/bob/docker-helper/examples/compose.md",
            "skills/bob/docker-helper/examples/swarm.md",
        ]);
        let entries = collapse_to_entries(&files);
        assert!(entries.contains(&"skills/alice/python-pro".to_string()));
        assert!(entries.contains(&"skills/bob/docker-helper".to_string()));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn collapse_skill_root_claims_all_descendants() {
        // Deeply nested files under a SKILL.md root must not produce entries.
        let files = strs(&[
            "skills/k8s/SKILL.md",
            "skills/k8s/refs/helm.md",
            "skills/k8s/refs/deep/nested/config.md",
            "skills/k8s/examples/deploy.md",
        ]);
        let entries = collapse_to_entries(&files);
        assert_eq!(entries, vec!["skills/k8s"]);
    }

    #[test]
    fn collapse_no_marker_heuristic_fallback() {
        // Files without SKILL.md markers still use the old heuristic.
        let files = strs(&["agents/reviewer.md", "agents/planner.md", "tools/linter.md"]);
        let entries = collapse_to_entries(&files);
        // agents/ has 2 files → dir entry; tools/ has 1 → file entry.
        assert!(entries.contains(&"agents".to_string()));
        assert!(entries.contains(&"tools/linter.md".to_string()));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn collapse_mixed_markers_and_heuristic() {
        let files = strs(&[
            "skills/browser/SKILL.md",
            "skills/browser/refs/config.md",
            "agents/reviewer.md",
            "agents/planner.md",
        ]);
        let entries = collapse_to_entries(&files);
        assert!(entries.contains(&"skills/browser".to_string()));
        assert!(entries.contains(&"agents".to_string()));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn collapse_skill_root_does_not_claim_sibling() {
        // A SKILL.md root should not claim siblings in a different directory.
        let files = strs(&["skills/docker/SKILL.md", "skills/git/commit.md"]);
        let entries = collapse_to_entries(&files);
        assert!(entries.contains(&"skills/docker".to_string()));
        assert!(entries.contains(&"skills/git/commit.md".to_string()));
        assert_eq!(entries.len(), 2);
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
        assert_eq!(entries.len(), 1, "descendants absorbed by SKILL.md root");
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

    // -----------------------------------------------------------------------
    // list_repo_skill_entries_under — scoped discovery
    // -----------------------------------------------------------------------

    #[test]
    fn skill_entries_under_scoped_to_skills() {
        let mut client = MockClient::new();
        let json = tree_json(&[
            ("skills/browser/SKILL.md", "blob"),
            ("skills/git.md", "blob"),
            ("agents/reviewer.md", "blob"),
        ]);
        client.add_json(&tree_url("org/repo", "main"), &json);

        let entries = list_repo_skill_entries_under(&client, "org/repo", "skills/");
        assert!(entries.contains(&"skills/browser".to_string()));
        assert!(entries.contains(&"skills/git.md".to_string()));
        assert!(!entries.contains(&"agents/reviewer.md".to_string()));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn skill_entries_under_dot_returns_everything() {
        let mut client = MockClient::new();
        let json = tree_json(&[("skills/git.md", "blob"), ("agents/reviewer.md", "blob")]);
        client.add_json(&tree_url("org/repo", "main"), &json);

        let entries = list_repo_skill_entries_under(&client, "org/repo", ".");
        assert!(entries.contains(&"skills/git.md".to_string()));
        assert!(entries.contains(&"agents/reviewer.md".to_string()));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn skill_entries_under_nonexistent_path_returns_empty() {
        let mut client = MockClient::new();
        let json = tree_json(&[("skills/git.md", "blob")]);
        client.add_json(&tree_url("org/repo", "main"), &json);

        let entries = list_repo_skill_entries_under(&client, "org/repo", "nonexistent/");
        assert!(entries.is_empty());
    }

    #[test]
    fn skill_entries_under_network_failure_returns_empty() {
        let mut client = MockClient::new();
        client.add_json_err(&tree_url("org/repo", "main"), "timeout");
        client.add_json_err(&tree_url("org/repo", "master"), "timeout");

        let entries = list_repo_skill_entries_under(&client, "org/repo", "skills/");
        assert!(entries.is_empty());
    }

    #[test]
    fn skill_entries_under_no_trailing_slash() {
        // base_path without trailing slash should still work
        let mut client = MockClient::new();
        let json = tree_json(&[
            ("skills/browser/SKILL.md", "blob"),
            ("skills/git.md", "blob"),
        ]);
        client.add_json(&tree_url("org/repo", "main"), &json);

        let entries = list_repo_skill_entries_under(&client, "org/repo", "skills");
        assert!(entries.contains(&"skills/browser".to_string()));
        assert!(entries.contains(&"skills/git.md".to_string()));
        assert_eq!(entries.len(), 2);
    }
}
