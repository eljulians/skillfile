mod update_check;

use skillfile::commands;
use skillfile::config;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};
use skillfile_core::error::SkillfileError;
use skillfile_deploy::adapter::known_adapters;

/// Read entry names from the Skillfile in the current directory for shell completion.
fn complete_entry_names() -> Vec<CompletionCandidate> {
    let path = std::path::Path::new("Skillfile");
    let Ok(result) = skillfile_core::parser::parse_manifest(path) else {
        return Vec::new();
    };
    result
        .manifest
        .entries
        .iter()
        .map(|e| CompletionCandidate::new(&e.name))
        .collect()
}

/// Parse and validate entity type (must be "skill" or "agent").
fn parse_entity_type(s: &str) -> Result<String, String> {
    match s {
        "skill" | "agent" => Ok(s.to_string()),
        _ => Err(format!("invalid type '{s}': expected 'skill' or 'agent'")),
    }
}

fn cli_long_about() -> String {
    let supported = known_adapters().join(", ");
    format!(
        "\
Tool-agnostic AI skill & agent manager - the Brewfile for your AI tooling.

Declare skills and agents in a Skillfile, lock them to exact SHAs, and deploy
to any supported platform with a single command.

Supported platforms: {supported}.

Quick start:
  skillfile init                          # configure platforms
  skillfile add github skill owner/repo path/to/SKILL.md
  skillfile install                       # fetch + deploy"
    )
}

fn cli_command() -> clap::Command {
    Cli::command().long_about(cli_long_about())
}

fn completion_env_shell(shell: clap_complete::Shell) -> &'static str {
    match shell.to_string().as_str() {
        "bash" => "bash",
        "elvish" => "elvish",
        "fish" => "fish",
        "powershell" => "powershell",
        "zsh" => "zsh",
        other => panic!("unsupported completion shell: {other}"),
    }
}

fn completion_registration_output(
    completer: &Path,
    shell_name: &str,
) -> Result<Vec<u8>, SkillfileError> {
    let output = std::process::Command::new(completer)
        .env("COMPLETE", shell_name)
        .output()
        .map_err(|e| {
            SkillfileError::Install(format!("failed to generate shell completions: {e}"))
        })?;

    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(SkillfileError::Install(format!(
            "failed to generate shell completions: {stderr}"
        )))
    }
}

fn completion_binary_name() -> std::ffi::OsString {
    format!("skillfile{}", std::env::consts::EXE_SUFFIX).into()
}

fn completion_registration_completer_from(
    current_exe: PathBuf,
    binary_name: &std::ffi::OsStr,
) -> PathBuf {
    if current_exe
        .file_name()
        .is_some_and(|name| name == binary_name)
    {
        return current_exe;
    }

    let Some(profile_dir) = current_exe.parent().and_then(|p| p.parent()) else {
        return current_exe;
    };
    let candidate = profile_dir.join(binary_name);

    if candidate.exists() {
        candidate
    } else {
        current_exe
    }
}

fn completion_registration_completer() -> Result<PathBuf, SkillfileError> {
    let current_exe = std::env::current_exe()
        .map_err(|e| SkillfileError::Install(format!("failed to locate skillfile binary: {e}")))?;
    let binary_name = completion_binary_name();
    Ok(completion_registration_completer_from(
        current_exe,
        &binary_name,
    ))
}

fn write_completion_registration(shell: clap_complete::Shell) -> Result<(), SkillfileError> {
    let completer = completion_registration_completer()?;
    let output = completion_registration_output(&completer, completion_env_shell(shell))?;
    std::io::stdout()
        .write_all(&output)
        .map_err(SkillfileError::Io)
}

#[derive(Parser)]
#[command(
    name = "skillfile",
    about = "Tool-agnostic AI skill & agent manager",
    version,
    after_long_help = "\
ENVIRONMENT VARIABLES:
  SKILLFILE_QUIET            Suppress progress output (same as --quiet)
  GITHUB_TOKEN, GH_TOKEN    GitHub API token for SHA resolution and private repos
  MERGETOOL                  Merge tool for `skillfile resolve` (default: $EDITOR)
  EDITOR                     Fallback editor for `skillfile resolve`"
)]
struct Cli {
    /// Suppress progress output (or set SKILLFILE_QUIET=1)
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    // -- Setup (display_order 10-19) ------------------------------------------
    /// Configure install targets interactively
    #[command(display_order = 10)]
    #[command(long_about = "\
Configure which platforms and scopes to install for.

Writes `install` lines to your Skillfile (e.g. `install claude-code global`).
Run this once when setting up a new project.

Examples:
  skillfile init")]
    Init,

