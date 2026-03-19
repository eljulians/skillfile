use std::io::IsTerminal;
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
    let locked = read_lock(repo_root)?;
    let client = skillfile_sources::http::UreqClient::new();
    let mut ctx = SyncContext {
        repo_root: repo_root.to_path_buf(),
        dry_run: false,
        update: false,
        sha_cache: std::collections::HashMap::new(),
        locked,
    };
    sync_entry(&client, entry, &mut ctx)?;
    write_lock(repo_root, &ctx.locked)?;

    let all_adapters = adapters();
    for target in &manifest.install_targets {
        if all_adapters.contains(&target.adapter) {
            install_entry(
                entry,
                target,
                &skillfile_deploy::install::InstallCtx {
                    repo_root,
                    opts: None,
                },
            )?;
        }
    }
    Ok(())
}

/// CLI arguments for building a GitHub entry.
pub struct GithubEntryArgs<'a> {
    pub entity_type: &'a str,
    pub owner_repo: &'a str,
    pub path: &'a str,
    pub ref_: Option<&'a str>,
    pub name: Option<&'a str>,
}

/// Build an Entry from CLI arguments for a github source.
pub fn entry_from_github(args: &GithubEntryArgs<'_>) -> Entry {
    let inferred = infer_name(args.path);
    Entry {
        entity_type: EntityType::parse(args.entity_type).unwrap_or(EntityType::Skill),
        name: args.name.unwrap_or(&inferred).to_string(),
        source: SourceFields::Github {
            owner_repo: args.owner_repo.to_string(),
            path_in_repo: args.path.to_string(),
            ref_: args.ref_.unwrap_or(DEFAULT_REF).to_string(),
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

/// Arguments for [`cmd_add_bulk`].
pub struct BulkAddArgs<'a> {
    pub entity_type: &'a str,
    pub owner_repo: &'a str,
    pub base_path: &'a str,
    pub ref_: Option<&'a str>,
    pub no_interactive: bool,
}

/// Discover entries in a GitHub repo under `base_path`, present a multi-select,
/// and add each selected entry via [`cmd_add`].
pub fn cmd_add_bulk(args: &BulkAddArgs<'_>, repo_root: &Path) -> Result<(), SkillfileError> {
    use skillfile_core::output::Spinner;
    use skillfile_sources::resolver::list_repo_skill_entries_under;

    // Validate repo exists before spending time on Tree API discovery.
    validate_github_repo(args.owner_repo)?;

    let spinner = Spinner::new(&format!(
        "Discovering {}s in {}...",
        args.entity_type, args.owner_repo
    ));
    let client = skillfile_sources::http::UreqClient::new();
    let entries = list_repo_skill_entries_under(&client, args.owner_repo, args.base_path);
    spinner.finish();

    if entries.is_empty() {
        return Err(SkillfileError::Manifest(format!(
            "no {}s found under '{}' in {}",
            args.entity_type, args.base_path, args.owner_repo
        )));
    }

    // Single entry discovered — skip TUI, add directly.
    if entries.len() == 1 {
        add_selected(&entries, args, repo_root);
        return Ok(());
    }

    let selected = select_entries(&entries, args)?;
    if !selected.is_empty() {
        add_selected(&selected, args, repo_root);
    }
    Ok(())
}

/// Interactive or automatic entry selection.
fn select_entries(
    entries: &[String],
    args: &BulkAddArgs<'_>,
) -> Result<Vec<String>, SkillfileError> {
    if args.no_interactive {
        return Ok(entries.to_vec());
    }
    if !std::io::stderr().is_terminal() {
        return Err(SkillfileError::Manifest(
            "interactive selection requires a terminal; use --no-interactive".to_string(),
        ));
    }
    let ref_ = args.ref_.unwrap_or(DEFAULT_REF);
    let selected = super::add_tui::run_add_tui(entries, args.owner_repo, ref_)
        .map_err(|e| SkillfileError::Install(format!("TUI error: {e}")))?;
    if selected.is_empty() {
        println!("No entries selected.");
    }
    Ok(selected)
}

/// Add each selected path to the manifest, collecting results.
fn add_selected(selected: &[String], args: &BulkAddArgs<'_>, repo_root: &Path) {
    let mut added = 0usize;
    let mut skipped = 0usize;
    for path in selected {
        let entry = entry_from_github(&GithubEntryArgs {
            entity_type: args.entity_type,
            owner_repo: args.owner_repo,
            path,
            ref_: args.ref_,
            name: None,
        });
        match cmd_add(&entry, repo_root) {
            Ok(()) => added += 1,
            Err(SkillfileError::Manifest(msg)) if msg.contains("already exists") => {
                eprintln!("warning: skipped '{}' (already in Skillfile)", entry.name);
                skipped += 1;
            }
            Err(e) => {
                eprintln!("warning: failed to add '{}': {e}", entry.name);
                skipped += 1;
            }
        }
    }
    let plural = if added == 1 { "" } else { "s" };
    let skip_msg = if skipped > 0 {
        format!(" ({skipped} skipped)")
    } else {
        String::new()
    };
    println!(
        "Added {added} {}{plural} to Skillfile.{skip_msg}",
        args.entity_type
    );
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

/// Interactive add wizard — launched by bare `skillfile add` with no subcommand.
///
/// Guides the user through source selection and delegates to existing
/// add functions. Each flow terminates in a function that is already
/// tested independently.
pub fn cmd_add_interactive(repo_root: &Path) -> Result<(), SkillfileError> {
    if !std::io::stderr().is_terminal() {
        return Err(SkillfileError::Manifest(
            "interactive wizard requires a terminal; use `skillfile add github|local|url` directly"
                .to_owned(),
        ));
    }

    cliclack::intro(console::style(" skillfile add ").on_cyan().black())?;

    let source: &str = cliclack::select("How do you want to add a skill or agent?")
        .item(
            "github",
            "GitHub repository",
            "Browse and pick from any repo",
        )
        .item(
            "search",
            "Search registries",
            "Find on agentskill.sh, skills.sh",
        )
        .item("local", "Local file", "A .md file already in this repo")
        .item("url", "URL", "Direct link to a .md file")
        .interact()?;

    match source {
        "github" => wizard_github(repo_root),
        "search" => wizard_search(repo_root),
        "local" => wizard_local(repo_root),
        "url" => wizard_url(repo_root),
        _ => unreachable!(),
    }
}

/// Validate that a GitHub repo exists. Returns `Ok(())` or a user-facing error.
fn validate_github_repo(owner_repo: &str) -> Result<(), SkillfileError> {
    use skillfile_sources::http::HttpClient;
    let client = skillfile_sources::http::UreqClient::new();
    let url = format!("https://api.github.com/repos/{owner_repo}");
    match client.get_json(&url)? {
        Some(_) => Ok(()),
        None => Err(SkillfileError::Network(format!(
            "repository '{owner_repo}' not found on GitHub"
        ))),
    }
}

/// Discover top-level directories in a repo for path hint display.
fn discover_top_level_dirs(owner_repo: &str) -> Vec<String> {
    use skillfile_sources::resolver::list_repo_skill_entries_under;
    let client = skillfile_sources::http::UreqClient::new();
    let entries = list_repo_skill_entries_under(&client, owner_repo, ".");
    let mut dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for entry in &entries {
        if let Some(first_seg) = entry.split('/').next() {
            dirs.insert(first_seg.to_owned());
        }
    }
    dirs.into_iter().collect()
}

/// GitHub wizard flow: owner/repo → validate → entity type → path with hints → discovery TUI.
fn wizard_github(repo_root: &Path) -> Result<(), SkillfileError> {
    use skillfile_core::output::Spinner;

    let owner_repo: String = cliclack::input("GitHub repository (owner/repo)")
        .placeholder("e.g. anthropics/skills")
        .validate(|v: &String| {
            if v.contains('/') && v.len() > 2 {
                Ok(())
            } else {
                Err("Expected format: owner/repo")
            }
        })
        .interact()?;

    // Validate repo exists before asking more questions.
    let spinner = Spinner::new(&format!("Checking {owner_repo}..."));
    let valid = validate_github_repo(&owner_repo);
    spinner.finish();
    valid?;

    // Default to "skill" — agents are rare and structurally identical.
    // Users adding agents can use `skillfile add github agent ...` directly.
    let entity_type = "skill";

    // Discover top-level dirs for the path hint.
    let spinner = Spinner::new("Scanning repository...");
    let top_dirs = discover_top_level_dirs(&owner_repo);
    spinner.finish();

    let path_hint = if top_dirs.is_empty() {
        "press Enter to scan the entire repo".to_owned()
    } else {
        format!("found: {}  (or . for all)", top_dirs.join(", "))
    };

    let base_path: String = cliclack::input("Path within repo")
        .placeholder(&path_hint)
        .default_input(".")
        .interact()?;

    cmd_add_bulk(
        &BulkAddArgs {
            entity_type,
            owner_repo: &owner_repo,
            base_path: &base_path,
            ref_: None,
            no_interactive: false,
        },
        repo_root,
    )
}

/// Search wizard flow: query → delegates to `cmd_search`.
fn wizard_search(repo_root: &Path) -> Result<(), SkillfileError> {
    let query: String = cliclack::input("Search query")
        .placeholder("code review")
        .interact()?;

    cliclack::outro("Launching search...")?;

    super::search::cmd_search(&super::search::SearchConfig {
        query: &query,
        limit: 50,
        min_score: None,
        json: false,
        registry: None,
        no_interactive: false,
        repo_root,
    })
}

/// Local file wizard flow: entity type → path → `cmd_add`.
fn wizard_local(repo_root: &Path) -> Result<(), SkillfileError> {
    let entity_type: &str = cliclack::select("What are you adding?")
        .item("skill", "Skill", "")
        .item("agent", "Agent", "")
        .interact()?;

    let path: String = cliclack::input("Path to .md file")
        .placeholder("skills/my-skill/SKILL.md")
        .interact()?;

    let entry = entry_from_local(entity_type, &path, None);
    cmd_add(&entry, repo_root)
}

/// URL wizard flow: entity type → URL → optional name → `cmd_add`.
fn wizard_url(repo_root: &Path) -> Result<(), SkillfileError> {
    let entity_type: &str = cliclack::select("What are you adding?")
        .item("skill", "Skill", "")
        .item("agent", "Agent", "")
        .interact()?;

    let url: String = cliclack::input("URL to .md file")
        .placeholder("https://example.com/skill.md")
        .interact()?;

    let name: String = cliclack::input("Name override (leave empty to infer from URL)")
        .default_input("")
        .interact()?;

    let name_opt = (!name.is_empty()).then_some(name.as_str());
    let entry = entry_from_url(entity_type, &url, name_opt);
    cmd_add(&entry, repo_root)
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
        let entry = entry_from_github(&GithubEntryArgs {
            entity_type: "agent",
            owner_repo: "owner/repo",
            path: "agents/agent.md",
            ref_: None,
            name: None,
        });
        cmd_add(&entry, dir.path()).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("github  agent  owner/repo  agents/agent.md"));
    }

    #[test]
    fn add_github_entry_explicit_name_and_ref() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let entry = entry_from_github(&GithubEntryArgs {
            entity_type: "agent",
            owner_repo: "owner/repo",
            path: "agents/agent.md",
            ref_: Some("v1.0"),
            name: Some("my-agent"),
        });
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
        let entry = entry_from_github(&GithubEntryArgs {
            entity_type: "agent",
            owner_repo: "owner/repo",
            path: "agents/core-dev",
            ref_: None,
            name: None,
        });
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
        let entry = entry_from_github(&GithubEntryArgs {
            entity_type: "agent",
            owner_repo: "owner/repo",
            path: "agents/tool.md",
            ref_: Some("v2.0"),
            name: Some("my-tool"),
        });
        let line = format_line(&entry);
        assert_eq!(
            line,
            "github  agent  my-tool  owner/repo  agents/tool.md  v2.0"
        );
    }

    #[test]
    fn format_line_github_default_ref_omitted() {
        // When ref is "main" (DEFAULT_REF) it must be omitted from the line.
        let entry = entry_from_github(&GithubEntryArgs {
            entity_type: "skill",
            owner_repo: "owner/repo",
            path: "skills/tool.md",
            ref_: None,
            name: Some("tool"),
        });
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
        let entry = entry_from_github(&GithubEntryArgs {
            entity_type: "skill",
            owner_repo: "o/r",
            path: "path.md",
            ref_: None,
            name: None,
        });
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
        let entry = entry_from_github(&GithubEntryArgs {
            entity_type: "skill",
            owner_repo: "o/r",
            path: "path.md",
            ref_: Some("v1.2.3"),
            name: None,
        });
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
