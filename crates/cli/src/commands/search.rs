//! `skillfile search` command — search community registries for skills and agents.
//!
//! Queries one or more registries and displays matching skills/agents with name,
//! description, owner, security score, and a link to the skill page. In interactive
//! mode (default when a TTY is attached), results are presented as a navigable list
//! that allows selecting a skill to add to the Skillfile.

use std::io::{IsTerminal, Write};
use std::path::Path;

use skillfile_core::error::SkillfileError;
use skillfile_core::output::Spinner;
use skillfile_sources::http::UreqClient;
use skillfile_sources::registry::{
    fetch_agentskill_github_meta, scrape_github_meta_from_page, search_all, search_registry,
    RegistryId, SearchOptions, SearchResponse,
};
use skillfile_sources::resolver::list_repo_skill_entries;

use super::add::{cmd_add, entry_from_github, GithubEntryArgs};

/// CLI arguments for `skillfile search` grouped as a Parameter Object.
pub struct SearchConfig<'a> {
    pub query: &'a str,
    pub limit: usize,
    pub min_score: Option<u8>,
    pub json: bool,
    pub registry: Option<&'a str>,
    pub no_interactive: bool,
    pub repo_root: &'a Path,
}

/// Run the `skillfile search` command.
///
/// Queries registries for skills matching the query and presents results. In
/// interactive mode (TTY attached, not `--json`, not `--no-interactive`), shows
/// a navigable selection list that feeds into `skillfile add`. Otherwise, prints
/// a plain-text table or JSON.
///
/// # Errors
///
/// Returns `SkillfileError::Network` if registries are unreachable or
/// return unexpected data.
pub fn cmd_search(cfg: &SearchConfig<'_>) -> Result<(), SkillfileError> {
    let opts = SearchOptions {
        limit: cfg.limit,
        min_score: cfg.min_score,
    };

    let spinner = Spinner::new("Searching registries");
    let resp = if let Some(name) = cfg.registry {
        search_registry(name, cfg.query, &opts)
    } else {
        search_all(cfg.query, &opts)
    };
    spinner.finish();
    let resp = resp?;

    let mut out = std::io::stdout().lock();
    if cfg.json {
        print_json(&mut out, &resp)?;
    } else if !cfg.no_interactive && is_interactive_tty() && !resp.items.is_empty() {
        interactive_select(&resp, cfg.repo_root)?;
    } else {
        print_table(&mut out, &resp, cfg.registry);
    }
    Ok(())
}

/// Returns `true` when both stdin and stderr are connected to a terminal.
///
/// `inquire` reads from stdin and renders its UI to stderr via crossterm,
/// so both must be terminals for interactive mode to work.
fn is_interactive_tty() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

// ===========================================================================
// Interactive selection
// ===========================================================================

/// Resolve the GitHub source coordinates for a search result.
///
/// For `agentskill.sh` items that lack a `source_path`, fetches the detail
/// API to obtain the real `owner/repo` and path. For all other items the
/// coordinates are taken directly from the search result.
fn resolve_source_coords(
    item: &skillfile_sources::registry::SearchResult,
) -> (Option<String>, Option<String>) {
    if item.registry != RegistryId::AgentskillSh
        || item.source_repo.is_some()
        || item.source_path.is_some()
    {
        return (item.source_repo.clone(), item.source_path.clone());
    }
    // No GitHub coordinates from the search API. Extract the registry
    // slug from the URL and try the detail API to resolve the real
    // owner/repo and path.
    let slug = item
        .url
        .strip_prefix("https://agentskill.sh/@")
        .unwrap_or("");
    if slug.is_empty() {
        return (None, None);
    }
    let client = UreqClient::new();
    let spinner = Spinner::new("Resolving GitHub coordinates");
    let meta = fetch_agentskill_github_meta(&client, slug, &item.name);
    spinner.finish();
    if let Some(m) = meta {
        return (Some(m.source_repo), Some(m.source_path));
    }
    // Detail API couldn't find the slug. Fall back to scraping the skill
    // page for the GitHub repo URL and path.
    let spinner = Spinner::new("Fetching source from skill page");
    let meta = scrape_github_meta_from_page(&client, slug);
    spinner.finish();
    match meta {
        Some(m) => {
            let path = (!m.source_path.is_empty()).then_some(m.source_path);
            (Some(m.source_repo), path)
        }
        None => (None, None),
    }
}