    /// Add an entry to the Skillfile
    #[command(display_order = 11)]
    #[command(long_about = "\
Add a skill or agent entry to the Skillfile. The entry is appended to the file
and automatically synced and installed if install targets are configured.

If the sync or install fails, the Skillfile and lock are rolled back.

Examples:
  skillfile add github skill owner/repo skills/SKILL.md
  skillfile add github agent owner/repo agents/reviewer.md v2.0 --name reviewer
  skillfile add local skill skills/git/commit.md
  skillfile add url agent https://example.com/agent.md --name my-agent")]
    Add {
        #[command(subcommand)]
        source: Option<AddSource>,
    },

    /// Remove an entry from the Skillfile
    #[command(display_order = 12)]
    #[command(long_about = "\
Remove a named entry from the Skillfile, its lock record, and its cached files.

Examples:
  skillfile remove browser
  skillfile remove code-refactorer")]
    Remove {
        /// Entry name to remove
        #[arg(add = ArgValueCandidates::new(complete_entry_names))]
        name: String,
    },

    // -- Workflow (display_order 20-29) ---------------------------------------
    /// Re-resolve all refs, update the lock, and redeploy
    #[command(display_order = 20)]
    #[command(long_about = "\
Re-resolve all entry refs, update Skillfile.lock to the latest SHAs, and
redeploy to all configured platforms. Equivalent to `install --update`.

Examples:
  skillfile upgrade
  skillfile upgrade --dry-run")]
    Upgrade {
        /// Show planned actions without fetching or installing
        #[arg(long)]
        dry_run: bool,
    },

    /// Fetch entries and deploy to platform directories
    #[command(display_order = 21)]
    #[command(long_about = "\
Fetch all entries into .skillfile/cache/ and deploy them to the directories
expected by each configured platform.

On a fresh clone, this reads Skillfile.lock and fetches the exact pinned
content. Patches from .skillfile/patches/ are applied after deployment.

Examples:
  skillfile install
  skillfile install --dry-run
  skillfile install --update      # re-resolve refs, update the lock")]
    Install {
        /// Show planned actions without fetching or installing
        #[arg(long)]
        dry_run: bool,
        /// Re-resolve all refs and update the lock
        #[arg(long)]
        update: bool,
    },

    /// Fetch entries into .skillfile/cache/ without deploying
    #[command(display_order = 22)]
    #[command(long_about = "\
Fetch community entries into .skillfile/cache/ and update Skillfile.lock,
but do not deploy to platform directories. Useful for reviewing changes
before deploying.

Examples:
  skillfile sync
  skillfile sync --dry-run
  skillfile sync --entry browser
  skillfile sync --update")]
    Sync {
        /// Show planned actions without fetching
        #[arg(long)]
        dry_run: bool,
        /// Sync only this named entry
        #[arg(long, value_name = "NAME")]
        entry: Option<String>,
        /// Re-resolve all refs and update the lock
        #[arg(long)]
        update: bool,
    },

    /// Show state of all entries
    #[command(display_order = 23)]
    #[command(long_about = "\
Show the state of every entry: locked, unlocked, pinned, or missing.

With --check-upstream, resolves the current upstream SHA for each entry
and shows whether an update is available.

Examples:
  skillfile status
  skillfile status --check-upstream")]
    Status {
        /// Check current upstream SHA (makes API calls)
        #[arg(long)]
        check_upstream: bool,
    },

    /// Show detailed information about a single entry
    #[command(display_order = 24)]
    #[command(long_about = "\
Display all known information about a single entry: source, lock state,
pin state, modified state, installed paths across all targets, and cache path.

No network calls — reads only local manifest, lock, patch, and cache state.

Examples:
  skillfile info browser
  skillfile info code-refactorer")]
    Info {
        /// Entry name to inspect
        #[arg(add = ArgValueCandidates::new(complete_entry_names))]
        name: String,
    },

