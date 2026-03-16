use std::path::Path;

use skillfile_core::error::SkillfileError;
use skillfile_core::lock::{read_lock, write_lock};
use skillfile_core::models::{EntityType, Entry, SourceFields, DEFAULT_REF};
use skillfile_core::parser::{infer_name, parse_manifest, MANIFEST_NAME};
use skillfile_deploy::adapter::adapters;

use super::format::sorted_manifest_text;
use skillfile_deploy::install::install_entry;
use skillfile_sources::strategy::format_parts;
use skillfile_sources::sync::{sync_entry, SyncContext};

/// Format an entry as a Skillfile line.
fn format_line(entry: &Entry) -> String {
    let mut parts = vec![
        entry.source_type().to_string(),
        entry.entity_type.to_string(),
    ];
    parts.extend(format_parts(entry));
    parts.join("  ")
}

/// Sync a single entry and install it to all configured targets.
fn sync_and_install(
    entry: &Entry,
    repo_root: &Path,
    manifest: &skillfile_core::models::Manifest,
) -> Result<(), SkillfileError> {
    let mut locked = read_lock(repo_root)?;
    let client = skillfile_sources::http::UreqClient::new();
    let mut ctx = SyncContext {
        repo_root: repo_root.to_path_buf(),
        dry_run: false,
        update: false,
        sha_cache: std::collections::HashMap::new(),
    };
    sync_entry(&client, entry, &mut ctx, &mut locked)?;
    write_lock(repo_root, &locked)?;

    let all_adapters = adapters();
    for target in &manifest.install_targets {
        if all_adapters.contains(&target.adapter) {
            install_entry(entry, target, repo_root, None)?;
        }
    }
    Ok(())
}

/// Build an Entry from CLI arguments for a github source.
#[allow(clippy::too_many_arguments)]
pub fn entry_from_github(
    entity_type: &str,
    owner_repo: &str,
    path: &str,
    ref_: Option<&str>,
    name: Option<&str>,
) -> Entry {
    let inferred = infer_name(path);
    Entry {
        entity_type: EntityType::parse(entity_type).unwrap_or(EntityType::Skill),
        name: name.unwrap_or(&inferred).to_string(),
        source: SourceFields::Github {
            owner_repo: owner_repo.to_string(),
            path_in_repo: path.to_string(),
            ref_: ref_.unwrap_or(DEFAULT_REF).to_string(),
        },
    }
}

/// Build an Entry from CLI arguments for a local source.
pub fn entry_from_local(entity_type: &str, path: &str, name: Option<&str>) -> Entry {
    let inferred = infer_name(path);
    Entry {
        entity_type: EntityType::parse(entity_type).unwrap_or(EntityType::Skill),
        name: name.unwrap_or(&inferred).to_string(),
        source: SourceFields::Local {
            path: path.to_string(),
        },
    }
}

/// Build an Entry from CLI arguments for a url source.
pub fn entry_from_url(entity_type: &str, url: &str, name: Option<&str>) -> Entry {
    let inferred = infer_name(url);
    Entry {
        entity_type: EntityType::parse(entity_type).unwrap_or(EntityType::Skill),
        name: name.unwrap_or(&inferred).to_string(),
        source: SourceFields::Url {
            url: url.to_string(),
        },
    }
}

