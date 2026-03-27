use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::Command;

use skillfile_core::error::SkillfileError;
use skillfile_core::models::EntityType;
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
        use std::fmt::Write as _;
        let _ = writeln!(output, "install  {adapter}  {scope}");
    }
    output.push('\n');
    for line in &non_install {
        output.push_str(line);
        output.push('\n');
    }

    output
}

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

fn supported_types_hint(adapter_name: &str) -> &'static str {
    let reg = adapters();
    match reg.get(adapter_name) {
        Some(a) => match (a.supports(EntityType::Skill), a.supports(EntityType::Agent)) {
            (true, true) => "skill, agent",
            (true, false) => "skill only",
            (false, true) => "agent only",
            _ => "",
        },
        None => "",
    }
}

fn write_personal_config(new_targets: &[(String, String)]) -> Result<(), SkillfileError> {
    use skillfile_core::models::{InstallTarget, Scope};
    let targets: Vec<InstallTarget> = new_targets
        .iter()
        .map(|(a, s)| InstallTarget {
            adapter: a.clone(),
            scope: Scope::parse(s).unwrap_or(Scope::Local),
        })
        .collect();
    crate::config::write_user_targets(&targets)?;
    Ok(())
}

fn select_platforms_and_scope(
    existing_set: &std::collections::HashSet<&str>,
) -> Result<Option<Vec<(String, String)>>, SkillfileError> {
    let adapter_names = known_adapters();
    let mut multi =
        cliclack::multiselect("Select platforms to install to (space to toggle, enter to confirm)");
    for name in &adapter_names {
        multi = multi.item(*name, *name, supported_types_hint(name));
    }

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
        return Ok(None);
    }

    let scope: &str = cliclack::select("Default scope for selected platforms?")
        .item("local", "local", "project-specific")
        .item("global", "global", "user-wide (~/.tool/)")
        .item("both", "both", "add global and local for each platform")
        .interact()?;

    let targets = if scope == "both" {
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

    Ok(Some(targets))
}

fn select_destination() -> Result<&'static str, SkillfileError> {
    let config_location = crate::config::config_path().map_or_else(
        || "~/.config/skillfile/config.toml".into(),
        |p| p.display().to_string(),
    );
    let destination: &str = cliclack::select(
        "Where should platform config be stored?\n\
         Tip: In shared repos, personal config avoids merge conflicts when\n\
         teammates use different AI tools.\n\
         Precedence: Skillfile targets always override personal config.",
    )
    .item(
        "personal",
        "Personal config (recommended for shared repos)",
        format!("saved to {config_location} — each developer picks their own platforms"),
    )
    .item(
        "skillfile",
        "Skillfile (shared with team)",
        "committed to git, visible to all collaborators",
    )
    .interact()?;
    Ok(destination)
}

// ---------------------------------------------------------------------------
// GitHub token setup helpers
// ---------------------------------------------------------------------------

/// Check for an existing token via env vars, config file, or `gh` CLI directly.
/// Does NOT use the `OnceLock`-cached `github_token()` — that may already be
/// populated with `None` by the time init runs.
fn detect_existing_token() -> bool {
    let has_env = std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .is_ok_and(|t| !t.is_empty());
    if has_env {
        return true;
    }
    if crate::config::read_config_token().is_some() {
        return true;
    }
    Command::new("gh")
        .args(["auth", "token"])
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}

