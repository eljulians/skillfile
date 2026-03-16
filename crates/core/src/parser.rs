use std::collections::HashSet;
use std::path::Path;

use crate::error::SkillfileError;
use crate::models::{EntityType, Entry, InstallTarget, Manifest, Scope, SourceFields, DEFAULT_REF};

pub const MANIFEST_NAME: &str = "Skillfile";
const KNOWN_SOURCES: &[&str] = &["github", "local", "url"];

/// Result of parsing a Skillfile: the manifest plus any warnings.
#[derive(Debug)]
pub struct ParseResult {
    pub manifest: Manifest,
    pub warnings: Vec<String>,
}

/// Infer an entry name from a path or URL (filename stem).
///
/// ```
/// use skillfile_core::parser::infer_name;
/// assert_eq!(infer_name("skills/requesting-code-review"), "requesting-code-review");
/// assert_eq!(infer_name("agents/code-refactorer.md"), "code-refactorer");
/// assert_eq!(infer_name("https://example.com/browser-skill.md"), "browser-skill");
/// assert_eq!(infer_name("."), "content");
/// ```
#[must_use]
pub fn infer_name(path_or_url: &str) -> String {
    let p = std::path::Path::new(path_or_url);
    match p.file_stem().and_then(|s| s.to_str()) {
        Some(stem) if !stem.is_empty() && stem != "." => stem.to_string(),
        _ => "content".to_string(),
    }
}

/// Check if a name is filesystem-safe: alphanumeric, dot, hyphen, underscore.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
}

/// Push `current` into `parts` if non-empty, leaving `current` empty.
fn flush_token(current: &mut String, parts: &mut Vec<String>) {
    if !current.is_empty() {
        parts.push(std::mem::take(current));
    }
}

/// Split a manifest line respecting double-quoted fields.
///
/// Unquoted lines split identically to whitespace split.
/// Double-quoted fields preserve internal spaces.
fn split_line(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in line.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
            continue;
        }
        if ch.is_whitespace() && !in_quotes {
            flush_token(&mut current, &mut parts);
            continue;
        }
        current.push(ch);
    }
    flush_token(&mut current, &mut parts);
    parts
}

/// Remove inline comment (# ...) from field list.
fn strip_inline_comment(parts: Vec<String>) -> Vec<String> {
    if let Some(pos) = parts.iter().position(|p| p.starts_with('#')) {
        parts[..pos].to_vec()
    } else {
        parts
    }
}

/// Parse a github entry line. parts[0]=source_type, parts[1]=entity_type, etc.
fn parse_github_entry(
    parts: &[String],
    entity_type: EntityType,
    lineno: usize,
) -> (Option<Entry>, Vec<String>) {
    let mut warnings = Vec::new();

    // Detection: if parts[2] contains '/' → it's owner/repo (inferred name)
    if parts[2].contains('/') {
        if parts.len() < 4 {
            warnings.push(format!(
                "warning: line {lineno}: github entry needs at least: owner/repo path"
            ));
            return (None, warnings);
        }
        let owner_repo = &parts[2];
        let path_in_repo = &parts[3];
        let ref_ = if parts.len() > 4 {
            &parts[4]
        } else {
            DEFAULT_REF
        };
        let name = infer_name(path_in_repo);
        (
            Some(Entry {
                entity_type,
                name,
                source: SourceFields::Github {
                    owner_repo: owner_repo.clone(),
                    path_in_repo: path_in_repo.clone(),
                    ref_: ref_.to_string(),
                },
            }),
            warnings,
        )
    } else {
        if parts.len() < 5 {
            warnings.push(format!(
                "warning: line {lineno}: github entry needs at least: name owner/repo path"
            ));
            return (None, warnings);
        }
        let name = &parts[2];
        let owner_repo = &parts[3];
        let path_in_repo = &parts[4];
        let ref_ = if parts.len() > 5 {
            &parts[5]
        } else {
            DEFAULT_REF
        };
        (
            Some(Entry {
                entity_type,
                name: name.clone(),
                source: SourceFields::Github {
                    owner_repo: owner_repo.clone(),
                    path_in_repo: path_in_repo.clone(),
                    ref_: ref_.to_string(),
                },
            }),
            warnings,
        )
    }
}