/// Resolve the GitHub `owner/repo`. If already known, returns it. Otherwise
/// prompts the user (e.g. for skillhub.club results with no GitHub info).
fn resolve_owner_repo(source_repo: Option<&str>) -> Result<Option<String>, SkillfileError> {
    if let Some(repo) = source_repo {
        return Ok(Some(repo.to_string()));
    }
    println!("  Enter the GitHub repository for this skill.");
    prompt_result(
        inquire::Text::new("GitHub owner/repo:")
            .with_help_message("e.g. owner/repo — check the skill page for the source")
            .prompt(),
    )
}

/// Present search results in the ratatui TUI.
///
/// On selection, gathers the information needed to construct a `skillfile add`
/// command (entity type, GitHub coordinates) and delegates to [`cmd_add`].
fn interactive_select(resp: &SearchResponse, repo_root: &Path) -> Result<(), SkillfileError> {
    let selected_idx = super::search_tui::run_tui(&resp.items, resp.total)
        .map_err(|e| SkillfileError::Install(format!("TUI error: {e}")))?;

    let Some(idx) = selected_idx else {
        return Ok(());
    };

    let item = &resp.items[idx];
    let (source_repo, source_path) = resolve_source_coords(item);

    // Show selection context before follow-up prompts.
    println!();
    println!("  {}", item.url);
    if let Some(repo) = &source_repo {
        println!("  source: {repo}");
    }
    println!();

    // If an agentskill.sh slug couldn't be resolved to GitHub coordinates,
    // bail with actionable guidance instead of showing confusing prompts.
    if source_repo.is_none() && source_path.is_none() && item.registry == RegistryId::AgentskillSh {
        eprintln!(
            "  Could not resolve GitHub coordinates for this skill.\n  \
             Check the skill page for the source repository, then add manually:\n\n  \
             skillfile add github skill <owner/repo> <path>"
        );
        return Ok(());
    }

    let Some(entity_type) =
        prompt_result(inquire::Select::new("Entity type:", vec!["skill", "agent"]).prompt())?
    else {
        return Ok(());
    };

    let Some(owner_repo) = resolve_owner_repo(source_repo.as_deref())? else {
        return Ok(());
    };

    // Resolve the path-in-repo for the Skillfile entry.
    // If the registry gave us the exact GitHub path, derive the entry from it.
    // Otherwise, query the Tree API and match by name.
    let path = if let Some(gh_path) = &source_path {
        let entry_path = entry_path_from_github_path(gh_path);
        println!("  path: {entry_path}");
        entry_path
    } else {
        let Some(p) = resolve_skill_path(&owner_repo, &item.name)? else {
            return Ok(());
        };
        p
    };

    let entry = entry_from_github(&GithubEntryArgs {
        entity_type,
        owner_repo: &owner_repo,
        path: &path,
        ref_: None,
        name: None,
    });
    cmd_add(&entry, repo_root)
}

/// Convert a GitHub file path into a Skillfile entry path.
///
/// If the path points to a `SKILL.md` at root → `.`.
/// If it points to a `SKILL.md` in a directory → the directory (dir entry).
/// Otherwise → the file path as-is (single file entry).
fn entry_path_from_github_path(github_path: &str) -> String {
    let filename = github_path.rsplit('/').next().unwrap_or(github_path);
    if filename.eq_ignore_ascii_case("SKILL.md") {
        // It's a SKILL.md — the entry is the parent directory (or "." for root).
        match github_path.rfind('/') {
            Some(pos) => github_path[..pos].to_string(),
            None => ".".to_string(),
        }
    } else {
        github_path.to_string()
    }
}