    // -- Discovery (display_order 26-29) ----------------------------------------
    /// Search community registries and add skills interactively
    #[command(display_order = 26)]
    #[command(long_about = "\
Search community registries for skills and agents.

By default, queries agentskill.sh (110K+ skills, public) and skills.sh.
Use --registry to target a single registry. skillhub.club is included
automatically when SKILLHUB_API_KEY is set.

In interactive mode (the default when a terminal is attached), results
are shown in a navigable TUI with a preview pane. Selecting a result
walks you through adding it to your Skillfile via `skillfile add`.

Non-interactive output (--json, --no-interactive, or piped stdout)
prints a plain-text table or JSON without prompts.

Results are sorted by popularity (stars). The preview pane shows
description, owner, stars, security score, and source repo when
available. Security audit details are fetched on demand for
registries that support them.")]
    #[command(after_help = "\
Examples:
  skillfile search \"code review\"          Search across all registries
  skillfile search docker --limit 5        Limit to 5 results
  skillfile search linting --min-score 80  Only high-trust results
  skillfile search testing --json          Machine-readable output
  skillfile search docker --registry agentskill.sh
  skillfile search docker --no-interactive Plain text, no TUI")]
    Search {
        /// Search query
        query: String,
        /// Maximum number of results
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Minimum security score (0-100)
        #[arg(long, value_name = "SCORE")]
        min_score: Option<u8>,
        /// Output results as JSON instead of the interactive TUI
        #[arg(long)]
        json: bool,
        /// Search only this registry
        #[arg(long, value_name = "NAME", value_parser = clap::builder::PossibleValuesParser::new(skillfile_sources::registry::REGISTRY_NAMES))]
        registry: Option<String>,
        /// Print plain-text table instead of the interactive TUI
        #[arg(long)]
        no_interactive: bool,
    },

    #[cfg(debug_assertions)]
    #[command(name = "__search-tui-test", hide = true)]
    SearchTuiTest,

    #[cfg(debug_assertions)]
    #[command(name = "__github-auth-test", hide = true)]
    GithubAuthTest,

    #[cfg(debug_assertions)]
    #[command(name = "__search-path-resolution-test", hide = true)]
    SearchPathResolutionTest,

    // -- Validation (display_order 30-39) -------------------------------------
    /// Check the Skillfile for errors
    #[command(display_order = 30)]
    #[command(long_about = "\
Parse the Skillfile and report any errors: syntax issues, unknown platforms,
duplicate entry names, orphaned lock entries, and duplicate install targets.

Examples:
  skillfile validate")]
    Validate,

    /// Format and sort entries in the Skillfile into a standard order
    #[command(display_order = 31)]
    #[command(long_about = "\
Format and canonicalize the Skillfile in-place. Entries are ordered by source
type, then entity type, then name. Install lines come first.

Examples:
  skillfile format
  skillfile format --dry-run")]
    Format {
        /// Print formatted output without writing
        #[arg(long)]
        dry_run: bool,
    },

    // -- Customization (display_order 40-49) ----------------------------------
    /// Capture local edits so they survive upstream updates
    #[command(display_order = 40)]
    #[command(long_about = "\
Diff your installed copy against the cached upstream version and save the
result as a patch in .skillfile/patches/. Future `install` commands apply
your patch after fetching upstream content.

Examples:
  skillfile pin browser
  skillfile pin browser --dry-run")]
    Pin {
        /// Entry name to pin
        #[arg(add = ArgValueCandidates::new(complete_entry_names))]
        name: String,
        /// Show what would be pinned without writing
        #[arg(long)]
        dry_run: bool,
    },

    /// Discard pinned customisations and restore upstream
    #[command(display_order = 41)]
    #[command(long_about = "\
Remove the patch for an entry from .skillfile/patches/. The next `install`
will deploy the pure upstream version.

Examples:
  skillfile unpin browser")]
    Unpin {
        /// Entry name to unpin
        #[arg(add = ArgValueCandidates::new(complete_entry_names))]
        name: String,
    },

    /// Show local changes or upstream delta after a conflict
    #[command(display_order = 42)]
    #[command(long_about = "\
Show the diff between your installed copy and the cached upstream version.
During a conflict, shows the upstream delta that triggered it.

Examples:
  skillfile diff browser")]
    Diff {
        /// Entry name
        #[arg(add = ArgValueCandidates::new(complete_entry_names))]
        name: String,
    },

    /// Merge upstream changes with your customisations after a conflict
    #[command(display_order = 43)]
    #[command(long_about = "\
When `install --update` detects that upstream changed and you have a patch,
it writes a conflict. Use `resolve` to open a three-way merge in your
configured merge tool ($MERGETOOL or $EDITOR).

Use --abort to discard the conflict state without merging.

Examples:
  skillfile resolve browser
  skillfile resolve --abort")]
    Resolve {
        /// Entry name to resolve
        #[arg(add = ArgValueCandidates::new(complete_entry_names))]
        name: Option<String>,
        /// Clear pending conflict state without merging
        #[arg(long)]
        abort: bool,
    },

