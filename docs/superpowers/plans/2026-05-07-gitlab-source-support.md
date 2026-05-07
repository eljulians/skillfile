# GitLab Source Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add GitLab as a source type for Skillfile, mirroring the existing GitHub integration including token discovery, SHA resolution, file fetching, and private repo support.

**Architecture:** Mirror the existing GitHub integration pattern: add a `SourceFields::Gitlab` enum variant, a `GitlabToken` opaque type with URL-gated access, GitLab API resolver functions, and wire everything through the parser, strategy, sync, and CLI layers. Self-hosted GitLab is supported via `GITLAB_HOST` env var or `gitlab_host` config key (defaults to `gitlab.com`).

**Tech Stack:** Rust, ureq (HTTP), serde_json (API parsing), toml (config), existing `HttpClient` trait for testability.

---

## GitLab API Reference

These are the GitLab v4 API endpoints the resolver will call. The project identifier in URLs is the `owner/repo` string URL-encoded (e.g., `my-group%2Fmy-project`).

| Operation | GitHub | GitLab |
|-----------|--------|--------|
| Resolve SHA | `GET api.github.com/repos/{owner}/{repo}/commits/{ref}` | `GET {host}/api/v4/projects/{encoded}/repository/commits/{ref}` |
| Fetch raw file | `GET raw.githubusercontent.com/{owner}/{repo}/{sha}/{path}` | `GET {host}/api/v4/projects/{encoded}/repository/files/{encoded_path}/raw?ref={sha}` |
| List tree | `GET api.github.com/repos/{owner}/{repo}/git/trees/{ref}?recursive=1` | `GET {host}/api/v4/projects/{encoded}/repository/tree?ref={ref}&recursive=true&per_page=100` (paginated) |
| Auth header | `Authorization: Bearer {token}` | `PRIVATE-TOKEN: {token}` |

## File Structure

### Files to modify:

| File | Responsibility |
|------|----------------|
| `crates/core/src/models.rs` | Add `SourceFields::Gitlab` variant + `as_gitlab()` accessor |
| `crates/core/src/parser.rs` | Add `"gitlab"` to `KNOWN_SOURCES`, add `parse_gitlab_entry()` |
| `crates/sources/src/http.rs` | Add `GitlabToken`, `discover_gitlab_token()`, `is_gitlab_url()`, inject PRIVATE-TOKEN in `UreqClient` |
| `crates/sources/src/resolver.rs` | Add `resolve_gitlab_sha()`, `fetch_gitlab_file()`, `list_gitlab_dir_recursive()` |
| `crates/sources/src/strategy.rs` | Handle `Gitlab` variant in `content_file()`, `is_dir_entry()`, `format_parts()`, add `"gitlab"` to `KNOWN_SOURCES` |
| `crates/sources/src/sync.rs` | Add `Gitlab` match arm in `sync_entry()`, `sync_entry_core()`, `content_exists()`, and parallel SHA resolution |
| `crates/cli/src/config.rs` | Add `gitlab_token` and `gitlab_host` to Config schema + read/write helpers |

### Files with trivial `Gitlab` arm additions (exhaustive match fixes):

| File | Match site | Fix |
|------|-----------|-----|
| `crates/cli/src/commands/info.rs:36` | `format_source()` | Add `Gitlab` arm mirroring `Github` but with `"gitlab"` label |
| `crates/cli/src/commands/format.rs:35` | `sort_key()` | Add `Gitlab` arm mirroring `Github` |
| `crates/deploy/src/paths.rs:96` | `source_path()` | Add `Gitlab` to the `Github \| Url` or-pattern |

### Files NOT modified (no changes needed):

| File | Why |
|------|-----|
| `crates/core/src/lock.rs` | `lock_key()` uses `entry.source_type()` which returns `"gitlab"` automatically |
| `crates/core/src/error.rs` | Existing `SkillfileError::Network` covers all error cases |
| `crates/cli/src/commands/validate.rs` | Uses `let-else` pattern — non-exhaustive, compiles as-is |
| `crates/cli/src/commands/status.rs` | Uses `let-else` and `matches!()` — non-exhaustive, compiles as-is |
| `crates/cli/src/commands/pin.rs` | Only constructs `SourceFields` in test code, no exhaustive matches |

### Known limitations (future work, not in this plan):

- `fetch_file_at_sha()` and `fetch_dir_at_sha()` in `sync.rs` only support GitHub entries. The `diff` and `resolve` commands will return "only supports github entries" for GitLab. This is acceptable for v1.
- GitLab tree API pagination: `list_gitlab_dir_recursive()` fetches only the first page (100 entries). Repos with >100 files in a directory subtree will be truncated. Follow-up to add `X-Next-Page` pagination.

---

## Task 1: Core Model — `SourceFields::Gitlab` variant

**Files:**
- Modify: `crates/core/src/models.rs`

This task adds the data representation. No HTTP, no parsing, no IO — just the enum variant and its methods.

- [ ] **Step 1: Write failing tests for the Gitlab variant**

Add these tests in the existing `#[cfg(test)] mod tests` block in `models.rs`:

```rust
#[test]
fn source_fields_gitlab_accessors() {
    let gl = SourceFields::Gitlab {
        owner_repo: "group/project".into(),
        path_in_repo: "skills/my-skill.md".into(),
        ref_: "main".into(),
    };
    assert_eq!(gl.source_type(), "gitlab");
    assert_eq!(gl.as_gitlab(), Some(("group/project", "skills/my-skill.md", "main")));
    assert_eq!(gl.as_github(), None);
    assert_eq!(gl.as_local(), None);
    assert_eq!(gl.as_url(), None);
}

#[test]
fn gitlab_entry_source_type() {
    let e = Entry {
        entity_type: EntityType::Agent,
        name: "test".into(),
        source: SourceFields::Gitlab {
            owner_repo: "g/p".into(),
            path_in_repo: "a.md".into(),
            ref_: "main".into(),
        },
    };
    assert_eq!(e.source_type(), "gitlab");
    assert_eq!(e.to_string(), "gitlab/agent/test");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p skillfile-core -- source_fields_gitlab 2>&1 | tail -20`
Expected: compilation error — `Gitlab` variant doesn't exist yet.

- [ ] **Step 3: Implement the Gitlab variant**

In `SourceFields` enum, add after the `Github` variant:

```rust
Gitlab {
    owner_repo: String,
    path_in_repo: String,
    ref_: String,
},
```

In `source_type()`, add:
```rust
SourceFields::Gitlab { .. } => "gitlab",
```

Add `as_gitlab()` method mirroring `as_github()`:
```rust
#[must_use]
pub fn as_gitlab(&self) -> Option<(&str, &str, &str)> {
    match self {
        SourceFields::Gitlab {
            owner_repo,
            path_in_repo,
            ref_,
        } => Some((owner_repo, path_in_repo, ref_)),
        _ => None,
    }
}
```

- [ ] **Step 4: Fix compiler errors in other crates**

Adding a new enum variant will cause exhaustive match errors. The goal is to get the project compiling. Use `todo!("gitlab support")` for arms in files that have their own dedicated tasks later.

**Exhaustive match sites that WILL break (must fix all):**

| File | Function | Line | Fix |
|------|----------|------|-----|
| `crates/sources/src/strategy.rs` | `content_file()` | 13 | `Gitlab { path_in_repo, .. } => todo!()` |
| `crates/sources/src/strategy.rs` | `is_dir_entry()` | 49 | `Gitlab { path_in_repo, .. } => todo!()` |
| `crates/sources/src/strategy.rs` | `format_parts()` | 67 | `Gitlab { .. } => todo!()` |
| `crates/sources/src/sync.rs` | `content_exists()` | 47 | `Gitlab { .. } => todo!()` |
| `crates/sources/src/sync.rs` | `sync_entry()` | 83 | `Gitlab { .. } => todo!()` |
| `crates/sources/src/sync.rs` | `sync_entry_core()` | 479 | `Gitlab { .. } => todo!()` |
| `crates/cli/src/commands/info.rs` | `format_source()` | 36 | Add `Gitlab` arm: `vec![("Source", format!("gitlab ({owner_repo})")), ("Path", path_in_repo.clone()), ("Ref", ref_.clone())]` |
| `crates/cli/src/commands/format.rs` | `sort_key()` | 35 | Add `Gitlab` arm mirroring `Github`: `(owner_repo.clone(), path_in_repo.clone())` |
| `crates/deploy/src/paths.rs` | `source_path()` | 96 | Change `Github { .. } \| Url { .. }` to `Github { .. } \| Gitlab { .. } \| Url { .. }` |

Run `cargo check 2>&1` to find any sites missed above. Fix until it compiles cleanly.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p skillfile-core 2>&1 | tail -10`
Expected: all tests pass, including new gitlab tests.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/models.rs crates/sources/src/strategy.rs crates/sources/src/sync.rs
git commit -m "feat(models): add SourceFields::Gitlab variant"
```

---

## Task 2: Parser — parse `gitlab` manifest lines

**Files:**
- Modify: `crates/core/src/parser.rs`

The `gitlab` manifest format is identical to `github`: `gitlab skill [name] owner/repo path [ref]`. We reuse the same parsing logic.

- [ ] **Step 1: Write failing tests**

Add in `parser.rs` tests module:

```rust
#[test]
fn gitlab_entry_explicit_name_and_ref() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_manifest(
        dir.path(),
        "gitlab  skill  my-skill  my-group/my-project  skills/my-skill.md  v2.0",
    );
    let r = parse_manifest(&p).unwrap();
    assert_eq!(r.manifest.entries.len(), 1);
    let e = &r.manifest.entries[0];
    assert_eq!(e.source_type(), "gitlab");
    assert_eq!(e.entity_type, EntityType::Skill);
    assert_eq!(e.name, "my-skill");
    let (or, pir, ref_) = e.source.as_gitlab().unwrap();
    assert_eq!(or, "my-group/my-project");
    assert_eq!(pir, "skills/my-skill.md");
    assert_eq!(ref_, "v2.0");
}

#[test]
fn gitlab_entry_inferred_name_default_ref() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_manifest(
        dir.path(),
        "gitlab  agent  my-group/my-project  agents/reviewer.md",
    );
    let r = parse_manifest(&p).unwrap();
    assert_eq!(r.manifest.entries.len(), 1);
    let e = &r.manifest.entries[0];
    assert_eq!(e.source_type(), "gitlab");
    assert_eq!(e.name, "reviewer");
    let (or, pir, ref_) = e.source.as_gitlab().unwrap();
    assert_eq!(or, "my-group/my-project");
    assert_eq!(pir, "agents/reviewer.md");
    assert_eq!(ref_, "main");
}

#[test]
fn gitlab_entry_too_few_fields_warns() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_manifest(dir.path(), "gitlab  skill");
    let r = parse_manifest(&p).unwrap();
    assert!(r.manifest.entries.is_empty());
    assert!(r.warnings.iter().any(|w| w.contains("warning")));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p skillfile-core -- gitlab_entry 2>&1 | tail -20`
