use std::io::{self, IsTerminal, Write};
use std::path::Path;

use skillfile_core::error::SkillfileError;
use skillfile_core::parser::{parse_manifest, MANIFEST_NAME};
use skillfile_deploy::adapter::{adapters, known_adapters};

const GITIGNORE_ENTRIES: &[&str] = &[".skillfile/cache/", ".skillfile/conflict"];

// ---------------------------------------------------------------------------
// Pure helpers (no IO — fully testable)
// ---------------------------------------------------------------------------

/// Build a new manifest string with install lines replaced.
/// Pure transformation: takes existing content and new targets, returns new content.
fn build_manifest_with_targets(existing: &str, new_targets: &[(String, String)]) -> String {
    let mut non_install: Vec<&str> = existing
        .lines()
        .filter(|line| {
            let stripped = line.trim();
            !stripped.starts_with("install ") && stripped != "install"
        })
        .collect();

    // Strip leading blank lines from remaining content
    while non_install.first().is_some_and(|l| l.trim().is_empty()) {
        non_install.remove(0);
    }

    let mut output = String::new();
    for (adapter, scope) in new_targets {
        output.push_str(&format!("install  {adapter}  {scope}\n"));
    }
    output.push('\n');
    for line in &non_install {
        output.push_str(line);
        output.push('\n');
    }

    output
}

/// Compute gitignore lines to append (if any).
/// Returns `None` if nothing needs to be added.
fn gitignore_additions(existing: &str) -> Option<String> {
    let lines: Vec<&str> = existing.lines().collect();

    let missing: Vec<&&str> = GITIGNORE_ENTRIES
        .iter()
        .filter(|e| !lines.iter().any(|l| l == *e))
        .collect();

    if missing.is_empty() {
        return None;
    }

    let mut additions = String::new();
    if !lines.is_empty() && lines.last().is_some_and(|l| !l.is_empty()) {
        additions.push('\n');
    }
    additions.push_str("# skillfile\n");
    for entry in &missing {
        additions.push_str(entry);
        additions.push('\n');
    }

    Some(additions)
}

// ---------------------------------------------------------------------------
// IO wrappers
// ---------------------------------------------------------------------------

fn rewrite_install_lines(
    manifest_path: &Path,
    new_targets: &[(String, String)],
) -> Result<(), SkillfileError> {
    let text = std::fs::read_to_string(manifest_path)?;
    let output = build_manifest_with_targets(&text, new_targets);
    std::fs::write(manifest_path, &output)?;
    Ok(())
}

fn update_gitignore(repo_root: &Path) -> Result<(), SkillfileError> {
    let gitignore = repo_root.join(".gitignore");
    let existing = if gitignore.exists() {
        std::fs::read_to_string(&gitignore)?
    } else {
        String::new()
    };

    if let Some(additions) = gitignore_additions(&existing) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&gitignore)?;
        write!(file, "{additions}")?;
    }

    Ok(())
}

/// Return a human-readable hint for the entity types supported by a platform.
fn supported_types_hint(adapter_name: &str) -> &'static str {
    let reg = adapters();
    match reg.get(adapter_name) {
        Some(a) => match (a.supports("skill"), a.supports("agent")) {
            (true, true) => "skill, agent",
            (true, false) => "skill only",
            (false, true) => "agent only",
            _ => "",
        },
        None => "",
    }
}

// ---------------------------------------------------------------------------
// Public entry point — interactive cliclack flow
// ---------------------------------------------------------------------------

