use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use skillfile_core::error::SkillfileError;
use skillfile_core::lock::{lock_key, read_lock, write_lock};
use skillfile_core::models::{short_sha, Entry, LockEntry, SourceFields};
use skillfile_core::parser::{parse_manifest, MANIFEST_NAME};
use skillfile_core::progress;

use crate::http::{HttpClient, UreqClient};
use crate::resolver::{
    fetch_files_parallel, fetch_github_file, http_get, list_github_dir_recursive,
    resolve_github_sha,
};
use crate::strategy::{content_file, is_dir_entry, meta_sha};

pub const VENDOR_DIR: &str = ".skillfile/cache";

/// Options for sync/install operations.
pub struct SyncContext {
    pub repo_root: PathBuf,
    pub dry_run: bool,
    pub update: bool,
    pub sha_cache: HashMap<(String, String), String>,
}

/// Compute the vendor cache directory for an entry.
#[must_use]
pub fn vendor_dir_for(entry: &Entry, repo_root: &Path) -> PathBuf {
    repo_root
        .join(VENDOR_DIR)
        .join(entry.entity_type.dir_name())
        .join(&entry.name)
}

/// Check if content exists in the vendor directory for an entry.
fn content_exists(entry: &Entry, vdir: &Path) -> bool {
    match &entry.source {
        SourceFields::Github { .. } => {
            if is_dir_entry(entry) {
                vdir.is_dir()
                    && std::fs::read_dir(vdir)
                        .map(|rd| rd.filter_map(|e| e.ok()).any(|e| e.file_name() != ".meta"))
                        .unwrap_or(false)
            } else {
                let cf = content_file(entry);
                !cf.is_empty() && vdir.join(&cf).exists()
            }
        }
        SourceFields::Local { .. } => false,
        SourceFields::Url { .. } => {
            let cf = content_file(entry);
            !cf.is_empty() && vdir.join(&cf).exists()
        }
    }
}

/// Sync a single entry. Returns updated lock map.
pub fn sync_entry(
    client: &dyn HttpClient,
    entry: &Entry,
    ctx: &mut SyncContext,
    locked: &mut BTreeMap<String, LockEntry>,
) -> Result<(), SkillfileError> {
    match &entry.source {
        SourceFields::Local { .. } => {
            progress!("  {entry}: local — skipping");
            Ok(())
        }
        SourceFields::Github { .. } => sync_github(client, entry, ctx, locked),
        SourceFields::Url { url } => sync_url(client, entry, url, ctx, locked),
    }
}

/// Read-only parameters shared by all sync operations.
struct SyncParams<'a> {
    repo_root: &'a Path,
    dry_run: bool,
    update: bool,
    sha_cache: &'a HashMap<(String, String), String>,
    locked: &'a BTreeMap<String, LockEntry>,
}

/// Resolved coordinates for a GitHub entry, passed to `fetch_and_write_github`.
struct GithubRef<'a> {
    owner_repo: &'a str,
    path_in_repo: &'a str,
    sha: &'a str,
}

