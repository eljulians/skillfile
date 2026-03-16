use std::path::Path;

use skillfile_core::error::SkillfileError;
use skillfile_core::models::{EntityType, Entry, Manifest};
use skillfile_core::parser::{parse_manifest, MANIFEST_NAME};
use skillfile_sources::strategy::format_parts;

const INSTALL_COMMENT: &str = "# install  <platform>  <scope>";

fn section_headers(entity_type: &str) -> Vec<&'static str> {
    match entity_type {
        "agent" => vec![
            "# --- Agents ---",
            "# github  agent  [name]  <owner/repo>  <path-or-dir>  [ref]",
        ],
        "skill" => vec![
            "# --- Skills ---",
            "# github  skill  [name]  <owner/repo>  <path-or-dir>  [ref]",
        ],
        _ => vec![],
    }
}

/// Format an entry as a Skillfile line.
pub fn format_line(entry: &Entry) -> String {
    let mut parts = vec![
        entry.source_type().to_string(),
        entry.entity_type.to_string(),
    ];
    parts.extend(format_parts(entry));
    parts.join("  ")
}

fn sort_key(entry: &Entry) -> (String, String, String) {
    let source_type = entry.source_type().to_string();
    let (repo, path) = match &entry.source {
        skillfile_core::models::SourceFields::Github {
            owner_repo,
            path_in_repo,
            ..
        } => (owner_repo.clone(), path_in_repo.clone()),
        skillfile_core::models::SourceFields::Local { path } => (String::new(), path.clone()),
        skillfile_core::models::SourceFields::Url { url } => (String::new(), url.clone()),
    };
    (source_type, repo, path)
}

fn entry_repo_key(entry: &Entry) -> (String, String) {
    let repo = match &entry.source {
        skillfile_core::models::SourceFields::Github { owner_repo, .. } => owner_repo.clone(),
        _ => String::new(),
    };
    (entry.source_type().to_string(), repo)
}

fn flush_group<'a>(groups: &mut Vec<Vec<&'a Entry>>, current_group: &mut Vec<&'a Entry>) {
    if !current_group.is_empty() {
        groups.push(std::mem::take(current_group));
    }
}

