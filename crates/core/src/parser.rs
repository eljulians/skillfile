use std::collections::HashSet;
use std::path::Path;

use crate::error::SkillfileError;
use crate::models::{EntityType, Entry, InstallTarget, Manifest, Scope, SourceFields, DEFAULT_REF};

pub const MANIFEST_NAME: &str = "Skillfile";
const KNOWN_SOURCES: &[&str] = &["github", "gitlab", "local", "url"];

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

/// Split a manifest line using shell-style field quoting.
///
/// Unquoted lines split identically to whitespace split.
/// Quoted fields preserve internal spaces and comment markers.
fn split_line(line: &str) -> Option<Vec<String>> {
    shlex::split(line)
}

fn strip_inline_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    let mut previous_was_whitespace = false;

    for (index, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            previous_was_whitespace = false;
            continue;
        }
        match quote {
            Some(active) if ch == active => quote = None,
            Some('"') | None if ch == '\\' => escaped = true,
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch == '#' && previous_was_whitespace => {
                return line[..index].trim_end();
            }
            Some(_) | None => {}
        }
        previous_was_whitespace = quote.is_none() && ch.is_whitespace();
    }

    line
}

/// Quote a manifest field so shell-style parsing recovers the original value.
#[must_use]
pub fn quote_field(field: &str) -> String {
    match shlex::try_quote(field) {
        Ok(quoted) => quoted.into_owned(),
        Err(_) => field.to_string(),
    }
}

/// Parse `owner/repo[@ref]` into `(owner_repo, ref_)`.
///
/// Supports:
/// - `owner/repo` → `("owner/repo", None)`
/// - `owner/repo@v4` → `("owner/repo", Some("v4"))`
/// - `owner/repo@main` → `("owner/repo", Some("main"))`
#[must_use]
pub fn parse_owner_repo_ref(input: &str) -> (String, Option<String>) {
    match input.split_once('@') {
        Some((repo, ref_)) if !repo.is_empty() && !ref_.is_empty() => {
            (repo.to_string(), Some(ref_.to_string()))
        }
        _ => (input.to_string(), None),
    }
}

/// Resolve the effective ref from the `@`-syntax parsed ref and an explicit positional ref.
///
/// Priority: `owner/repo@ref` > explicit positional ref > `DEFAULT_REF`.
#[must_use]
pub fn resolve_explicit_owner_repo_ref(
    at_ref: Option<String>,
    positional_ref: Option<&str>,
) -> Option<String> {
    at_ref.filter(|ref_| !ref_.trim().is_empty()).or_else(|| {
        positional_ref
            .filter(|ref_| !ref_.trim().is_empty())
            .map(String::from)
    })
}

/// Resolve the effective ref from the `@`-syntax parsed ref and an explicit positional ref.
///
/// Priority: `owner/repo@ref` > explicit positional ref > `DEFAULT_REF`.
#[must_use]
pub fn resolve_owner_repo_ref(at_ref: Option<String>, positional_ref: Option<&str>) -> String {
    resolve_explicit_owner_repo_ref(at_ref, positional_ref)
        .unwrap_or_else(|| DEFAULT_REF.to_string())
}

fn parse_github_owner_repo(
    raw_owner_repo: &str,
    lineno: usize,
    warnings: &mut Vec<String>,
) -> Option<(String, Option<String>)> {
    let (owner_repo, at_ref) = parse_owner_repo_ref(raw_owner_repo);
    if owner_repo.contains('/') {
        return Some((owner_repo, at_ref));
    }
    warnings.push(format!(
        "warning: line {lineno}: invalid owner/repo '{raw_owner_repo}' \
         — expected 'owner/repo' or 'owner/repo@ref' format"
    ));
    None
}

fn validate_repo_path(path: &str, source_type: &str, lineno: usize) -> Result<(), String> {
    let path = path.trim();
    if path == "." || Path::new(path).file_name().is_some() {
        return Ok(());
    }
    Err(format!(
        "warning: line {lineno}: {source_type} entry has invalid repository path '{path}' \
         — expected a non-root path or '.'"
    ))
}