Expected: FAIL — `"gitlab"` is an unknown source type, entries are empty.

- [ ] **Step 3: Implement gitlab parsing**

1. Add `"gitlab"` to `KNOWN_SOURCES`:
   ```rust
   const KNOWN_SOURCES: &[&str] = &["github", "gitlab", "local", "url"];
   ```

2. Add `parse_gitlab_entry()` function. Since the format is identical to GitHub, delegate to the same parsing logic but construct `SourceFields::Gitlab`:

   ```rust
   fn parse_gitlab_entry(
       parts: &[String],
       entity_type: EntityType,
       lineno: usize,
   ) -> (Option<Entry>, Vec<String>) {
       let mut warnings = Vec::new();

       let (name, owner_repo, path_in_repo, ref_) = if parts[2].contains('/') {
           if parts.len() < 4 {
               warnings.push(format!(
                   "warning: line {lineno}: gitlab entry needs at least: owner/repo path"
               ));
               return (None, warnings);
           }
           let ref_ = parts.get(4).map_or(DEFAULT_REF, String::as_str);
           (infer_name(&parts[3]), &parts[2], &parts[3], ref_)
       } else {
           if parts.len() < 5 {
               warnings.push(format!(
                   "warning: line {lineno}: gitlab entry needs at least: name owner/repo path"
               ));
               return (None, warnings);
           }
           if !parts[3].contains('/') {
               warnings.push(format!(
                   "warning: line {lineno}: invalid owner/repo '{}' \
                    — expected 'owner/repo' format",
                   parts[3],
               ));
               return (None, warnings);
           }
           let ref_ = parts.get(5).map_or(DEFAULT_REF, String::as_str);
           (parts[2].clone(), &parts[3], &parts[4], ref_)
       };

       let entry = Entry {
           entity_type,
           name,
           source: SourceFields::Gitlab {
               owner_repo: owner_repo.clone(),
               path_in_repo: path_in_repo.clone(),
               ref_: ref_.to_owned(),
           },
       };
       (Some(entry), warnings)
   }
   ```

3. Add the match arm in `parse_source_entry()`:
   ```rust
   "gitlab" => parse_gitlab_entry(parts, entity_type, lineno),
   ```

4. Add the match arm in `parse_manifest_line()`:
   ```rust
   "gitlab" => parse_gitlab_entry(&parts, entity_type, 0),
   ```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p skillfile-core 2>&1 | tail -10`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/parser.rs
git commit -m "feat(parser): parse gitlab source lines in Skillfile"
```

---

## Task 3: Strategy — handle `Gitlab` variant

**Files:**
- Modify: `crates/sources/src/strategy.rs`

The strategy functions for GitLab are identical to GitHub — same `content_file()`, `is_dir_entry()`, and `format_parts()` logic.

- [ ] **Step 1: Write failing tests**

Add in `strategy.rs` tests:

```rust
fn gitlab_entry(path_in_repo: &str) -> Entry {
    Entry {
        entity_type: EntityType::Skill,
        name: "test".into(),
        source: SourceFields::Gitlab {
            owner_repo: "group/project".into(),
            path_in_repo: path_in_repo.into(),
            ref_: "main".into(),
        },
    }
}

#[test]
fn content_file_gitlab_single_file() {
    let e = gitlab_entry("skills/my-skill.md");
    assert_eq!(content_file(&e), "my-skill.md");
}

#[test]
fn content_file_gitlab_dot_path() {
    let e = gitlab_entry(".");
    assert_eq!(content_file(&e), "SKILL.md");
}

#[test]
fn content_file_gitlab_dir_entry() {
    let e = gitlab_entry("skills/python-pro");
    assert_eq!(content_file(&e), "");
}

#[test]
fn is_dir_entry_gitlab_md_file() {
    assert!(!is_dir_entry(&gitlab_entry("skills/foo.md")));
}

#[test]
fn is_dir_entry_gitlab_dot_path() {
    assert!(!is_dir_entry(&gitlab_entry(".")));
}

#[test]
fn is_dir_entry_gitlab_directory() {
    assert!(is_dir_entry(&gitlab_entry("skills/python-pro")));
}

#[test]
fn format_parts_gitlab_inferred_name() {
    let e = Entry {
        entity_type: EntityType::Skill,
        name: "my-skill".into(),
        source: SourceFields::Gitlab {
            owner_repo: "group/project".into(),
            path_in_repo: "skills/my-skill.md".into(),
            ref_: "main".into(),
        },
    };
    assert_eq!(format_parts(&e), vec!["group/project", "skills/my-skill.md"]);
}

#[test]
fn format_parts_gitlab_explicit_name_and_ref() {
    let e = Entry {
        entity_type: EntityType::Skill,
        name: "custom-name".into(),
        source: SourceFields::Gitlab {
            owner_repo: "group/project".into(),
            path_in_repo: "skills/my-skill.md".into(),
            ref_: "v2.0".into(),
        },
    };
    assert_eq!(
        format_parts(&e),
        vec!["custom-name", "group/project", "skills/my-skill.md", "v2.0"]
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p skillfile-sources -- gitlab 2>&1 | tail -20`
Expected: FAIL — `todo!()` panics from Task 1.

- [ ] **Step 3: Implement the Gitlab arms**

Replace the `todo!()` arms added in Task 1 with real implementations:

In `content_file()`:
```rust
SourceFields::Gitlab { path_in_repo, .. } => github_content_file(entry, path_in_repo),
```

In `is_dir_entry()`:
```rust
SourceFields::Gitlab { path_in_repo, .. } => {
    path_in_repo != "."
        && !Path::new(path_in_repo)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
}
```

