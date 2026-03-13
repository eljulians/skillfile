use std::io::{self, IsTerminal, Write};
use std::path::Path;

use skillfile_core::error::SkillfileError;
use skillfile_core::parser::{parse_manifest, MANIFEST_NAME};
use skillfile_deploy::adapter::{adapters, known_adapters};

const GITIGNORE_ENTRIES: &[&str] = &[".skillfile/cache/", ".skillfile/conflict"];

// ---------------------------------------------------------------------------
// Pure helpers (no terminal interaction — fully testable)
// ---------------------------------------------------------------------------

fn rewrite_install_lines(
    manifest_path: &Path,
    new_targets: &[(String, String)],
) -> Result<(), SkillfileError> {
    let text = std::fs::read_to_string(manifest_path)?;
    let mut non_install: Vec<&str> = text
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

    std::fs::write(manifest_path, &output)?;
    Ok(())
}

fn update_gitignore(repo_root: &Path) -> Result<(), SkillfileError> {
    let gitignore = repo_root.join(".gitignore");
    let existing: Vec<String> = if gitignore.exists() {
        std::fs::read_to_string(&gitignore)?
            .lines()
            .map(|l| l.to_string())
            .collect()
    } else {
        Vec::new()
    };

    let missing: Vec<&&str> = GITIGNORE_ENTRIES
        .iter()
        .filter(|e| !existing.iter().any(|l| l == **e))
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&gitignore)?;

    if !existing.is_empty() && existing.last().is_some_and(|l| !l.is_empty()) {
        writeln!(file)?;
    }
    writeln!(file, "# skillfile")?;
    for entry in &missing {
        writeln!(file, "{entry}")?;
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
    let mut multi = cliclack::multiselect("Select platforms to install to");
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
        .interact()?;

    // Build install targets
    let new_targets: Vec<(String, String)> = selected
        .iter()
        .map(|p| (p.to_string(), scope.to_string()))
        .collect();

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
// Tests — pure function coverage (interactive flow tested via functional tests)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::write(dir.join(MANIFEST_NAME), content).unwrap();
    }

    // -- rewrite_install_lines --

    #[test]
    fn writes_install_lines() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");

        let targets = vec![("claude-code".into(), "global".into())];
        rewrite_install_lines(&dir.path().join(MANIFEST_NAME), &targets).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("install  claude-code  global"));
    }

    #[test]
    fn install_lines_at_top() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");

        let targets = vec![("claude-code".into(), "global".into())];
        rewrite_install_lines(&dir.path().join(MANIFEST_NAME), &targets).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "install  claude-code  global");
    }

    #[test]
    fn preserves_existing_entries() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");

        let targets = vec![("claude-code".into(), "local".into())];
        rewrite_install_lines(&dir.path().join(MANIFEST_NAME), &targets).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("local  skill  skills/foo.md"));
        assert!(text.contains("install  claude-code  local"));
    }

    #[test]
    fn multiple_adapters() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");

        let targets = vec![
            ("claude-code".into(), "global".into()),
            ("gemini-cli".into(), "local".into()),
        ];
        rewrite_install_lines(&dir.path().join(MANIFEST_NAME), &targets).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("install  claude-code  global"));
        assert!(text.contains("install  gemini-cli  local"));
    }

    #[test]
    fn replaces_existing_install_targets() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  global\nlocal  skill  skills/foo.md\n",
        );

        let targets = vec![("gemini-cli".into(), "local".into())];
        rewrite_install_lines(&dir.path().join(MANIFEST_NAME), &targets).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(!text.contains("claude-code"));
        assert!(text.contains("install  gemini-cli  local"));
        assert!(text.contains("local  skill  skills/foo.md"));
    }

    #[test]
    fn strips_leading_blanks_after_install_removal() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  old  global\n\n\nlocal  skill  keep.md\n",
        );

        let targets = vec![("new".into(), "local".into())];
        rewrite_install_lines(&dir.path().join(MANIFEST_NAME), &targets).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // install line, blank separator, then entry — no extra blank lines
        assert_eq!(lines[0], "install  new  local");
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "local  skill  keep.md");
    }

    // -- update_gitignore --

    #[test]
    fn creates_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        update_gitignore(dir.path()).unwrap();

        let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gitignore.contains(".skillfile/cache/"));
        assert!(gitignore.contains(".skillfile/conflict"));
    }

    #[test]
    fn gitignore_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".gitignore"),
            "# skillfile\n.skillfile/cache/\n.skillfile/conflict\n",
        )
        .unwrap();

        update_gitignore(dir.path()).unwrap();

        let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(
            gitignore.matches(".skillfile/cache/").count(),
            1,
            "gitignore should not have duplicates"
        );
    }

    #[test]
    fn gitignore_does_not_include_patches() {
        let dir = tempfile::tempdir().unwrap();
        update_gitignore(dir.path()).unwrap();

        let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(!gitignore.contains("patches"));
    }

    #[test]
    fn gitignore_appends_only_missing_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".gitignore"),
            "# skillfile\n.skillfile/cache/\n",
        )
        .unwrap();

        update_gitignore(dir.path()).unwrap();

        let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gitignore.contains(".skillfile/conflict"));
        assert_eq!(gitignore.matches(".skillfile/cache/").count(), 1);
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

    // -- TTY guard --

    #[test]
    fn init_fails_without_tty() {
        // In test context, stdin is not a TTY, so cmd_init should fail
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_init(dir.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("interactive terminal"), "got: {msg}");
    }
}
