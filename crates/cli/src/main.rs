mod update_check;

use skillfile::commands;
use skillfile::config;

use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};
use skillfile_core::error::SkillfileError;

/// Parse and validate entity type (must be "skill" or "agent").
fn parse_entity_type(s: &str) -> Result<String, String> {
    match s {
        "skill" | "agent" => Ok(s.to_string()),
        _ => Err(format!("invalid type '{s}': expected 'skill' or 'agent'")),
    }
}

#[derive(Parser)]
#[command(
    name = "skillfile",
    about = "Tool-agnostic AI skill & agent manager",
    long_about = "\
Tool-agnostic AI skill & agent manager - the Brewfile for your AI tooling.

Declare skills and agents in a Skillfile, lock them to exact SHAs, and deploy
to any supported platform with a single command.

Supported platforms: claude-code, codex, copilot, cursor, gemini-cli,
opencode, windsurf.

Quick start:
  skillfile init                          # configure platforms
  skillfile add github skill owner/repo path/to/SKILL.md
  skillfile install                       # fetch + deploy",
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
        source: AddSource,
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
        name: String,
    },

    // -- Workflow (display_order 20-29) ---------------------------------------
    /// Fetch entries and deploy to platform directories
    #[command(display_order = 20)]
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
    #[command(display_order = 21)]
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
    #[command(display_order = 22)]
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

    // -- Discovery (display_order 25-29) ----------------------------------------
    /// Search community registries and add skills interactively
    #[command(display_order = 25)]
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
        name: Option<String>,
        /// Clear pending conflict state without merging
        #[arg(long)]
        abort: bool,
    },
}

#[derive(Subcommand)]
enum AddSource {
    /// Add a GitHub-hosted entry
    Github {
        /// Entity type: skill or agent
        #[arg(value_name = "TYPE", value_parser = parse_entity_type)]
        entity_type: String,
        /// GitHub repository (e.g. owner/repo)
        #[arg(value_name = "OWNER/REPO")]
        owner_repo: String,
        /// Path to the .md file within the repo
        #[arg(value_name = "PATH")]
        path: String,
        /// Branch, tag, or SHA (default: main)
        #[arg(value_name = "REF")]
        ref_: Option<String>,
        /// Override name (default: filename stem)
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
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

fn handle_add(source: AddSource, repo_root: &std::path::Path) -> Result<(), SkillfileError> {
    let entry = match source {
        AddSource::Github {
            entity_type,
            owner_repo,
            path,
            ref_,
            name,
        } => commands::add::entry_from_github(
            &entity_type,
            &owner_repo,
            &path,
            ref_.as_deref(),
            name.as_deref(),
        ),
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
    commands::add::cmd_add(entry, repo_root)
}

fn run() -> Result<(), SkillfileError> {
    let cli = Cli::parse();
    let quiet = cli.quiet || std::env::var("SKILLFILE_QUIET").is_ok_and(|v| !v.is_empty());
    skillfile_core::output::set_quiet(quiet);
    let repo_root = PathBuf::from(".");

    match cli.command {
        Command::Sync {
            dry_run,
            entry,
            update,
        } => {
            skillfile_sources::sync::cmd_sync(&repo_root, dry_run, entry.as_deref(), update)?;
        }
        Command::Status { check_upstream } => {
            commands::status::cmd_status(&repo_root, check_upstream)?;
        }
        Command::Init => {
            commands::init::cmd_init(&repo_root)?;
        }
        Command::Install { dry_run, update } => {
            let user_targets = config::read_user_targets();
            let extra = if user_targets.is_empty() {
                None
            } else {
                Some(user_targets.as_slice())
            };
            skillfile_deploy::install::cmd_install(&repo_root, dry_run, update, extra)?;
        }
        Command::Add { source } => {
            handle_add(source, &repo_root)?;
        }
        Command::Remove { name } => {
            commands::remove::cmd_remove(&name, &repo_root)?;
        }
        Command::Search {
            query,
            limit,
            min_score,
            json,
            registry,
            no_interactive,
        } => {
            commands::search::cmd_search(&commands::search::SearchConfig {
                query: &query,
                limit,
                min_score,
                json,
                registry: registry.as_deref(),
                no_interactive,
                repo_root: &repo_root,
            })?;
        }
        Command::Validate => {
            commands::validate::cmd_validate(&repo_root)?;
        }
        Command::Format { dry_run } => {
            commands::format::cmd_format(&repo_root, dry_run)?;
        }
        Command::Pin { name, dry_run } => {
            commands::pin::cmd_pin(&name, &repo_root, dry_run)?;
        }
        Command::Unpin { name } => {
            commands::pin::cmd_unpin(&name, &repo_root)?;
        }
        Command::Diff { name } => {
            commands::diff::cmd_diff(&name, &repo_root)?;
        }
        Command::Resolve { name, abort } => {
            commands::resolve::cmd_resolve(name.as_deref(), abort, &repo_root)?;
        }
    }

    Ok(())
}

fn main() {
    // Spawn background update check (non-blocking)
    let update_rx = if update_check::should_check() {
        Some(update_check::spawn_check())
    } else {
        None
    };

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
