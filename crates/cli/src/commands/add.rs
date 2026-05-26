use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use super::format::sorted_manifest_text;
use skillfile_core::error::SkillfileError;
use skillfile_core::lock::{read_lock, write_lock};
use skillfile_core::models::{EntityType, Entry, Manifest, SourceFields, DEFAULT_REF};
use skillfile_core::parser::{infer_name, parse_manifest, parse_owner_repo_ref, MANIFEST_NAME};
use skillfile_deploy::install::{
    capture_install_snapshot, install_entry_with_outcome, InstallOutcome, InstallSkipReason,
    InstallSnapshot,
};
use skillfile_sources::strategy::format_parts;
use skillfile_sources::sync::{sync_entry, vendor_dir_for, SyncContext};

fn format_line(entry: &Entry) -> String {
    let mut parts = vec![
        entry.source_type().to_string(),
        entry.entity_type.to_string(),
    ];
    parts.extend(format_parts(entry));
    parts.join("  ")
}

struct AddInstallCtx<'a, 'b> {
    manifest: &'a Manifest,
    rollback: &'a mut RollbackState,
    repo_root: &'b Path,
}

struct InstallSnapshotCapture<'a> {
    entry: &'a Entry,
    manifest: &'a Manifest,
    repo_root: &'a Path,
}

fn sync_and_install(
    entry: &Entry,
    ctx: &mut AddInstallCtx<'_, '_>,
) -> Result<InstallReport, SkillfileError> {
    let locked = read_lock(ctx.repo_root)?;
    let client = skillfile_sources::http::UreqClient::new();
    let mut sync_ctx = SyncContext {
        repo_root: ctx.repo_root.to_path_buf(),
        dry_run: false,
        update: false,
        sha_cache: std::collections::HashMap::new(),
        locked,
    };
    sync_entry(&client, entry, &mut sync_ctx)?;
    write_lock(ctx.repo_root, &sync_ctx.locked)?;
    ctx.rollback
        .capture_install_snapshot(&InstallSnapshotCapture {
            entry,
            manifest: ctx.manifest,
            repo_root: ctx.repo_root,
        })?;

    let mut report = InstallReport::default();
    for target in &ctx.manifest.install_targets {
        let outcome = install_entry_with_outcome(
            entry,
            target,
            &skillfile_deploy::install::InstallCtx {
                repo_root: ctx.repo_root,
                opts: None,
            },
        )?;
        let label = format!("{target}");
        match outcome {
            InstallOutcome::Installed => report.installed.push(label),
            InstallOutcome::Skipped(reason) => {
                report.skipped.push(format!(
                    "{label} [{}]",
                    skip_reason_text(reason, entry.entity_type)
                ));
            }
        }
    }
    Ok(report)
}

pub struct GithubEntryArgs<'a> {
    pub entity_type: &'a str,
    pub owner_repo: &'a str,
    pub path: &'a str,
    pub ref_: Option<&'a str>,
    pub name: Option<&'a str>,
}

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

pub struct GitlabEntryArgs<'a> {
    pub entity_type: &'a str,
    pub owner_repo: &'a str,
    pub path: &'a str,
    pub ref_: Option<&'a str>,
    pub name: Option<&'a str>,
}

pub fn entry_from_gitlab(args: &GitlabEntryArgs<'_>) -> Entry {
    let inferred = infer_name(args.path);
    Entry {
        entity_type: EntityType::parse(args.entity_type).unwrap_or(EntityType::Skill),
        name: args.name.unwrap_or(&inferred).to_string(),
        source: SourceFields::Gitlab {
            owner_repo: args.owner_repo.to_string(),
            path_in_repo: args.path.to_string(),
            ref_: args.ref_.unwrap_or(DEFAULT_REF).to_string(),
        },
    }
}

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
    use skillfile_sources::resolver::{list_repo_skill_entries_under_query, RepoEntryQuery};

    // Validate repo exists before spending time on Tree API discovery.
    validate_github_repo(args.owner_repo)?;

    let spinner = Spinner::new(&format!(
        "Discovering {}s in {}...",
        args.entity_type, args.owner_repo
    ));
    let client = skillfile_sources::http::UreqClient::new();
    let entries = list_repo_skill_entries_under_query(
        &client,
        &RepoEntryQuery {
            owner_repo: args.owner_repo,
            base_path: args.base_path,
            ref_: args.ref_,
        },
    );
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