/// Parse a github entry line. parts[0]=source_type, parts[1]=entity_type, etc.
fn parse_github_entry(
    parts: &[String],
    entity_type: EntityType,
    lineno: usize,
) -> (Option<Entry>, Vec<String>) {
    let mut warnings = Vec::new();

    // Detection: if parts[2] contains '/' → it's owner/repo (inferred name)
    let (name, owner_repo, path_in_repo, ref_) = if parts[2].contains('/') {
        if parts.len() < 4 {
            warnings.push(format!(
                "warning: line {lineno}: github entry needs at least: owner/repo path"
            ));
            return (None, warnings);
        }
        let Some((parsed_repo, parsed_ref)) =
            parse_github_owner_repo(&parts[2], lineno, &mut warnings)
        else {
            return (None, warnings);
        };
        let ref_ = resolve_owner_repo_ref(parsed_ref, parts.get(4).map(String::as_str));
        (infer_name(&parts[3]), parsed_repo, &parts[3], ref_)
    } else {
        if parts.len() < 5 {
            warnings.push(format!(
                "warning: line {lineno}: github entry needs at least: name owner/repo path"
            ));
            return (None, warnings);
        }
        let Some((parsed_repo, parsed_ref)) =
            parse_github_owner_repo(&parts[3], lineno, &mut warnings)
        else {
            return (None, warnings);
        };
        let ref_ = resolve_owner_repo_ref(parsed_ref, parts.get(5).map(String::as_str));
        (parts[2].clone(), parsed_repo, &parts[4], ref_)
    };

    if let Err(warning) = validate_repo_path(path_in_repo, "github", lineno) {
        warnings.push(warning);
        return (None, warnings);
    }

    let entry = Entry {
        entity_type,
        name,
        source: SourceFields::Github {
            owner_repo,
            path_in_repo: path_in_repo.clone(),
            ref_,
        },
    };
    (Some(entry), warnings)
}

/// Parse a gitlab entry line. Identical logic to github but creates `SourceFields::Gitlab`.
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
        let ref_ = resolve_owner_repo_ref(None, parts.get(4).map(String::as_str));
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
        let ref_ = resolve_owner_repo_ref(None, parts.get(5).map(String::as_str));
        (parts[2].clone(), &parts[3], &parts[4], ref_)
    };

    if let Err(warning) = validate_repo_path(path_in_repo, "gitlab", lineno) {
        warnings.push(warning);
        return (None, warnings);
    }

    let entry = Entry {
        entity_type,
        name,
        source: SourceFields::Gitlab {
            owner_repo: owner_repo.clone(),
            path_in_repo: path_in_repo.clone(),
            ref_,
        },
    };
    (Some(entry), warnings)
}

fn parse_local_entry(
    parts: &[String],
    entity_type: EntityType,
    lineno: usize,
) -> (Option<Entry>, Vec<String>) {
    // Detection: if parts[2] ends in ".md" or contains '/' → path (inferred name).
    // With 4+ parts, parts[2] is always the explicit name and parts[3] the path.
    // With exactly 3 parts, parts[2] is always the path (even bare directory names
    // like "commit" that don't contain '/' or end in '.md').
    let looks_like_path = Path::new(&parts[2])
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        || parts[2].contains('/');
    let (name, local_path) = if looks_like_path || parts.len() < 4 {
        (infer_name(&parts[2]), &parts[2])
    } else {
        (parts[2].clone(), &parts[3])
    };
    if local_path.trim().is_empty() {
        return (
            None,
            vec![format!(
                "warning: line {lineno}: local entry path must not be empty"
            )],
        );
    }
    (
        Some(Entry {
            entity_type,
            name,
            source: SourceFields::Local {
                path: local_path.clone(),
            },
        }),
        Vec::new(),
    )
}

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
        if url.trim().is_empty() {
            warnings.push(format!(
                "warning: line {lineno}: url entry URL must not be empty"
            ));
            return (None, warnings);
        }
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

struct ParseAccumulator {
    entries: Vec<Entry>,
    install_targets: Vec<InstallTarget>,
    warnings: Vec<String>,
    seen_names: HashSet<String>,
}