/// Split sorted entries into sub-lists by (source_type, owner_repo).
fn group_by_repo<'a>(entries: &'a [&'a Entry]) -> Vec<Vec<&'a Entry>> {
    let mut groups: Vec<Vec<&Entry>> = Vec::new();
    let mut current_key: Option<(String, String)> = None;
    let mut current_group: Vec<&Entry> = Vec::new();

    for entry in entries {
        let key = entry_repo_key(entry);
        if current_key.as_ref() != Some(&key) {
            flush_group(&mut groups, &mut current_group);
            current_key = Some(key);
        }
        current_group.push(entry);
    }
    flush_group(&mut groups, &mut current_group);
    groups
}

/// Extract entry-adjacent comments from raw manifest text.
fn extract_entry_comments(raw_text: &str) -> std::collections::HashMap<String, Vec<String>> {
    let mut attached = std::collections::HashMap::new();
    let mut pending: Vec<String> = Vec::new();

    for line in raw_text.lines() {
        let stripped = line.trim();
        if stripped.starts_with('#') {
            pending.push(line.trim_end().to_string());
            continue;
        }
        if stripped.is_empty() || stripped.starts_with("install") {
            pending.clear();
            continue;
        }
        if !pending.is_empty() {
            attached.insert(stripped.to_string(), pending.clone());
        }
        pending.clear();
    }

    attached
}

fn push_repo_group(
    lines: &mut Vec<String>,
    repo_group: &[&Entry],
    entry_comments: &std::collections::HashMap<String, Vec<String>>,
) {
    lines.push(String::new());
    for entry in repo_group {
        let formatted = format_line(entry);
        lines.extend(
            entry_comments
                .get(&formatted)
                .into_iter()
                .flatten()
                .cloned(),
        );
        lines.push(formatted);
    }
}

pub fn sorted_manifest_text(manifest: &Manifest, raw_text: &str) -> String {
    let entry_comments = if raw_text.is_empty() {
        std::collections::HashMap::new()
    } else {
        extract_entry_comments(raw_text)
    };
    let mut lines: Vec<String> = Vec::new();

    // Install targets section
    if !manifest.install_targets.is_empty() {
        lines.push(INSTALL_COMMENT.to_string());
        for target in &manifest.install_targets {
            lines.push(format!("install  {}  {}", target.adapter, target.scope));
        }
    }

    let mut agents: Vec<&Entry> = manifest
        .entries
        .iter()
        .filter(|e| e.entity_type == EntityType::Agent)
        .collect();
    agents.sort_by_key(|e| sort_key(e));

    let mut skills: Vec<&Entry> = manifest
        .entries
        .iter()
        .filter(|e| e.entity_type == EntityType::Skill)
        .collect();
    skills.sort_by_key(|e| sort_key(e));

    for (entity_type, group) in [("agent", agents), ("skill", skills)] {
        if group.is_empty() {
            continue;
        }
        lines.push(String::new());
        for header in section_headers(entity_type) {
            lines.push(header.to_string());
        }
        for repo_group in group_by_repo(&group) {
            push_repo_group(&mut lines, &repo_group, &entry_comments);
        }
    }

    lines.join("\n") + "\n"
}

pub fn cmd_format(repo_root: &Path, dry_run: bool) -> Result<(), SkillfileError> {
    let manifest_path = repo_root.join(MANIFEST_NAME);
    if !manifest_path.exists() {
        return Err(SkillfileError::Manifest(format!(
            "{MANIFEST_NAME} not found in {}. Create one and run `skillfile init`.",
            repo_root.display()
        )));
    }

    let result = parse_manifest(&manifest_path)?;
    let manifest = result.manifest;
    let raw_text = std::fs::read_to_string(&manifest_path)?;
    let text = sorted_manifest_text(&manifest, &raw_text);

    if dry_run {
        print!("{text}");
        return Ok(());
    }

    std::fs::write(&manifest_path, &text)?;
    let n = manifest.entries.len();
    let word = if n == 1 { "entry" } else { "entries" };
    println!("Formatted {n} {word}.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::write(dir.join(MANIFEST_NAME), content).unwrap();
    }

    fn parse_and_sort(dir: &Path, content: &str) -> String {
        write_manifest(dir, content);
        let result = parse_manifest(&dir.join(MANIFEST_NAME)).unwrap();
        sorted_manifest_text(&result.manifest, content)
    }

    #[test]
    fn install_comment_generated() {
        let dir = tempfile::tempdir().unwrap();
        let text = parse_and_sort(
            dir.path(),
            "install  claude-code  global\ngithub  skill  a/repo  a.md\n",
        );
        assert!(text.contains("# install  <platform>  <scope>"));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "# install  <platform>  <scope>");
        assert_eq!(lines[1], "install  claude-code  global");
    }

    #[test]
    fn agents_section_header_generated() {
        let dir = tempfile::tempdir().unwrap();
        let text = parse_and_sort(dir.path(), "github  agent  owner/repo  agent.md\n");
        assert!(text.contains("# --- Agents ---"));
    }

    #[test]
    fn skills_section_header_generated() {
        let dir = tempfile::tempdir().unwrap();
        let text = parse_and_sort(dir.path(), "github  skill  owner/repo  skill.md\n");
        assert!(text.contains("# --- Skills ---"));
    }

    #[test]
    fn section_format_hint_generated() {
        let dir = tempfile::tempdir().unwrap();
        let text = parse_and_sort(
            dir.path(),
            "github  agent  owner/repo  agent.md\ngithub  skill  owner/repo  skill.md\n",
        );
        assert!(text.contains("# github  agent  [name]  <owner/repo>  <path-or-dir>  [ref]"));
        assert!(text.contains("# github  skill  [name]  <owner/repo>  <path-or-dir>  [ref]"));
    }

    #[test]
    fn no_install_section_when_no_targets() {
        let dir = tempfile::tempdir().unwrap();
        let text = parse_and_sort(dir.path(), "github  skill  a/repo  a.md\n");
        assert!(!text.contains("install"));
    }

    #[test]
    fn entries_grouped_by_repo_with_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let text = parse_and_sort(
            dir.path(),
            "github  skill  b/repo  b.md\ngithub  skill  a/repo  a.md\ngithub  skill  a/repo  z.md\n",
        );
        let lines: Vec<&str> = text.lines().collect();
        let skill_lines: Vec<&&str> = lines
            .iter()
            .filter(|l| l.starts_with("github  skill"))
            .collect();
        assert_eq!(*skill_lines[0], "github  skill  a/repo  a.md");
        assert_eq!(*skill_lines[1], "github  skill  a/repo  z.md");
        assert_eq!(*skill_lines[2], "github  skill  b/repo  b.md");
    }

    #[test]
    fn agents_before_skills() {
        let dir = tempfile::tempdir().unwrap();
        let text = parse_and_sort(
            dir.path(),
            "github  skill  owner/repo  skill.md\ngithub  agent  owner/repo  agent.md\n",
        );
        assert!(text.find("# --- Agents ---").unwrap() < text.find("# --- Skills ---").unwrap());
    }

    #[test]
    fn entry_adjacent_comment_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let text = parse_and_sort(
            dir.path(),
            "github  skill  z/repo  z.md\n# my annotation\ngithub  skill  a/repo  a.md\n",
        );
        let lines: Vec<&str> = text.lines().collect();
        let idx = lines.iter().position(|l| *l == "# my annotation").unwrap();
        assert_eq!(lines[idx + 1], "github  skill  a/repo  a.md");
    }

    #[test]
    fn section_comment_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let text = parse_and_sort(
            dir.path(),
            "# old section header\n\ngithub  skill  b/repo  b.md\ngithub  skill  a/repo  a.md\n",
        );
        assert!(!text.contains("old section header"));
    }

    #[test]
    fn cmd_format_rewrites_file() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "github  skill  z/repo  z.md\ngithub  skill  a/repo  a.md\n",
        );
        cmd_format(dir.path(), false).unwrap();
        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        let skill_lines: Vec<&str> = text.lines().filter(|l| l.starts_with("github")).collect();
        assert_eq!(skill_lines[0], "github  skill  a/repo  a.md");
        assert_eq!(skill_lines[1], "github  skill  z/repo  z.md");
    }

    #[test]
    fn cmd_format_dry_run_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let original = "github  skill  z/repo  z.md\ngithub  skill  a/repo  a.md\n";
        write_manifest(dir.path(), original);
        cmd_format(dir.path(), true).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap(),
            original
        );
    }

    #[test]
    fn cmd_format_no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_format(dir.path(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