#[derive(Default)]
struct InstallReport {
    installed: Vec<String>,
    skipped: Vec<String>,
}

impl InstallReport {
    fn print(&self) {
        if self.installed.is_empty() {
            println!("No configured platforms were updated.");
        } else {
            println!("Installed to: {}", self.installed.join(", "));
        }
        if !self.skipped.is_empty() {
            println!("Skipped: {}", self.skipped.join(", "));
        }
    }
}

fn skip_reason_text(reason: InstallSkipReason, entity_type: EntityType) -> String {
    match reason {
        InstallSkipReason::UnknownAdapter => "unknown platform".to_string(),
        InstallSkipReason::UnsupportedEntity => format!("unsupported {entity_type}"),
        InstallSkipReason::MissingSource => "source missing".to_string(),
        InstallSkipReason::NothingDeployed => "nothing updated".to_string(),
        InstallSkipReason::DryRun => "dry-run".to_string(),
    }
}

fn record_io_result(errors: &mut Vec<String>, label: &str, result: std::io::Result<()>) {
    if let Err(error) = result {
        errors.push(format!("{label}: {error}"));
    }
}

struct RollbackState {
    manifest_path: PathBuf,
    original_manifest: String,
    lock_path: PathBuf,
    original_lock: Option<String>,
    cache_dir: PathBuf,
    install_snapshot: InstallSnapshot,
}

impl RollbackState {
    fn capture_install_snapshot(
        &mut self,
        ctx: &InstallSnapshotCapture<'_>,
    ) -> Result<(), SkillfileError> {
        self.install_snapshot =
            capture_install_snapshot(ctx.entry, &ctx.manifest.install_targets, ctx.repo_root)?;
        Ok(())
    }

    fn rollback(&self, entry_name: &str) -> Result<(), SkillfileError> {
        let mut errors = Vec::new();

        if let Err(error) = self.install_snapshot.restore() {
            errors.push(format!("restore installed files: {error}"));
        }
        record_io_result(
            &mut errors,
            &format!("restore {MANIFEST_NAME}"),
            std::fs::write(&self.manifest_path, &self.original_manifest),
        );
        match &self.original_lock {
            None if !self.lock_path.exists() => {}
            None => record_io_result(
                &mut errors,
                "remove Skillfile.lock",
                std::fs::remove_file(&self.lock_path),
            ),
            Some(text) => record_io_result(
                &mut errors,
                "restore Skillfile.lock",
                std::fs::write(&self.lock_path, text),
            ),
        }
        if self.cache_dir.exists() {
            record_io_result(
                &mut errors,
                "remove cache dir",
                std::fs::remove_dir_all(&self.cache_dir),
            );
        }

        if !errors.is_empty() {
            return Err(SkillfileError::Install(errors.join("; ")));
        }
        eprintln!("Rolled back: removed '{entry_name}' from {MANIFEST_NAME}");
        Ok(())
    }
}

fn finish_add_install(
    entry_name: &str,
    rollback: &RollbackState,
    result: Result<InstallReport, SkillfileError>,
) -> Result<InstallReport, SkillfileError> {
    match result {
        Ok(report) => Ok(report),
        Err(original_error) => {
            if let Err(rollback_error) = rollback.rollback(entry_name) {
                return Err(SkillfileError::Install(format!(
                    "failed to install '{entry_name}' and rollback also failed: {rollback_error}; \
                     repository may need manual cleanup (original error: {original_error})"
                )));
            }
            Err(original_error)
        }
    }
}