pub fn cmd_add(entry: &Entry, repo_root: &Path) -> Result<(), SkillfileError> {
    let manifest_path = repo_root.join(MANIFEST_NAME);
    if !manifest_path.exists() {
        return Err(SkillfileError::Manifest(format!(
            "{MANIFEST_NAME} not found in {}. Create one and run `skillfile init`.",
            repo_root.display()
        )));
    }

    let result = parse_manifest(&manifest_path)?;
    let existing_names: std::collections::HashSet<String> = result
        .manifest
        .entries
        .iter()
        .map(|e| e.name.clone())
        .collect();
    if existing_names.contains(&entry.name) {
        return Err(SkillfileError::Manifest(format!(
            "entry '{}' already exists in {MANIFEST_NAME}",
            entry.name
        )));
    }

    let line = format_line(entry);
    let original_manifest = std::fs::read_to_string(&manifest_path)?;

    // Append the new entry
    let mut content = original_manifest.clone();
    content.push_str(&line);
    content.push('\n');
    std::fs::write(&manifest_path, &content)?;

    // Auto-format the Skillfile silently
    let result = parse_manifest(&manifest_path)?;
    let formatted = sorted_manifest_text(&result.manifest, &content);
    std::fs::write(&manifest_path, &formatted)?;

    println!("Added: {line}");

    // Re-parse to check install targets
    let result = parse_manifest(&manifest_path)?;
    if result.manifest.install_targets.is_empty() {
        println!(
            "No install targets configured — run `skillfile init` then `skillfile install` to deploy."
        );
        return Ok(());
    }

    // Auto sync + install with rollback on failure
    let lock_path = repo_root.join("Skillfile.lock");
    let original_lock = if lock_path.exists() {
        Some(std::fs::read_to_string(&lock_path)?)
    } else {
        None
    };

    let sync_install_result = sync_and_install(entry, repo_root, &result.manifest);

    if let Err(e) = sync_install_result {
        // Rollback
        std::fs::write(&manifest_path, &original_manifest)?;
        match &original_lock {
            None => {
                let _ = std::fs::remove_file(&lock_path);
            }
            Some(text) => {
                std::fs::write(&lock_path, text)?;
            }
        }
        eprintln!("Rolled back: removed '{}' from {MANIFEST_NAME}", entry.name);
        return Err(e);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::write(dir.join(MANIFEST_NAME), content).unwrap();
    }

    #[test]
    fn no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let entry = entry_from_local("skill", "skills/foo.md", None);
        let result = cmd_add(&entry, dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn add_local_entry() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let entry = entry_from_local("skill", "skills/foo.md", None);
        cmd_add(&entry, dir.path()).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("local  skill  skills/foo.md"));
    }

    #[test]
    fn add_local_entry_explicit_name() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let entry = entry_from_local("skill", "skills/foo.md", Some("my-foo"));
        cmd_add(&entry, dir.path()).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("local  skill  my-foo  skills/foo.md"));
    }

    #[test]
    fn add_github_entry_inferred_name() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let entry = entry_from_github("agent", "owner/repo", "agents/agent.md", None, None);
        cmd_add(&entry, dir.path()).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("github  agent  owner/repo  agents/agent.md"));
    }

    #[test]
    fn add_github_entry_explicit_name_and_ref() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let entry = entry_from_github(
            "agent",
            "owner/repo",
            "agents/agent.md",
            Some("v1.0"),
            Some("my-agent"),
        );
        cmd_add(&entry, dir.path()).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("github  agent  my-agent  owner/repo  agents/agent.md  v1.0"));
    }

    #[test]
    fn add_url_entry() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let entry = entry_from_url("skill", "https://example.com/skill.md", None);
        cmd_add(&entry, dir.path()).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("url  skill  https://example.com/skill.md"));
    }

    #[test]
    fn add_url_entry_explicit_name() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let entry = entry_from_url("skill", "https://example.com/skill.md", Some("my-skill"));
        cmd_add(&entry, dir.path()).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("url  skill  my-skill  https://example.com/skill.md"));
    }

    #[test]
    fn add_duplicate_name_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");
        let entry = entry_from_local("agent", "agents/foo.md", Some("foo"));
        let result = cmd_add(&entry, dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn add_appends_to_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");
        let entry = entry_from_local("skill", "skills/bar.md", None);
        cmd_add(&entry, dir.path()).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("skills/foo.md"));
        assert!(text.contains("skills/bar.md"));
    }

    #[test]
    fn add_github_dir_entry() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let entry = entry_from_github("agent", "owner/repo", "agents/core-dev", None, None);
        cmd_add(&entry, dir.path()).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        // Name "core-dev" is inferred from path, so omitted from line
        assert!(text.contains("github  agent  owner/repo  agents/core-dev"));
    }

    #[test]
    fn add_no_install_targets_prints_message() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let entry = entry_from_local("skill", "skills/foo.md", None);
        // Should succeed without install targets
        cmd_add(&entry, dir.path()).unwrap();
    }

    // --- format_line direct tests ---

    #[test]
    fn format_line_local() {
        // Name differs from the inferred stem ("foo"), so it must appear in the line.
        let entry = entry_from_local("skill", "skills/foo.md", Some("my-foo"));
        let line = format_line(&entry);
        assert_eq!(line, "local  skill  my-foo  skills/foo.md");
    }

    #[test]
    fn format_line_local_inferred_name_omitted() {
        // When name matches the inferred stem, it is omitted from the line.
        let entry = entry_from_local("skill", "skills/foo.md", None);
        let line = format_line(&entry);
        assert_eq!(line, "local  skill  skills/foo.md");
    }

    #[test]
    fn format_line_github() {
        let entry = entry_from_github(
            "agent",
            "owner/repo",
            "agents/tool.md",
            Some("v2.0"),
            Some("my-tool"),
        );
        let line = format_line(&entry);
        assert_eq!(
            line,
            "github  agent  my-tool  owner/repo  agents/tool.md  v2.0"
        );
    }

    #[test]
    fn format_line_github_default_ref_omitted() {
        // When ref is "main" (DEFAULT_REF) it must be omitted from the line.
        let entry = entry_from_github("skill", "owner/repo", "skills/tool.md", None, Some("tool"));
        let line = format_line(&entry);
        assert_eq!(line, "github  skill  owner/repo  skills/tool.md");
    }

    #[test]
    fn format_line_url() {
        // Name differs from the inferred stem ("my-skill"), so it must appear in the line.
        let entry = entry_from_url(
            "skill",
            "https://example.com/my-skill.md",
            Some("custom-name"),
        );
        let line = format_line(&entry);
        assert_eq!(
            line,
            "url  skill  custom-name  https://example.com/my-skill.md"
        );
    }

    // --- entry_from_github tests ---

    #[test]
    fn entry_from_github_default_ref() {
        let entry = entry_from_github("skill", "o/r", "path.md", None, None);
        match &entry.source {
            SourceFields::Github { ref_, .. } => {
                assert_eq!(
                    ref_, DEFAULT_REF,
                    "expected DEFAULT_REF ('main') when ref is None"
                );
            }
            _ => panic!("expected Github source"),
        }
    }

    #[test]
    fn entry_from_github_explicit_ref() {
        let entry = entry_from_github("skill", "o/r", "path.md", Some("v1.2.3"), None);
        match &entry.source {
            SourceFields::Github { ref_, .. } => {
                assert_eq!(ref_, "v1.2.3");
            }
            _ => panic!("expected Github source"),
        }
    }

    // --- entry_from_url tests ---

    #[test]
    fn entry_from_url_inferred_name() {
        let entry = entry_from_url("skill", "https://example.com/browser-skill.md", None);
        assert_eq!(
            entry.name, "browser-skill",
            "name should be inferred from the URL filename stem"
        );
    }

    #[test]
    fn entry_from_url_explicit_name_overrides_inference() {
        let entry = entry_from_url("agent", "https://example.com/agent.md", Some("my-agent"));
        assert_eq!(entry.name, "my-agent");
    }
}