fn parse_install_line(parts: &[String], lineno: usize, acc: &mut ParseAccumulator) {
    if parts.len() < 3 {
        acc.warnings.push(format!(
            "warning: line {lineno}: install line needs: adapter scope"
        ));
        return;
    }
    let adapter = &parts[1];
    if adapter.trim().is_empty() {
        acc.warnings.push(format!(
            "warning: line {lineno}: install adapter must not be empty"
        ));
        return;
    }
    let scope_str = &parts[2];
    if let Some(scope) = Scope::parse(scope_str) {
        acc.install_targets
            .push(InstallTarget::platform(adapter.clone(), scope));
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

fn parse_install_path_line(parts: &[String], lineno: usize, acc: &mut ParseAccumulator) {
    if parts.len() != 4 {
        acc.warnings.push(format!(
            "warning: line {lineno}: install-path line needs exactly: tool-name entity-type path"
        ));
        return;
    }

    let tool_name = &parts[1];
    if tool_name.trim().is_empty() {
        acc.warnings.push(format!(
            "warning: line {lineno}: install-path tool-name must not be empty"
        ));
        return;
    }

    let entity_type_str = &parts[2];
    let Some(entity_type) = EntityType::parse(entity_type_str) else {
        let valid: Vec<&str> = EntityType::ALL
            .iter()
            .map(super::models::EntityType::as_str)
            .collect();
        acc.warnings.push(format!(
            "warning: line {lineno}: invalid entity type '{entity_type_str}', \
             must be one of: {}",
            valid.join(", ")
        ));
        return;
    };

    let path = &parts[3];
    if path.trim().is_empty() {
        acc.warnings.push(format!(
            "warning: line {lineno}: install-path path must not be empty"
        ));
        return;
    }

    acc.install_targets.push(InstallTarget::path(
        tool_name.clone(),
        entity_type,
        path.clone(),
    ));
}

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
        "gitlab" => parse_gitlab_entry(parts, entity_type, lineno),
        "local" => parse_local_entry(parts, entity_type, lineno),
        "url" => parse_url_entry(parts, entity_type, lineno),
        _ => (None, vec![]),
    }
}