fn append_and_format_entry(entry: &Entry, manifest_path: &Path) -> Result<String, SkillfileError> {
    let line = format_line(entry);
    let original = std::fs::read_to_string(manifest_path)?;
    let mut content = original.clone();
    content.push_str(&line);
    content.push('\n');
    std::fs::write(manifest_path, &content)?;
    let result = parse_manifest(manifest_path)?;
    let formatted = sorted_manifest_text(&result.manifest, &content);
    std::fs::write(manifest_path, &formatted)?;
    Ok(original)
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
    let original_manifest = append_and_format_entry(entry, &manifest_path)?;

    let result = parse_manifest(&manifest_path)?;
    let mut manifest = result.manifest;
    crate::config::resolve_targets_into(&mut manifest);
    if manifest.install_targets.is_empty() {
        println!("Added: {line}");
        println!("No install targets configured yet. Run `skillfile init` to pick platforms.");
        return Ok(());
    }

    let lock_path = repo_root.join("Skillfile.lock");
    let mut rb = RollbackState {
        manifest_path: manifest_path.clone(),
        original_manifest,
        original_lock: lock_path
            .exists()
            .then(|| std::fs::read_to_string(&lock_path))
            .transpose()?,
        lock_path,
        cache_dir: vendor_dir_for(entry, repo_root),
        install_snapshot: InstallSnapshot::default(),
    };
    let sync_result = {
        let mut install_ctx = AddInstallCtx {
            manifest: &manifest,
            rollback: &mut rb,
            repo_root,
        };
        sync_and_install(entry, &mut install_ctx)
    };
    let report = finish_add_install(&entry.name, &rb, sync_result)?;

    println!("Added: {line}");
    report.print();
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
            "gitlab",
            "GitLab project",
            "Browse and pick from any GitLab project",
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
        "gitlab" => wizard_gitlab(repo_root),
        "search" => wizard_search(repo_root),
        "local" => wizard_local(repo_root),
        "url" => wizard_url(repo_root),
        _ => unreachable!(),
    }
}

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

    let owner_repo_input: String = cliclack::input("GitHub repository (owner/repo)")
        .placeholder("e.g. anthropics/skills or nuxt/ui@v4")
        .validate(|v: &String| {
            if v.contains('/') && v.len() > 2 {
                Ok(())
            } else {
                Err("Expected format: owner/repo or owner/repo@ref")
            }
        })
        .interact()?;

    let (parsed_repo, parsed_ref) = parse_owner_repo_ref(&owner_repo_input);

    // Validate repo exists before asking more questions.
    let spinner = Spinner::new(&format!("Checking {parsed_repo}..."));
    let valid = validate_github_repo(&parsed_repo);
    spinner.finish();
    valid?;

    // Default to "skill" — agents are rare and structurally identical.
    // Users adding agents can use `skillfile add github agent ...` directly.
    let entity_type = "skill";

    // Discover top-level dirs for the path hint.
    let spinner = Spinner::new("Scanning repository...");
    let top_dirs = discover_top_level_dirs(&parsed_repo);
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
            owner_repo: &parsed_repo,
            base_path: &base_path,
            ref_: parsed_ref.as_deref(),
            no_interactive: false,
        },
        repo_root,
    )
}

