use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use skillfile_core::error::SkillfileError;
use skillfile_core::lock::{lock_key, read_lock, write_lock};
use skillfile_core::models::{short_sha, Entry, LockEntry, SourceFields};
use skillfile_core::parser::{parse_manifest, MANIFEST_NAME};
use skillfile_core::progress;

use crate::http::{HttpClient, UreqClient};
use crate::resolver::{
    fetch_files_parallel, fetch_github_file, fetch_gitlab_file, http_get,
    list_github_dir_recursive, list_gitlab_dir_recursive, resolve_github_sha, resolve_gitlab_sha,
    DirEntry, GithubFetch, GitlabFetch,
};
use crate::strategy::{content_file, is_cached_dir_entry, is_dir_entry, meta_sha};

pub const VENDOR_DIR: &str = ".skillfile/cache";
static CACHE_TX_COUNTER: AtomicU64 = AtomicU64::new(0);

fn cache_transaction_path(vdir: &Path, role: &str) -> PathBuf {
    let sequence = CACHE_TX_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = vdir.file_name().unwrap_or_default().to_string_lossy();
    vdir.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{name}.{role}-{}-{sequence}", std::process::id()))
}

fn remove_cache_path(path: &Path) -> std::io::Result<()> {
    if path.is_dir() && !path.is_symlink() {
        std::fs::remove_dir_all(path)
    } else if path.exists() || path.is_symlink() {
        std::fs::remove_file(path)
    } else {
        Ok(())
    }
}

fn restore_cache_after_publish_failure(
    backup: &Path,
    vdir: &Path,
    publish_error: &std::io::Error,
) -> Result<(), SkillfileError> {
    std::fs::rename(backup, vdir).map_err(|rollback_error| {
        SkillfileError::Install(format!(
            "failed to publish cache: {publish_error}; rollback failed: {rollback_error}"
        ))
    })
}

fn publish_staged_cache(staging: &Path, vdir: &Path) -> Result<(), SkillfileError> {
    let backup = cache_transaction_path(vdir, "backup");
    let had_cache = vdir.exists() || vdir.is_symlink();
    if had_cache {
        std::fs::rename(vdir, &backup)?;
    }
    if let Err(publish_error) = std::fs::rename(staging, vdir) {
        if had_cache {
            restore_cache_after_publish_failure(&backup, vdir, &publish_error)?;
        }
        return Err(publish_error.into());
    }
    if had_cache {
        remove_cache_path(&backup)?;
    }
    Ok(())
}

fn replace_cache_transactionally<T>(
    vdir: &Path,
    write: impl FnOnce(&Path) -> Result<T, SkillfileError>,
) -> Result<T, SkillfileError> {
    let staging = cache_transaction_path(vdir, "staging");
    std::fs::create_dir_all(vdir.parent().unwrap_or_else(|| Path::new(".")))?;
    std::fs::create_dir(&staging)?;
    let value = match write(&staging) {
        Ok(value) => value,
        Err(error) => {
            let _ = remove_cache_path(&staging);
            return Err(error);
        }
    };
    if let Err(error) = publish_staged_cache(&staging, vdir) {
        let _ = remove_cache_path(&staging);
        return Err(error);
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ForgeType {
    Github,
    Gitlab,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RemoteRefKey {
    forge: ForgeType,
    owner_repo: String,
    ref_: String,
}

impl RemoteRefKey {
    fn new(forge: ForgeType, owner_repo: &str, ref_: &str) -> Self {
        Self {
            forge,
            owner_repo: owner_repo.to_owned(),
            ref_: ref_.to_owned(),
        }
    }
}

pub struct SyncContext {
    pub repo_root: PathBuf,
    pub dry_run: bool,
    pub update: bool,
    pub sha_cache: HashMap<RemoteRefKey, String>,
    pub locked: BTreeMap<String, LockEntry>,
}

#[must_use]
pub fn vendor_dir_for(entry: &Entry, repo_root: &Path) -> PathBuf {
    repo_root
        .join(VENDOR_DIR)
        .join(entry.entity_type.dir_name())
        .join(&entry.name)
}

/// Return `true` if `vdir` is a directory with at least one non-`.meta` file.
fn dir_has_content(vdir: &Path) -> bool {
    if !vdir.is_dir() {
        return false;
    }
    std::fs::read_dir(vdir).is_ok_and(|rd| {
        rd.filter_map(std::result::Result::ok)
            .any(|e| e.file_name() != ".meta")
    })
}

fn content_exists(entry: &Entry, vdir: &Path) -> bool {
    match &entry.source {
        SourceFields::Github { .. } | SourceFields::Gitlab { .. } => {
            if is_cached_dir_entry(entry, vdir) {
                dir_has_content(vdir)
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

fn remote_repo_ref(entry: &Entry) -> Option<(&str, &str, ForgeType)> {
    match &entry.source {
        SourceFields::Github {
            owner_repo, ref_, ..
        } => Some((owner_repo, ref_, ForgeType::Github)),
        SourceFields::Gitlab {
            owner_repo, ref_, ..
        } => Some((owner_repo, ref_, ForgeType::Gitlab)),
        _ => None,
    }
}

fn cache_remote_sha(ctx: &mut SyncContext, entry: &Entry, sha: &str) {
    let Some((owner_repo, ref_, forge)) = remote_repo_ref(entry) else {
        return;
    };
    let cache_key = RemoteRefKey::new(forge, owner_repo, ref_);
    ctx.sha_cache
        .entry(cache_key)
        .or_insert_with(|| sha.to_owned());
}

/// Sync a single entry. Updates `ctx.locked` in place.
pub fn sync_entry(
    client: &dyn HttpClient,
    entry: &Entry,
    ctx: &mut SyncContext,
) -> Result<(), SkillfileError> {
    match &entry.source {
        SourceFields::Local { .. } => {
            progress!("  {entry}: local — skipping");
            Ok(())
        }
        SourceFields::Github { .. } => {
            let params = SyncParams {
                repo_root: &ctx.repo_root,
                dry_run: ctx.dry_run,
                update: ctx.update,
                sha_cache: &ctx.sha_cache,
                locked: &ctx.locked,
            };
            if let Some((key, le)) = sync_github_core(client, entry, &params)? {
                cache_remote_sha(ctx, entry, &le.sha);
                ctx.locked.insert(key, le);
            }
            Ok(())
        }
        SourceFields::Gitlab { .. } => {
            let params = SyncParams {
                repo_root: &ctx.repo_root,
                dry_run: ctx.dry_run,
                update: ctx.update,
                sha_cache: &ctx.sha_cache,
                locked: &ctx.locked,
            };
            if let Some((key, le)) = sync_gitlab_core(client, entry, &params)? {
                cache_remote_sha(ctx, entry, &le.sha);
                ctx.locked.insert(key, le);
            }
            Ok(())
        }
        SourceFields::Url { url } => sync_url_core(&UrlSyncOp {
            client,
            entry,
            url,
            repo_root: &ctx.repo_root,
            dry_run: ctx.dry_run,
        }),
    }
}

/// Read-only parameters shared by all sync operations.
struct SyncParams<'a> {
    repo_root: &'a Path,
    dry_run: bool,
    update: bool,
    sha_cache: &'a HashMap<RemoteRefKey, String>,
    locked: &'a BTreeMap<String, LockEntry>,
}

struct GithubRef<'a> {
    owner_repo: &'a str,
    path_in_repo: &'a str,
    sha: &'a str,
}

/// Bundles the `HttpClient` reference with the GitHub coordinates for a write operation.
///
/// This lets `write_github_dir` and `write_github_file` stay within the 3-argument limit
/// while still receiving client, coordinates, and destination.
struct FetchOp<'a> {
    client: &'a dyn HttpClient,
    gh: &'a GithubRef<'a>,
    vdir: &'a Path,
    display_vdir: &'a Path,
    label: &'a str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RootRepoLayout {
    SingleFile,
    Directory,
}

fn has_root_skill_marker(entries: &[DirEntry]) -> bool {
    root_skill_marker_path(entries).is_some()
}

fn root_skill_marker_path(entries: &[DirEntry]) -> Option<&str> {
    entries
        .iter()
        .find(|entry| entry.relative_path.eq_ignore_ascii_case("SKILL.md"))
        .map(|entry| entry.relative_path.as_str())
}

fn required_root_skill_marker_path<'a>(
    entries: &'a [DirEntry],
    owner_repo: &str,
) -> Result<&'a str, SkillfileError> {
    root_skill_marker_path(entries).ok_or_else(|| {
        SkillfileError::Network(format!(
            "root skill repo '{owner_repo}' is missing SKILL.md at the repo root"
        ))
    })
}

fn root_dir_has_auxiliary_files(entries: &[DirEntry]) -> bool {
    entries
        .iter()
        .any(|entry| !entry.relative_path.eq_ignore_ascii_case("SKILL.md"))
}

fn root_repo_layout(
    entries: &[DirEntry],
    owner_repo: &str,
) -> Result<RootRepoLayout, SkillfileError> {
    if !has_root_skill_marker(entries) {
        return Err(SkillfileError::Install(format!(
            "root skill repo '{owner_repo}' is missing SKILL.md at the repo root"
        )));
    }
    if root_dir_has_auxiliary_files(entries) {
        Ok(RootRepoLayout::Directory)
    } else {
        Ok(RootRepoLayout::SingleFile)
    }
}

fn write_fetched_dir(
    vdir: &Path,
    fetched: &[(String, crate::resolver::FileContent)],
) -> Result<(), SkillfileError> {
    for (relative_path, content) in fetched {
        let dest = vdir.join(relative_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, content.as_bytes())?;
    }
    Ok(())
}

fn github_dir_raw_url(owner_repo: &str, path_in_repo: &str, sha: &str) -> String {
    if path_in_repo == "." {
        return format!("https://api.github.com/repos/{owner_repo}/git/trees/{sha}?recursive=1");
    }
    format!("https://api.github.com/repos/{owner_repo}/contents/{path_in_repo}?ref={sha}")
}

/// Fetch and write a GitHub directory entry. Returns the lock `raw_url`.
fn write_github_dir(op: &FetchOp<'_>) -> Result<String, SkillfileError> {
    let FetchOp {
        client,
        gh: GithubRef {
            owner_repo,
            path_in_repo,
            sha,
        },
        vdir: _,
        display_vdir: _,
        label: _,
    } = op;
    let ghf = GithubFetch {
        client: *client,
        owner_repo,
        ref_: sha,
    };
    let dir_entries = list_github_dir_recursive(&ghf, path_in_repo)?;
    write_github_dir_entries(op, &dir_entries)
}

fn write_github_dir_entries(
    op: &FetchOp<'_>,
    dir_entries: &[DirEntry],
) -> Result<String, SkillfileError> {
    let FetchOp {
        client,
        gh: GithubRef {
            owner_repo,
            path_in_repo,
            sha,
        },
        vdir,
        display_vdir,
        label,
    } = op;
    if dir_entries.is_empty() {
        return Err(SkillfileError::Install(format!(
            "directory {owner_repo}/{path_in_repo} contains no files"
        )));
    }
    let fetched = fetch_files_parallel(*client, dir_entries)?;
    write_fetched_dir(vdir, &fetched)?;
    progress!(
        "{label}: sha={} -> {}/ ({} files)",
        short_sha(sha),
        display_vdir.display(),
        fetched.len()
    );
    Ok(github_dir_raw_url(owner_repo, path_in_repo, sha))
}

fn maybe_write_github_root_dir(op: &FetchOp<'_>) -> Result<Option<String>, SkillfileError> {
    if op.gh.path_in_repo != "." {
        return Ok(None);
    }
    let ghf = GithubFetch {
        client: op.client,
        owner_repo: op.gh.owner_repo,
        ref_: op.gh.sha,
    };
    let dir_entries = list_github_dir_recursive(&ghf, ".").map_err(|err| {
        SkillfileError::Install(format!(
            "failed to inspect root skill repo '{}': {err}",
            op.gh.owner_repo
        ))
    })?;
    if root_repo_layout(&dir_entries, op.gh.owner_repo)? == RootRepoLayout::SingleFile {
        return Ok(None);
    }
    Ok(Some(write_github_dir_entries(op, &dir_entries)?))
}

/// Fetch and write a single GitHub file entry. Returns the lock `raw_url`.
fn write_github_file(op: &FetchOp<'_>) -> Result<String, SkillfileError> {
    let FetchOp {
        client,
        gh: GithubRef {
            owner_repo,
            path_in_repo,
            sha,
        },
        vdir,
        display_vdir,
        label,
    } = op;
    let ghf = GithubFetch {
        client: *client,
        owner_repo,
        ref_: sha,
    };
    let root_marker_path;
    let effective_path = if *path_in_repo == "." {
        let dir_entries = list_github_dir_recursive(&ghf, ".").map_err(|err| {
            SkillfileError::Install(format!(
                "failed to inspect root skill repo '{owner_repo}': {err}"
            ))
        })?;
        root_marker_path = root_skill_marker_path(&dir_entries)
            .ok_or_else(|| {
                SkillfileError::Install(format!(
                    "root skill repo '{owner_repo}' is missing SKILL.md at the repo root"
                ))
            })?
            .to_string();
        root_marker_path.as_str()
    } else {
        path_in_repo
    };
    let content = fetch_github_file(&ghf, effective_path)?;
    let filename = if *path_in_repo == "." {
        "SKILL.md"
    } else {
        std::path::Path::new(effective_path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("content.md")
    };
    let dest = vdir.join(filename);
    std::fs::write(&dest, &content)?;
    progress!(
        "{label}: sha={} -> {}",
        short_sha(sha),
        display_vdir.join(filename).display()
    );
    Ok(format!(
        "https://raw.githubusercontent.com/{owner_repo}/{sha}/{effective_path}"
    ))
}

/// Fetch and write the content for a GitHub entry into `op.vdir`.
///
/// Dispatches to `write_github_dir` or `write_github_file` based on `is_dir`.
/// Returns the canonical `raw_url` to store in the lock file.
fn fetch_and_write_github(op: &FetchOp<'_>, is_dir: bool) -> Result<String, SkillfileError> {
    std::fs::create_dir_all(op.vdir)?;
    if is_dir {
        return write_github_dir(op);
    }
    if let Some(raw_url) = maybe_write_github_root_dir(op)? {
        return Ok(raw_url);
    }
    write_github_file(op)
}

struct GithubMeta<'a> {
    vdir: &'a Path,
    owner_repo: &'a str,
    path_in_repo: &'a str,
    ref_: &'a str,
    sha: &'a str,
    raw_url: &'a str,
}

/// Write the `.meta` JSON file for a GitHub entry.
fn write_github_meta(m: &GithubMeta<'_>) -> Result<(), SkillfileError> {
    let GithubMeta {
        vdir,
        owner_repo,
        path_in_repo,
        ref_,
        sha,
        raw_url,
    } = m;
    let meta_data = serde_json::json!({
        "source_type": "github",
        "owner_repo": owner_repo,
        "path_in_repo": path_in_repo,
        "ref": ref_,
        "sha": sha,
        "raw_url": raw_url,
    });
    std::fs::write(
        vdir.join(".meta"),
        serde_json::to_string_pretty(&meta_data).expect("json! values are always serializable")
            + "\n",
    )?;
    Ok(())
}

/// Input for `resolve_sha_for_entry`: coordinates + label + previously-locked SHA.
enum ShaResolver<'a> {
    Github,
    Gitlab { host: &'a str },
}

struct ShaLookup<'a> {
    cache_key: RemoteRefKey,
    owner_repo: &'a str,
    ref_: &'a str,
    label: &'a str,
    locked_sha: Option<&'a str>,
    resolver: ShaResolver<'a>,
}

impl ShaResolver<'_> {
    fn resolve(
        &self,
        client: &dyn HttpClient,
        lookup: &ShaLookup<'_>,
    ) -> Result<String, SkillfileError> {
        match self {
            Self::Github => resolve_github_sha(client, lookup.owner_repo, lookup.ref_),
            Self::Gitlab { host } => {
                resolve_gitlab_sha(client, lookup.owner_repo, lookup.ref_, host)
            }
        }
    }
}