/// Resolve the path to a skill file inside a GitHub repo.
///
/// Lists `.md` files via the Tree API and uses `skill_name` to narrow down
/// candidates. When a strong match is found (file stem matches the skill name),
/// auto-selects it. Otherwise presents a filtered pick list or falls back to
/// a text prompt.
fn resolve_skill_path(
    owner_repo: &str,
    skill_name: &str,
) -> Result<Option<String>, SkillfileError> {
    let client = UreqClient::new();
    let spinner = Spinner::new(&format!("Listing files in {owner_repo}"));
    let md_files = list_repo_skill_entries(&client, owner_repo);
    spinner.finish();

    if md_files.is_empty() {
        return prompt_result(
            inquire::Text::new("Path in repo:")
                .with_default(".")
                .with_help_message(&format!(
                    "path to .md file in {owner_repo} (use . for root)"
                ))
                .prompt(),
        );
    }

    if md_files.len() == 1 {
        println!("  file: {}", md_files[0]);
        return Ok(Some(md_files[0].clone()));
    }

    // Score entries against the skill name to narrow down candidates.
    let ranked = rank_by_name(&md_files, skill_name);

    // If the top match is exact, auto-select it.
    if let Some((path, score)) = ranked.first() {
        if *score == MatchScore::Exact {
            println!("  path: {path}");
            return Ok(Some(path.clone()));
        }
    }

    // Show only files that matched the name. If nothing matched, show all.
    let candidates: Vec<String> = ranked.iter().map(|(p, _)| p.clone()).collect();
    let list = if candidates.is_empty() {
        md_files
    } else {
        candidates
    };

    prompt_result(inquire::Select::new("Select file:", list).prompt())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MatchScore {
    Exact,
    Contains,
}

/// Rank entry paths by how well they match the skill name. Returns only
/// entries that match, sorted best-first.
///
/// Entry paths may be directories (`skills/kubernetes-specialist`),
/// single files (`agents/reviewer.md`), or `.` (root SKILL.md).
fn rank_by_name(entries: &[String], skill_name: &str) -> Vec<(String, MatchScore)> {
    let name_lower = skill_name.to_ascii_lowercase();
    let mut scored: Vec<(String, MatchScore)> = entries
        .iter()
        .filter_map(|path| {
            let path_lower = path.to_ascii_lowercase();
            // For "skills/kubernetes-specialist" → "kubernetes-specialist"
            // For "agents/reviewer.md" → "reviewer" (strip .md)
            let tail = path_lower.rsplit('/').next().unwrap_or(&path_lower);
            let key = tail.strip_suffix(".md").unwrap_or(tail);

            if key == name_lower {
                Some((path.clone(), MatchScore::Exact))
            } else if path_lower.contains(&name_lower) {
                Some((path.clone(), MatchScore::Contains))
            } else {
                None
            }
        })
        .collect();

    scored.sort_by_key(|(_, score)| *score);
    scored
}

/// Convert an `inquire` prompt result into `Ok(Some(value))` on success,
/// `Ok(None)` on user cancellation, or `Err` on I/O failure.
fn prompt_result<T>(result: Result<T, inquire::InquireError>) -> Result<Option<T>, SkillfileError> {
    match result {
        Ok(val) => Ok(Some(val)),
        Err(
            inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted,
        ) => Ok(None),
        Err(e) => Err(SkillfileError::Install(format!("prompt failed: {e}"))),
    }
}

// ===========================================================================
// Plain text output
// ===========================================================================

fn append_meta_field(meta: &mut String, text: &str) {
    if !meta.is_empty() {
        meta.push_str("  ");
    }
    meta.push_str(text);
}

fn build_meta_line(item: &skillfile_sources::registry::SearchResult) -> String {
    use std::fmt::Write;
    let mut meta = String::new();
    if !item.owner.is_empty() {
        let _ = write!(meta, "by {}", item.owner);
    }
    if let Some(stars) = item.stars {
        append_meta_field(&mut meta, &format!("{stars} stars"));
    }
    if let Some(score) = item.security_score {
        append_meta_field(&mut meta, &format!("score: {score}/100"));
    }
    meta
}

pub fn print_table(w: &mut dyn Write, resp: &SearchResponse, single_registry: Option<&str>) {
    if resp.items.is_empty() {
        let _ = writeln!(w, "No results found.");
        return;
    }

    for item in &resp.items {
        // Name line (include registry tag when showing multiple registries)
        let desc = item.description.as_deref().unwrap_or("");
        if single_registry.is_some() {
            let _ = writeln!(w, "  {:<24}{desc}", item.name);
        } else {
            let _ = writeln!(
                w,
                "  {:<24}{:<16}{desc}",
                item.name,
                format!("[{}]", item.registry),
            );
        }

        // Source line: owner + url
        let meta = build_meta_line(item);
        if !meta.is_empty() {
            let _ = writeln!(w, "  {:<24}{meta}", "");
        }

        // URL line
        let _ = writeln!(w, "  {:<24}{}", "", item.url);
        let _ = writeln!(w);
    }

    let n = resp.items.len();
    let total = resp.total;
    let word = if n == 1 { "result" } else { "results" };
    let source_label = match single_registry {
        Some(name) => format!("via {name}"),
        None => "across all registries".to_string(),
    };
    if total > n {
        let _ = writeln!(w, "{n} {word} shown ({total} total, {source_label})");
    } else {
        let _ = writeln!(w, "{n} {word} ({source_label})");
    }
}

pub fn print_json(w: &mut dyn Write, resp: &SearchResponse) -> Result<(), SkillfileError> {
    let json = serde_json::to_string_pretty(resp)
        .map_err(|e| SkillfileError::Install(format!("failed to serialize search results: {e}")))?;
    let _ = writeln!(w, "{json}");
    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use skillfile_sources::registry::SearchResult;

    #[test]
    fn prompt_result_ok_returns_some() {
        let result: Result<String, inquire::InquireError> = Ok("test".to_string());
        let value = prompt_result(result).unwrap();
        assert_eq!(value, Some("test".to_string()));
    }

    #[test]
    fn prompt_result_canceled_returns_none() {
        let result: Result<String, inquire::InquireError> =
            Err(inquire::InquireError::OperationCanceled);
        let value = prompt_result(result).unwrap();
        assert!(value.is_none());
    }

    #[test]
    fn prompt_result_interrupted_returns_none() {
        let result: Result<String, inquire::InquireError> =
            Err(inquire::InquireError::OperationInterrupted);
        let value = prompt_result(result).unwrap();
        assert!(value.is_none());
    }

    #[test]
    fn prompt_result_io_error_returns_err() {
        let io_err = std::io::Error::other("test error");
        let result: Result<String, inquire::InquireError> = Err(inquire::InquireError::IO(io_err));
        let err = prompt_result(result).unwrap_err();
        assert!(err.to_string().contains("prompt failed"));
    }

    // -----------------------------------------------------------------------
    // entry_path_from_github_path
    // -----------------------------------------------------------------------

    #[test]
    fn entry_path_root_skill_md() {
        assert_eq!(entry_path_from_github_path("SKILL.md"), ".");
    }

    #[test]
    fn entry_path_root_skill_md_case_insensitive() {
        assert_eq!(entry_path_from_github_path("skill.md"), ".");
        assert_eq!(entry_path_from_github_path("Skill.md"), ".");
    }

    #[test]
    fn entry_path_nested_skill_md_becomes_dir() {
        assert_eq!(
            entry_path_from_github_path("skills/kubernetes-specialist/SKILL.md"),
            "skills/kubernetes-specialist"
        );
    }

    #[test]
    fn entry_path_deeply_nested_skill_md() {
        assert_eq!(
            entry_path_from_github_path("skills/arnarsson/fzf-fuzzy-finder/SKILL.md"),
            "skills/arnarsson/fzf-fuzzy-finder"
        );
    }

    #[test]
    fn entry_path_regular_md_stays_as_is() {
        assert_eq!(
            entry_path_from_github_path("agents/code-reviewer.md"),
            "agents/code-reviewer.md"
        );
    }

    #[test]
    fn entry_path_non_skill_md_stays_as_is() {
        assert_eq!(
            entry_path_from_github_path("skills/docker/helper.md"),
            "skills/docker/helper.md"
        );
    }

    // -----------------------------------------------------------------------
    // rank_by_name — matches skill name against entry paths
    // -----------------------------------------------------------------------

    fn paths(strs: &[&str]) -> Vec<String> {
        strs.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn rank_exact_dir_entry() {
        // Directory entry: last segment matches skill name exactly.
        let entries = paths(&["skills/kubernetes-specialist", "skills/docker-helper"]);
        let ranked = rank_by_name(&entries, "kubernetes-specialist");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0, "skills/kubernetes-specialist");
        assert_eq!(ranked[0].1, MatchScore::Exact);
    }

    #[test]
    fn rank_exact_single_file() {
        // Single-file entry: stem (without .md) matches skill name.
        let entries = paths(&["skills/kubernetes-specialist.md", "skills/docker-helper.md"]);
        let ranked = rank_by_name(&entries, "kubernetes-specialist");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0, "skills/kubernetes-specialist.md");
        assert_eq!(ranked[0].1, MatchScore::Exact);
    }

    #[test]
    fn rank_exact_case_insensitive() {
        let entries = paths(&["skills/Kubernetes-Specialist", "skills/other"]);
        let ranked = rank_by_name(&entries, "kubernetes-specialist");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].1, MatchScore::Exact);
    }

    #[test]
    fn rank_contains_match() {
        let entries = paths(&["skills/advanced-kubernetes-specialist-v2", "skills/docker"]);
        let ranked = rank_by_name(&entries, "kubernetes-specialist");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0, "skills/advanced-kubernetes-specialist-v2");
        assert_eq!(ranked[0].1, MatchScore::Contains);
    }

    #[test]
    fn rank_exact_beats_contains() {
        let entries = paths(&[
            "skills/extra-kubernetes-specialist-stuff",
            "skills/kubernetes-specialist",
        ]);
        let ranked = rank_by_name(&entries, "kubernetes-specialist");
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].1, MatchScore::Exact);
        assert_eq!(ranked[0].0, "skills/kubernetes-specialist");
        assert_eq!(ranked[1].1, MatchScore::Contains);
    }

    #[test]
    fn rank_no_matches_returns_empty() {
        let entries = paths(&["skills/docker", "skills/python", "skills/rust.md"]);
        let ranked = rank_by_name(&entries, "kubernetes-specialist");
        assert!(ranked.is_empty());
    }

    #[test]
    fn rank_dot_entry_never_matches() {
        // "." is the root SKILL.md — should not match a specific name.
        let entries = paths(&["."]);
        let ranked = rank_by_name(&entries, "some-skill");
        assert!(ranked.is_empty());
    }

    #[test]
    fn rank_empty_entries_returns_empty() {
        let ranked = rank_by_name(&[], "anything");
        assert!(ranked.is_empty());
    }

    #[test]
    fn rank_contains_matches_parent_dir() {
        // Name appears in a parent dir segment.
        let entries = paths(&["kubernetes-specialist/references", "unrelated/thing.md"]);
        let ranked = rank_by_name(&entries, "kubernetes-specialist");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].1, MatchScore::Contains);
    }

    #[test]
    fn rank_multi_skill_repo_finds_right_one() {
        // Simulates a repo like jeffallan/claude-skills after collapse.
        let entries = paths(&[
            "skills/kubernetes-specialist",
            "skills/docker-helper",
            "skills/python-pro",
            "skills/code-reviewer.md",
        ]);
        let ranked = rank_by_name(&entries, "kubernetes-specialist");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0, "skills/kubernetes-specialist");
        assert_eq!(ranked[0].1, MatchScore::Exact);
    }

    // -----------------------------------------------------------------------
    // print_table / print_json — output formatting
    // -----------------------------------------------------------------------

    fn sample_response() -> SearchResponse {
        SearchResponse {
            total: 2,
            items: vec![
                SearchResult {
                    name: "code-reviewer".to_string(),
                    owner: "alice".to_string(),
                    description: Some("Review code changes".to_string()),
                    security_score: Some(92),
                    stars: Some(150),
                    url: "https://agentskill.sh/@alice/code-reviewer".to_string(),
                    registry: RegistryId::AgentskillSh,
                    source_repo: Some("alice/code-reviewer".to_string()),
                    source_path: None,
                },
                SearchResult {
                    name: "pr-review".to_string(),
                    owner: "bob".to_string(),
                    description: None,
                    security_score: None,
                    stars: None,
                    url: "https://agentskill.sh/@bob/pr-review".to_string(),
                    registry: RegistryId::AgentskillSh,
                    source_repo: Some("bob/pr-review".to_string()),
                    source_path: None,
                },
            ],
        }
    }

    fn multi_registry_response() -> SearchResponse {
        SearchResponse {
            total: 3,
            items: vec![
                SearchResult {
                    name: "code-reviewer".to_string(),
                    owner: "alice".to_string(),
                    description: Some("Review code changes".to_string()),
                    security_score: Some(92),
                    stars: Some(150),
                    url: "https://agentskill.sh/@alice/code-reviewer".to_string(),
                    registry: RegistryId::AgentskillSh,
                    source_repo: Some("alice/code-reviewer".to_string()),
                    source_path: None,
                },
                SearchResult {
                    name: "docker-helper".to_string(),
                    owner: "dockerfan".to_string(),
                    description: None,
                    security_score: None,
                    stars: Some(500),
                    url: "https://skills.sh/dockerfan/docker-helper/docker-helper".to_string(),
                    registry: RegistryId::SkillsSh,
                    source_repo: Some("dockerfan/docker-helper".to_string()),
                    source_path: None,
                },
                SearchResult {
                    name: "testing-pro".to_string(),
                    owner: "testmaster".to_string(),
                    description: Some("Advanced testing".to_string()),
                    security_score: Some(88),
                    stars: Some(75),
                    url: "https://www.skillhub.club/skills/testing-pro".to_string(),
                    registry: RegistryId::SkillhubClub,
                    source_repo: None,
                    source_path: None,
                },
            ],
        }
    }

    #[test]
    fn table_single_registry_shows_via_label() {
        let resp = sample_response();
        let mut buf = Vec::new();
        print_table(&mut buf, &resp, Some("agentskill.sh"));
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("via agentskill.sh"));
        assert!(out.contains("code-reviewer"));
        assert!(out.contains("Review code changes"));
        assert!(out.contains("by alice"));
        assert!(out.contains("150 stars"));
        assert!(out.contains("score: 92/100"));
    }

    #[test]
    fn table_single_registry_omits_registry_tag() {
        let resp = sample_response();
        let mut buf = Vec::new();
        print_table(&mut buf, &resp, Some("agentskill.sh"));
        let out = String::from_utf8(buf).unwrap();
        assert!(!out.contains("[agentskill.sh]"));
    }

    #[test]
    fn table_multi_registry_shows_tags_and_label() {
        let resp = multi_registry_response();
        let mut buf = Vec::new();
        print_table(&mut buf, &resp, None);
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("[agentskill.sh]"));
        assert!(out.contains("[skills.sh]"));
        assert!(out.contains("[skillhub.club]"));
        assert!(out.contains("across all registries"));
    }

    #[test]
    fn table_empty_results() {
        let resp = SearchResponse {
            total: 0,
            items: vec![],
        };
        let mut buf = Vec::new();
        print_table(&mut buf, &resp, None);
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("No results found."));
    }

    #[test]
    fn table_shows_total_when_more() {
        let resp = SearchResponse {
            total: 50,
            items: vec![SearchResult {
                name: "test".to_string(),
                owner: "owner".to_string(),
                description: Some("A test skill".to_string()),
                security_score: Some(80),
                stars: Some(10),
                url: "https://agentskill.sh/@owner/test".to_string(),
                registry: RegistryId::AgentskillSh,
                source_repo: None,
                source_path: None,
            }],
        };
        let mut buf = Vec::new();
        print_table(&mut buf, &resp, Some("agentskill.sh"));
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("1 result shown (50 total, via agentskill.sh)"));
    }

    #[test]
    fn table_result_without_optional_fields() {
        let resp = SearchResponse {
            total: 1,
            items: vec![SearchResult {
                name: "minimal".to_string(),
                owner: String::new(),
                description: None,
                security_score: None,
                stars: None,
                url: "https://agentskill.sh/@x/minimal".to_string(),
                registry: RegistryId::AgentskillSh,
                source_repo: None,
                source_path: None,
            }],
        };
        let mut buf = Vec::new();
        print_table(&mut buf, &resp, Some("agentskill.sh"));
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("minimal"));
        assert!(out.contains("agentskill.sh/@x/minimal"));
        assert!(!out.contains("by "));
        assert!(!out.contains("stars"));
        assert!(!out.contains("score:"));
    }

    #[test]
    fn json_outputs_valid_json_with_registry() {
        let resp = sample_response();
        let mut buf = Vec::new();
        print_json(&mut buf, &resp).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(parsed["items"].is_array());
        assert!(parsed["total"].is_number());
        for item in parsed["items"].as_array().unwrap() {
            assert!(item["registry"].is_string());
        }
    }

    #[test]
    fn json_empty() {
        let resp = SearchResponse {
            total: 0,
            items: vec![],
        };
        let mut buf = Vec::new();
        print_json(&mut buf, &resp).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["total"], 0);
        assert!(parsed["items"].as_array().unwrap().is_empty());
    }

    #[test]
    fn json_multi_registry_includes_all_tags() {
        let resp = multi_registry_response();
        let mut buf = Vec::new();
        print_json(&mut buf, &resp).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("\"registry\": \"agentskill.sh\""));
        assert!(out.contains("\"registry\": \"skills.sh\""));
        assert!(out.contains("\"registry\": \"skillhub.club\""));
    }
}