In `format_parts()`:
```rust
SourceFields::Gitlab {
    owner_repo,
    path_in_repo,
    ref_,
} => {
    let mut parts = Vec::new();
    if entry.name != infer_name(path_in_repo) {
        parts.push(entry.name.clone());
    }
    parts.push(owner_repo.clone());
    parts.push(path_in_repo.clone());
    if ref_ != DEFAULT_REF {
        parts.push(ref_.clone());
    }
    parts
}
```

Add `"gitlab"` to `KNOWN_SOURCES` in strategy.rs:
```rust
pub const KNOWN_SOURCES: &[&str] = &["github", "gitlab", "local", "url"];
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p skillfile-sources -- strategy 2>&1 | tail -10`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/sources/src/strategy.rs
git commit -m "feat(strategy): handle Gitlab variant in content_file, is_dir_entry, format_parts"
```

---

## Task 4: GitLab Token Discovery & URL Allowlist

**Files:**
- Modify: `crates/sources/src/http.rs`

Mirror the `GithubToken` pattern: opaque type, env var discovery chain, URL-gated access, `PRIVATE-TOKEN` header injection. Self-hosted support via `gitlab_host()` function.

- [ ] **Step 1: Write failing tests**

Add in `http.rs` tests module:

```rust
// -- GitlabToken newtype tests --

#[test]
fn gitlab_token_type_for_url_allows_gitlab_api() {
    let token = GitlabToken(Some("glpat-secret"));
    assert_eq!(
        token.for_url("https://gitlab.com/api/v4/projects/foo/bar"),
        Some("glpat-secret")
    );
}

#[test]
fn gitlab_token_type_for_url_rejects_github() {
    let token = GitlabToken(Some("glpat-secret"));
    assert!(token.for_url("https://api.github.com/repos/o/r").is_none());
}

#[test]
fn gitlab_token_type_for_url_returns_none_without_token() {
    let token = GitlabToken(None);
    assert!(token.for_url("https://gitlab.com/api/v4/projects/foo").is_none());
}

#[test]
fn gitlab_token_type_for_url_rejects_registries() {
    let token = GitlabToken(Some("glpat-secret"));
    assert!(token.for_url("https://agentskill.sh/api/search").is_none());
    assert!(token.for_url("https://skills.sh/api/search").is_none());
}

// -- is_gitlab_url tests --

#[test]
fn gitlab_api_url_is_gitlab() {
    assert!(is_gitlab_url("https://gitlab.com/api/v4/projects/foo%2Fbar", "gitlab.com"));
}

#[test]
fn gitlab_self_hosted_url_is_gitlab() {
    assert!(is_gitlab_url("https://gitlab.mycompany.com/api/v4/projects/foo", "gitlab.mycompany.com"));
}

#[test]
fn github_url_is_not_gitlab() {
    assert!(!is_gitlab_url("https://api.github.com/repos/o/r", "gitlab.com"));
}

#[test]
fn spoofed_gitlab_subdomain_is_not_gitlab() {
    assert!(!is_gitlab_url("https://gitlab.com.evil.com/api", "gitlab.com"));
}