fn process_source_line(parts: &[String], lineno: usize, acc: &mut ParseAccumulator) {
    let source_type = parts[0].as_str();
    let (entry_opt, mut entry_warnings) = parse_source_entry(parts, lineno, source_type);
    acc.warnings.append(&mut entry_warnings);
    if let Some(entry) = entry_opt {
        validate_and_push_entry(entry, lineno, acc);
    }
}

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

        let Some(parts) = split_line(strip_inline_comment(line)) else {
            acc.warnings
                .push(format!("warning: line {lineno}: invalid quoting, skipping"));
            continue;
        };
        if parts.len() < 2 {
            acc.warnings
                .push(format!("warning: line {lineno}: too few fields, skipping"));
            continue;
        }

        match parts[0].as_str() {
            "install" => parse_install_line(&parts, lineno, &mut acc),
            "install-path" => parse_install_path_line(&parts, lineno, &mut acc),
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

#[must_use]
pub fn parse_manifest_line(line: &str) -> Option<Entry> {
    let parts = split_line(strip_inline_comment(line))?;
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
        "gitlab" => parse_gitlab_entry(&parts, entity_type, 0),
        "local" => parse_local_entry(&parts, entity_type, 0),
        "url" => parse_url_entry(&parts, entity_type, 0),
        _ => return None,
    };
    entry_opt
}

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

    fn assert_repo_path_rejected(source_type: &str, path: &str) {
        for line in [
            format!("{source_type}  skill  owner/repo  {path}"),
            format!("{source_type}  skill  named  owner/repo  {path}"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let p = write_manifest(dir.path(), &line);
            let r = parse_manifest(&p).unwrap();
            assert!(r.manifest.entries.is_empty(), "accepted: {line}");
            assert!(
                r.warnings.iter().any(|warning| warning
                    .contains(&format!("{source_type} entry has invalid repository path"))),
                "missing warning for: {line}"
            );
        }
    }

    fn assert_dot_repo_path_preserved(source_type: &str) {
        for line in [
            format!("{source_type}  skill  owner/repo  ."),
            format!("{source_type}  skill  named  owner/repo  ."),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let p = write_manifest(dir.path(), &line);
            let r = parse_manifest(&p).unwrap();
            assert_eq!(r.manifest.entries.len(), 1, "rejected: {line}");
            assert!(matches!(
                &r.manifest.entries[0].source,
                SourceFields::Github { path_in_repo, .. }
                    | SourceFields::Gitlab { path_in_repo, .. }
                    if path_in_repo == "."
            ));
        }
    }

    fn assert_empty_repo_ref_defaults_to_main(source_type: &str) {
        for line in [
            format!("{source_type}  skill  owner/repo  path  \"\""),
            format!("{source_type}  skill  named  owner/repo  path  \"\""),
            format!("{source_type}  skill  owner/repo  path  \"   \""),
            format!("{source_type}  skill  named  owner/repo  path  \"   \""),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let p = write_manifest(dir.path(), &line);
            let r = parse_manifest(&p).unwrap();
            assert_eq!(r.manifest.entries.len(), 1, "rejected: {line}");
            assert!(matches!(
                &r.manifest.entries[0].source,
                SourceFields::Github { ref_, .. } | SourceFields::Gitlab { ref_, .. }
                    if ref_ == DEFAULT_REF
            ));
        }
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
    fn local_entry_bare_dir_name() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "local  skill  bash-craftsman");
        let r = parse_manifest(&p).unwrap();
        assert!(
            r.warnings.is_empty(),
            "unexpected warnings: {:?}",
            r.warnings
        );
        assert_eq!(r.manifest.entries.len(), 1);
        let e = &r.manifest.entries[0];
        assert_eq!(e.source_type(), "local");
        assert_eq!(e.entity_type, EntityType::Skill);
        assert_eq!(e.name, "bash-craftsman");
        assert_eq!(e.local_path(), "bash-craftsman");
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

    #[test]
    fn repository_sources_reject_empty_and_root_only_paths() {
        for (source_type, path) in [
            ("github", "\"\""),
            ("github", "\"   \""),
            ("github", "/"),
            ("github", "//"),
            ("github", "/./"),
            ("gitlab", "\"\""),
            ("gitlab", "\"   \""),
            ("gitlab", "/"),
            ("gitlab", "//"),
            ("gitlab", "/./"),
        ] {
            assert_repo_path_rejected(source_type, path);
        }
    }

    #[test]
    fn repository_sources_preserve_dot_root_path() {
        for source_type in ["github", "gitlab"] {
            assert_dot_repo_path_preserved(source_type);
        }
    }

    #[test]
    fn repository_sources_default_empty_refs() {
        for source_type in ["github", "gitlab"] {
            assert_empty_repo_ref_defaults_to_main(source_type);
        }
    }

    #[test]
    fn empty_source_and_install_fields_are_rejected() {
        for (line, warning) in [
            ("local  skill  \"\"", "local entry path must not be empty"),
            (
                "local  skill  named  \"   \"",
                "local entry path must not be empty",
            ),
            ("url  skill  named  \"\"", "url entry URL must not be empty"),
            (
                "install  \"   \"  global",
                "install adapter must not be empty",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let p = write_manifest(dir.path(), line);
            let result = parse_manifest(&p).unwrap();

            assert!(result.manifest.entries.is_empty(), "accepted: {line}");
            assert!(
                result.manifest.install_targets.is_empty(),
                "accepted: {line}"
            );
            assert!(
                result
                    .warnings
                    .iter()
                    .any(|message| message.contains(warning)),
                "missing warning for: {line}"
            );
        }
    }

    // -------------------------------------------------------------------
    // @-syntax ref (owner/repo@ref)
    // -------------------------------------------------------------------

    #[test]
    fn github_entry_at_ref_inferred_name() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "github  skill  nuxt/ui@v4  path/to/SKILL.md");
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        let e = &r.manifest.entries[0];
        assert_eq!(e.name, "SKILL");
        assert_eq!(e.owner_repo(), "nuxt/ui");
        assert_eq!(e.ref_(), "v4");
    }

    #[test]
    fn github_entry_at_ref_explicit_name() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "github  skill  my-skill  nuxt/ui@v4  path/to/SKILL.md",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        let e = &r.manifest.entries[0];
        assert_eq!(e.name, "my-skill");
        assert_eq!(e.owner_repo(), "nuxt/ui");
        assert_eq!(e.ref_(), "v4");
    }

    #[test]
    fn github_entry_at_ref_with_main() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "github  skill  owner/repo@main  path/to/SKILL.md",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries[0].owner_repo(), "owner/repo");
        assert_eq!(r.manifest.entries[0].ref_(), "main");
    }

    #[test]
    fn github_entry_at_ref_with_sha() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "github  skill  owner/repo@abc123def456  path/to/SKILL.md",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries[0].owner_repo(), "owner/repo");
        assert_eq!(r.manifest.entries[0].ref_(), "abc123def456");
    }

    #[test]
    fn github_entry_at_ref_takes_priority_over_positional() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "github  skill  nuxt/ui@v4  path/to/SKILL.md  v3",
        );
        let r = parse_manifest(&p).unwrap();
        let e = &r.manifest.entries[0];
        assert_eq!(e.owner_repo(), "nuxt/ui");
        assert_eq!(e.ref_(), "v4");
    }

    #[test]
    fn github_entry_at_ref_requires_owner_repo_before_ref_separator() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "github  skill  us@tal/repo  path/to/SKILL.md");
        let r = parse_manifest(&p).unwrap();
        assert!(r.manifest.entries.is_empty());
        assert!(r
            .warnings
            .iter()
            .any(|warning| warning.contains("invalid owner/repo 'us@tal/repo'")));
    }

    #[test]
    fn github_entry_at_ref_requires_owner_repo_before_ref_separator_with_name() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "github  skill  my-skill  us@tal/repo  path/to/SKILL.md",
        );
        let r = parse_manifest(&p).unwrap();
        assert!(r.manifest.entries.is_empty());
        assert!(r
            .warnings
            .iter()
            .any(|warning| warning.contains("invalid owner/repo 'us@tal/repo'")));
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
        assert_eq!(t, &InstallTarget::platform("claude-code", Scope::Global));
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
        assert_eq!(
            r.manifest.install_targets[0],
            InstallTarget::platform("claude-code", Scope::Global)
        );
        assert_eq!(
            r.manifest.install_targets[1],
            InstallTarget::platform("claude-code", Scope::Local)
        );
    }

    #[test]
    fn install_path_target_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "install-path  openclaw  skill  ~/.openclaw/skills",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.install_targets.len(), 1);
        assert_eq!(
            r.manifest.install_targets[0],
            InstallTarget::path("openclaw", EntityType::Skill, "~/.openclaw/skills")
        );
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
        assert_eq!(
            r.manifest.install_targets[0],
            InstallTarget::platform("claude-code", Scope::Global)
        );
    }

    #[test]
    fn inline_comment_on_install_path_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(
            dir.path(),
            "install-path  misc-target  agent  ./agents  # custom target",
        );
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.install_targets.len(), 1);
        assert_eq!(
            r.manifest.install_targets[0],
            InstallTarget::path("misc-target", EntityType::Agent, "./agents")
        );
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
    fn quoted_hash_path_is_not_an_inline_comment() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(MANIFEST_NAME);
        fs::write(&p, "local  skill  hash-skill  \"#skills/hash.md\"\n").unwrap();
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        assert_eq!(r.manifest.entries[0].name, "hash-skill");
        assert_eq!(r.manifest.entries[0].local_path(), "#skills/hash.md");
    }

    #[test]
    fn single_quoted_hash_path_is_not_an_inline_comment() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(MANIFEST_NAME);
        fs::write(&p, "local  skill  hash-skill  'my #skills/hash.md'\n").unwrap();
        let r = parse_manifest(&p).unwrap();
        assert_eq!(r.manifest.entries.len(), 1);
        assert_eq!(r.manifest.entries[0].local_path(), "my #skills/hash.md");
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
            assert_eq!(
                r.manifest.install_targets[0],
                InstallTarget::platform("claude-code", *expected)
            );
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

    #[test]
    fn invalid_install_path_entity_type_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "install-path  misc-target  bot  ./target");
        let r = parse_manifest(&p).unwrap();
        assert!(r.manifest.install_targets.is_empty());
        assert!(r.warnings.iter().any(|w| w.contains("invalid entity type")));
    }

    #[test]
    fn empty_install_path_tool_name_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "install-path  \"\"  skill  ./target");
        let r = parse_manifest(&p).unwrap();
        assert!(r.manifest.install_targets.is_empty());
        assert!(r.warnings.iter().any(|w| w.contains("tool-name")));
    }

    #[test]
    fn empty_install_path_path_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "install-path  misc-target  skill  \"\"");
        let r = parse_manifest(&p).unwrap();
        assert!(r.manifest.install_targets.is_empty());
        assert!(r.warnings.iter().any(|w| w.contains("path")));
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
            InstallTarget::platform("claude-code", Scope::Global)
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

    #[test]
    fn github_invalid_owner_repo_skipped_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        // Explicit-name form: name is "my-skill", owner_repo is "noslash"
        let p = write_manifest(dir.path(), "github  skill  my-skill  noslash  path.md");
        let r = parse_manifest(&p).unwrap();
        assert!(
            r.manifest.entries.is_empty(),
            "entry with invalid owner/repo should be skipped"
        );
        assert!(r.warnings.iter().any(|w| w.contains("owner/repo")));
    }

    #[test]
    fn github_invalid_owner_repo_after_lossy_utf8_decode_skipped_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(MANIFEST_NAME);
        fs::write(
            &p,
            [
                240, 174, 174, 174, 240, 174, 174, 170, 240, 105, 116, 104, 117, 97, 10, 103, 105,
                116, 104, 117, 98, 32, 97, 103, 101, 110, 116, 32, 117, 115, 64, 116, 97, 108, 170,
                170, 115, 47, 108, 1, 57, 32, 108, 32, 59, 239, 191, 10,
            ],
        )
        .unwrap();
        let r = parse_manifest(&p).unwrap();
        assert!(r.manifest.entries.is_empty());
        assert!(r.warnings.iter().any(|w| w.contains("invalid owner/repo")));
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
            split_line("github  agent  owner/repo  agent.md").unwrap(),
            vec!["github", "agent", "owner/repo", "agent.md"]
        );
    }

    #[test]
    fn split_line_quoted() {
        assert_eq!(
            split_line("local  skill  \"my dir/foo.md\"").unwrap(),
            vec!["local", "skill", "my dir/foo.md"]
        );
    }

    #[test]
    fn split_line_single_quoted() {
        assert_eq!(
            split_line("local  skill  'my dir/foo.md'").unwrap(),
            vec!["local", "skill", "my dir/foo.md"]
        );
    }

    #[test]
    fn quote_field_round_trips_shell_sensitive_values() {
        for field in [
            r"C:\skills\win.md",
            "skills/it's.md",
            r#"skills/"quoted".md"#,
            "skills/my dir/#skill.md",
        ] {
            assert_eq!(
                split_line(&quote_field(field)),
                Some(vec![field.to_string()])
            );
        }
    }

    #[test]
    fn invalid_quoting_warns_and_skips_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_manifest(dir.path(), "local  skill  \"unterminated.md");
        let r = parse_manifest(&p).unwrap();
        assert!(r.manifest.entries.is_empty());
        assert!(r.warnings.iter().any(|w| w.contains("invalid quoting")));
    }

    #[test]
    fn split_line_tabs() {
        assert_eq!(
            split_line("local\tskill\tfoo.md").unwrap(),
            vec!["local", "skill", "foo.md"]
        );
    }

    // -------------------------------------------------------------------
    // GitLab entries
    // -------------------------------------------------------------------

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
}