fn gh_available() -> bool {
    Command::new("gh")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn validate_token(token: &str) -> bool {
    ureq::Agent::new_with_defaults()
        .get("https://api.github.com/user")
        .header("Authorization", &format!("Bearer {token}"))
        .header("User-Agent", "skillfile/1.0")
        .call()
        .is_ok_and(|r| r.status() == 200)
}

fn handle_paste_token() -> Result<(), SkillfileError> {
    let token: String =
        cliclack::password("Paste your GitHub personal access token:").interact()?;
    if validate_token(&token) {
        crate::config::write_config_token(&token)?;
        cliclack::log::success("Token saved to config (0o600)")?;
    } else {
        cliclack::log::warning(
            "Token validation failed — not saved. You can set GITHUB_TOKEN manually.",
        )?;
    }
    Ok(())
}

fn handle_gh_cli() -> Result<(), SkillfileError> {
    cliclack::log::info("Run `gh auth login` in another terminal, then press Enter.")?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    if detect_existing_token() {
        cliclack::log::success("GitHub token found via gh CLI")?;
    } else {
        cliclack::log::warning("Still no token detected. You can set GITHUB_TOKEN manually.")?;
    }
    Ok(())
}

/// Interactive GitHub token setup step for `skillfile init`.
///
/// Skips when a token is already available. Otherwise presents options for gh
/// CLI auth, pasting a token, or skipping (with a rate-limit warning).
fn setup_github_token() -> Result<(), SkillfileError> {
    if detect_existing_token() {
        cliclack::log::success("GitHub token found")?;
        return Ok(());
    }

    let show_gh = gh_available();
    let mut select = cliclack::select("No GitHub token found. How would you like to authenticate?");
    if show_gh {
        select = select.item(
            "gh",
            "Use gh CLI",
            "run `gh auth login` in another terminal",
        );
    }
    select = select
        .item("paste", "Paste a token", "github.com/settings/tokens")
        .item("skip", "Skip", "unauthenticated: 60 req/hr limit");

    let choice: &str = select.interact()?;
    match choice {
        "gh" => handle_gh_cli(),
        "paste" => handle_paste_token(),
        _ => {
            cliclack::log::warning(
                "Skipping token setup. GitHub API limited to 60 req/hr without a token.",
            )?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point — interactive cliclack flow
// ---------------------------------------------------------------------------

/// Write targets to manifest or personal config and print a summary note.
fn persist_targets(
    manifest_path: &Path,
    destination: &str,
    new_targets: &[(String, String)],
) -> Result<(), SkillfileError> {
    let summary: Vec<String> = new_targets
        .iter()
        .map(|(a, s)| format!("install  {a}  {s}"))
        .collect();

    if destination == "personal" {
        write_personal_config(new_targets)?;
        cliclack::note(
            "Install config written to personal config",
            summary.join("\n"),
        )?;
    } else {
        rewrite_install_lines(manifest_path, new_targets)?;
        cliclack::note("Install config written to Skillfile", summary.join("\n"))?;
    }
    Ok(())
}

pub fn cmd_init(repo_root: &Path) -> Result<(), SkillfileError> {
    // TTY guard: cliclack requires an interactive terminal. Check stdin, stdout,
    // and the CI env var because some CI runners (macOS GitHub Actions) report
    // piped fds as TTY.
    let is_ci = std::env::var("CI").is_ok();
    if is_ci || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
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
    let user_targets = crate::config::read_user_targets();

    // Show existing config
    let existing_set: std::collections::HashSet<&str> = existing
        .iter()
        .chain(user_targets.iter())
        .map(|t| t.adapter.as_str())
        .collect();

    if !existing.is_empty() || !user_targets.is_empty() {
        let mut lines: Vec<String> = existing
            .iter()
            .map(|t| format!("install  {}  {}  (Skillfile)", t.adapter, t.scope))
            .collect();
        for t in &user_targets {
            lines.push(format!(
                "install  {}  {}  (personal config)",
                t.adapter, t.scope
            ));
        }
        cliclack::note("Existing config", lines.join("\n"))?;
    }

    // Platform + scope selection
    let Some(new_targets) = select_platforms_and_scope(&existing_set)? else {
        cliclack::outro_cancel("No platforms selected.")?;
        return Ok(());
    };

    let destination = select_destination()?;
    persist_targets(&manifest_path, destination, &new_targets)?;
    setup_github_token()?;
    update_gitignore(repo_root)?;

    let outro = if result.manifest.entries.is_empty() {
        "You're all set! Next up:".to_string()
    } else {
        let n = result.manifest.entries.len();
        let word = if n == 1 { "entry" } else { "entries" };
        format!(
            "Platforms configured! This Skillfile already has {n} {word}.\n  \
                 \u{1f680} Run `skillfile install` to fetch and deploy them."
        )
    };
    cliclack::outro(format!(
        "{outro}\n  \
         \u{2795} `skillfile add` to add a skill or agent\n  \
         \u{1f50d} `skillfile search` to discover community skills"
    ))?;

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