/// Parse a local entry line.
fn parse_local_entry(
    parts: &[String],
    entity_type: EntityType,
    lineno: usize,
) -> (Option<Entry>, Vec<String>) {
    let mut warnings = Vec::new();

    // Detection: if parts[2] ends in ".md" or contains '/' → path (inferred name)
    if Path::new(&parts[2])
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        || parts[2].contains('/')
    {
        let local_path = &parts[2];
        let name = infer_name(local_path);
        (
            Some(Entry {
                entity_type,
                name,
                source: SourceFields::Local {
                    path: local_path.clone(),
                },
            }),
            warnings,
        )
    } else {
        if parts.len() < 4 {
            warnings.push(format!(
                "warning: line {lineno}: local entry needs: name path"
            ));
            return (None, warnings);
        }
        let name = &parts[2];
        let local_path = &parts[3];
        (
            Some(Entry {
                entity_type,
                name: name.clone(),
                source: SourceFields::Local {
                    path: local_path.clone(),
                },
            }),
            warnings,
        )
    }
}

/// Parse a url entry line.
fn parse_url_entry(
    parts: &[String],
    entity_type: EntityType,
    lineno: usize,
) -> (Option<Entry>, Vec<String>) {
    let mut warnings = Vec::new();

    // Detection: if parts[2] starts with "http" → URL (inferred name)
    if parts[2].starts_with("http") {
        let url = &parts[2];
        let name = infer_name(url);
        (
            Some(Entry {
                entity_type,
                name,
                source: SourceFields::Url { url: url.clone() },
            }),
            warnings,
        )
    } else {
        if parts.len() < 4 {
            warnings.push(format!("warning: line {lineno}: url entry needs: name url"));
            return (None, warnings);
        }
        let name = &parts[2];
        let url = &parts[3];
        (
            Some(Entry {
                entity_type,
                name: name.clone(),
                source: SourceFields::Url { url: url.clone() },
            }),
            warnings,
        )
    }
}

/// Accumulator for manifest parsing state passed to helper functions.
struct ParseAccumulator {
    entries: Vec<Entry>,
    install_targets: Vec<InstallTarget>,
    warnings: Vec<String>,
    seen_names: HashSet<String>,
}

/// Process an `install` line, pushing a target or a warning.
fn parse_install_line(parts: &[String], lineno: usize, acc: &mut ParseAccumulator) {
    if parts.len() < 3 {
        acc.warnings.push(format!(
            "warning: line {lineno}: install line needs: adapter scope"
        ));
        return;
    }
    let scope_str = &parts[2];
    if let Some(scope) = Scope::parse(scope_str) {
        acc.install_targets.push(InstallTarget {
            adapter: parts[1].clone(),
            scope,
        });
    } else {
        let valid: Vec<&str> = Scope::ALL
            .iter()
            .map(super::models::Scope::as_str)
            .collect();
        acc.warnings.push(format!(
            "warning: line {lineno}: invalid scope '{scope_str}', \
             must be one of: {}",
            valid.join(", ")
        ));
    }
}

/// Validate an entry name and insert it into the accumulator, warning on problems.
fn validate_and_push_entry(entry: Entry, lineno: usize, acc: &mut ParseAccumulator) {
    if !is_valid_name(&entry.name) {
        acc.warnings.push(format!(
            "warning: line {lineno}: invalid name '{}' \
             — names must match [a-zA-Z0-9._-], skipping",
            entry.name
        ));
    } else if acc.seen_names.contains(&entry.name) {
        acc.warnings.push(format!(
            "warning: line {lineno}: duplicate entry name '{}'",
            entry.name
        ));
        acc.entries.push(entry);
    } else {
        acc.seen_names.insert(entry.name.clone());
        acc.entries.push(entry);
    }
}