/// GitLab wizard flow: project path -> entity type -> path -> ref -> add.
fn wizard_gitlab(repo_root: &Path) -> Result<(), SkillfileError> {
    let entity_type: &str = cliclack::select("What are you adding?")
        .item("skill", "Skill", "")
        .item("agent", "Agent", "")
        .interact()?;

    let owner_repo: String = cliclack::input("GitLab project path (e.g. group/project)")
        .placeholder("my-group/my-project")
        .interact()?;

    let path: String = cliclack::input("Path within the repo")
        .placeholder("skills/my-skill.md")
        .interact()?;

    let ref_input: String = cliclack::input("Branch, tag, or SHA")
        .placeholder("main")
        .default_input("main")
        .interact()?;
    let ref_ = if ref_input.is_empty() || ref_input == "main" {
        None
    } else {
        Some(ref_input.as_str())
    };

    let entry = entry_from_gitlab(&GitlabEntryArgs {
        entity_type,
        owner_repo: &owner_repo,
        path: &path,
        ref_,
        name: None,
    });

    cmd_add(&entry, repo_root)
}

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

    // --- append_and_format_entry tests ---

    #[test]
    fn append_and_format_entry_returns_original() {
        let dir = tempfile::tempdir().unwrap();
        let initial = "local  skill  skills/existing.md\n";
        write_manifest(dir.path(), initial);
        let entry = entry_from_local("skill", "skills/new.md", None);
        let original = append_and_format_entry(&entry, &dir.path().join(MANIFEST_NAME)).unwrap();
        assert_eq!(
            original, initial,
            "should return the pre-edit manifest text"
        );
        let updated = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(updated.contains("skills/new.md"));
        assert!(updated.contains("skills/existing.md"));
    }

    #[test]
    fn append_and_format_entry_empty_manifest() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let entry = entry_from_local("skill", "skills/foo.md", None);
        let original = append_and_format_entry(&entry, &dir.path().join(MANIFEST_NAME)).unwrap();
        assert_eq!(original, "");
        let updated = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(updated.contains("skills/foo.md"));
    }

    // --- RollbackState tests ---

    #[test]
    fn rollback_restores_manifest_and_removes_lock_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join(MANIFEST_NAME);
        let lock_path = dir.path().join("Skillfile.lock");
        let cache_dir = dir.path().join(".skillfile/cache/skills/foo");
        let original = "local  skill  skills/foo.md\n";
        std::fs::write(&manifest_path, "corrupted content").unwrap();
        // No lock file pre-existed.
        std::fs::write(&lock_path, "some lock").unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(cache_dir.join("foo.md"), "cached").unwrap();
        let rb = RollbackState {
            manifest_path,
            original_manifest: original.to_string(),
            lock_path,
            original_lock: None,
            cache_dir: cache_dir.clone(),
            install_snapshot: InstallSnapshot::default(),
        };
        rb.rollback("foo").unwrap();
        let restored = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert_eq!(restored, original);
        assert!(
            !dir.path().join("Skillfile.lock").exists(),
            "lock should be removed when original was None"
        );
        assert!(
            !cache_dir.exists(),
            "cache dir should be removed on rollback"
        );
    }

    #[test]
    fn rollback_restores_manifest_and_lock_when_some() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join(MANIFEST_NAME);
        let lock_path = dir.path().join("Skillfile.lock");
        let cache_dir = dir.path().join(".skillfile/cache/skills/bar");
        let original_manifest = "local  skill  skills/bar.md\n";
        let original_lock = "lock content\n";
        std::fs::write(&manifest_path, "corrupted").unwrap();
        std::fs::write(&lock_path, "new lock").unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        let rb = RollbackState {
            manifest_path,
            original_manifest: original_manifest.to_string(),
            lock_path,
            original_lock: Some(original_lock.to_string()),
            cache_dir: cache_dir.clone(),
            install_snapshot: InstallSnapshot::default(),
        };
        rb.rollback("bar").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap(),
            original_manifest
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Skillfile.lock")).unwrap(),
            original_lock
        );
        assert!(
            !cache_dir.exists(),
            "cache dir should be removed on rollback"
        );
    }

    #[test]
    fn rollback_returns_error_when_manifest_restore_fails() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join(MANIFEST_NAME);
        std::fs::create_dir_all(&manifest_path).unwrap();
        let rb = RollbackState {
            manifest_path,
            original_manifest: "local  skill  skills/baz.md\n".to_string(),
            lock_path: dir.path().join("Skillfile.lock"),
            original_lock: None,
            cache_dir: dir.path().join(".skillfile/cache/skills/baz"),
            install_snapshot: InstallSnapshot::default(),
        };
        let result = rb.rollback("baz");
        assert!(matches!(result, Err(SkillfileError::Install(message)) if
            message.contains("restore Skillfile")));
    }

    // --- add_selected tests ---

    #[test]
    fn add_selected_single_entry() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let paths = vec!["skills/my-tool.md".to_string()];
        let args = BulkAddArgs {
            entity_type: "skill",
            owner_repo: "owner/repo",
            base_path: "skills",
            ref_: None,
            no_interactive: true,
        };
        add_selected(&paths, &args, dir.path());
        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("owner/repo"));
        assert!(text.contains("skills/my-tool.md"));
    }

    #[test]
    fn add_selected_multiple_entries() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let paths = vec!["skills/alpha.md".to_string(), "skills/beta.md".to_string()];
        let args = BulkAddArgs {
            entity_type: "skill",
            owner_repo: "owner/repo",
            base_path: "skills",
            ref_: None,
            no_interactive: true,
        };
        add_selected(&paths, &args, dir.path());
        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("skills/alpha.md"));
        assert!(text.contains("skills/beta.md"));
    }

    // --- entry_from_gitlab tests ---

    #[test]
    fn entry_from_gitlab_default_ref() {
        let entry = entry_from_gitlab(&GitlabEntryArgs {
            entity_type: "skill",
            owner_repo: "group/project",
            path: "skills/my-skill.md",
            ref_: None,
            name: None,
        });
        assert_eq!(entry.source_type(), "gitlab");
        assert_eq!(entry.name, "my-skill");
        let (or, pir, ref_) = entry.source.as_gitlab().unwrap();
        assert_eq!(or, "group/project");
        assert_eq!(pir, "skills/my-skill.md");
        assert_eq!(ref_, DEFAULT_REF);
    }

    #[test]
    fn entry_from_gitlab_explicit_ref() {
        let entry = entry_from_gitlab(&GitlabEntryArgs {
            entity_type: "agent",
            owner_repo: "group/subgroup/project",
            path: "agents/reviewer.md",
            ref_: Some("v2.0"),
            name: None,
        });
        assert_eq!(entry.entity_type, EntityType::Agent);
        let (or, _, ref_) = entry.source.as_gitlab().unwrap();
        assert_eq!(or, "group/subgroup/project");
        assert_eq!(ref_, "v2.0");
    }

    #[test]
    fn entry_from_gitlab_explicit_name() {
        let entry = entry_from_gitlab(&GitlabEntryArgs {
            entity_type: "skill",
            owner_repo: "group/project",
            path: "skills/foo.md",
            ref_: None,
            name: Some("custom-name"),
        });
        assert_eq!(entry.name, "custom-name");
    }

    #[test]
    fn entry_from_gitlab_dot_path() {
        let entry = entry_from_gitlab(&GitlabEntryArgs {
            entity_type: "skill",
            owner_repo: "group/project",
            path: ".",
            ref_: None,
            name: None,
        });
        assert_eq!(entry.name, "content");
        let (_, pir, _) = entry.source.as_gitlab().unwrap();
        assert_eq!(pir, ".");
    }

    #[test]
    fn add_selected_skips_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-populate with "alpha"
        write_manifest(dir.path(), "github  skill  owner/repo  skills/alpha.md\n");
        let paths = vec![
            "skills/alpha.md".to_string(), // duplicate — will be skipped
            "skills/gamma.md".to_string(),
        ];
        let args = BulkAddArgs {
            entity_type: "skill",
            owner_repo: "owner/repo",
            base_path: "skills",
            ref_: None,
            no_interactive: true,
        };
        add_selected(&paths, &args, dir.path());
        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        // gamma should have been added, alpha should still be there (not duplicated)
        assert!(text.contains("skills/gamma.md"));
        let alpha_count = text.matches("skills/alpha.md").count();
        assert_eq!(alpha_count, 1, "alpha should appear exactly once");
    }
}