/// Resolve the SHA for a remote entry, returning `None` in dry-run mode.
///
/// Uses locked SHA if available (not in `--update` mode), otherwise looks
/// up the sha_cache, or resolves via the forge API.
fn resolve_remote_sha_for_entry(
    client: &dyn HttpClient,
    q: &ShaLookup<'_>,
    params: &SyncParams<'_>,
) -> Result<Option<String>, SkillfileError> {
    if let Some(ls) = q.locked_sha {
        progress!(
            "{}: re-fetching (locked sha={}) ...",
            q.label,
            short_sha(ls)
        );
        return Ok(Some(ls.to_owned()));
    }

    if let Some(cached) = params.sha_cache.get(&q.cache_key) {
        return Ok(Some(cached.clone()));
    }

    if params.dry_run {
        progress!(
            "{}: resolving {}@{} ... [dry-run]",
            q.label,
            q.owner_repo,
            q.ref_
        );
        return Ok(None);
    }

    // Fallback: resolve inline (single-entry mode or missed cache)
    let sha = q.resolver.resolve(client, q)?;
    Ok(Some(sha))
}

/// Core sync logic for a github entry. Parallel-safe: all inputs are read-only,
/// result is returned rather than inserted into a mutable map.
///
/// Parameters for `fetch_store_github`.
struct StoreOp<'a> {
    client: &'a dyn HttpClient,
    entry: &'a Entry,
    vdir: &'a Path,
    label: &'a str,
    sha: &'a str,
}

/// Fetch content, write it to disk, and record `.meta`. Returns the lock `raw_url`.
///
/// Used by `sync_github_core` after SHA resolution.
fn fetch_store_github(op: &StoreOp<'_>) -> Result<String, SkillfileError> {
    let StoreOp {
        client,
        entry,
        vdir,
        label,
        sha,
    } = op;
    let SourceFields::Github {
        owner_repo,
        path_in_repo,
        ref_,
    } = &entry.source
    else {
        unreachable!()
    };
    let gh = GithubRef {
        owner_repo,
        path_in_repo,
        sha,
    };
    replace_cache_transactionally(vdir, |staging| {
        let fetch_op = FetchOp {
            client: *client,
            gh: &gh,
            vdir: staging,
            display_vdir: vdir,
            label,
        };
        let raw_url = fetch_and_write_github(&fetch_op, is_dir_entry(entry))?;
        write_github_meta(&GithubMeta {
            vdir: staging,
            owner_repo,
            path_in_repo,
            ref_,
            sha,
            raw_url: &raw_url,
        })?;
        Ok(raw_url)
    })
}

/// `sha_cache` maps a forge-qualified remote ref to its resolved SHA.
/// Pre-populated by `resolve_shas_parallel` so no mutation is needed here.
fn sync_github_core(
    client: &dyn HttpClient,
    entry: &Entry,
    params: &SyncParams<'_>,
) -> Result<Option<(String, LockEntry)>, SkillfileError> {
    let label = format!("  {entry}");
    let vdir = vendor_dir_for(entry, params.repo_root);
    let key = lock_key(entry);

    let SourceFields::Github {
        owner_repo, ref_, ..
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

    let q = ShaLookup {
        cache_key: RemoteRefKey::new(ForgeType::Github, owner_repo, ref_),
        owner_repo,
        ref_,
        label: &label,
        locked_sha: locked_sha.as_deref(),
        resolver: ShaResolver::Github,
    };
    let Some(sha) = resolve_remote_sha_for_entry(client, &q, params)? else {
        return Ok(None); // dry-run
    };

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

    let raw_url = fetch_store_github(&StoreOp {
        client,
        entry,
        vdir: &vdir,
        label: &label,
        sha: &sha,
    })?;
    Ok(Some((key, LockEntry { sha, raw_url })))
}

// ---------------------------------------------------------------------------
// GitLab sync helpers — mirrors the GitHub equivalents above
// ---------------------------------------------------------------------------

struct GitlabRef<'a> {
    owner_repo: &'a str,
    path_in_repo: &'a str,
    sha: &'a str,
    host: &'a str,
}