/// Dispatch a known-source line to the appropriate parser and return the result.
fn parse_source_entry(
    parts: &[String],
    lineno: usize,
    source_type: &str,
) -> (Option<Entry>, Vec<String>) {
    if parts.len() < 3 {
        return (
            None,
            vec![format!("warning: line {lineno}: too few fields, skipping")],
        );
    }
    let Some(entity_type) = EntityType::parse(&parts[1]) else {
        return (
            None,
            vec![format!(
                "warning: line {lineno}: unknown entity type '{}', skipping",
                parts[1]
            )],
        );
    };
    match source_type {
        "github" => parse_github_entry(parts, entity_type, lineno),
        "local" => parse_local_entry(parts, entity_type, lineno),
        "url" => parse_url_entry(parts, entity_type, lineno),
        _ => (None, vec![]),
    }
}

/// Parse and accumulate a known-source line: dispatch, collect warnings, push entry.
/// `parts[0]` must be a known source type (`github`, `local`, or `url`).
fn process_source_line(parts: &[String], lineno: usize, acc: &mut ParseAccumulator) {
    let source_type = parts[0].as_str();
    let (entry_opt, mut entry_warnings) = parse_source_entry(parts, lineno, source_type);
    acc.warnings.append(&mut entry_warnings);
    if let Some(entry) = entry_opt {
        validate_and_push_entry(entry, lineno, acc);
    }
}

/// Parse a Skillfile manifest from the given path.
pub fn parse_manifest(manifest_path: &Path) -> Result<ParseResult, SkillfileError> {
    let raw_bytes = std::fs::read(manifest_path)?;

    // Strip UTF-8 BOM if present
    let text = if raw_bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(&raw_bytes[3..]).into_owned()
    } else {
        String::from_utf8_lossy(&raw_bytes).into_owned()
    };

    let mut acc = ParseAccumulator {
        entries: Vec::new(),
        install_targets: Vec::new(),
        warnings: Vec::new(),
        seen_names: HashSet::new(),
    };

    for (lineno, raw) in text.lines().enumerate() {
        let lineno = lineno + 1; // 1-indexed
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let parts = strip_inline_comment(split_line(line));
        if parts.len() < 2 {
            acc.warnings
                .push(format!("warning: line {lineno}: too few fields, skipping"));
            continue;
        }

        match parts[0].as_str() {
            "install" => parse_install_line(&parts, lineno, &mut acc),
            _ if KNOWN_SOURCES.contains(&parts[0].as_str()) => {
                process_source_line(&parts, lineno, &mut acc);
            }
            st => {
                acc.warnings.push(format!(
                    "warning: line {lineno}: unknown source type '{st}', skipping"
                ));
            }
        }
    }

    Ok(ParseResult {
        manifest: Manifest {
            entries: acc.entries,
            install_targets: acc.install_targets,
        },
        warnings: acc.warnings,
    })
}

/// Try to parse a single manifest line as an entry. Returns `Some(entry)` on success.
#[must_use]
pub fn parse_manifest_line(line: &str) -> Option<Entry> {
    let parts = split_line(line);
    let parts = strip_inline_comment(parts);
    if parts.len() < 3 {
        return None;
    }
    let source_type = parts[0].as_str();
    if !KNOWN_SOURCES.contains(&source_type) || source_type == "install" {
        return None;
    }
    let entity_type = EntityType::parse(&parts[1])?;
    let (entry_opt, _) = match source_type {
        "github" => parse_github_entry(&parts, entity_type, 0),
        "local" => parse_local_entry(&parts, entity_type, 0),
        "url" => parse_url_entry(&parts, entity_type, 0),
        _ => return None,
    };
    entry_opt
}