/// Fetch and write the content for a GitHub entry into `vdir`.
///
/// Handles both directory entries (via Tree API) and single-file entries.
/// Returns the canonical `raw_url` to store in the lock file.
fn fetch_and_write_github(
    client: &dyn HttpClient,
    gh: &GithubRef<'_>,
    is_dir: bool,
    vdir: &Path,
    label: &str,
) -> Result<String, SkillfileError> {
    let GithubRef {
        owner_repo,
        path_in_repo,
        sha,
    } = gh;
    std::fs::create_dir_all(vdir)?;

    if is_dir {
        let dir_entries = list_github_dir_recursive(client, owner_repo, path_in_repo, sha)?;
        let fetched = fetch_files_parallel(client, &dir_entries)?;
        for (relative_path, content) in &fetched {
            let dest = vdir.join(relative_path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, content.as_bytes())?;
        }
        progress!(
            "{label}: sha={} -> {}/ ({} files)",
            short_sha(sha),
            vdir.display(),
            fetched.len()
        );
        Ok(format!(
            "https://api.github.com/repos/{owner_repo}/contents/{path_in_repo}?ref={sha}"
        ))
    } else {
        let content = fetch_github_file(client, owner_repo, path_in_repo, sha)?;
        let effective_path = if *path_in_repo == "." {
            "SKILL.md"
        } else {
            path_in_repo
        };
        let filename = std::path::Path::new(effective_path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("content.md");
        let dest = vdir.join(filename);
        std::fs::write(&dest, &content)?;
        progress!("{label}: sha={} -> {}", short_sha(sha), dest.display());
        Ok(format!(
            "https://raw.githubusercontent.com/{owner_repo}/{sha}/{effective_path}"
        ))
    }
}

/// Core sync logic for a github entry. Parallel-safe: all inputs are read-only,
/// result is returned rather than inserted into a mutable map.
///
/// `sha_cache` maps `(owner_repo, ref_)` → resolved SHA. Pre-populated by
/// `resolve_shas_parallel` so no mutation is needed here.
fn sync_github_core(
    client: &dyn HttpClient,
    entry: &Entry,
    params: &SyncParams<'_>,
) -> Result<Option<(String, LockEntry)>, SkillfileError> {
    let label = format!("  {entry}");
    let vdir = vendor_dir_for(entry, params.repo_root);
    let key = lock_key(entry);

    let SourceFields::Github {
        owner_repo,
        path_in_repo,
        ref_,
    } = &entry.source
    else {
        unreachable!()
    };
    let locked_sha = if params.update {
        None
    } else {
        params.locked.get(&key).map(|le| le.sha.clone())
    };
    let meta = meta_sha(&vdir);
    let has_content = content_exists(entry, &vdir);

    // Skip if locked SHA matches meta and content exists
    if let Some(ref ls) = locked_sha {
        if meta.as_deref() == Some(ls.as_str()) && has_content {
            progress!("{label}: up to date (sha={})", short_sha(ls));
            return Ok(None);
        }
    }

    // Resolve SHA
    let sha = if let Some(ref ls) = locked_sha {
        progress!("{label}: re-fetching (locked sha={}) ...", short_sha(ls));
        ls.clone()
    } else {
        let cache_key = (owner_repo.to_string(), ref_.to_string());
        if let Some(cached) = params.sha_cache.get(&cache_key) {
            cached.clone()
        } else if params.dry_run {
            progress!("{label}: resolving {owner_repo}@{ref_} ... [dry-run]");
            return Ok(None);
        } else {
            // Fallback: resolve inline (single-entry mode or missed cache)
            resolve_github_sha(client, owner_repo, ref_)?
        }
    };

    if params.dry_run {
        progress!("{label}: sha={} [dry-run]", short_sha(&sha));
        return Ok(None);
    }

    // After resolving SHA on --update, skip download if cache is current
    if params.update && meta.as_deref() == Some(sha.as_str()) && has_content {
        progress!("{label}: up to date (sha={})", short_sha(&sha));
        let raw_url = params
            .locked
            .get(&key)
            .map(|le| le.raw_url.clone())
            .unwrap_or_default();
        return Ok(Some((key, LockEntry { sha, raw_url })));
    }

    // Fetch and write
    let gh = GithubRef {
        owner_repo,
        path_in_repo,
        sha: &sha,
    };
    let raw_url = fetch_and_write_github(client, &gh, is_dir_entry(entry), &vdir, &label)?;

    // Write .meta
    let meta_data = serde_json::json!({
        "source_type": "github",
        "owner_repo": owner_repo,
        "path_in_repo": path_in_repo,
        "ref": ref_,
        "sha": &sha,
        "raw_url": &raw_url,
    });
    std::fs::write(
        vdir.join(".meta"),
        serde_json::to_string_pretty(&meta_data).expect("json! values are always serializable")
            + "\n",
    )?;

    Ok(Some((key, LockEntry { sha, raw_url })))
}

/// Thin wrapper around `sync_github_core` for backward-compatible sequential use.
fn sync_github(
    client: &dyn HttpClient,
    entry: &Entry,
    ctx: &mut SyncContext,
    locked: &mut BTreeMap<String, LockEntry>,
) -> Result<(), SkillfileError> {
    let params = SyncParams {
        repo_root: &ctx.repo_root,
        dry_run: ctx.dry_run,
        update: ctx.update,
        sha_cache: &ctx.sha_cache,
        locked,
    };
    let result = sync_github_core(client, entry, &params)?;
    // Update mutable state from the result
    if let Some((key, le)) = result {
        // If the SHA was freshly resolved (not from cache), insert into sha_cache
        if let SourceFields::Github {
            owner_repo, ref_, ..
        } = &entry.source
        {
            let cache_key = (owner_repo.clone(), ref_.clone());
            ctx.sha_cache
                .entry(cache_key)
                .or_insert_with(|| le.sha.clone());
        }
        locked.insert(key, le);
    }
    Ok(())
}

/// Core sync logic for a URL entry. Parallel-safe.
fn sync_url_core(
    client: &dyn HttpClient,
    entry: &Entry,
    url: &str,
    repo_root: &Path,
    dry_run: bool,
) -> Result<(), SkillfileError> {
    let label = format!("  {entry}");
    let vdir = vendor_dir_for(entry, repo_root);

    if dry_run {
        progress!("{label}: fetching {url} ... [dry-run]");
        return Ok(());
    }

    let content = http_get(client, url)?;
    let filename = std::path::Path::new(url)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("content.md");

    std::fs::create_dir_all(&vdir)?;
    std::fs::write(vdir.join(filename), &content)?;

    let meta_data = serde_json::json!({
        "source_type": "url",
        "url": url,
    });
    std::fs::write(
        vdir.join(".meta"),
        serde_json::to_string_pretty(&meta_data).expect("json! values are always serializable")
            + "\n",
    )?;

    progress!("{label}: {url} -> {}", vdir.join(filename).display());
    // URL entries don't have SHA-based locking
    Ok(())
}

/// Thin wrapper for backward-compatible sequential use.
fn sync_url(
    client: &dyn HttpClient,
    entry: &Entry,
    url: &str,
    ctx: &SyncContext,
    _locked: &mut BTreeMap<String, LockEntry>,
) -> Result<(), SkillfileError> {
    sync_url_core(client, entry, url, &ctx.repo_root, ctx.dry_run)
}

/// Dispatch a single entry sync using parallel-safe core functions.
/// Returns `Ok(Some((key, lock_entry)))` for entries that update the lock file.
fn sync_entry_core(
    client: &dyn HttpClient,
    entry: &Entry,
    params: &SyncParams<'_>,
) -> Result<Option<(String, LockEntry)>, SkillfileError> {
    match &entry.source {
        SourceFields::Local { .. } => {
            progress!("  {entry}: local — skipping");
            Ok(None)
        }
        SourceFields::Github { .. } => sync_github_core(client, entry, params),
        SourceFields::Url { url } => {
            sync_url_core(client, entry, url, params.repo_root, params.dry_run)?;
            Ok(None)
        }
    }
}

/// Return `true` if any entry using `(owner_repo, ref_)` lacks a locked SHA.
///
/// Used by `resolve_shas_parallel` to decide whether resolution is needed
/// in non-update mode.
fn needs_sha_resolution(
    entries: &[&Entry],
    owner_repo: &str,
    ref_: &str,
    locked: &BTreeMap<String, LockEntry>,
) -> bool {
    entries.iter().any(|e| {
        if let SourceFields::Github {
            owner_repo: or,
            ref_: r,
            ..
        } = &e.source
        {
            or == owner_repo && r == ref_ && locked.get(&lock_key(e)).is_none()
        } else {
            false
        }
    })
}

/// Resolve all unique `(owner_repo, ref_)` SHAs in parallel.
///
/// In `--update` mode, all github entries need resolution (locked SHAs ignored).
/// In normal mode, only entries without a locked SHA need resolution.
fn resolve_shas_parallel(
    client: &dyn HttpClient,
    entries: &[&Entry],
    locked: &BTreeMap<String, LockEntry>,
    update: bool,
) -> Result<HashMap<(String, String), String>, SkillfileError> {
    // Collect unique (owner_repo, ref_) pairs that need resolution
    let mut need_resolve: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();

    for entry in entries {
        if let SourceFields::Github {
            owner_repo, ref_, ..
        } = &entry.source
        {
            let key_pair = (owner_repo.clone(), ref_.clone());
            if seen.insert(key_pair.clone()) {
                // In update mode, always re-resolve. Otherwise, skip if all entries
                // with this repo+ref already have locked SHAs.
                if update || needs_sha_resolution(entries, owner_repo, ref_, locked) {
                    need_resolve.push(key_pair);
                }
            }
        }
    }

    if need_resolve.is_empty() {
        return Ok(HashMap::new());
    }

    // Resolve in parallel using scoped threads — borrows avoid cloning.
    let results: Vec<Result<String, SkillfileError>> = std::thread::scope(|s| {
        let handles: Vec<_> = need_resolve
            .iter()
            .map(|(owner_repo, ref_)| s.spawn(|| resolve_github_sha(client, owner_repo, ref_)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("SHA resolution thread panicked"))
            .collect()
    });

    let mut cache = HashMap::with_capacity(results.len());
    for (key, result) in need_resolve.into_iter().zip(results) {
        let sha = result?;
        progress!("  resolved {}@{} -> {}", key.0, key.1, short_sha(&sha));
        cache.insert(key, sha);
    }
    Ok(cache)
}

/// Run the `sync` command.
pub fn cmd_sync(
    repo_root: &Path,
    dry_run: bool,
    entry_filter: Option<&str>,
    update: bool,
) -> Result<(), SkillfileError> {
    let manifest_path = repo_root.join(MANIFEST_NAME);
    if !manifest_path.exists() {
        return Err(SkillfileError::Manifest(format!(
            "{MANIFEST_NAME} not found in {}. Create one and run `skillfile init`.",
            repo_root.display()
        )));
    }

    let result = parse_manifest(&manifest_path)?;
    for w in &result.warnings {
        eprintln!("{w}");
    }

    let mut entries: Vec<&Entry> = result.manifest.entries.iter().collect();

    if let Some(name) = entry_filter {
        entries.retain(|e| e.name == name);
        if entries.is_empty() {
            return Err(SkillfileError::Manifest(format!(
                "no entry named '{name}' in {MANIFEST_NAME}"
            )));
        }
    }

    if entries.is_empty() {
        eprintln!("No entries found in {MANIFEST_NAME}.");
        return Ok(());
    }

    let mode = if dry_run { " [dry-run]" } else { "" };
    let count = entries.len();
    let noun = if count == 1 { "entry" } else { "entries" };
    progress!("Syncing {count} {noun}{mode}...");

    let locked = read_lock(repo_root)?;
    let client = UreqClient::new();

    // For single-entry filter, dry-run, or trivial manifests, use sequential path
    if entry_filter.is_some() || dry_run || entries.len() <= 1 {
        let mut locked = locked;
        let mut ctx = SyncContext {
            repo_root: repo_root.to_path_buf(),
            dry_run,
            update,
            sha_cache: HashMap::new(),
        };
        for entry in &entries {
            sync_entry(&client, entry, &mut ctx, &mut locked)?;
        }
        if !dry_run {
            write_lock(repo_root, &locked)?;
            progress!("Done.");
        }
        return Ok(());
    }

    // Phase 1: Resolve all SHAs in parallel
    let sha_cache = resolve_shas_parallel(&client, &entries, &locked, update)?;

    // Phase 2: Sync all entries in parallel
    let params = SyncParams {
        repo_root,
        dry_run,
        update,
        sha_cache: &sha_cache,
        locked: &locked,
    };
    let results: Vec<Result<Option<(String, LockEntry)>, SkillfileError>> =
        std::thread::scope(|s| {
            let handles: Vec<_> = entries
                .iter()
                .map(|entry| s.spawn(|| sync_entry_core(&client, entry, &params)))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("sync thread panicked"))
                .collect()
        });

    // Merge results into locked map
    let mut locked = locked;
    for result in results {
        if let Some((key, le)) = result? {
            locked.insert(key, le);
        }
    }

    if !dry_run {
        write_lock(repo_root, &locked)?;
        progress!("Done.");
    }

    Ok(())
}

/// Fetch the text content of a single-file GitHub entry at a specific SHA.
/// Used by `diff` and `resolve` in conflict mode.
///
/// Returns the file content as a UTF-8 string, or an error for binary files.
pub fn fetch_file_at_sha(
    client: &dyn HttpClient,
    entry: &Entry,
    sha: &str,
) -> Result<String, SkillfileError> {
    let SourceFields::Github {
        owner_repo,
        path_in_repo,
        ..
    } = &entry.source
    else {
        return Err(SkillfileError::Network(
            "fetch_file_at_sha only supports github entries".into(),
        ));
    };
    let bytes = crate::resolver::fetch_github_file(client, owner_repo, path_in_repo, sha)?;
    crate::resolver::decode_safe(bytes)
        .map_err(|_| SkillfileError::Network(format!("binary file at sha {sha}")))
}

/// Fetch all text files in a directory GitHub entry at a specific SHA.
///
/// Returns a map of `relative_path -> content`. Binary files are silently skipped.
/// Used by `diff` and `resolve` in conflict mode.
pub fn fetch_dir_at_sha(
    client: &dyn HttpClient,
    entry: &Entry,
    sha: &str,
) -> Result<HashMap<String, String>, SkillfileError> {
    let SourceFields::Github {
        owner_repo,
        path_in_repo,
        ..
    } = &entry.source
    else {
        return Err(SkillfileError::Network(
            "fetch_dir_at_sha only supports github entries".into(),
        ));
    };
    let dir_entries =
        crate::resolver::list_github_dir_recursive(client, owner_repo, path_in_repo, sha)?;
    let fetched = crate::resolver::fetch_files_parallel(client, &dir_entries)?;
    let mut result = HashMap::new();
    for (path, content) in fetched {
        if let crate::resolver::FileContent::Text(text) = content {
            result.insert(path, text);
        }
        // skip binary files silently
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::path::Path;

    use skillfile_core::error::SkillfileError;
    use skillfile_core::models::{EntityType, Entry, LockEntry, SourceFields};

    use crate::http::HttpClient;

    use super::{
        content_exists, fetch_dir_at_sha, fetch_file_at_sha, sync_entry, vendor_dir_for,
        SyncContext, VENDOR_DIR,
    };

    // -----------------------------------------------------------------------
    // MockClient
    // -----------------------------------------------------------------------

    /// A deterministic HTTP client for tests.
    ///
    /// Responses are keyed by URL.  A missing key causes an error, ensuring that
    /// tests are explicit about every URL they expect to be called.
    struct MockClient {
        bytes_responses: HashMap<String, Vec<u8>>,
        json_responses: HashMap<String, Option<String>>,
    }

    impl MockClient {
        fn new() -> Self {
            Self {
                bytes_responses: HashMap::new(),
                json_responses: HashMap::new(),
            }
        }

        fn with_bytes(mut self, url: impl Into<String>, bytes: Vec<u8>) -> Self {
            self.bytes_responses.insert(url.into(), bytes);
            self
        }

        fn with_json(mut self, url: impl Into<String>, body: Option<String>) -> Self {
            self.json_responses.insert(url.into(), body);
            self
        }
    }

    impl HttpClient for MockClient {
        fn get_bytes(&self, url: &str) -> Result<Vec<u8>, SkillfileError> {
            self.bytes_responses
                .get(url)
                .cloned()
                .ok_or_else(|| SkillfileError::Network(format!("unexpected get_bytes: {url}")))
        }

        fn get_json(&self, url: &str) -> Result<Option<String>, SkillfileError> {
            self.json_responses
                .get(url)
                .cloned()
                .ok_or_else(|| SkillfileError::Network(format!("unexpected get_json: {url}")))
        }

        fn post_json(&self, url: &str, _body: &str) -> Result<Vec<u8>, SkillfileError> {
            Err(SkillfileError::Network(format!(
                "unexpected post_json: {url}"
            )))
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn skill_entry(name: &str, path_in_repo: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: name.into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: path_in_repo.into(),
                ref_: "main".into(),
            },
        }
    }

    fn agent_entry(name: &str, path_in_repo: &str) -> Entry {
        Entry {
            entity_type: EntityType::Agent,
            name: name.into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: path_in_repo.into(),
                ref_: "main".into(),
            },
        }
    }

    fn local_entry(name: &str, path: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: name.into(),
            source: SourceFields::Local { path: path.into() },
        }
    }

    fn url_entry(name: &str, url: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: name.into(),
            source: SourceFields::Url { url: url.into() },
        }
    }

    fn make_sync_ctx(repo_root: &Path) -> SyncContext {
        SyncContext {
            repo_root: repo_root.to_path_buf(),
            dry_run: false,
            update: false,
            sha_cache: HashMap::new(),
        }
    }

    // -----------------------------------------------------------------------
    // vendor_dir_for
    // -----------------------------------------------------------------------

    #[test]
    fn vendor_dir_for_skill_entry() {
        let dir = tempfile::tempdir().unwrap();
        let entry = skill_entry("my-skill", "skills/my-skill.md");
        let result = vendor_dir_for(&entry, dir.path());
        let expected = dir.path().join(VENDOR_DIR).join("skills").join("my-skill");
        assert_eq!(result, expected);
    }

    #[test]
    fn vendor_dir_for_agent_entry() {
        let dir = tempfile::tempdir().unwrap();
        let entry = agent_entry("my-agent", "agents/my-agent.md");
        let result = vendor_dir_for(&entry, dir.path());
        let expected = dir.path().join(VENDOR_DIR).join("agents").join("my-agent");
        assert_eq!(result, expected);
    }

    #[test]
    fn vendor_dir_for_is_nested_under_vendor_dir() {
        let dir = tempfile::tempdir().unwrap();
        let entry = skill_entry("foo", "foo.md");
        let result = vendor_dir_for(&entry, dir.path());
        // Path must start with <repo_root>/.skillfile/cache
        assert!(result.starts_with(dir.path().join(VENDOR_DIR)));
    }

    #[test]
    fn vendor_dir_for_uses_entry_name_not_path_stem() {
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "custom-name".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "skills/different-name.md".into(),
                ref_: "main".into(),
            },
        };
        let result = vendor_dir_for(&entry, dir.path());
        assert!(result.ends_with("custom-name"));
        assert!(!result.to_string_lossy().contains("different-name"));
    }

    // -----------------------------------------------------------------------
    // content_exists
    // -----------------------------------------------------------------------

    #[test]
    fn content_exists_github_single_file_present() {
        let dir = tempfile::tempdir().unwrap();
        let entry = skill_entry("my-skill", "skills/my-skill.md");
        let vdir = dir.path().join("vdir");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("my-skill.md"), b"# skill").unwrap();
        assert!(content_exists(&entry, &vdir));
    }

    #[test]
    fn content_exists_github_single_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let entry = skill_entry("my-skill", "skills/my-skill.md");
        let vdir = dir.path().join("vdir");
        std::fs::create_dir_all(&vdir).unwrap();
        // No content file written
        assert!(!content_exists(&entry, &vdir));
    }

    #[test]
    fn content_exists_github_single_file_vdir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let entry = skill_entry("my-skill", "skills/my-skill.md");
        let vdir = dir.path().join("nonexistent");
        assert!(!content_exists(&entry, &vdir));
    }

    #[test]
    fn content_exists_github_dir_entry_with_files() {
        let dir = tempfile::tempdir().unwrap();
        // "skills/python-pro" has no ".md" suffix → dir entry
        let entry = skill_entry("python-pro", "skills/python-pro");
        let vdir = dir.path().join("vdir");
        std::fs::create_dir_all(&vdir).unwrap();
        // Must have at least one non-.meta file
        std::fs::write(vdir.join("python.md"), b"# Python").unwrap();
        std::fs::write(vdir.join(".meta"), b"{}").unwrap();
        assert!(content_exists(&entry, &vdir));
    }

    #[test]
    fn content_exists_github_dir_entry_only_meta() {
        let dir = tempfile::tempdir().unwrap();
        let entry = skill_entry("python-pro", "skills/python-pro");
        let vdir = dir.path().join("vdir");
        std::fs::create_dir_all(&vdir).unwrap();
        // Only .meta — should be treated as empty
        std::fs::write(vdir.join(".meta"), b"{}").unwrap();
        assert!(!content_exists(&entry, &vdir));
    }

    #[test]
    fn content_exists_github_dir_entry_vdir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let entry = skill_entry("python-pro", "skills/python-pro");
        let vdir = dir.path().join("nonexistent");
        assert!(!content_exists(&entry, &vdir));
    }

    #[test]
    fn content_exists_local_always_false() {
        let dir = tempfile::tempdir().unwrap();
        let entry = local_entry("git-commit", "skills/git/commit.md");
        let vdir = dir.path().join("vdir");
        // Even if vdir and content file somehow exist, local always returns false
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("commit.md"), b"# local").unwrap();
        assert!(!content_exists(&entry, &vdir));
    }

    #[test]
    fn content_exists_url_present() {
        let dir = tempfile::tempdir().unwrap();
        let entry = url_entry("my-skill", "https://example.com/skill.md");
        let vdir = dir.path().join("vdir");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("skill.md"), b"# skill").unwrap();
        assert!(content_exists(&entry, &vdir));
    }

    #[test]
    fn content_exists_url_missing() {
        let dir = tempfile::tempdir().unwrap();
        let entry = url_entry("my-skill", "https://example.com/skill.md");
        let vdir = dir.path().join("vdir");
        std::fs::create_dir_all(&vdir).unwrap();
        // No content file
        assert!(!content_exists(&entry, &vdir));
    }

    // -----------------------------------------------------------------------
    // sync_entry — local entry is skipped
    // -----------------------------------------------------------------------

    #[test]
    fn sync_entry_local_skips_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let entry = local_entry("git-commit", "skills/git/commit.md");
        let client = MockClient::new(); // no responses needed
        let mut ctx = make_sync_ctx(dir.path());
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();

        let result = sync_entry(&client, &entry, &mut ctx, &mut locked);
        assert!(result.is_ok());
        // Lock must not have been modified for a local entry
        assert!(locked.is_empty());
    }

    // -----------------------------------------------------------------------
    // sync_entry — github single-file entry, dry_run
    // -----------------------------------------------------------------------

    #[test]
    fn sync_entry_github_dry_run_skips_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let entry = skill_entry("my-skill", "skills/my-skill.md");
        // MockClient intentionally has no responses: if any HTTP call is made the
        // test will fail with "unexpected get_*" rather than a false positive.
        let client = MockClient::new();
        let mut ctx = make_sync_ctx(dir.path());
        ctx.dry_run = true;
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();

        let result = sync_entry(&client, &entry, &mut ctx, &mut locked);
        assert!(result.is_ok());
        // Dry-run must not persist any lock entry
        assert!(locked.is_empty());
    }

    // -----------------------------------------------------------------------
    // sync_entry — github single-file entry, up-to-date (locked SHA matches meta)
    // -----------------------------------------------------------------------

    #[test]
    fn sync_entry_github_up_to_date_skips_fetch() {
        let sha = "aabbccdd1122334455667788aabbccdd11223344";
        let dir = tempfile::tempdir().unwrap();
        let entry = skill_entry("my-skill", "skills/my-skill.md");
        let vdir = vendor_dir_for(&entry, dir.path());
        std::fs::create_dir_all(&vdir).unwrap();

        // Write .meta with the same SHA that is in the lock
        let meta = serde_json::json!({
            "sha": sha,
            "source_type": "github",
            "owner_repo": "owner/repo",
            "path_in_repo": "skills/my-skill.md",
            "ref": "main",
            "raw_url": "https://raw.githubusercontent.com/owner/repo/aabbccdd/skills/my-skill.md"
        });
        std::fs::write(
            vdir.join(".meta"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
        // Write the content file so content_exists returns true
        std::fs::write(vdir.join("my-skill.md"), b"# skill").unwrap();

        // Populate lock with the same SHA
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();
        locked.insert(
            "github/skill/my-skill".to_string(),
            LockEntry {
                sha: sha.to_string(),
                raw_url: "https://raw.githubusercontent.com/owner/repo/aabbccdd/skills/my-skill.md"
                    .into(),
            },
        );

        // MockClient has no responses: if any HTTP call is made, the test fails
        let client = MockClient::new();
        let mut ctx = make_sync_ctx(dir.path());

        let result = sync_entry(&client, &entry, &mut ctx, &mut locked);
        assert!(result.is_ok(), "unexpected error: {result:?}");
        // Lock entry must remain unchanged
        assert_eq!(locked["github/skill/my-skill"].sha, sha);
    }

    // -----------------------------------------------------------------------
    // sync_entry — github single-file entry, SHA resolved via mock
    // -----------------------------------------------------------------------

    #[test]
    fn sync_entry_github_fetches_and_writes_file() {
        let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let dir = tempfile::tempdir().unwrap();
        let entry = skill_entry("my-skill", "skills/my-skill.md");

        // Commit SHA resolution URL
        let sha_url = "https://api.github.com/repos/owner/repo/commits/main".to_string();
        let sha_json = serde_json::json!({ "sha": sha }).to_string();

        // Raw file download URL
        let raw_url =
            format!("https://raw.githubusercontent.com/owner/repo/{sha}/skills/my-skill.md");

        let client = MockClient::new()
            .with_json(sha_url, Some(sha_json))
            .with_bytes(raw_url.clone(), b"# My Skill\nContent here.".to_vec());

        let mut ctx = make_sync_ctx(dir.path());
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();

        let result = sync_entry(&client, &entry, &mut ctx, &mut locked);
        assert!(result.is_ok(), "sync_entry failed: {result:?}");

        // Lock must be updated with the resolved SHA
        let lock_entry = locked
            .get("github/skill/my-skill")
            .expect("lock entry missing");
        assert_eq!(lock_entry.sha, sha);

        // Content file must exist on disk
        let vdir = vendor_dir_for(&entry, dir.path());
        assert!(vdir.join("my-skill.md").exists());
        let written = std::fs::read_to_string(vdir.join("my-skill.md")).unwrap();
        assert_eq!(written, "# My Skill\nContent here.");

        // .meta must exist on disk
        assert!(vdir.join(".meta").exists());
    }

    // -----------------------------------------------------------------------
    // sync_entry — github, SHA cached across entries
    // -----------------------------------------------------------------------

    #[test]
    fn sync_entry_github_sha_cached_on_second_call() {
        let sha = "cafebabecafebabecafebabecafebabecafebabe";
        let dir = tempfile::tempdir().unwrap();

        let entry1 = skill_entry("skill-one", "skills/one.md");
        let entry2 = skill_entry("skill-two", "skills/two.md");

        let sha_url = "https://api.github.com/repos/owner/repo/commits/main".to_string();
        let sha_json = serde_json::json!({ "sha": sha }).to_string();

        let raw_url1 = format!("https://raw.githubusercontent.com/owner/repo/{sha}/skills/one.md");
        let raw_url2 = format!("https://raw.githubusercontent.com/owner/repo/{sha}/skills/two.md");

        // SHA resolution URL appears only once in the mock — the second call must
        // use the cache, otherwise MockClient would error on the second get_json.
        let client = MockClient::new()
            .with_json(sha_url, Some(sha_json))
            .with_bytes(raw_url1, b"# One".to_vec())
            .with_bytes(raw_url2, b"# Two".to_vec());

        let mut ctx = make_sync_ctx(dir.path());
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();

        sync_entry(&client, &entry1, &mut ctx, &mut locked).unwrap();
        // After the first call the SHA must be in the cache
        assert!(ctx
            .sha_cache
            .contains_key(&("owner/repo".to_string(), "main".to_string())));

        // The second call must succeed using the cached SHA (no second get_json call)
        sync_entry(&client, &entry2, &mut ctx, &mut locked).unwrap();

        assert!(locked.contains_key("github/skill/skill-one"));
        assert!(locked.contains_key("github/skill/skill-two"));
    }

    // -----------------------------------------------------------------------
    // sync_entry — github dot-path (SKILL.md convention)
    // -----------------------------------------------------------------------

    #[test]
    fn sync_entry_github_dot_path_writes_skill_md() {
        let sha = "1234567890abcdef1234567890abcdef12345678";
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "root-skill".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: ".".into(),
                ref_: "main".into(),
            },
        };

        let sha_url = "https://api.github.com/repos/owner/repo/commits/main".to_string();
        let sha_json = serde_json::json!({ "sha": sha }).to_string();
        let raw_url = format!("https://raw.githubusercontent.com/owner/repo/{sha}/SKILL.md");

        let client = MockClient::new()
            .with_json(sha_url, Some(sha_json))
            .with_bytes(raw_url, b"# Root Skill".to_vec());

        let mut ctx = make_sync_ctx(dir.path());
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();

        sync_entry(&client, &entry, &mut ctx, &mut locked).unwrap();

        let vdir = vendor_dir_for(&entry, dir.path());
        assert!(vdir.join("SKILL.md").exists());
    }

    // -----------------------------------------------------------------------
    // sync_url
    // -----------------------------------------------------------------------

    #[test]
    fn sync_url_fetches_and_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let url = "https://example.com/skill.md";
        let entry = url_entry("example-skill", url);

        let client =
            MockClient::new().with_bytes(url, b"# Example Skill\nFetched content.".to_vec());

        let mut ctx = make_sync_ctx(dir.path());
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();

        let result = sync_entry(&client, &entry, &mut ctx, &mut locked);
        assert!(result.is_ok(), "sync_url failed: {result:?}");

        let vdir = vendor_dir_for(&entry, dir.path());
        assert!(vdir.join("skill.md").exists());
        let written = std::fs::read_to_string(vdir.join("skill.md")).unwrap();
        assert_eq!(written, "# Example Skill\nFetched content.");

        // .meta must be written with source_type = "url"
        let meta_text = std::fs::read_to_string(vdir.join(".meta")).unwrap();
        let meta: serde_json::Value = serde_json::from_str(&meta_text).unwrap();
        assert_eq!(meta["source_type"], "url");
        assert_eq!(meta["url"], url);
    }

    #[test]
    fn sync_url_dry_run_skips_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let url = "https://example.com/skill.md";
        let entry = url_entry("example-skill", url);

        // No bytes registered: any get_bytes call would error
        let client = MockClient::new();
        let mut ctx = make_sync_ctx(dir.path());
        ctx.dry_run = true;
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();

        let result = sync_entry(&client, &entry, &mut ctx, &mut locked);
        assert!(result.is_ok());

        // Nothing must have been written
        let vdir = vendor_dir_for(&entry, dir.path());
        assert!(!vdir.exists());
    }

    // -----------------------------------------------------------------------
    // cmd_sync — local-only manifest (no network)
    // -----------------------------------------------------------------------

    #[test]
    fn cmd_sync_local_only_manifest_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Skillfile"),
            "install  claude-code  global\n\
             local  skill  git-commit  skills/git/commit.md\n",
        )
        .unwrap();
        // Create the local skill file so it is valid
        let skills_dir = dir.path().join("skills").join("git");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("commit.md"), b"# git commit").unwrap();

        let result = super::cmd_sync(dir.path(), false, None, false);
        assert!(result.is_ok(), "cmd_sync failed: {result:?}");
    }

    #[test]
    fn cmd_sync_no_entries_in_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Skillfile"),
            "install  claude-code  global\n",
        )
        .unwrap();

        let result = super::cmd_sync(dir.path(), false, None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_sync_missing_skillfile_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        // No Skillfile written

        let result = super::cmd_sync(dir.path(), false, None, false);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Skillfile"),
            "error message should mention Skillfile: {msg}"
        );
    }

    #[test]
    fn cmd_sync_entry_filter_local_only_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Skillfile"),
            "install  claude-code  global\n\
             local  skill  alpha  skills/alpha.md\n\
             local  skill  beta   skills/beta.md\n",
        )
        .unwrap();

        // Filter to only 'alpha' — must succeed
        let result = super::cmd_sync(dir.path(), false, Some("alpha"), false);
        assert!(result.is_ok(), "cmd_sync with filter failed: {result:?}");
    }

    #[test]
    fn cmd_sync_entry_filter_not_found_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Skillfile"),
            "install  claude-code  global\n\
             local  skill  alpha  skills/alpha.md\n",
        )
        .unwrap();

        let result = super::cmd_sync(dir.path(), false, Some("nonexistent"), false);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("nonexistent"),
            "error message should mention the missing entry name: {msg}"
        );
    }

    #[test]
    fn cmd_sync_dry_run_local_only_does_not_write_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Skillfile"),
            "install  claude-code  global\n\
             local  skill  git-commit  skills/git/commit.md\n",
        )
        .unwrap();

        let result = super::cmd_sync(dir.path(), true, None, false);
        assert!(result.is_ok(), "cmd_sync dry-run failed: {result:?}");

        // No Skillfile.lock must have been written
        assert!(!dir.path().join("Skillfile.lock").exists());
    }

    // -----------------------------------------------------------------------
    // fetch_file_at_sha
    // -----------------------------------------------------------------------

    #[test]
    fn fetch_file_at_sha_github_returns_content() {
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        let entry = skill_entry("my-skill", "skills/my-skill.md");
        let raw_url =
            format!("https://raw.githubusercontent.com/owner/repo/{sha}/skills/my-skill.md");
        let client = MockClient::new().with_bytes(raw_url, b"# My Skill\nHello world.".to_vec());

        let result = fetch_file_at_sha(&client, &entry, sha);
        assert!(result.is_ok(), "fetch_file_at_sha failed: {result:?}");
        assert_eq!(result.unwrap(), "# My Skill\nHello world.");
    }

    #[test]
    fn fetch_file_at_sha_dot_path_uses_skill_md() {
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "root".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: ".".into(),
                ref_: "main".into(),
            },
        };
        let raw_url = format!("https://raw.githubusercontent.com/owner/repo/{sha}/SKILL.md");
        let client = MockClient::new().with_bytes(raw_url, b"# Root skill content".to_vec());

        let result = fetch_file_at_sha(&client, &entry, sha);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "# Root skill content");
    }

    #[test]
    fn fetch_file_at_sha_non_github_returns_error() {
        let entry = local_entry("git-commit", "skills/commit.md");
        let client = MockClient::new();

        let result = fetch_file_at_sha(&client, &entry, "somesha");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("github"), "error should mention github: {msg}");
    }

    #[test]
    fn fetch_file_at_sha_url_entry_returns_error() {
        let entry = url_entry("my-skill", "https://example.com/skill.md");
        let client = MockClient::new();

        let result = fetch_file_at_sha(&client, &entry, "somesha");
        assert!(result.is_err());
    }

    #[test]
    fn fetch_file_at_sha_binary_returns_error() {
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        let entry = skill_entry("my-skill", "skills/my-skill.md");
        let raw_url =
            format!("https://raw.githubusercontent.com/owner/repo/{sha}/skills/my-skill.md");
        // Simulate a binary (non-UTF-8) response
        let client = MockClient::new().with_bytes(
            raw_url,
            vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        );

        let result = fetch_file_at_sha(&client, &entry, sha);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("binary"),
            "error should mention binary file: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // fetch_dir_at_sha
    // -----------------------------------------------------------------------

    /// Build the JSON that `list_github_dir_recursive` expects from the Git Trees API.
    fn make_tree_json(owner_repo: &str, _sha: &str, base: &str, files: &[&str]) -> String {
        let prefix = format!("{base}/");
        let tree: Vec<serde_json::Value> = files
            .iter()
            .map(|f| {
                serde_json::json!({
                    "type": "blob",
                    "path": format!("{prefix}{f}"),
                    "url":  format!("https://api.github.com/repos/{owner_repo}/git/blobs/dummy")
                })
            })
            .collect();
        serde_json::json!({ "tree": tree }).to_string()
    }

    #[test]
    fn fetch_dir_at_sha_github_returns_map() {
        let sha = "deadbeef1234deadbeef1234deadbeef12345678";
        let entry = skill_entry("python-pro", "skills/python-pro");

        let tree_url =
            format!("https://api.github.com/repos/owner/repo/git/trees/{sha}?recursive=1");
        let tree_json = make_tree_json(
            "owner/repo",
            sha,
            "skills/python-pro",
            &["python.md", "advanced.md"],
        );

        let raw_url_py = format!(
            "https://raw.githubusercontent.com/owner/repo/{sha}/skills/python-pro/python.md"
        );
        let raw_url_adv = format!(
            "https://raw.githubusercontent.com/owner/repo/{sha}/skills/python-pro/advanced.md"
        );

        let client = MockClient::new()
            .with_json(tree_url, Some(tree_json))
            .with_bytes(raw_url_py, b"# Python skill".to_vec())
            .with_bytes(raw_url_adv, b"# Advanced skill".to_vec());

        let result = fetch_dir_at_sha(&client, &entry, sha);
        assert!(result.is_ok(), "fetch_dir_at_sha failed: {result:?}");

        let map = result.unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map["python.md"], "# Python skill");
        assert_eq!(map["advanced.md"], "# Advanced skill");
    }

    #[test]
    fn fetch_dir_at_sha_non_github_returns_error() {
        let entry = local_entry("python-pro", "skills/python-pro");
        let client = MockClient::new();

        let result = fetch_dir_at_sha(&client, &entry, "somesha");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("github"), "error should mention github: {msg}");
    }

    #[test]
    fn fetch_dir_at_sha_url_entry_returns_error() {
        let entry = url_entry("my-skill", "https://example.com/skills/");
        let client = MockClient::new();

        let result = fetch_dir_at_sha(&client, &entry, "somesha");
        assert!(result.is_err());
    }

    #[test]
    fn fetch_dir_at_sha_skips_binary_files() {
        let sha = "deadbeef1234deadbeef1234deadbeef12345678";
        let entry = skill_entry("mixed-dir", "skills/mixed-dir");

        let tree_url =
            format!("https://api.github.com/repos/owner/repo/git/trees/{sha}?recursive=1");
        let tree_json = make_tree_json(
            "owner/repo",
            sha,
            "skills/mixed-dir",
            &["text.md", "image.png"],
        );

        let raw_url_txt =
            format!("https://raw.githubusercontent.com/owner/repo/{sha}/skills/mixed-dir/text.md");
        let raw_url_bin = format!(
            "https://raw.githubusercontent.com/owner/repo/{sha}/skills/mixed-dir/image.png"
        );

        let client = MockClient::new()
            .with_json(tree_url, Some(tree_json))
            .with_bytes(raw_url_txt, b"# Text content".to_vec())
            // Non-UTF-8 bytes for the binary file
            .with_bytes(raw_url_bin, vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A]);

        let result = fetch_dir_at_sha(&client, &entry, sha);
        assert!(result.is_ok(), "fetch_dir_at_sha failed: {result:?}");

        let map = result.unwrap();
        // Binary file must be silently skipped
        assert_eq!(map.len(), 1);
        assert_eq!(map["text.md"], "# Text content");
        assert!(!map.contains_key("image.png"));
    }

    #[test]
    fn fetch_dir_at_sha_empty_directory_returns_empty_map() {
        let sha = "deadbeef1234deadbeef1234deadbeef12345678";
        let entry = skill_entry("empty-dir", "skills/empty-dir");

        let tree_url =
            format!("https://api.github.com/repos/owner/repo/git/trees/{sha}?recursive=1");
        // Tree with no entries under the prefix
        let tree_json = serde_json::json!({ "tree": [] }).to_string();

        let client = MockClient::new().with_json(tree_url, Some(tree_json));

        let result = fetch_dir_at_sha(&client, &entry, sha);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