pub fn cmd_init(repo_root: &Path) -> Result<(), SkillfileError> {
    // TTY guard: cliclack requires an interactive terminal.
    if !io::stdin().is_terminal() {
        return Err(SkillfileError::Manifest(
            "skillfile init requires an interactive terminal.\n\
             Use `skillfile add` for scripted/CI usage."
                .into(),
        ));
    }

    cliclack::intro(console::style(" skillfile init ").on_cyan().black())?;

    // Create Skillfile if missing
    let manifest_path = repo_root.join(MANIFEST_NAME);
    if !manifest_path.exists() {
        std::fs::write(&manifest_path, "")?;
        cliclack::log::info(format!("Created {MANIFEST_NAME}"))?;
    }

    // Parse existing manifest
    let result = parse_manifest(&manifest_path)?;
    let existing = &result.manifest.install_targets;

    // Show existing config
    let existing_set: std::collections::HashSet<&str> =
        existing.iter().map(|t| t.adapter.as_str()).collect();

    if !existing.is_empty() {
        let lines: Vec<String> = existing
            .iter()
            .map(|t| format!("install  {}  {}", t.adapter, t.scope))
            .collect();
        cliclack::note("Existing config", lines.join("\n"))?;
    }

    // Platform multi-select
    let adapter_names = known_adapters();
    let mut multi =
        cliclack::multiselect("Select platforms to install to (space to toggle, enter to confirm)");
    for name in &adapter_names {
        multi = multi.item(*name, *name, supported_types_hint(name));
    }

    // Pre-select platforms that already have install lines
    let initial: Vec<&str> = adapter_names
        .iter()
        .copied()
        .filter(|n| existing_set.contains(n))
        .collect();
    if !initial.is_empty() {
        multi = multi.initial_values(initial);
    }

    let selected: Vec<&str> = multi.interact()?;

    if selected.is_empty() {
        cliclack::outro_cancel("No platforms selected.")?;
        return Ok(());
    }

    // Scope selection
    let scope: &str = cliclack::select("Default scope for selected platforms?")
        .item("local", "local", "project-specific (recommended)")
        .item("global", "global", "user-wide (~/.tool/)")
        .item("both", "both", "add global and local for each platform")
        .interact()?;

    // Build install targets
    let new_targets: Vec<(String, String)> = if scope == "both" {
        selected
            .iter()
            .flat_map(|p| {
                [
                    (p.to_string(), "global".to_string()),
                    (p.to_string(), "local".to_string()),
                ]
            })
            .collect()
    } else {
        selected
            .iter()
            .map(|p| (p.to_string(), scope.to_string()))
            .collect()
    };

    rewrite_install_lines(&manifest_path, &new_targets)?;
    update_gitignore(repo_root)?;

    let summary: Vec<String> = new_targets
        .iter()
        .map(|(a, s)| format!("install  {a}  {s}"))
        .collect();
    cliclack::note("Install config written to Skillfile", summary.join("\n"))?;

    cliclack::outro("Run `skillfile install` to fetch and deploy.")?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests — pure functions only, no IO
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- build_manifest_with_targets --

    #[test]
    fn writes_install_lines() {
        let result = build_manifest_with_targets("", &[("claude-code".into(), "global".into())]);
        assert!(result.contains("install  claude-code  global"));
    }

    #[test]
    fn install_lines_at_top() {
        let result = build_manifest_with_targets(
            "local  skill  skills/foo.md\n",
            &[("claude-code".into(), "global".into())],
        );
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines[0], "install  claude-code  global");
    }

    #[test]
    fn preserves_existing_entries() {
        let result = build_manifest_with_targets(
            "local  skill  skills/foo.md\n",
            &[("claude-code".into(), "local".into())],
        );
        assert!(result.contains("local  skill  skills/foo.md"));
        assert!(result.contains("install  claude-code  local"));
    }

    #[test]
    fn multiple_adapters() {
        let result = build_manifest_with_targets(
            "",
            &[
                ("claude-code".into(), "global".into()),
                ("gemini-cli".into(), "local".into()),
            ],
        );
        assert!(result.contains("install  claude-code  global"));
        assert!(result.contains("install  gemini-cli  local"));
    }

    #[test]
    fn replaces_existing_install_targets() {
        let result = build_manifest_with_targets(
            "install  claude-code  global\nlocal  skill  skills/foo.md\n",
            &[("gemini-cli".into(), "local".into())],
        );
        assert!(!result.contains("claude-code"));
        assert!(result.contains("install  gemini-cli  local"));
        assert!(result.contains("local  skill  skills/foo.md"));
    }

    #[test]
    fn strips_leading_blanks_after_install_removal() {
        let result = build_manifest_with_targets(
            "install  old  global\n\n\nlocal  skill  keep.md\n",
            &[("new".into(), "local".into())],
        );
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines[0], "install  new  local");
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "local  skill  keep.md");
    }

    // -- gitignore_additions --

    #[test]
    fn gitignore_from_empty() {
        let additions = gitignore_additions("");
        let text = additions.unwrap();
        assert!(text.contains(".skillfile/cache/"));
        assert!(text.contains(".skillfile/conflict"));
    }

    #[test]
    fn gitignore_idempotent() {
        let existing = "# skillfile\n.skillfile/cache/\n.skillfile/conflict\n";
        assert!(gitignore_additions(existing).is_none());
    }

    #[test]
    fn gitignore_does_not_include_patches() {
        let text = gitignore_additions("").unwrap();
        assert!(!text.contains("patches"));
    }

    #[test]
    fn gitignore_appends_only_missing_entries() {
        let text = gitignore_additions("# skillfile\n.skillfile/cache/\n").unwrap();
        assert!(text.contains(".skillfile/conflict"));
        assert!(!text.contains(".skillfile/cache/"));
    }

    #[test]
    fn gitignore_adds_blank_separator_after_content() {
        let text = gitignore_additions("node_modules/").unwrap();
        assert!(text.starts_with('\n'), "should add blank line separator");
    }

    #[test]
    fn gitignore_no_blank_separator_after_trailing_blank_line() {
        // File already ends with a blank line — don't double up.
        let text = gitignore_additions("node_modules/\n\n").unwrap();
        assert!(!text.starts_with('\n'), "should not double-blank");
    }

    // -- supported_types_hint --

    #[test]
    fn hint_for_full_adapter() {
        assert_eq!(supported_types_hint("claude-code"), "skill, agent");
    }

    #[test]
    fn hint_for_skill_only_adapter() {
        assert_eq!(supported_types_hint("codex"), "skill only");
    }

    #[test]
    fn hint_for_unknown_adapter() {
        assert_eq!(supported_types_hint("nonexistent"), "");
    }
}