struct GitlabFetchOp<'a> {
    client: &'a dyn HttpClient,
    gl: &'a GitlabRef<'a>,
    vdir: &'a Path,
    display_vdir: &'a Path,
    label: &'a str,
}

fn gitlab_dir_raw_url(gl: &GitlabRef<'_>) -> String {
    let encoded_project = gl.owner_repo.replace('/', "%2F");
    if gl.path_in_repo == "." {
        return format!(
            "https://{}/api/v4/projects/{encoded_project}/repository/tree?ref={}&recursive=true",
            gl.host, gl.sha
        );
    }
    let encoded_path = gl.path_in_repo.replace('/', "%2F");
    format!(
        "https://{}/api/v4/projects/{encoded_project}/repository/tree?ref={}&path={encoded_path}",
        gl.host, gl.sha
    )
}

/// Fetch and write a GitLab directory entry. Returns the lock `raw_url`.
fn write_gitlab_dir(op: &GitlabFetchOp<'_>) -> Result<String, SkillfileError> {
    let GitlabFetchOp {
        client,
        gl:
            GitlabRef {
                owner_repo,
                path_in_repo,
                sha,
                host,
            },
        vdir: _,
        display_vdir: _,
        label: _,
    } = op;
    let glf = GitlabFetch {
        client: *client,
        owner_repo,
        ref_: sha,
        host,
    };
    let dir_entries = list_gitlab_dir_recursive(&glf, path_in_repo)?;
    write_gitlab_dir_entries(op, &dir_entries)
}

fn write_gitlab_dir_entries(
    op: &GitlabFetchOp<'_>,
    dir_entries: &[DirEntry],
) -> Result<String, SkillfileError> {
    let GitlabFetchOp {
        client,
        gl:
            GitlabRef {
                owner_repo,
                path_in_repo,
                sha,
                host,
            },
        vdir,
        display_vdir,
        label,
    } = op;
    if dir_entries.is_empty() {
        return Err(SkillfileError::Install(format!(
            "directory {owner_repo}/{path_in_repo} contains no files"
        )));
    }
    let fetched = fetch_files_parallel(*client, dir_entries)?;
    write_fetched_dir(vdir, &fetched)?;
    progress!(
        "{label}: sha={} -> {}/ ({} files)",
        short_sha(sha),
        display_vdir.display(),
        fetched.len()
    );
    Ok(gitlab_dir_raw_url(&GitlabRef {
        owner_repo,
        path_in_repo,
        sha,
        host,
    }))
}

fn maybe_write_gitlab_root_dir(op: &GitlabFetchOp<'_>) -> Result<Option<String>, SkillfileError> {
    if op.gl.path_in_repo != "." {
        return Ok(None);
    }
    let glf = GitlabFetch {
        client: op.client,
        owner_repo: op.gl.owner_repo,
        ref_: op.gl.sha,
        host: op.gl.host,
    };
    let dir_entries = list_gitlab_dir_recursive(&glf, ".").map_err(|err| {
        SkillfileError::Install(format!(
            "failed to inspect root skill repo '{}': {err}",
            op.gl.owner_repo
        ))
    })?;
    if root_repo_layout(&dir_entries, op.gl.owner_repo)? == RootRepoLayout::SingleFile {
        return Ok(None);
    }
    Ok(Some(write_gitlab_dir_entries(op, &dir_entries)?))
}

/// Fetch and write a single GitLab file entry. Returns the lock `raw_url`.
fn write_gitlab_file(op: &GitlabFetchOp<'_>) -> Result<String, SkillfileError> {
    let GitlabFetchOp {
        client,
        gl:
            GitlabRef {
                owner_repo,
                path_in_repo,
                sha,
                host,
            },
        vdir,
        display_vdir,
        label,
    } = op;
    let glf = GitlabFetch {
        client: *client,
        owner_repo,
        ref_: sha,
        host,
    };
    let root_marker_path;
    let effective_path = if *path_in_repo == "." {
        let dir_entries = list_gitlab_dir_recursive(&glf, ".").map_err(|err| {
            SkillfileError::Install(format!(
                "failed to inspect root skill repo '{owner_repo}': {err}"
            ))
        })?;
        root_marker_path = root_skill_marker_path(&dir_entries)
            .ok_or_else(|| {
                SkillfileError::Install(format!(
                    "root skill repo '{owner_repo}' is missing SKILL.md at the repo root"
                ))
            })?
            .to_string();
        root_marker_path.as_str()
    } else {
        path_in_repo
    };
    let content = fetch_gitlab_file(&glf, effective_path)?;
    let filename = if *path_in_repo == "." {
        "SKILL.md"
    } else {
        std::path::Path::new(effective_path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("content.md")
    };
    let dest = vdir.join(filename);
    std::fs::write(&dest, &content)?;
    progress!(
        "{label}: sha={} -> {}",
        short_sha(sha),
        display_vdir.join(filename).display()
    );
    let encoded_project = owner_repo.replace('/', "%2F");
    let encoded_path = effective_path.replace('/', "%2F");
    Ok(format!(
        "https://{host}/api/v4/projects/{encoded_project}/repository/files/{encoded_path}/raw?ref={sha}"
    ))
}

/// Fetch and write the content for a GitLab entry into `op.vdir`.
fn fetch_and_write_gitlab(op: &GitlabFetchOp<'_>, is_dir: bool) -> Result<String, SkillfileError> {
    std::fs::create_dir_all(op.vdir)?;
    if is_dir {
        return write_gitlab_dir(op);
    }
    if let Some(raw_url) = maybe_write_gitlab_root_dir(op)? {
        return Ok(raw_url);
    }
    write_gitlab_file(op)
}

struct GitlabMeta<'a> {
    vdir: &'a Path,
    owner_repo: &'a str,
    path_in_repo: &'a str,
    ref_: &'a str,
    sha: &'a str,
    raw_url: &'a str,
    host: &'a str,
}

/// Write the `.meta` JSON file for a GitLab entry.
fn write_gitlab_meta(m: &GitlabMeta<'_>) -> Result<(), SkillfileError> {
    let GitlabMeta {
        vdir,
        owner_repo,
        path_in_repo,
        ref_,
        sha,
        raw_url,
        host,
    } = m;
    let meta_data = serde_json::json!({
        "source_type": "gitlab",
        "host": host,
        "owner_repo": owner_repo,
        "path_in_repo": path_in_repo,
        "ref": ref_,
        "sha": sha,
        "raw_url": raw_url,
    });
    std::fs::write(
        vdir.join(".meta"),
        serde_json::to_string_pretty(&meta_data).expect("json! values are always serializable")
            + "\n",
    )?;
    Ok(())
}

struct GitlabStoreOp<'a> {
    store: &'a StoreOp<'a>,
    host: &'a str,
}

fn fetch_store_gitlab(op: &GitlabStoreOp<'_>) -> Result<String, SkillfileError> {
    let StoreOp {
        client,
        entry,
        vdir,
        label,
        sha,
    } = op.store;
    let SourceFields::Gitlab {
        owner_repo,
        path_in_repo,
        ref_,
    } = &entry.source
    else {
        unreachable!()
    };
    let host = op.host;
    let gl = GitlabRef {
        owner_repo,
        path_in_repo,
        sha,
        host,
    };
    replace_cache_transactionally(vdir, |staging| {
        let fetch_op = GitlabFetchOp {
            client: *client,
            gl: &gl,
            vdir: staging,
            display_vdir: vdir,
            label,
        };
        let raw_url = fetch_and_write_gitlab(&fetch_op, is_dir_entry(entry))?;
        write_gitlab_meta(&GitlabMeta {
            vdir: staging,
            owner_repo,
            path_in_repo,
            ref_,
            sha,
            raw_url: &raw_url,
            host,
        })?;
        Ok(raw_url)
    })
}

/// Core sync logic for a GitLab entry. Mirrors `sync_github_core`.
fn sync_gitlab_core(
    client: &dyn HttpClient,
    entry: &Entry,
    params: &SyncParams<'_>,
) -> Result<Option<(String, LockEntry)>, SkillfileError> {
    let label = format!("  {entry}");
    let vdir = vendor_dir_for(entry, params.repo_root);
    let key = lock_key(entry);

    let SourceFields::Gitlab {
        owner_repo, ref_, ..
    } = &entry.source
    else {
        unreachable!()
    };
    let host = crate::http::gitlab_host();
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

    let lookup = ShaLookup {
        cache_key: RemoteRefKey::new(ForgeType::Gitlab, owner_repo, ref_),
        owner_repo,
        ref_,
        label: &label,
        locked_sha: locked_sha.as_deref(),
        resolver: ShaResolver::Gitlab { host: &host },
    };
    let Some(sha) = resolve_remote_sha_for_entry(client, &lookup, params)? else {
        return Ok(None); // dry-run
    };

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

    let store_op = StoreOp {
        client,
        entry,
        vdir: &vdir,
        label: &label,
        sha: &sha,
    };
    let raw_url = fetch_store_gitlab(&GitlabStoreOp {
        store: &store_op,
        host: &host,
    })?;
    Ok(Some((key, LockEntry { sha, raw_url })))
}

struct UrlSyncOp<'a> {
    client: &'a dyn HttpClient,
    entry: &'a Entry,
    url: &'a str,
    repo_root: &'a Path,
    dry_run: bool,
}

/// Core sync logic for a URL entry. Parallel-safe.
fn sync_url_core(op: &UrlSyncOp<'_>) -> Result<(), SkillfileError> {
    let UrlSyncOp {
        client,
        entry,
        url,
        repo_root,
        dry_run,
    } = op;
    let label = format!("  {entry}");
    let vdir = vendor_dir_for(entry, repo_root);

    if *dry_run {
        progress!("{label}: fetching {url} ... [dry-run]");
        return Ok(());
    }

    let content = http_get(*client, url)?;
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
        SourceFields::Gitlab { .. } => sync_gitlab_core(client, entry, params),
        SourceFields::Url { url } => {
            sync_url_core(&UrlSyncOp {
                client,
                entry,
                url,
                repo_root: params.repo_root,
                dry_run: params.dry_run,
            })?;
            Ok(None)
        }
    }
}