    // -- Shell completions (display_order 50) ---------------------------------
    /// Generate shell completions for bash, zsh, fish, or powershell
    #[command(display_order = 50)]
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand)]
enum AddSource {
    /// Add a GitHub-hosted entry (use trailing / to discover and bulk-add)
    Github {
        /// Entity type: skill or agent
        #[arg(value_name = "TYPE", value_parser = parse_entity_type)]
        entity_type: String,
        /// GitHub repository (e.g. owner/repo or owner/repo@ref)
        #[arg(value_name = "OWNER/REPO[@REF]")]
        owner_repo: String,
        /// Path within the repo (omit to discover all entries)
        #[arg(value_name = "PATH")]
        path: Option<String>,
        /// Branch, tag, or SHA (default: main)
        #[arg(value_name = "REF")]
        ref_: Option<String>,
        /// Override name (default: filename stem)
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Add all discovered entries without interactive selection
        #[arg(long)]
        no_interactive: bool,
    },
    /// Add a local file entry
    Local {
        /// Entity type: skill or agent
        #[arg(value_name = "TYPE", value_parser = parse_entity_type)]
        entity_type: String,
        /// Path to the .md file relative to repo root
        #[arg(value_name = "PATH")]
        path: String,
        /// Override name (default: filename stem)
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
    },
    /// Add a URL entry
    Url {
        /// Entity type: skill or agent
        #[arg(value_name = "TYPE", value_parser = parse_entity_type)]
        entity_type: String,
        /// Direct URL to the .md file
        #[arg(value_name = "URL")]
        url: String,
        /// Override name (default: filename stem)
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
    },
}