#[test]
fn empty_url_is_not_gitlab() {
    assert!(!is_gitlab_url("", "gitlab.com"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p skillfile-sources -- gitlab_token 2>&1 | tail -20`
Expected: compilation error — `GitlabToken` doesn't exist yet.

- [ ] **Step 3: Implement GitLab token infrastructure**

Add after the GitHub token section in `http.rs`:

```rust
// ---------------------------------------------------------------------------
// GitLab token discovery
// ---------------------------------------------------------------------------

static GITLAB_TOKEN_CACHE: OnceLock<Option<String>> = OnceLock::new();

static GITLAB_CONFIG_TOKEN: OnceLock<Option<String>> = OnceLock::new();

pub fn set_gitlab_config_token(token: Option<String>) {
    let _ = GITLAB_CONFIG_TOKEN.set(token);
}

pub struct GitlabToken(Option<&'static str>);

impl GitlabToken {
    #[must_use]
    pub fn for_url(&self, url: &str) -> Option<&'static str> {
        is_gitlab_url(url, &gitlab_host()).then_some(self.0).flatten()
    }
}

#[must_use]
pub fn gitlab_token() -> GitlabToken {
    GitlabToken(GITLAB_TOKEN_CACHE.get_or_init(discover_gitlab_token).as_deref())
}

fn discover_gitlab_token() -> Option<String> {
    if let Some(token) = env_token("GITLAB_TOKEN") {
        return Some(token);
    }
    if let Some(token) = env_token("GITLAB_PRIVATE_TOKEN") {
        return Some(token);
    }
    if let Some(Some(token)) = GITLAB_CONFIG_TOKEN.get() {
        if !token.is_empty() {
            return Some(token.clone());
        }
    }
    glab_cli_token()
}

fn glab_cli_token() -> Option<String> {
    let output = Command::new("glab").args(["auth", "token"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!token.is_empty()).then_some(token)
}

static GITLAB_CONFIG_HOST: OnceLock<Option<String>> = OnceLock::new();

pub fn set_gitlab_config_host(host: Option<String>) {
    let _ = GITLAB_CONFIG_HOST.set(host);
}

/// Returns the GitLab host. Priority: GITLAB_HOST env > config file > "gitlab.com".
pub fn gitlab_host() -> String {
    if let Some(host) = std::env::var("GITLAB_HOST").ok().filter(|h| !h.is_empty()) {
        return host;
    }
    if let Some(Some(host)) = GITLAB_CONFIG_HOST.get() {
        if !host.is_empty() {
            return host.clone();
        }
    }
    "gitlab.com".to_string()
}

fn is_gitlab_url(url: &str, expected_host: &str) -> bool {
    let host = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .and_then(|s| s.split('/').next())
        .unwrap_or("");
    host == expected_host
}
```

- [ ] **Step 4: Inject PRIVATE-TOKEN in UreqClient**

In `UreqClient::build_get()`, after the GitHub token check:
```rust
if let Some(token) = gitlab_token().for_url(url) {
    req = req.header("PRIVATE-TOKEN", token);
}
```

Same in `UreqClient::build_post()`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p skillfile-sources -- http::tests 2>&1 | tail -10`
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/sources/src/http.rs
git commit -m "feat(http): add GitLab token discovery and PRIVATE-TOKEN header injection"
```

---

## Task 5: Config — `gitlab_token` and `gitlab_host`

**Files:**
- Modify: `crates/cli/src/config.rs`

Add `gitlab_token` and `gitlab_host` fields to the config schema with read/write helpers.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn read_gitlab_config_token_from_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "gitlab_token = \"glpat-test123\"\n").unwrap();
    let config = read_config_from(&path);
    assert_eq!(config.gitlab_token.as_deref(), Some("glpat-test123"));
}

#[test]
fn read_gitlab_host_from_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "gitlab_host = \"gitlab.mycompany.com\"\n").unwrap();
    let config = read_config_from(&path);
    assert_eq!(config.gitlab_host.as_deref(), Some("gitlab.mycompany.com"));
}

#[test]
fn write_config_preserves_gitlab_and_github_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "github_token = \"ghp_keep\"\ngitlab_token = \"glpat-keep\"\ngitlab_host = \"gitlab.internal\"\n",
    )
    .unwrap();

    let targets = vec![InstallTarget {
        adapter: "claude-code".to_string(),
        scope: Scope::Global,
    }];
    write_user_targets_to(&targets, &path).unwrap();

    let config = read_config_from(&path);
    assert_eq!(config.github_token.as_deref(), Some("ghp_keep"));
    assert_eq!(config.gitlab_token.as_deref(), Some("glpat-keep"));
    assert_eq!(config.gitlab_host.as_deref(), Some("gitlab.internal"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p skillfile -- read_gitlab 2>&1 | tail -20`
Expected: FAIL — `gitlab_token` field doesn't exist on `Config`.

- [ ] **Step 3: Implement config changes**

Add fields to `Config` struct:
```rust
#[serde(skip_serializing_if = "Option::is_none")]
gitlab_token: Option<String>,
#[serde(skip_serializing_if = "Option::is_none")]
gitlab_host: Option<String>,
```

Add public read helpers:
```rust
pub fn read_gitlab_config_token() -> Option<String> {
    let path = config_path()?;
    let config = read_config_from(&path);
    config.gitlab_token.filter(|t| !t.is_empty())
}

pub fn read_gitlab_config_host() -> Option<String> {
    let path = config_path()?;
    let config = read_config_from(&path);
    config.gitlab_host.filter(|h| !h.is_empty())
}
```

Update `write_user_targets_to()` to preserve both tokens and host:
```rust
let config = Config {
    github_token: existing.github_token,
    gitlab_token: existing.gitlab_token,
    gitlab_host: existing.gitlab_host,
    install: targets.iter().map(InstallEntry::from).collect(),
};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p skillfile -- config 2>&1 | tail -10`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/config.rs
git commit -m "feat(config): add gitlab_token and gitlab_host to config schema"
```

---

## Task 6: Resolver — GitLab SHA resolution

**Files:**
- Modify: `crates/sources/src/resolver.rs`

Add `resolve_gitlab_sha()` using the GitLab v4 API. GitLab doesn't have the same main/master ambiguity as GitHub, but we keep the same fallback pattern for consistency.

- [ ] **Step 1: Write failing tests**

Add in `resolver.rs` tests module:

```rust
// -- GitLab resolver helpers --

fn gitlab_commit_url(owner_repo: &str, ref_: &str) -> String {
    let encoded = owner_repo.replace('/', "%2F");
    format!("https://gitlab.com/api/v4/projects/{encoded}/repository/commits/{ref_}")
}

// -- resolve_gitlab_sha tests --

#[test]
fn resolve_gitlab_sha_happy_path() {
    let mut client = MockClient::new();
    let url = gitlab_commit_url("my-group/my-project", "main");
    client.add_json(&url, &gitlab_sha_json("aabbccdd11223344"));

    let sha = resolve_gitlab_sha(&client, "my-group/my-project", "main", "gitlab.com").unwrap();
    assert_eq!(sha, "aabbccdd11223344");
}

#[test]
fn resolve_gitlab_sha_with_tag() {
    let mut client = MockClient::new();
    let url = gitlab_commit_url("group/repo", "v1.0.0");
    client.add_json(&url, r#"{"id": "cafebabe12345678"}"#);

    let sha = resolve_gitlab_sha(&client, "group/repo", "v1.0.0", "gitlab.com").unwrap();
    assert_eq!(sha, "cafebabe12345678");
}

#[test]
fn resolve_gitlab_sha_not_found() {
    let mut client = MockClient::new();
    client.add_json_none(&gitlab_commit_url("group/repo", "nonexistent"));

    let err = resolve_gitlab_sha(&client, "group/repo", "nonexistent", "gitlab.com").unwrap_err();
    assert!(err.to_string().contains("could not resolve"));
}

#[test]
fn resolve_gitlab_sha_main_falls_back_to_master() {
    let mut client = MockClient::new();
    client.add_json_none(&gitlab_commit_url("group/repo", "main"));
    client.add_json(&gitlab_commit_url("group/repo", "master"), &gitlab_sha_json("fallback123"));

    let sha = resolve_gitlab_sha(&client, "group/repo", "main", "gitlab.com").unwrap();
    assert_eq!(sha, "fallback123");
}

#[test]
fn resolve_gitlab_sha_self_hosted() {
    let mut client = MockClient::new();
    let encoded = "group%2Frepo";
    let url = format!("https://gitlab.internal/api/v4/projects/{encoded}/repository/commits/main");
    client.add_json(&url, &gitlab_sha_json("selfhosted123"));

    let sha = resolve_gitlab_sha(&client, "group/repo", "main", "gitlab.internal").unwrap();
    assert_eq!(sha, "selfhosted123");
}
```

Note: GitLab's commit API returns `{"id": "full_sha", ...}` not `{"sha": "..."}` like GitHub. Add a helper:

```rust
fn gitlab_sha_json(sha: &str) -> String {
    format!(r#"{{"id": "{sha}"}}"#)
}
```

The tests above already use `gitlab_sha_json()` (first two) or inline JSON with `"id"` field (the tag test).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p skillfile-sources -- resolve_gitlab 2>&1 | tail -20`
Expected: compilation error — `resolve_gitlab_sha` doesn't exist.

- [ ] **Step 3: Implement `resolve_gitlab_sha()`**

```rust
fn gitlab_api_commit_url(host: &str, owner_repo: &str, ref_: &str) -> String {
    let encoded = owner_repo.replace('/', "%2F");
    format!("https://{host}/api/v4/projects/{encoded}/repository/commits/{ref_}")
}

fn try_resolve_gitlab_sha(
    client: &dyn HttpClient,
    owner_repo: &str,
    ref_: &str,
    host: &str,
) -> Result<Option<String>, SkillfileError> {
    let url = gitlab_api_commit_url(host, owner_repo, ref_);
    let Some(text) = client.get_json(&url)? else {
        return Ok(None);
    };
    let data: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        SkillfileError::Network(format!(
            "invalid JSON in GitLab SHA response for {owner_repo}@{ref_}: {e}"
        ))
    })?;
    // GitLab returns "id" for commit SHA (not "sha" like GitHub)
    Ok(data["id"].as_str().map(ToString::to_string))
}

pub fn resolve_gitlab_sha(
    client: &dyn HttpClient,
    owner_repo: &str,
    ref_: &str,
    host: &str,
) -> Result<String, SkillfileError> {
    if let Some(sha) = try_resolve_gitlab_sha(client, owner_repo, ref_, host)? {
        return Ok(sha);
    }
    let fallback = match ref_ {
        "main" => Some("master"),
        "master" => Some("main"),
        _ => None,
    };
    if let Some(fb) = fallback {
        if let Some(sha) = try_resolve_gitlab_sha(client, owner_repo, fb, host)? {
            return Ok(sha);
        }
    }
    Err(SkillfileError::Network(format!(
        "could not resolve {owner_repo}@{ref_} on GitLab ({host}) -- check that the project exists and the ref is valid"
    )))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p skillfile-sources -- resolve_gitlab 2>&1 | tail -10`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/sources/src/resolver.rs
git commit -m "feat(resolver): add resolve_gitlab_sha with main/master fallback"
```

---

## Task 7: Resolver — GitLab file fetching & directory listing

**Files:**
- Modify: `crates/sources/src/resolver.rs`

Add `fetch_gitlab_file()` and `list_gitlab_dir_recursive()`.

- [ ] **Step 1: Write failing tests for `fetch_gitlab_file()`**

```rust
#[test]
fn fetch_gitlab_file_basic() {
    let sha = "abc123";
    let encoded_project = "group%2Fproject";
    let encoded_path = "skills%2Fgit.md";
    let url = format!(
        "https://gitlab.com/api/v4/projects/{encoded_project}/repository/files/{encoded_path}/raw?ref={sha}"
    );
    let mut client = MockClient::new();
    client.add_bytes(&url, b"# Git skill".to_vec());

    let gh = GitlabFetch {
        client: &client,
        owner_repo: "group/project",
        ref_: sha,
        host: "gitlab.com",
    };
    let result = fetch_gitlab_file(&gh, "skills/git.md").unwrap();
    assert_eq!(result, b"# Git skill");
}

#[test]
fn fetch_gitlab_file_dot_path_becomes_skill_md() {
    let sha = "def456";
    let encoded_project = "group%2Fproject";
    let encoded_path = "SKILL.md";
    let url = format!(
        "https://gitlab.com/api/v4/projects/{encoded_project}/repository/files/{encoded_path}/raw?ref={sha}"
    );
    let mut client = MockClient::new();
    client.add_bytes(&url, b"# Root skill".to_vec());

    let gh = GitlabFetch {
        client: &client,
        owner_repo: "group/project",
        ref_: sha,
        host: "gitlab.com",
    };
    let result = fetch_gitlab_file(&gh, ".").unwrap();
    assert_eq!(result, b"# Root skill");
}
```

- [ ] **Step 2: Write failing tests for `list_gitlab_dir_recursive()`**

```rust
fn gitlab_tree_url(host: &str, owner_repo: &str, ref_: &str) -> String {
    let encoded = owner_repo.replace('/', "%2F");
    format!("https://{host}/api/v4/projects/{encoded}/repository/tree?ref={ref_}&recursive=true&per_page=100")
}

#[test]
fn list_gitlab_dir_recursive_returns_blobs_under_prefix() {
    let owner_repo = "group/project";
    let ref_ = "main";
    let url = gitlab_tree_url("gitlab.com", owner_repo, ref_);

    let json = r#"[
        {"path": "skills/dir/file1.md", "type": "blob"},
        {"path": "skills/dir/file2.md", "type": "blob"},
        {"path": "skills/dir/sub", "type": "tree"},
        {"path": "skills/other/file.md", "type": "blob"},
        {"path": "readme.md", "type": "blob"}
    ]"#;

    let mut client = MockClient::new();
    client.add_json(&url, json);

    let gh = GitlabFetch {
        client: &client,
        owner_repo,
        ref_,
        host: "gitlab.com",
    };
    let entries = list_gitlab_dir_recursive(&gh, "skills/dir").unwrap();

    assert_eq!(entries.len(), 2);
    let paths: Vec<&str> = entries.iter().map(|e| e.relative_path.as_str()).collect();
    assert!(paths.contains(&"file1.md"));
    assert!(paths.contains(&"file2.md"));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p skillfile-sources -- fetch_gitlab 2>&1 | tail -20`
Expected: compilation error.

- [ ] **Step 4: Implement `GitlabFetch`, `fetch_gitlab_file()`, and `list_gitlab_dir_recursive()`**

```rust
pub struct GitlabFetch<'a> {
    pub client: &'a dyn HttpClient,
    pub owner_repo: &'a str,
    pub ref_: &'a str,
    pub host: &'a str,
}

fn gitlab_file_url(host: &str, owner_repo: &str, ref_: &str, path: &str) -> String {
    let encoded_project = owner_repo.replace('/', "%2F");
    let encoded_path = encode_url_path(path).replace('/', "%2F");
    format!(
        "https://{host}/api/v4/projects/{encoded_project}/repository/files/{encoded_path}/raw?ref={ref_}"
    )
}

pub fn fetch_gitlab_file(
    gl: &GitlabFetch<'_>,
    path_in_repo: &str,
) -> Result<Vec<u8>, SkillfileError> {
    let effective_path = if path_in_repo == "." {
        "SKILL.md"
    } else {
        path_in_repo
    };
    let url = gitlab_file_url(gl.host, gl.owner_repo, gl.ref_, effective_path);
    http_get(gl.client, &url)
}

pub(crate) fn list_gitlab_dir_recursive(
    gl: &GitlabFetch<'_>,
    base_path: &str,
) -> Result<Vec<DirEntry>, SkillfileError> {
    let encoded = gl.owner_repo.replace('/', "%2F");
    let url = format!(
        "https://{}/api/v4/projects/{}/repository/tree?ref={}&recursive=true&per_page=100",
        gl.host, encoded, gl.ref_
    );
    let Some(text) = gl.client.get_json(&url)? else {
        return Ok(Vec::new());
    };
    let items: Vec<serde_json::Value> = serde_json::from_str(&text)
        .map_err(|e| SkillfileError::Network(format!("invalid GitLab tree JSON: {e}")))?;

    let prefix = format!("{}/", base_path.trim_end_matches('/'));

    let entries = items
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
            let download_url = gitlab_file_url(gl.host, gl.owner_repo, gl.ref_, path);
            Some(DirEntry {
                relative_path,
                download_url,
            })
        })
        .collect();

    Ok(entries)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p skillfile-sources -- gitlab 2>&1 | tail -10`
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/sources/src/resolver.rs
git commit -m "feat(resolver): add GitLab file fetching and tree listing"
```

---

## Task 8: Sync — wire GitLab through the sync pipeline

**Files:**
- Modify: `crates/sources/src/sync.rs`

Replace the `todo!()` arms from Task 1 with real GitLab sync logic. The sync flow mirrors GitHub: resolve SHA, fetch content, write to disk, write `.meta`, update lock.

- [ ] **Step 1: Write failing tests**

```rust
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

#[test]
fn sync_entry_gitlab_fetches_and_writes_file() {
    let sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let dir = tempfile::tempdir().unwrap();
    let entry = gitlab_skill_entry("my-skill", "skills/my-skill.md");

    let encoded = "group%2Fproject";
    let sha_url = format!(
        "https://gitlab.com/api/v4/projects/{encoded}/repository/commits/main"
    );
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
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p skillfile-sources -- sync_entry_gitlab 2>&1 | tail -20`
Expected: FAIL — `todo!()` panics.

- [ ] **Step 3: Implement GitLab sync**

**3a. Replace `content_exists()` todo arm:**

```rust
SourceFields::Gitlab { .. } => {
    if is_dir_entry(entry) {
        dir_has_content(vdir)
    } else {
        let cf = content_file(entry);
        !cf.is_empty() && vdir.join(&cf).exists()
    }
}
```

**3b. Create `sync_gitlab_core()` mirroring `sync_github_core()`:**

The function signature and flow are identical to `sync_github_core()` but:
- Extracts `SourceFields::Gitlab { owner_repo, path_in_repo, ref_ }` instead of `Github`
- Calls `resolve_gitlab_sha(client, owner_repo, ref_, &gitlab_host())` for SHA resolution
- Uses `GitlabFetch { client, owner_repo, ref_: &sha, host: &gitlab_host() }` for fetching
- Calls `fetch_gitlab_file()` / `list_gitlab_dir_recursive()` via a `fetch_store_gitlab()` function
- Writes `.meta` with `"source_type": "gitlab"` and `"host": &gitlab_host()`
- `raw_url` in lock entry uses `gitlab_file_url()` format

Create these functions mirroring the GitHub equivalents:
- `cache_gitlab_sha()` — same as `cache_github_sha()` but matches `SourceFields::Gitlab`
- `fetch_store_gitlab()` — same as `fetch_store_github()` but uses `GitlabFetch`
- `write_gitlab_meta()` — same as `write_github_meta()` but includes `"host"` field

**3c. Wire `sync_entry()` and `sync_entry_core()`:**

Replace the `todo!()` arms:

```rust
// In sync_entry():
SourceFields::Gitlab { .. } => {
    let params = SyncParams {
        repo_root: &ctx.repo_root,
        dry_run: ctx.dry_run,
        update: ctx.update,
        sha_cache: &ctx.sha_cache,
        locked: &ctx.locked,
    };
    if let Some((key, le)) = sync_gitlab_core(client, entry, &params)? {
        cache_gitlab_sha(ctx, entry, &le.sha);
        ctx.locked.insert(key, le);
    }
    Ok(())
}
```

**3d. Update parallel SHA resolution:**

In `collect_pairs_to_resolve()`, the existing code only matches `SourceFields::Github`. Add a match for `Gitlab`:

```rust
let SourceFields::Github { owner_repo, ref_, .. } = &entry.source else {
    // Also handle Gitlab
    if let SourceFields::Gitlab { owner_repo, ref_, .. } = &entry.source {
        // same logic with key_pair
    } else {
        continue;
    }
};
```

Better approach: extract a helper that returns `Option<(&str, &str)>` for any remote source:

```rust
fn remote_repo_ref(entry: &Entry) -> Option<(&str, &str)> {
    match &entry.source {
        SourceFields::Github { owner_repo, ref_, .. }
        | SourceFields::Gitlab { owner_repo, ref_, .. } => Some((owner_repo, ref_)),
        _ => None,
    }
}
```

Then use it in `collect_pairs_to_resolve()`, `entry_needs_resolution()`, and `needs_sha_resolution()`.

In `resolve_shas_parallel()`, tag each pair with its source type so the thread body dispatches correctly:

```rust
enum ForgeType { Github, Gitlab }

// Collect pairs as (owner_repo, ref_, forge_type)
// In the thread body:
match forge_type {
    ForgeType::Github => resolve_github_sha(client, &owner_repo, &ref_),
    ForgeType::Gitlab => resolve_gitlab_sha(client, &owner_repo, &ref_, &gitlab_host()),
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p skillfile-sources -- sync 2>&1 | tail -10`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/sources/src/sync.rs
git commit -m "feat(sync): wire GitLab entries through sync pipeline"
```

---

## Task 9: CLI token injection at startup

**Files:**
- Modify: `crates/cli/src/main.rs` (or wherever `set_config_token()` is called at startup)

The CLI needs to inject the GitLab config token at startup, just like it does for GitHub.

- [ ] **Step 1: Find the GitHub token injection call site**

Search for `set_config_token` in the CLI crate to find where to add the GitLab equivalent.

- [ ] **Step 2: Add GitLab token and host injection**

After the existing `set_config_token(read_config_token())` call, add:
```rust
skillfile_sources::http::set_gitlab_config_token(config::read_gitlab_config_token());
skillfile_sources::http::set_gitlab_config_host(config::read_gitlab_config_host());
```

The `set_gitlab_config_host()` function was defined in Task 4 alongside `GITLAB_CONFIG_HOST` OnceLock. The `read_gitlab_config_host()` function was defined in Task 5. This wiring connects them at startup.

- [ ] **Step 3: Run full test suite**

Run: `cargo test 2>&1 | tail -20`
Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cli/src/main.rs crates/sources/src/http.rs
git commit -m "feat(cli): inject GitLab token and host from config at startup"
```

---

## Task 10: Integration test — full CLI round-trip

**Files:**
- Modify: `tests/cli.rs`

Add an integration test that exercises the full `gitlab` flow through the CLI: create a Skillfile with a `gitlab` entry, run `skillfile sync --dry-run`, and verify it doesn't error.

- [ ] **Step 1: Write integration test**

```rust
#[test]
fn gitlab_entry_dry_run_sync() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  global\ngitlab  skill  my-group/my-project  skills/my-skill.md\n",
    )
    .unwrap();

    // dry-run should succeed without network access
    let output = Command::new(env!("CARGO_BIN_EXE_skillfile"))
        .args(["sync", "--dry-run"])
        .current_dir(dir.path())
        .env("GITLAB_TOKEN", "")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "sync --dry-run failed: {stderr}"
    );
    // Verify the GitLab entry was actually parsed (not silently dropped)
    assert!(
        stderr.contains("gitlab/skill/my-skill"),
        "output should mention the gitlab entry: {stderr}"
    );
}

#[test]
fn gitlab_entry_validate_passes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  global\ngitlab  skill  my-group/my-project  skills/my-skill.md\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_skillfile"))
        .args(["validate"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "validate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test --test cli -- gitlab_entry_dry_run_sync 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/cli.rs
git commit -m "test: add GitLab integration test for dry-run sync"
```

---

## Post-Implementation Checklist

After all tasks are complete:

- [ ] `cargo test` — all tests pass
- [ ] `cargo clippy` — no warnings
- [ ] `cargo fmt --check` — properly formatted
- [ ] Manual review: every `match` on `SourceFields` handles `Gitlab`
- [ ] Manual review: no `todo!()` markers remain
- [ ] Lock keys use `"gitlab/..."` prefix (automatic via `source_type()`)
- [ ] Tokens only leak to the configured GitLab host (verify via `is_gitlab_url` tests)