struct RepoRef<'a> {
    owner_repo: &'a str,
    ref_: &'a str,
    forge: ForgeType,
}

/// Return `true` if the entry matches `rr` and lacks a locked SHA.
fn entry_needs_resolution(
    e: &Entry,
    rr: &RepoRef<'_>,
    locked: &BTreeMap<String, LockEntry>,
) -> bool {
    let Some((or, r, forge)) = remote_repo_ref(e) else {
        return false;
    };
    or == rr.owner_repo && r == rr.ref_ && forge == rr.forge && locked.get(&lock_key(e)).is_none()
}

/// Return `true` if any entry matching `rr` lacks a locked SHA.
///
/// Used by `collect_pairs_to_resolve` to decide whether resolution is needed
/// in non-update mode.
fn needs_sha_resolution(
    entries: &[&Entry],
    rr: &RepoRef<'_>,
    locked: &BTreeMap<String, LockEntry>,
) -> bool {
    entries
        .iter()
        .any(|e| entry_needs_resolution(e, rr, locked))
}

/// Collect unique remote refs that need SHA resolution.
fn collect_pairs_to_resolve(
    entries: &[&Entry],
    locked: &BTreeMap<String, LockEntry>,
    update: bool,
) -> Vec<RemoteRefKey> {
    let mut need_resolve = Vec::new();
    let mut seen = HashSet::new();

    for entry in entries {
        let Some((owner_repo, ref_, forge)) = remote_repo_ref(entry) else {
            continue;
        };
        let key = RemoteRefKey::new(forge, owner_repo, ref_);
        if !seen.insert(key.clone()) {
            continue;
        }
        // In update mode, always re-resolve. Otherwise, skip if all entries
        // with this repo+ref already have locked SHAs.
        let rr = RepoRef {
            owner_repo,
            ref_,
            forge,
        };
        if update || needs_sha_resolution(entries, &rr, locked) {
            need_resolve.push(key);
        }
    }

    need_resolve
}

struct ResolveCtx<'a> {
    entries: &'a [&'a Entry],
    locked: &'a BTreeMap<String, LockEntry>,
    update: bool,
}

struct PendingResolve {
    key: RemoteRefKey,
}

impl PendingResolve {
    /// Dispatch to the correct forge API.
    fn resolve(&self, client: &dyn HttpClient) -> Result<String, SkillfileError> {
        match self.key.forge {
            ForgeType::Github => resolve_github_sha(client, &self.key.owner_repo, &self.key.ref_),
            ForgeType::Gitlab => {
                let host = crate::http::gitlab_host();
                resolve_gitlab_sha(client, &self.key.owner_repo, &self.key.ref_, &host)
            }
        }
    }
}

/// Resolve all unique remote refs in parallel.
///
/// In `--update` mode, all remote entries need resolution (locked SHAs ignored).
/// In normal mode, only entries without a locked SHA need resolution.
fn resolve_shas_parallel(
    client: &dyn HttpClient,
    ctx: &ResolveCtx<'_>,
) -> HashMap<RemoteRefKey, String> {
    let keys = collect_pairs_to_resolve(ctx.entries, ctx.locked, ctx.update);

    if keys.is_empty() {
        return HashMap::new();
    }

    let pending: Vec<PendingResolve> = keys.into_iter().map(|key| PendingResolve { key }).collect();

    // Resolve in parallel using scoped threads — borrows avoid cloning.
    let results: Vec<Result<String, SkillfileError>> = std::thread::scope(|s| {
        let handles: Vec<_> = pending
            .iter()
            .map(|p| s.spawn(|| p.resolve(client)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("SHA resolution thread panicked"))
            .collect()
    });

    let mut cache = HashMap::with_capacity(results.len());
    for (p, result) in pending.into_iter().zip(results) {
        let Ok(sha) = result else {
            continue;
        };
        progress!(
            "  resolved {}@{} -> {}",
            p.key.owner_repo,
            p.key.ref_,
            short_sha(&sha)
        );
        cache.insert(p.key, sha);
    }
    cache
}

struct SyncJob<'a> {
    client: &'a dyn HttpClient,
    repo_root: &'a Path,
    entries: &'a [&'a Entry],
    dry_run: bool,
    update: bool,
    locked: BTreeMap<String, LockEntry>,
}

/// Options for the `sync` command.
pub struct SyncCmdOpts<'a> {
    pub repo_root: &'a Path,
    pub dry_run: bool,
    pub entry_filter: Option<&'a str>,
    pub update: bool,
}

/// Run the `sync` command.
pub fn cmd_sync(opts: &SyncCmdOpts<'_>) -> Result<(), SkillfileError> {
    let repo_root = opts.repo_root;
    let dry_run = opts.dry_run;
    let entry_filter = opts.entry_filter;
    let update = opts.update;
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

    let count = entries.len();
    let noun = if count == 1 { "entry" } else { "entries" };
    let mode = if dry_run { " [dry-run]" } else { "" };
    progress!("Syncing {count} {noun}{mode}...");

    let locked = read_lock(repo_root)?;
    let client = UreqClient::new();

    let job = SyncJob {
        client: &client,
        repo_root,
        entries: &entries,
        dry_run,
        update,
        locked,
    };

    // For single-entry filter, dry-run, or trivial manifests, use sequential path
    if entry_filter.is_some() || dry_run || entries.len() <= 1 {
        return run_sequential_sync(job);
    }

    run_parallel_sync(job)
}

/// Execute sync sequentially (used for single-entry, dry-run, or small manifests).
fn run_sequential_sync(job: SyncJob<'_>) -> Result<(), SkillfileError> {
    let SyncJob {
        client,
        repo_root,
        entries,
        dry_run,
        update,
        locked,
    } = job;
    let mut ctx = SyncContext {
        repo_root: repo_root.to_path_buf(),
        dry_run,
        update,
        sha_cache: HashMap::new(),
        locked,
    };
    for entry in entries {
        sync_entry(client, entry, &mut ctx)?;
    }
    if !dry_run {
        write_lock(repo_root, &ctx.locked)?;
        progress!("Done.");
    }
    Ok(())
}

fn merge_sync_results(
    results: Vec<Result<Option<(String, LockEntry)>, SkillfileError>>,
    locked: &mut BTreeMap<String, LockEntry>,
) -> Option<SkillfileError> {
    let mut first_error = None;
    for result in results {
        match result {
            Ok(Some((key, entry))) => {
                locked.insert(key, entry);
            }
            Ok(None) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    first_error
}

/// Execute sync in parallel using two-phase SHA resolution then fetch.
fn run_parallel_sync(job: SyncJob<'_>) -> Result<(), SkillfileError> {
    let SyncJob {
        client,
        repo_root,
        entries,
        dry_run,
        update,
        mut locked,
    } = job;

    // Phase 1: Resolve all SHAs in parallel
    let resolve_ctx = ResolveCtx {
        entries,
        locked: &locked,
        update,
    };
    let sha_cache = resolve_shas_parallel(client, &resolve_ctx);

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
                .map(|entry| s.spawn(|| sync_entry_core(client, entry, &params)))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("sync thread panicked"))
                .collect()
        });

    let sync_error = merge_sync_results(results, &mut locked);

    if !dry_run {
        write_lock(repo_root, &locked)?;
    }
    if let Some(error) = sync_error {
        return Err(error);
    }
    if !dry_run {
        progress!("Done.");
    }

    Ok(())
}

/// Used by `diff` and `resolve` in conflict mode.
/// Returns the file content as a UTF-8 string, or an error for binary files.
pub fn fetch_file_at_sha(
    client: &dyn HttpClient,
    entry: &Entry,
    sha: &str,
) -> Result<String, SkillfileError> {
    let bytes = match &entry.source {
        SourceFields::Github {
            owner_repo,
            path_in_repo,
            ..
        } => {
            let ghf = GithubFetch {
                client,
                owner_repo,
                ref_: sha,
            };
            let root_marker_path;
            let effective_path = if path_in_repo == "." {
                let dir_entries = list_github_dir_recursive(&ghf, ".")?;
                root_marker_path =
                    required_root_skill_marker_path(&dir_entries, owner_repo)?.to_string();
                root_marker_path.as_str()
            } else {
                path_in_repo
            };
            crate::resolver::fetch_github_file(&ghf, effective_path)?
        }
        SourceFields::Gitlab {
            owner_repo,
            path_in_repo,
            ..
        } => {
            let glf = GitlabFetch {
                client,
                owner_repo,
                ref_: sha,
                host: &crate::http::gitlab_host(),
            };
            let root_marker_path;
            let effective_path = if path_in_repo == "." {
                let dir_entries = list_gitlab_dir_recursive(&glf, ".")?;
                root_marker_path =
                    required_root_skill_marker_path(&dir_entries, owner_repo)?.to_string();
                root_marker_path.as_str()
            } else {
                path_in_repo
            };
            crate::resolver::fetch_gitlab_file(&glf, effective_path)?
        }
        _ => {
            return Err(SkillfileError::Network(
                "fetch_file_at_sha only supports github and gitlab entries".into(),
            ));
        }
    };
    crate::resolver::decode_safe(bytes)
        .map_err(|_| SkillfileError::Network(format!("binary file at sha {sha}")))
}