/// Returns `true` if `path` looks like a directory discovery request rather
/// than a single-file add. A path that doesn't end in `.md` is a directory.
fn is_discovery_path(path: &str) -> bool {
    path == "."
        || !std::path::Path::new(path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

/// Parse `owner/repo[@ref]` into `(owner_repo, ref_)`.
///
/// Supports:
/// - `owner/repo` → `("owner/repo", None)`
/// - `owner/repo@v4` → `("owner/repo", Some("v4"))`
/// - `owner/repo@main` → `("owner/repo", Some("main"))`
fn parse_owner_repo_ref(input: &str) -> (String, Option<String>) {
    match input.split_once('@') {
        Some((repo, ref_)) if !repo.is_empty() && !ref_.is_empty() => {
            (repo.to_string(), Some(ref_.to_string()))
        }
        _ => (input.to_string(), None),
    }
}

fn handle_add(source: AddSource, repo_root: &std::path::Path) -> Result<(), SkillfileError> {
    let entry = match source {
        AddSource::Github {
            entity_type,
            owner_repo,
            path,
            ref_,
            name: _,
            no_interactive,
        } if is_discovery_path(path.as_deref().unwrap_or(".")) => {
            let base_path = path.as_deref().unwrap_or(".");
            let (parsed_repo, parsed_ref) = parse_owner_repo_ref(&owner_repo);
            let effective_ref = ref_.or(parsed_ref);
            return commands::add::cmd_add_bulk(
                &commands::add::BulkAddArgs {
                    entity_type: &entity_type,
                    owner_repo: &parsed_repo,
                    base_path,
                    ref_: effective_ref.as_deref(),
                    no_interactive,
                },
                repo_root,
            );
        }
        AddSource::Github {
            entity_type,
            owner_repo,
            path,
            ref_,
            name,
            no_interactive: _,
        } => {
            let (parsed_repo, parsed_ref) = parse_owner_repo_ref(&owner_repo);
            let effective_ref = ref_.or(parsed_ref);
            commands::add::entry_from_github(&commands::add::GithubEntryArgs {
                entity_type: &entity_type,
                owner_repo: &parsed_repo,
                path: path.as_deref().unwrap_or("."),
                ref_: effective_ref.as_deref(),
                name: name.as_deref(),
            })
        }
        AddSource::Local {
            entity_type,
            path,
            name,
        } => commands::add::entry_from_local(&entity_type, &path, name.as_deref()),
        AddSource::Url {
            entity_type,
            url,
            name,
        } => commands::add::entry_from_url(&entity_type, &url, name.as_deref()),
    };
    commands::add::cmd_add(&entry, repo_root)
}

fn run_install(repo_root: &Path, dry_run: bool, update: bool) -> Result<(), SkillfileError> {
    let user_targets = config::read_user_targets();
    let extra = if user_targets.is_empty() {
        None
    } else {
        Some(user_targets.as_slice())
    };
    skillfile_deploy::install::cmd_install(
        repo_root,
        &skillfile_deploy::install::CmdInstallOpts {
            dry_run,
            update,
            extra_targets: extra,
        },
    )
}

fn run_content_commands(repo_root: &Path, cmd: Command) -> Result<(), SkillfileError> {
    match cmd {
        Command::Completions { shell } => write_completion_registration(shell),
        Command::Validate => commands::validate::cmd_validate(repo_root),
        Command::Info { name } => commands::info::cmd_info(&name, repo_root),
        Command::Format { dry_run } => commands::format::cmd_format(repo_root, dry_run),
        Command::Pin { name, dry_run } => commands::pin::cmd_pin(&name, repo_root, dry_run),
        Command::Unpin { name } => commands::pin::cmd_unpin(&name, repo_root),
        Command::Diff { name } => commands::diff::cmd_diff(&name, repo_root),
        Command::Resolve { name, abort } => {
            commands::resolve::cmd_resolve(name.as_deref(), abort, repo_root)
        }
        cmd => run_source_commands(repo_root, cmd),
    }
}

fn run_source_commands(repo_root: &Path, cmd: Command) -> Result<(), SkillfileError> {
    match cmd {
        Command::Sync {
            dry_run,
            entry,
            update,
        } => skillfile_sources::sync::cmd_sync(&skillfile_sources::sync::SyncCmdOpts {
            repo_root,
            dry_run,
            entry_filter: entry.as_deref(),
            update,
        }),
        Command::Status { check_upstream } => {
            commands::status::cmd_status(repo_root, check_upstream)
        }
        Command::Init => commands::init::cmd_init(repo_root),
        Command::Upgrade { dry_run } => run_install(repo_root, dry_run, true),
        Command::Install { dry_run, update } => run_install(repo_root, dry_run, update),
        Command::Add {
            source: Some(source),
        } => handle_add(source, repo_root),
        Command::Add { source: None } => commands::add::cmd_add_interactive(repo_root),
        Command::Remove { name } => commands::remove::cmd_remove(&name, repo_root),
        Command::Search {
            query,
            limit,
            min_score,
            json,
            registry,
            no_interactive,
        } => commands::search::cmd_search(&commands::search::SearchConfig {
            query: &query,
            limit,
            min_score,
            json,
            registry: registry.as_deref(),
            no_interactive,
            repo_root,
        }),
        #[cfg(debug_assertions)]
        Command::SearchTuiTest => run_search_tui_test(),
        #[cfg(debug_assertions)]
        Command::GithubAuthTest => run_github_auth_test(),
        #[cfg(debug_assertions)]
        Command::SearchPathResolutionTest => {
            commands::search::run_search_path_resolution_regression()
        }
        _ => Ok(()), // covered by run_content_commands
    }
}

#[cfg(debug_assertions)]
fn run_search_tui_test() -> Result<(), SkillfileError> {
    use skillfile_sources::registry::{RegistryId, SearchResult};

    let items = [SearchResult {
        name: "fixture-skill".to_string(),
        owner: "skillfile".to_string(),
        description: Some("Fixture result for interactive terminal smoke tests.".to_string()),
        security_score: Some(92),
        stars: Some(42),
        url: "https://example.com/fixture-skill".to_string(),
        registry: RegistryId::AgentskillSh,
        source_repo: Some("eljulians/skillfile".to_string()),
        source_path: Some("skills/fixture-skill/SKILL.md".to_string()),
    }];

    commands::search_tui::run_tui(&items, items.len())
        .map(|_| ())
        .map_err(|e| SkillfileError::Install(format!("TUI error: {e}")))
}

#[cfg(debug_assertions)]
fn run_github_auth_test() -> Result<(), SkillfileError> {
    if skillfile_sources::http::github_token()
        .for_url("https://api.github.com/user")
        .is_some()
    {
        println!("available");
        Ok(())
    } else {
        Err(SkillfileError::Install("github auth unavailable".into()))
    }
}

fn run() -> Result<(), SkillfileError> {
    // Inject config-file token before any command (and before the OnceLock is
    // populated by `github_token()`). This runs once; subsequent calls are no-ops.
    skillfile_sources::http::set_config_token(crate::config::read_config_token());

    let cli = match cli_command().try_get_matches() {
        Ok(matches) => Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit()),
        Err(e)
            if e.kind() == clap::error::ErrorKind::DisplayHelp
                || e.kind() == clap::error::ErrorKind::DisplayVersion =>
        {
            let _ = e.print();
            return Ok(());
        }
        Err(e) => {
            e.exit();
        }
    };
    let quiet = cli.quiet || std::env::var("SKILLFILE_QUIET").is_ok_and(|v| !v.is_empty());
    skillfile_core::output::set_quiet(quiet);
    let repo_root = PathBuf::from(".");
    run_content_commands(&repo_root, cli.command)
}

fn main() {
    // Handle dynamic shell completion requests before any other initialization.
    // Shells call the binary with COMPLETE=<shell> to get completions at runtime.
    clap_complete::CompleteEnv::with_factory(cli_command).complete();

    // Spawn background update check (non-blocking)
    let update_rx = update_check::should_check().then(update_check::spawn_check);

    let exit_code = match run() {
        Ok(()) => 0,
        Err(e) => {
            let msg = e.to_string();
            if !msg.is_empty() {
                eprintln!("error: {msg}");
            }
            1
        }
    };

    // Print update notice if the background check found a newer version.
    // Give the background thread a short window to finish if it hasn't yet.
    if let Some(rx) = update_rx {
        if let Ok(Some(notice)) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
            eprintln!("\n{notice}");
        }
    }

    if exit_code != 0 {
        process::exit(exit_code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;
    use std::path::Path;

    fn completions_non_empty(shell: clap_complete::Shell) {
        let mut buf = Vec::new();
        clap_complete::generate(shell, &mut cli_command(), "skillfile", &mut buf);
        assert!(
            !buf.is_empty(),
            "completions for {shell:?} should produce output"
        );
    }

    #[test]
    fn long_about_lists_dynamic_supported_platforms() {
        let long_about = cli_command().get_long_about().unwrap().to_string();
        assert!(long_about.contains("antigravity"));
        for name in known_adapters() {
            assert!(long_about.contains(name), "missing adapter: {name}");
        }
    }

    #[test]
    fn completions_bash() {
        completions_non_empty(clap_complete::Shell::Bash);
    }

    #[test]
    fn completions_zsh() {
        completions_non_empty(clap_complete::Shell::Zsh);
    }

    #[test]
    fn completions_fish() {
        completions_non_empty(clap_complete::Shell::Fish);
    }

    #[test]
    fn completions_powershell() {
        completions_non_empty(clap_complete::Shell::PowerShell);
    }

    #[test]
    fn completion_env_shell_names_match_clap() {
        for shell in clap_complete::Shell::value_variants() {
            assert_eq!(completion_env_shell(*shell), shell.to_string());
        }
    }

    #[test]
    fn completion_registration_output_reports_spawn_failure() {
        let err = completion_registration_output(Path::new("/definitely/missing-skillfile"), "zsh")
            .expect_err("missing completer should fail");
        let msg = err.to_string();
        assert!(msg.contains("failed to generate shell completions"));
    }

    #[test]
    fn completion_registration_completer_from_prefers_windows_cli_binary() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let profile_dir = dir.path().join("target/debug");
        let deps_dir = profile_dir.join("deps");
        std::fs::create_dir_all(&deps_dir).expect("deps dir should be created");

        let test_exe = deps_dir.join("skillfile-abc123.exe");
        std::fs::write(&test_exe, "").expect("test harness placeholder should be written");

        let cli_binary = profile_dir.join("skillfile.exe");
        std::fs::write(&cli_binary, "").expect("cli placeholder should be written");

        let resolved =
            completion_registration_completer_from(test_exe, std::ffi::OsStr::new("skillfile.exe"));

        assert_eq!(resolved, cli_binary);
    }

    #[test]
    fn completion_registration_completer_from_falls_back_when_cli_binary_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let profile_dir = dir.path().join("target/debug");
        let deps_dir = profile_dir.join("deps");
        std::fs::create_dir_all(&deps_dir).expect("deps dir should be created");

        let test_exe = deps_dir.join("skillfile-abc123.exe");
        std::fs::write(&test_exe, "").expect("test harness placeholder should be written");

        let resolved = completion_registration_completer_from(
            test_exe.clone(),
            std::ffi::OsStr::new("skillfile.exe"),
        );

        assert_eq!(resolved, test_exe);
    }
}