/// Find an entry by name in a manifest.
pub fn find_entry_in<'a>(name: &str, manifest: &'a Manifest) -> Result<&'a Entry, SkillfileError> {
    manifest
        .entries
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| {
            SkillfileError::Manifest(format!("no entry named '{name}' in {MANIFEST_NAME}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn dedent_line(line: &str, indent: usize) -> &str {
        if line.len() >= indent {
            &line[indent..]
        } else {
            line.trim()
        }
    }

    fn write_manifest(dir: &Path, content: &str) -> std::path::PathBuf {
        let p = dir.join(MANIFEST_NAME);
        // Dedent: strip leading whitespace common to all non-empty lines
        let lines: Vec<&str> = content.lines().collect();
        let min_indent = lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        let dedented: String = lines
            .iter()
            .map(|l| dedent_line(l, min_indent))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&p, dedented.trim_start_matches('\n').to_string() + "\n").unwrap();
        p
    }

    // -------------------------------------------------------------------
    // Existing entry types (explicit name + ref)
    // -------------------------------------------------------------------

    #[test]
    fn github_entry_explicit_name_and_ref() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "github  agent  backend-dev  owner/repo  path/to/agent.md  main",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        let e = &r.manifest.entries[0];
        assert_eq!(e.source_type(), "github");
        assert_eq!(e.entity_type, EntityType::Agent);
        assert_eq!(e.name, "backend-dev");
        assert_eq!(e.owner_repo(), "owner/repo");
        assert_eq!(e.path_in_repo(), "path/to/agent.md");
        assert_eq!(e.ref_(), "main");
    }

    #[test]
    fn local_entry_explicit_name() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "local  skill  git-commit  skills/git/commit.md");
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        let e = &r.manifest.entries[0];
        assert_eq!(e.source_type(), "local");
        assert_eq!(e.entity_type, EntityType::Skill);
        assert_eq!(e.name, "git-commit");
        assert_eq!(e.local_path(), "skills/git/commit.md");
    }

    #[test]
    fn url_entry_explicit_name() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "url  skill  my-skill  https://example.com/skill.md",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        let e = &r.manifest.entries[0];
        assert_eq!(e.source_type(), "url");
        assert_eq!(e.name, "my-skill");
        assert_eq!(e.url(), "https://example.com/skill.md");
    }

    // -------------------------------------------------------------------
    // Optional name inference
    // -------------------------------------------------------------------

    #[test]
    fn github_entry_inferred_name() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "github  agent  owner/repo  path/to/agent.md  main",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        let e = &r.manifest.entries[0];
        assert_eq!(e.name, "agent");
        assert_eq!(e.owner_repo(), "owner/repo");
        assert_eq!(e.path_in_repo(), "path/to/agent.md");
        assert_eq!(e.ref_(), "main");
    }

    #[test]
    fn local_entry_inferred_name_from_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "local  skill  skills/git/commit.md");
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        let e = &r.manifest.entries[0];
        assert_eq!(e.name, "commit");
        assert_eq!(e.local_path(), "skills/git/commit.md");
    }

    #[test]
    fn local_entry_inferred_name_from_md_extension() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "local  skill  commit.md");
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        assert_eq!(r.manifest.entries[0].name, "commit");
    }

    #[test]
    fn url_entry_inferred_name() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "url  skill  https://example.com/my-skill.md");
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        let e = &r.manifest.entries[0];
        assert_eq!(e.name, "my-skill");
        assert_eq!(e.url(), "https://example.com/my-skill.md");
    }

    // -------------------------------------------------------------------
    // Optional ref (defaults to main)
    // -------------------------------------------------------------------

    #[test]
    fn github_entry_inferred_name_default_ref() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "github  agent  owner/repo  path/to/agent.md");
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries[0].ref_(), "main");
    }

    #[test]
    fn github_entry_explicit_name_default_ref() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "github  agent  my-agent  owner/repo  path/to/agent.md",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries[0].ref_(), "main");
    }

    // -------------------------------------------------------------------
    // Install targets
    // -------------------------------------------------------------------

    #[test]
    fn install_target_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "install  claude-code  global");
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.install_targets.len(), 1);
        let t = &r.manifest.install_targets[0];
        assert_eq!(t.adapter, "claude-code");
        assert_eq!(t.scope, Scope::Global);
    }

    #[test]
    fn multiple_install_targets() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "install  claude-code  global\ninstall  claude-code  local",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.install_targets.len(), 2);
        assert_eq!(r.manifest.install_targets[0].scope, Scope::Global);
        assert_eq!(r.manifest.install_targets[1].scope, Scope::Local);
    }

    #[test]
    fn install_targets_not_in_entries() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "install  claude-code  global\ngithub  agent  owner/repo  path/to/agent.md",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        assert_eq!(r.manifest.install_targets.len(), 1);
    }

    // -------------------------------------------------------------------
    // Comments, blanks, errors
    // -------------------------------------------------------------------

    #[test]
    fn comments_and_blanks_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "# this is a comment\n\n# another comment\nlocal  skill  foo  skills/foo.md",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
    }

    #[test]
    fn malformed_too_few_fields() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "github  agent");
        let r = parse_manifest(&p).unwrap();
        assert!(r.manifest.entries.is_empty());
        assert!(r.warnings.iter().any(|w| w.contains("warning")));
    }

    #[test]
    fn unknown_source_type_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "svn  skill  foo  some/path");
        let r = parse_manifest(&p).unwrap();
        assert!(r.manifest.entries.is_empty());
        assert!(r.warnings.iter().any(|w| w.contains("warning")));
        assert!(r.warnings.iter().any(|w| w.contains("svn")));
    }

    // -------------------------------------------------------------------
    // Inline comments
    // -------------------------------------------------------------------

    #[test]
    fn inline_comment_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "github  agent  owner/repo  agents/foo.md  # my note",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        let e = &r.manifest.entries[0];
        assert_eq!(e.ref_(), "main"); // not "#"
        assert_eq!(e.name, "foo");
    }

    #[test]
    fn inline_comment_on_install_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "install  claude-code  global  # primary target");
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.install_targets.len(), 1);
        assert_eq!(r.manifest.install_targets[0].scope, Scope::Global);
    }

    #[test]
    fn inline_comment_after_ref() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "github  agent  my-agent  owner/repo  agents/foo.md  v1.0  # pinned version",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries[0].ref_(), "v1.0");
    }

    // -------------------------------------------------------------------
    // Quoted fields
    // -------------------------------------------------------------------

    #[test]
    fn quoted_path_with_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(MANIFEST_NAME);
        fs::write(&p, "local  skill  my-skill  \"skills/my dir/foo.md\"\n").unwrap();
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        assert_eq!(r.manifest.entries[0].local_path(), "skills/my dir/foo.md");
    }

    #[test]
    fn quoted_github_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(MANIFEST_NAME);
        fs::write(
            &p,
            "github  skill  owner/repo  \"path with spaces/skill.md\"\n",
        )
        .unwrap();
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        assert_eq!(
            r.manifest.entries[0].path_in_repo(),
            "path with spaces/skill.md"
        );
    }

    #[test]
    fn mixed_quoted_and_unquoted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(MANIFEST_NAME);
        fs::write(
            &p,
            "github  agent  my-agent  owner/repo  \"agents/path with spaces/foo.md\"\n",
        )
        .unwrap();
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        assert_eq!(r.manifest.entries[0].name, "my-agent");
        assert_eq!(
            r.manifest.entries[0].path_in_repo(),
            "agents/path with spaces/foo.md"
        );
    }

    #[test]
    fn unquoted_fields_parse_identically() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "github  agent  backend-dev  owner/repo  path/to/agent.md  main",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries[0].name, "backend-dev");
        assert_eq!(r.manifest.entries[0].ref_(), "main");
    }

    // -------------------------------------------------------------------
    // Name validation
    // -------------------------------------------------------------------

    #[test]
    fn valid_entry_name_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "local  skill  my-skill_v2.0  skills/foo.md");
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        assert_eq!(r.manifest.entries[0].name, "my-skill_v2.0");
    }

    #[test]
    fn invalid_entry_name_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(MANIFEST_NAME);
        fs::write(&p, "local  skill  \"my skill!\"  skills/foo.md\n").unwrap();
        let r = parse_manifest(&p).unwrap();
        assert!(r.manifest.entries.is_empty());
        assert!(r
            .warnings
            .iter()
            .any(|w| w.to_lowercase().contains("invalid name")
                || w.to_lowercase().contains("warning")));
    }

    #[test]
    fn inferred_name_validated() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "local  skill  skills/foo.md");
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        assert_eq!(r.manifest.entries[0].name, "foo");
    }

    // -------------------------------------------------------------------
    // Scope validation
    // -------------------------------------------------------------------

    #[test]
    fn valid_scope_accepted() {
        for (scope_str, expected) in &[("global", Scope::Global), ("local", Scope::Local)] {
            let dir = tempfile::tempdir().unwrap();
            let p = write_manifest(dir.path(), &format!("install  claude-code  {scope_str}"));
            let r = parse_manifest(&p).unwrap();
            assert_eq!(r.manifest.install_targets.len(), 1);
            assert_eq!(r.manifest.install_targets[0].scope, *expected);
        }
    }

    #[test]
    fn invalid_scope_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "install  claude-code  worldwide");
        let r = parse_manifest(&p).unwrap();
        assert!(r.manifest.install_targets.is_empty());
        assert!(r
            .warnings
            .iter()
            .any(|w| w.to_lowercase().contains("scope") || w.to_lowercase().contains("warning")));
    }

    // -------------------------------------------------------------------
    // Duplicate entry name warning
    // -------------------------------------------------------------------

    #[test]
    fn duplicate_entry_name_warns() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "local  skill  foo  skills/foo.md\nlocal  agent  foo  agents/foo.md",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 2); // both included
        assert!(r
            .warnings
            .iter()
            .any(|w| w.to_lowercase().contains("duplicate")));
    }

    // -------------------------------------------------------------------
    // UTF-8 BOM handling
    // -------------------------------------------------------------------

    #[test]
    fn utf8_bom_handled() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(MANIFEST_NAME);
        let mut content = vec![0xEF, 0xBB, 0xBF]; // BOM
        content.extend_from_slice(b"install  claude-code  global\n");
        fs::write(&p, content).unwrap();
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.install_targets.len(), 1);
        assert_eq!(
            r.manifest.install_targets[0],
            InstallTarget {
                adapter: "claude-code".into(),
                scope: Scope::Global,
            }
        );
    }

    // -------------------------------------------------------------------
    // Unknown entity type warning
    // -------------------------------------------------------------------

    #[test]
    fn unknown_entity_type_skipped_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "local  hook  foo  hooks/foo.md");
        let r = parse_manifest(&p).unwrap();
        assert!(r.manifest.entries.is_empty());
        assert!(r.warnings.iter().any(|w| w.contains("unknown entity type")));
    }

    // -------------------------------------------------------------------
    // find_entry_in
    // -------------------------------------------------------------------

    #[test]
    fn find_entry_in_found() {
        let e = Entry {
            entity_type: EntityType::Skill,
            name: "foo".into(),
            source: SourceFields::Local {
                path: "foo.md".into(),
            },
        };
        let m = Manifest {
            entries: vec![e.clone()],
            install_targets: vec![],
        };
        assert_eq!(find_entry_in("foo", &m).unwrap(), &e);
    }

    #[test]
    fn find_entry_in_not_found() {
        let m = Manifest::default();
        assert!(find_entry_in("missing", &m).is_err());
    }

    // -------------------------------------------------------------------
    // infer_name
    // -------------------------------------------------------------------

    #[test]
    fn infer_name_from_md_path() {
        assert_eq!(infer_name("path/to/agent.md"), "agent");
    }

    #[test]
    fn infer_name_from_dot() {
        assert_eq!(infer_name("."), "content");
    }

    #[test]
    fn infer_name_from_url() {
        assert_eq!(infer_name("https://example.com/my-skill.md"), "my-skill");
    }

    // -------------------------------------------------------------------
    // split_line
    // -------------------------------------------------------------------

    #[test]
    fn split_line_simple() {
        assert_eq!(
            split_line("github  agent  owner/repo  agent.md"),
            vec!["github", "agent", "owner/repo", "agent.md"]
        );
    }

    #[test]
    fn split_line_quoted() {
        assert_eq!(
            split_line("local  skill  \"my dir/foo.md\""),
            vec!["local", "skill", "my dir/foo.md"]
        );
    }

    #[test]
    fn split_line_tabs() {
        assert_eq!(
            split_line("local\tskill\tfoo.md"),
            vec!["local", "skill", "foo.md"]
        );
    }
}