/// Returns a map of `relative_path -> content`. Binary files are silently skipped.
/// Used by `diff` and `resolve` in conflict mode.
pub fn fetch_dir_at_sha(
    client: &dyn HttpClient,
    entry: &Entry,
    sha: &str,
) -> Result<HashMap<String, String>, SkillfileError> {
    let dir_entries = match &entry.source {
        SourceFields::Github {
            owner_repo,
            path_in_repo,
            ..
        } => {
            let ghf = GithubFetch {
                client,
                owner_repo,
                ref_: sha,
            };
            crate::resolver::list_github_dir_recursive(&ghf, path_in_repo)?
        }
        SourceFields::Gitlab {
            owner_repo,
            path_in_repo,
            ..
        } => {
            let glf = GitlabFetch {
                client,
                owner_repo,
                ref_: sha,
                host: &crate::http::gitlab_host(),
            };
            crate::resolver::list_gitlab_dir_recursive(&glf, path_in_repo)?
        }
        _ => {
            return Err(SkillfileError::Network(
                "fetch_dir_at_sha only supports github and gitlab entries".into(),
            ));
        }
    };
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
        content_exists, fetch_dir_at_sha, fetch_file_at_sha, replace_cache_transactionally,
        resolve_shas_parallel, run_parallel_sync, sync_entry, vendor_dir_for, write_github_file,
        write_gitlab_file, FetchOp, ForgeType, GithubRef, GitlabFetchOp, GitlabRef, RemoteRefKey,
        ResolveCtx, SyncContext, SyncJob, VENDOR_DIR,
    };

    #[test]
    fn transactional_cache_preserves_active_cache_when_staged_write_fails() {
        let dir = tempfile::tempdir().unwrap();
        let vdir = dir.path().join("skills/example");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), "# Existing\n").unwrap();
        std::fs::write(vdir.join(".meta"), r#"{"sha":"old"}"#).unwrap();

        let result: Result<(), SkillfileError> = replace_cache_transactionally(&vdir, |staging| {
            std::fs::write(staging.join("SKILL.md"), "# Partial replacement\n")?;
            Err(SkillfileError::Network("injected failure".into()))
        });

        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(vdir.join("SKILL.md")).unwrap(),
            "# Existing\n"
        );
        assert_eq!(
            std::fs::read_to_string(vdir.join(".meta")).unwrap(),
            r#"{"sha":"old"}"#
        );
        assert_eq!(
            std::fs::read_dir(vdir.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn transactional_cache_publishes_complete_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let vdir = dir.path().join("skills/example");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), "# Existing\n").unwrap();
        std::fs::write(vdir.join("stale.txt"), "stale\n").unwrap();

        replace_cache_transactionally(&vdir, |staging| {
            std::fs::write(staging.join("SKILL.md"), "# Replacement\n")?;
            std::fs::write(staging.join(".meta"), r#"{"sha":"new"}"#)?;
            Ok(())
        })
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(vdir.join("SKILL.md")).unwrap(),
            "# Replacement\n"
        );
        assert_eq!(
            std::fs::read_to_string(vdir.join(".meta")).unwrap(),
            r#"{"sha":"new"}"#
        );
        assert!(!vdir.join("stale.txt").exists());
        assert_eq!(
            std::fs::read_dir(vdir.parent().unwrap()).unwrap().count(),
            1
        );
    }

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

    fn github_repo_entry(name: &str, owner_repo: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: name.into(),
            source: SourceFields::Github {
                owner_repo: owner_repo.into(),
                path_in_repo: format!("skills/{name}.md"),
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
            locked: BTreeMap::new(),
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
        // "skills/python-pro" has no ".md" suffix -> dir entry
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
        // Only .meta -- should be treated as empty
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
    // sync_entry -- local entry is skipped
    // -----------------------------------------------------------------------

    #[test]
    fn sync_entry_local_skips_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let entry = local_entry("git-commit", "skills/git/commit.md");
        let client = MockClient::new(); // no responses needed
        let mut ctx = make_sync_ctx(dir.path());
        let result = sync_entry(&client, &entry, &mut ctx);
        assert!(result.is_ok());
        // Lock must not have been modified for a local entry
        assert!(ctx.locked.is_empty());
    }

    // -----------------------------------------------------------------------
    // sync_entry -- github single-file entry, dry_run
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
        let result = sync_entry(&client, &entry, &mut ctx);
        assert!(result.is_ok());
        // Dry-run must not persist any lock entry
        assert!(ctx.locked.is_empty());
    }

    // -----------------------------------------------------------------------
    // sync_entry -- github single-file entry, up-to-date (locked SHA matches meta)
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

        // MockClient has no responses: if any HTTP call is made, the test fails
        let client = MockClient::new();
        let mut ctx = make_sync_ctx(dir.path());
        // Populate lock with the same SHA
        ctx.locked.insert(
            "github/skill/my-skill".to_string(),
            LockEntry {
                sha: sha.to_string(),
                raw_url: "https://raw.githubusercontent.com/owner/repo/aabbccdd/skills/my-skill.md"
                    .into(),
            },
        );

        let result = sync_entry(&client, &entry, &mut ctx);
        assert!(result.is_ok(), "unexpected error: {result:?}");
        // Lock entry must remain unchanged
        assert_eq!(ctx.locked["github/skill/my-skill"].sha, sha);
    }

    // -----------------------------------------------------------------------
    // sync_entry -- github single-file entry, SHA resolved via mock
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
        let result = sync_entry(&client, &entry, &mut ctx);
        assert!(result.is_ok(), "sync_entry failed: {result:?}");

        // Lock must be updated with the resolved SHA
        let lock_entry = ctx
            .locked
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

    #[test]
    fn sync_entry_github_root_lowercase_skill_md_fetches_observed_path() {
        let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let dir = tempfile::tempdir().unwrap();
        let entry = skill_entry("root", ".");

        let sha_url = "https://api.github.com/repos/owner/repo/commits/main".to_string();
        let sha_json = serde_json::json!({ "sha": sha }).to_string();
        let tree_url =
            format!("https://api.github.com/repos/owner/repo/git/trees/{sha}?recursive=1");
        let tree_json = serde_json::json!({
            "tree": [
                { "type": "blob", "path": "skill.md" }
            ]
        })
        .to_string();
        let raw_url = format!("https://raw.githubusercontent.com/owner/repo/{sha}/skill.md");

        let client = MockClient::new()
            .with_json(sha_url, Some(sha_json))
            .with_json(tree_url, Some(tree_json))
            .with_bytes(raw_url.clone(), b"# Lowercase root skill".to_vec());

        let mut ctx = make_sync_ctx(dir.path());
        sync_entry(&client, &entry, &mut ctx).unwrap();

        let vdir = vendor_dir_for(&entry, dir.path());
        assert_eq!(
            std::fs::read_to_string(vdir.join("SKILL.md")).unwrap(),
            "# Lowercase root skill"
        );

        let lock_entry = ctx.locked.get("github/skill/root").unwrap();
        assert_eq!(lock_entry.raw_url, raw_url);
    }

    #[test]
    fn write_github_root_file_wraps_listing_error() {
        let dir = tempfile::tempdir().unwrap();
        let client = MockClient::new();
        let gh = GithubRef {
            owner_repo: "owner/repo",
            path_in_repo: ".",
            sha: "deadbeef",
        };
        let op = FetchOp {
            client: &client,
            gh: &gh,
            vdir: dir.path(),
            display_vdir: dir.path(),
            label: "root",
        };

        let error = write_github_file(&op).unwrap_err();
        assert!(error
            .to_string()
            .contains("failed to inspect root skill repo 'owner/repo'"));
    }

    #[test]
    fn write_github_root_file_requires_skill_marker() {
        let dir = tempfile::tempdir().unwrap();
        let tree_url = "https://api.github.com/repos/owner/repo/git/trees/deadbeef?recursive=1";
        let client = MockClient::new().with_json(
            tree_url,
            Some(
                serde_json::json!({
                    "tree": [{ "type": "blob", "path": "notes.txt" }]
                })
                .to_string(),
            ),
        );
        let gh = GithubRef {
            owner_repo: "owner/repo",
            path_in_repo: ".",
            sha: "deadbeef",
        };
        let op = FetchOp {
            client: &client,
            gh: &gh,
            vdir: dir.path(),
            display_vdir: dir.path(),
            label: "root",
        };

        let error = write_github_file(&op).unwrap_err();
        assert!(error
            .to_string()
            .contains("root skill repo 'owner/repo' is missing SKILL.md"));
    }

    #[test]
    fn sync_entry_github_missing_directory_does_not_commit_empty_cache() {
        let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "missing".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "skills/missing".into(),
                ref_: "main".into(),
            },
        };
        let client = MockClient::new()
            .with_json(
                "https://api.github.com/repos/owner/repo/commits/main",
                Some(serde_json::json!({ "sha": sha }).to_string()),
            )
            .with_json(
                format!("https://api.github.com/repos/owner/repo/git/trees/{sha}?recursive=1"),
                Some(r#"{"tree":[]}"#.into()),
            )
            .with_json(
                format!(
                    "https://api.github.com/repos/owner/repo/contents/skills/missing?ref={sha}"
                ),
                None,
            );

        let error = sync_entry(&client, &entry, &mut make_sync_ctx(dir.path())).unwrap_err();

        assert!(error.to_string().contains("failed to list directory"));
        assert!(!vendor_dir_for(&entry, dir.path()).exists());
    }

    // -----------------------------------------------------------------------
    // sync_entry -- github, SHA cached across entries
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

        // SHA resolution URL appears only once in the mock -- the second call must
        // use the cache, otherwise MockClient would error on the second get_json.
        let client = MockClient::new()
            .with_json(sha_url, Some(sha_json))
            .with_bytes(raw_url1, b"# One".to_vec())
            .with_bytes(raw_url2, b"# Two".to_vec());

        let mut ctx = make_sync_ctx(dir.path());

        sync_entry(&client, &entry1, &mut ctx).unwrap();
        // After the first call the SHA must be in the cache
        assert!(ctx.sha_cache.contains_key(&RemoteRefKey::new(
            ForgeType::Github,
            "owner/repo",
            "main"
        )));

        // The second call must succeed using the cached SHA (no second get_json call)
        sync_entry(&client, &entry2, &mut ctx).unwrap();

        assert!(ctx.locked.contains_key("github/skill/skill-one"));
        assert!(ctx.locked.contains_key("github/skill/skill-two"));
    }

    #[test]
    fn sync_entry_sha_cache_isolated_between_forges() {
        let github_sha = "1111111111111111111111111111111111111111";
        let gitlab_sha = "2222222222222222222222222222222222222222";
        let dir = tempfile::tempdir().unwrap();

        let github_entry = Entry {
            entity_type: EntityType::Skill,
            name: "gh-skill".into(),
            source: SourceFields::Github {
                owner_repo: "shared/repo".into(),
                path_in_repo: "skills/gh.md".into(),
                ref_: "main".into(),
            },
        };
        let gitlab_entry = Entry {
            entity_type: EntityType::Skill,
            name: "gl-skill".into(),
            source: SourceFields::Gitlab {
                owner_repo: "shared/repo".into(),
                path_in_repo: "skills/gl.md".into(),
                ref_: "main".into(),
            },
        };

        let github_sha_url = "https://api.github.com/repos/shared/repo/commits/main".to_string();
        let gitlab_sha_url =
            "https://gitlab.com/api/v4/projects/shared%2Frepo/repository/commits/main".to_string();
        let github_sha_json = serde_json::json!({ "sha": github_sha }).to_string();
        let gitlab_sha_json = serde_json::json!({ "id": gitlab_sha }).to_string();
        let github_raw_url =
            format!("https://raw.githubusercontent.com/shared/repo/{github_sha}/skills/gh.md");
        let gitlab_raw_url = format!(
            "https://gitlab.com/api/v4/projects/shared%2Frepo/repository/files/skills%2Fgl.md/raw?ref={gitlab_sha}"
        );

        let client = MockClient::new()
            .with_json(github_sha_url, Some(github_sha_json))
            .with_json(gitlab_sha_url, Some(gitlab_sha_json))
            .with_bytes(github_raw_url, b"# GitHub".to_vec())
            .with_bytes(gitlab_raw_url, b"# GitLab".to_vec());

        let mut ctx = make_sync_ctx(dir.path());
        sync_entry(&client, &github_entry, &mut ctx).unwrap();
        sync_entry(&client, &gitlab_entry, &mut ctx).unwrap();

        assert_eq!(ctx.locked["github/skill/gh-skill"].sha, github_sha);
        assert_eq!(ctx.locked["gitlab/skill/gl-skill"].sha, gitlab_sha);
    }

    #[test]
    fn resolve_shas_parallel_does_not_dedup_across_forges() {
        let github_entry = Entry {
            entity_type: EntityType::Skill,
            name: "gh-skill".into(),
            source: SourceFields::Github {
                owner_repo: "shared/repo".into(),
                path_in_repo: "skills/gh.md".into(),
                ref_: "main".into(),
            },
        };
        let gitlab_entry = Entry {
            entity_type: EntityType::Skill,
            name: "gl-skill".into(),
            source: SourceFields::Gitlab {
                owner_repo: "shared/repo".into(),
                path_in_repo: "skills/gl.md".into(),
                ref_: "main".into(),
            },
        };
        let entries = [&github_entry, &gitlab_entry];

        let client = MockClient::new()
            .with_json(
                "https://api.github.com/repos/shared/repo/commits/main",
                Some(serde_json::json!({ "sha": "githubsha" }).to_string()),
            )
            .with_json(
                "https://gitlab.com/api/v4/projects/shared%2Frepo/repository/commits/main",
                Some(serde_json::json!({ "id": "gitlabsha" }).to_string()),
            );
        let locked = BTreeMap::new();
        let resolve_ctx = ResolveCtx {
            entries: &entries,
            locked: &locked,
            update: false,
        };

        let cache = resolve_shas_parallel(&client, &resolve_ctx);

        assert_eq!(cache.len(), 2);
        assert_eq!(
            cache[&RemoteRefKey::new(ForgeType::Github, "shared/repo", "main")],
            "githubsha"
        );
        assert_eq!(
            cache[&RemoteRefKey::new(ForgeType::Gitlab, "shared/repo", "main")],
            "gitlabsha"
        );
    }

    #[test]
    fn resolve_shas_parallel_keeps_success_when_other_resolution_fails() {
        let good = github_repo_entry("good", "good/repo");
        let bad = github_repo_entry("bad", "bad/repo");
        let entries = [&good, &bad];
        let client = MockClient::new().with_json(
            "https://api.github.com/repos/good/repo/commits/main",
            Some(serde_json::json!({ "sha": "goodsha" }).to_string()),
        );
        let locked = BTreeMap::new();

        let cache = resolve_shas_parallel(
            &client,
            &ResolveCtx {
                entries: &entries,
                locked: &locked,
                update: false,
            },
        );

        assert_eq!(cache.len(), 1);
        assert_eq!(
            cache[&RemoteRefKey::new(ForgeType::Github, "good/repo", "main")],
            "goodsha"
        );
    }

    #[test]
    fn parallel_sync_fetches_healthy_entry_after_other_resolution_fails() {
        let dir = tempfile::tempdir().unwrap();
        let good = github_repo_entry("good", "good/repo");
        let bad = github_repo_entry("bad", "bad/repo");
        let entries = [&good, &bad];
        let sha = "goodsha";
        let client = MockClient::new()
            .with_json(
                "https://api.github.com/repos/good/repo/commits/main",
                Some(serde_json::json!({ "sha": sha }).to_string()),
            )
            .with_bytes(
                format!("https://raw.githubusercontent.com/good/repo/{sha}/skills/good.md"),
                b"# Good".to_vec(),
            );

        let error = run_parallel_sync(SyncJob {
            client: &client,
            repo_root: dir.path(),
            entries: &entries,
            dry_run: false,
            update: false,
            locked: BTreeMap::new(),
        })
        .unwrap_err();

        assert!(error.to_string().contains("bad/repo"));
        assert_eq!(
            std::fs::read_to_string(vendor_dir_for(&good, dir.path()).join("good.md")).unwrap(),
            "# Good"
        );
        let lock_text = std::fs::read_to_string(dir.path().join("Skillfile.lock")).unwrap();
        let locked: BTreeMap<String, LockEntry> = serde_json::from_str(&lock_text).unwrap();
        assert_eq!(locked["github/skill/good"].sha, sha);
    }

    #[test]
    fn parallel_sync_persists_healthy_entry_after_other_fetch_fails() {
        let dir = tempfile::tempdir().unwrap();
        let good = github_repo_entry("good", "good/repo");
        let bad = github_repo_entry("bad", "bad/repo");
        let entries = [&good, &bad];
        let good_sha = "goodsha";
        let bad_sha = "badsha";
        let client = MockClient::new()
            .with_json(
                "https://api.github.com/repos/good/repo/commits/main",
                Some(serde_json::json!({ "sha": good_sha }).to_string()),
            )
            .with_json(
                "https://api.github.com/repos/bad/repo/commits/main",
                Some(serde_json::json!({ "sha": bad_sha }).to_string()),
            )
            .with_bytes(
                format!("https://raw.githubusercontent.com/good/repo/{good_sha}/skills/good.md"),
                b"# Good".to_vec(),
            );

        let error = run_parallel_sync(SyncJob {
            client: &client,
            repo_root: dir.path(),
            entries: &entries,
            dry_run: false,
            update: false,
            locked: BTreeMap::new(),
        })
        .unwrap_err();

        assert!(error.to_string().contains("bad/repo"));
        assert_eq!(
            std::fs::read_to_string(vendor_dir_for(&good, dir.path()).join("good.md")).unwrap(),
            "# Good"
        );
        let lock_text = std::fs::read_to_string(dir.path().join("Skillfile.lock")).unwrap();
        let locked: BTreeMap<String, LockEntry> = serde_json::from_str(&lock_text).unwrap();
        assert_eq!(locked["github/skill/good"].sha, good_sha);
        assert!(!locked.contains_key("github/skill/bad"));
    }

    // -----------------------------------------------------------------------
    // sync_entry -- github dot-path (SKILL.md convention)
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
        let tree_url =
            format!("https://api.github.com/repos/owner/repo/git/trees/{sha}?recursive=1");
        let tree_json = serde_json::json!({
            "tree": [{"path": "SKILL.md", "type": "blob"}]
        })
        .to_string();
        let raw_url = format!("https://raw.githubusercontent.com/owner/repo/{sha}/SKILL.md");

        let client = MockClient::new()
            .with_json(sha_url, Some(sha_json))
            .with_json(tree_url, Some(tree_json))
            .with_bytes(raw_url, b"# Root Skill".to_vec());

        let mut ctx = make_sync_ctx(dir.path());

        sync_entry(&client, &entry, &mut ctx).unwrap();

        let vdir = vendor_dir_for(&entry, dir.path());
        assert!(vdir.join("SKILL.md").exists());
    }

    #[test]
    fn sync_entry_github_dot_path_with_auxiliary_files_writes_directory() {
        let sha = "1234567890abcdef1234567890abcdef12345678";
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "book-to-skill".into(),
            source: SourceFields::Github {
                owner_repo: "virgiliojr94/book-to-skill".into(),
                path_in_repo: ".".into(),
                ref_: "master".into(),
            },
        };
        let vdir = vendor_dir_for(&entry, dir.path());
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), "# Previous\n").unwrap();
        std::fs::write(vdir.join("removed-upstream.txt"), "stale\n").unwrap();

        let sha_url = "https://api.github.com/repos/virgiliojr94/book-to-skill/commits/master";
        let sha_json = serde_json::json!({ "sha": sha }).to_string();
        let tree_url = format!(
            "https://api.github.com/repos/virgiliojr94/book-to-skill/git/trees/{sha}?recursive=1"
        );
        let tree_json = serde_json::json!({
            "tree": [
                {"path": "SKILL.md", "type": "blob"},
                {"path": "README.md", "type": "blob"},
                {"path": "scripts/extract.py", "type": "blob"}
            ]
        })
        .to_string();
        let skill_url =
            format!("https://raw.githubusercontent.com/virgiliojr94/book-to-skill/{sha}/SKILL.md");
        let script_url = format!(
            "https://raw.githubusercontent.com/virgiliojr94/book-to-skill/{sha}/scripts/extract.py"
        );

        let client = MockClient::new()
            .with_json(sha_url, Some(sha_json))
            .with_json(tree_url, Some(tree_json))
            .with_bytes(skill_url, b"# Book to Skill".to_vec())
            .with_bytes(script_url, b"print('extract')\n".to_vec());

        let mut ctx = make_sync_ctx(dir.path());
        sync_entry(&client, &entry, &mut ctx).unwrap();

        assert!(vdir.join("SKILL.md").exists());
        assert!(vdir.join("scripts/extract.py").exists());
        assert!(!vdir.join("removed-upstream.txt").exists());
        assert_eq!(
            std::fs::read_to_string(vdir.join("scripts/extract.py")).unwrap(),
            "print('extract')\n"
        );
    }

    #[test]
    fn sync_entry_github_dot_path_without_root_skill_md_errors() {
        let sha = "1234567890abcdef1234567890abcdef12345678";
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "broken-root".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: ".".into(),
                ref_: "main".into(),
            },
        };

        let sha_url = "https://api.github.com/repos/owner/repo/commits/main";
        let sha_json = serde_json::json!({ "sha": sha }).to_string();
        let tree_url =
            format!("https://api.github.com/repos/owner/repo/git/trees/{sha}?recursive=1");
        let tree_json =
            serde_json::json!({ "tree": [{"path": "scripts/extract.py", "type": "blob"}] })
                .to_string();
        let script_url =
            format!("https://raw.githubusercontent.com/owner/repo/{sha}/scripts/extract.py");

        let client = MockClient::new()
            .with_json(sha_url, Some(sha_json))
            .with_json(tree_url, Some(tree_json))
            .with_bytes(script_url, b"print('extract')\n".to_vec());

        let mut ctx = make_sync_ctx(dir.path());
        let err = sync_entry(&client, &entry, &mut ctx).unwrap_err();
        assert!(err.to_string().contains("missing SKILL.md"));

        let vdir = vendor_dir_for(&entry, dir.path());
        assert!(!vdir.join("SKILL.md").exists());
        assert!(!vdir.join("scripts/extract.py").exists());
    }

    #[test]
    fn sync_entry_github_dot_path_listing_error_does_not_fallback() {
        let sha = "1234567890abcdef1234567890abcdef12345678";
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "root".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: ".".into(),
                ref_: "main".into(),
            },
        };

        let sha_url = "https://api.github.com/repos/owner/repo/commits/main";
        let sha_json = serde_json::json!({ "sha": sha }).to_string();
        let tree_url =
            format!("https://api.github.com/repos/owner/repo/git/trees/{sha}?recursive=1");
        let raw_url = format!("https://raw.githubusercontent.com/owner/repo/{sha}/SKILL.md");

        let client = MockClient::new()
            .with_json(sha_url, Some(sha_json))
            .with_json(tree_url, Some("not-json".to_string()))
            .with_bytes(raw_url, b"# Root Skill".to_vec());

        let mut ctx = make_sync_ctx(dir.path());
        let err = sync_entry(&client, &entry, &mut ctx).unwrap_err();
        assert!(err
            .to_string()
            .contains("failed to inspect root skill repo"));

        let vdir = vendor_dir_for(&entry, dir.path());
        assert!(!vdir.join("SKILL.md").exists());
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
        let result = sync_entry(&client, &entry, &mut ctx);
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
        let result = sync_entry(&client, &entry, &mut ctx);
        assert!(result.is_ok());

        // Nothing must have been written
        let vdir = vendor_dir_for(&entry, dir.path());
        assert!(!vdir.exists());
    }

    // -----------------------------------------------------------------------
    // cmd_sync -- local-only manifest (no network)
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

        let result = super::cmd_sync(&super::SyncCmdOpts {
            repo_root: dir.path(),
            dry_run: false,
            entry_filter: None,
            update: false,
        });
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

        let result = super::cmd_sync(&super::SyncCmdOpts {
            repo_root: dir.path(),
            dry_run: false,
            entry_filter: None,
            update: false,
        });
        assert!(result.is_ok());
    }

    #[test]
    fn cmd_sync_missing_skillfile_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        // No Skillfile written

        let result = super::cmd_sync(&super::SyncCmdOpts {
            repo_root: dir.path(),
            dry_run: false,
            entry_filter: None,
            update: false,
        });
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

        // Filter to only 'alpha' -- must succeed
        let result = super::cmd_sync(&super::SyncCmdOpts {
            repo_root: dir.path(),
            dry_run: false,
            entry_filter: Some("alpha"),
            update: false,
        });
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

        let result = super::cmd_sync(&super::SyncCmdOpts {
            repo_root: dir.path(),
            dry_run: false,
            entry_filter: Some("nonexistent"),
            update: false,
        });
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

        let result = super::cmd_sync(&super::SyncCmdOpts {
            repo_root: dir.path(),
            dry_run: true,
            entry_filter: None,
            update: false,
        });
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
        let tree_url =
            format!("https://api.github.com/repos/owner/repo/git/trees/{sha}?recursive=1");
        let tree_json = serde_json::json!({
            "tree": [{ "type": "blob", "path": "SKILL.md" }]
        })
        .to_string();
        let raw_url = format!("https://raw.githubusercontent.com/owner/repo/{sha}/SKILL.md");
        let client = MockClient::new()
            .with_json(tree_url, Some(tree_json))
            .with_bytes(raw_url, b"# Root skill content".to_vec());

        let result = fetch_file_at_sha(&client, &entry, sha);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "# Root skill content");
    }

    #[test]
    fn fetch_file_at_sha_github_dot_path_uses_observed_marker_case() {
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        let entry = skill_entry("root", ".");
        let tree_url =
            format!("https://api.github.com/repos/owner/repo/git/trees/{sha}?recursive=1");
        let tree_json = serde_json::json!({
            "tree": [{ "type": "blob", "path": "skill.md" }]
        })
        .to_string();
        let raw_url = format!("https://raw.githubusercontent.com/owner/repo/{sha}/skill.md");
        let client = MockClient::new()
            .with_json(tree_url, Some(tree_json))
            .with_bytes(raw_url, b"# Lowercase root skill".to_vec());

        let result = fetch_file_at_sha(&client, &entry, sha);
        assert!(result.is_ok(), "fetch_file_at_sha failed: {result:?}");
        assert_eq!(result.unwrap(), "# Lowercase root skill");
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
    fn make_tree_json(owner_repo: &str, base: &str, files: &[&str]) -> String {
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
        let tree_json = make_tree_json("owner/repo", "skills/mixed-dir", &["text.md", "image.png"]);

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
    fn fetch_dir_at_sha_missing_directory_errors() {
        let sha = "deadbeef1234deadbeef1234deadbeef12345678";
        let entry = skill_entry("empty-dir", "skills/empty-dir");

        let tree_url =
            format!("https://api.github.com/repos/owner/repo/git/trees/{sha}?recursive=1");
        let tree_json = serde_json::json!({ "tree": [] }).to_string();

        // Contents API fallback also returns nothing (404).
        let contents_url =
            format!("https://api.github.com/repos/owner/repo/contents/skills/empty-dir?ref={sha}");

        let client = MockClient::new()
            .with_json(tree_url, Some(tree_json))
            .with_json(contents_url, None);

        let result = fetch_dir_at_sha(&client, &entry, sha);
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("failed to list directory"));
    }

    // -----------------------------------------------------------------------
    // GitLab helpers
    // -----------------------------------------------------------------------

    fn gitlab_skill_entry(name: &str, path_in_repo: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: name.into(),
            source: SourceFields::Gitlab {
                owner_repo: "group/project".into(),
                path_in_repo: path_in_repo.into(),
                ref_: "main".into(),
            },
        }
    }

    // -----------------------------------------------------------------------
    // fetch_file_at_sha -- gitlab
    // -----------------------------------------------------------------------

    #[test]
    fn fetch_file_at_sha_gitlab_returns_content() {
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        let entry = gitlab_skill_entry("my-skill", "skills/my-skill.md");
        let encoded_project = "group%2Fproject";
        let encoded_path = "skills%2Fmy-skill.md";
        let raw_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded_project}/repository/files/{encoded_path}/raw?ref={sha}"
        );
        let client = MockClient::new().with_bytes(raw_url, b"# My Skill\nHello world.".to_vec());

        let result = fetch_file_at_sha(&client, &entry, sha);
        assert!(result.is_ok(), "fetch_file_at_sha failed: {result:?}");
        assert_eq!(result.unwrap(), "# My Skill\nHello world.");
    }

    #[test]
    fn fetch_file_at_sha_gitlab_dot_path_uses_observed_marker_case() {
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        let entry = gitlab_skill_entry("root", ".");
        let encoded_project = "group%2Fproject";
        let tree_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded_project}/repository/tree?ref={sha}&recursive=true&per_page=100&page=1"
        );
        let tree_json = serde_json::json!([{ "type": "blob", "path": "skill.md" }]).to_string();
        let raw_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded_project}/repository/files/skill.md/raw?ref={sha}"
        );
        let client = MockClient::new()
            .with_json(tree_url, Some(tree_json))
            .with_bytes(raw_url, b"# Lowercase root skill".to_vec());

        let result = fetch_file_at_sha(&client, &entry, sha);
        assert!(result.is_ok(), "fetch_file_at_sha failed: {result:?}");
        assert_eq!(result.unwrap(), "# Lowercase root skill");
    }

    #[test]
    fn fetch_file_at_sha_gitlab_binary_returns_error() {
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        let entry = gitlab_skill_entry("my-skill", "skills/my-skill.md");
        let encoded_project = "group%2Fproject";
        let encoded_path = "skills%2Fmy-skill.md";
        let raw_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded_project}/repository/files/{encoded_path}/raw?ref={sha}"
        );
        let client = MockClient::new().with_bytes(
            raw_url,
            vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        );

        let result = fetch_file_at_sha(&client, &entry, sha);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("binary"));
    }

    // -----------------------------------------------------------------------
    // fetch_dir_at_sha -- gitlab
    // -----------------------------------------------------------------------

    #[test]
    fn fetch_dir_at_sha_gitlab_returns_map() {
        let sha = "deadbeef1234deadbeef1234deadbeef12345678";
        let entry = gitlab_skill_entry("release-helper", "skills/release-helper");

        let encoded_project = "group%2Fproject";
        let encoded_path = "skills%2Frelease-helper";
        let tree_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded_project}/repository/tree?ref={sha}&recursive=true&per_page=100&page=1&path={encoded_path}"
        );
        let tree_json = r#"[
            {"path": "skills/release-helper/SKILL.md", "type": "blob"},
            {"path": "skills/release-helper/refs/guide.md", "type": "blob"}
        ]"#;

        let file1_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded_project}/repository/files/skills%2Frelease-helper%2FSKILL.md/raw?ref={sha}"
        );
        let file2_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded_project}/repository/files/skills%2Frelease-helper%2Frefs%2Fguide.md/raw?ref={sha}"
        );

        let client = MockClient::new()
            .with_json(tree_url, Some(tree_json.to_string()))
            .with_bytes(file1_url, b"# Release Helper".to_vec())
            .with_bytes(file2_url, b"# Guide".to_vec());

        let result = fetch_dir_at_sha(&client, &entry, sha);
        assert!(result.is_ok(), "fetch_dir_at_sha failed: {result:?}");

        let map = result.unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map["SKILL.md"], "# Release Helper");
        assert_eq!(map["refs/guide.md"], "# Guide");
    }

    // -----------------------------------------------------------------------
    // content_exists -- gitlab
    // -----------------------------------------------------------------------

    #[test]
    fn content_exists_gitlab_single_file_present() {
        let dir = tempfile::tempdir().unwrap();
        let entry = gitlab_skill_entry("my-skill", "skills/my-skill.md");
        let vdir = dir.path().join("vdir");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("my-skill.md"), b"# skill").unwrap();
        assert!(content_exists(&entry, &vdir));
    }

    #[test]
    fn content_exists_gitlab_single_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let entry = gitlab_skill_entry("my-skill", "skills/my-skill.md");
        let vdir = dir.path().join("vdir");
        std::fs::create_dir_all(&vdir).unwrap();
        assert!(!content_exists(&entry, &vdir));
    }

    // -----------------------------------------------------------------------
    // sync_entry -- gitlab dry_run
    // -----------------------------------------------------------------------

    #[test]
    fn sync_entry_gitlab_dry_run_skips_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let entry = gitlab_skill_entry("my-skill", "skills/my-skill.md");
        let client = MockClient::new();
        let mut ctx = make_sync_ctx(dir.path());
        ctx.dry_run = true;
        let result = sync_entry(&client, &entry, &mut ctx);
        assert!(result.is_ok());
        assert!(ctx.locked.is_empty());
    }

    // -----------------------------------------------------------------------
    // sync_entry -- gitlab single-file entry, SHA resolved via mock
    // -----------------------------------------------------------------------

    #[test]
    fn sync_entry_gitlab_fetches_and_writes_file() {
        let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let dir = tempfile::tempdir().unwrap();
        let entry = gitlab_skill_entry("my-skill", "skills/my-skill.md");

        let encoded = "group%2Fproject";
        let sha_url =
            format!("https://gitlab.com/api/v4/projects/{encoded}/repository/commits/main");
        let sha_json = serde_json::json!({ "id": sha }).to_string();

        let encoded_path = "skills%2Fmy-skill.md";
        let raw_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded}/repository/files/{encoded_path}/raw?ref={sha}"
        );

        let client = MockClient::new()
            .with_json(sha_url, Some(sha_json))
            .with_bytes(raw_url, b"# My Skill\nContent here.".to_vec());

        let mut ctx = make_sync_ctx(dir.path());
        let result = sync_entry(&client, &entry, &mut ctx);
        assert!(result.is_ok(), "sync_entry failed: {result:?}");

        let lock_entry = ctx
            .locked
            .get("gitlab/skill/my-skill")
            .expect("lock entry missing");
        assert_eq!(lock_entry.sha, sha);

        let vdir = vendor_dir_for(&entry, dir.path());
        assert!(vdir.join("my-skill.md").exists());
        let written = std::fs::read_to_string(vdir.join("my-skill.md")).unwrap();
        assert_eq!(written, "# My Skill\nContent here.");

        assert!(vdir.join(".meta").exists());
        let meta_text = std::fs::read_to_string(vdir.join(".meta")).unwrap();
        let meta: serde_json::Value = serde_json::from_str(&meta_text).unwrap();
        assert_eq!(meta["source_type"], "gitlab");
    }

    #[test]
    fn sync_entry_gitlab_root_lowercase_skill_md_fetches_observed_path() {
        let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "root".into(),
            source: SourceFields::Gitlab {
                owner_repo: "group/project".into(),
                path_in_repo: ".".into(),
                ref_: "main".into(),
            },
        };

        let encoded = "group%2Fproject";
        let sha_url =
            format!("https://gitlab.com/api/v4/projects/{encoded}/repository/commits/main");
        let sha_json = serde_json::json!({ "id": sha }).to_string();
        let tree_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded}/repository/tree?ref={sha}&recursive=true&per_page=100&page=1"
        );
        let tree_json = serde_json::json!([{"path": "skill.md", "type": "blob"}]).to_string();
        let raw_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded}/repository/files/skill.md/raw?ref={sha}"
        );

        let client = MockClient::new()
            .with_json(sha_url, Some(sha_json))
            .with_json(tree_url, Some(tree_json))
            .with_bytes(raw_url.clone(), b"# Lowercase root skill".to_vec());

        let mut ctx = make_sync_ctx(dir.path());
        sync_entry(&client, &entry, &mut ctx).unwrap();

        let vdir = vendor_dir_for(&entry, dir.path());
        assert_eq!(
            std::fs::read_to_string(vdir.join("SKILL.md")).unwrap(),
            "# Lowercase root skill"
        );

        let lock_entry = ctx.locked.get("gitlab/skill/root").unwrap();
        assert_eq!(lock_entry.raw_url, raw_url);
    }

    #[test]
    fn write_gitlab_root_file_wraps_listing_error() {
        let dir = tempfile::tempdir().unwrap();
        let client = MockClient::new();
        let gl = GitlabRef {
            owner_repo: "group/project",
            path_in_repo: ".",
            sha: "deadbeef",
            host: "gitlab.com",
        };
        let op = GitlabFetchOp {
            client: &client,
            gl: &gl,
            vdir: dir.path(),
            display_vdir: dir.path(),
            label: "root",
        };

        let error = write_gitlab_file(&op).unwrap_err();
        assert!(error
            .to_string()
            .contains("failed to inspect root skill repo 'group/project'"));
    }

    #[test]
    fn write_gitlab_root_file_requires_skill_marker() {
        let dir = tempfile::tempdir().unwrap();
        let tree_url = "https://gitlab.com/api/v4/projects/group%2Fproject/repository/tree?ref=deadbeef&recursive=true&per_page=100&page=1";
        let client = MockClient::new().with_json(
            tree_url,
            Some(serde_json::json!([{"path": "notes.txt", "type": "blob"}]).to_string()),
        );
        let gl = GitlabRef {
            owner_repo: "group/project",
            path_in_repo: ".",
            sha: "deadbeef",
            host: "gitlab.com",
        };
        let op = GitlabFetchOp {
            client: &client,
            gl: &gl,
            vdir: dir.path(),
            display_vdir: dir.path(),
            label: "root",
        };

        let error = write_gitlab_file(&op).unwrap_err();
        assert!(error
            .to_string()
            .contains("root skill repo 'group/project' is missing SKILL.md"));
    }

    #[test]
    fn sync_entry_gitlab_dot_path_with_auxiliary_files_writes_directory() {
        let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "root".into(),
            source: SourceFields::Gitlab {
                owner_repo: "group/project".into(),
                path_in_repo: ".".into(),
                ref_: "main".into(),
            },
        };

        let encoded = "group%2Fproject";
        let sha_url =
            format!("https://gitlab.com/api/v4/projects/{encoded}/repository/commits/main");
        let sha_json = serde_json::json!({ "id": sha }).to_string();
        let tree_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded}/repository/tree?ref={sha}&recursive=true&per_page=100&page=1"
        );
        let tree_json = serde_json::json!([
            {"path": "SKILL.md", "type": "blob"},
            {"path": "README.md", "type": "blob"},
            {"path": "scripts/extract.py", "type": "blob"}
        ])
        .to_string();
        let skill_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded}/repository/files/SKILL.md/raw?ref={sha}"
        );
        let script_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded}/repository/files/scripts%2Fextract.py/raw?ref={sha}"
        );

        let client = MockClient::new()
            .with_json(sha_url, Some(sha_json))
            .with_json(tree_url, Some(tree_json))
            .with_bytes(skill_url, b"# Root Skill".to_vec())
            .with_bytes(script_url, b"print('extract')\n".to_vec());

        let mut ctx = make_sync_ctx(dir.path());
        sync_entry(&client, &entry, &mut ctx).unwrap();

        let vdir = vendor_dir_for(&entry, dir.path());
        assert!(vdir.join("SKILL.md").exists());
        assert!(vdir.join("scripts/extract.py").exists());
    }

    #[test]
    fn sync_entry_gitlab_dot_path_listing_error_does_not_fallback() {
        let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "root".into(),
            source: SourceFields::Gitlab {
                owner_repo: "group/project".into(),
                path_in_repo: ".".into(),
                ref_: "main".into(),
            },
        };

        let encoded = "group%2Fproject";
        let sha_url =
            format!("https://gitlab.com/api/v4/projects/{encoded}/repository/commits/main");
        let sha_json = serde_json::json!({ "id": sha }).to_string();
        let tree_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded}/repository/tree?ref={sha}&recursive=true&per_page=100&page=1"
        );
        let raw_url = format!(
            "https://gitlab.com/api/v4/projects/{encoded}/repository/files/SKILL.md/raw?ref={sha}"
        );

        let client = MockClient::new()
            .with_json(sha_url, Some(sha_json))
            .with_json(tree_url, Some("not-json".to_string()))
            .with_bytes(raw_url, b"# Root Skill".to_vec());

        let mut ctx = make_sync_ctx(dir.path());
        let err = sync_entry(&client, &entry, &mut ctx).unwrap_err();
        assert!(err
            .to_string()
            .contains("failed to inspect root skill repo"));

        let vdir = vendor_dir_for(&entry, dir.path());
        assert!(!vdir.join("SKILL.md").exists());
    }
}
