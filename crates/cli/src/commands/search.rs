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
use skillfile_sources::http::{HttpClient, UreqClient};
use skillfile_sources::registry::{
    fetch_agentskill_github_meta, scrape_github_meta_from_page, search_all, search_registry,
    RegistryId, SearchOptions, SearchResponse,
};
use skillfile_sources::resolver::{fetch_github_file, try_list_repo_skill_entries, GithubFetch};

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
        let Some(p) = resolve_skill_path(&owner_repo, &item.name, entity_type)? else {
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
    entity_type: &str,
) -> Result<Option<String>, SkillfileError> {
    let client = UreqClient::new();
    let spinner = Spinner::new(&format!("Listing files in {owner_repo}"));
    let query = SearchPathQuery {
        owner_repo,
        skill_name,
        entity_type,
    };
    let discovery = discover_skill_path(&client, &query);
    spinner.finish();
    let (candidates, resolved) = discovery?;

    if let Some(path) = resolved {
        println!("  path: {path}");
        return Ok(Some(path));
    }

    if candidates.is_empty() {
        return prompt_result(
            inquire::Text::new("Path in repo:")
                .with_default(".")
                .with_help_message(&format!(
                    "path to .md file in {owner_repo} (use . for root)"
                ))
                .prompt(),
        );
    }

    if candidates.len() == 1 {
        println!("  file: {}", candidates[0]);
        return Ok(Some(candidates[0].clone()));
    }

    prompt_result(inquire::Select::new("Select file:", candidates).prompt())
}

struct SearchPathQuery<'a> {
    owner_repo: &'a str,
    skill_name: &'a str,
    entity_type: &'a str,
}

fn discover_skill_path(
    client: &dyn HttpClient,
    query: &SearchPathQuery<'_>,
) -> Result<(Vec<String>, Option<String>), SkillfileError> {
    let mut md_files = try_list_repo_skill_entries(client, query.owner_repo)?;

    if md_files.is_empty() {
        let (canonical_files, canonical_path) =
            try_canonical_resolution(client, query.owner_repo, query.skill_name)?;
        if let Some(path) = canonical_path {
            return Ok((Vec::new(), Some(path)));
        }
        if !canonical_files.is_empty() {
            md_files = canonical_files;
        } else if let Some(path) =
            probe_common_skill_paths(client, query.owner_repo, query.skill_name)
        {
            return Ok((Vec::new(), Some(path)));
        } else {
            return Ok((Vec::new(), None));
        }
    }

    if md_files.len() == 1 {
        return Ok((Vec::new(), Some(md_files[0].clone())));
    }

    let ranked = rank_by_name_for_entity(&md_files, query.skill_name, query.entity_type);
    if let Some((path, score)) = ranked.first() {
        if *score == MatchScore::Exact {
            return Ok((Vec::new(), Some(path.clone())));
        }
    }

    let candidates: Vec<String> = ranked.iter().map(|(p, _)| p.clone()).collect();
    let list = if candidates.is_empty() {
        md_files
    } else {
        candidates
    };

    Ok((list, None))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MatchScore {
    Exact,
    Contains,
}

fn rank_by_name_for_entity(
    entries: &[String],
    skill_name: &str,
    entity_type: &str,
) -> Vec<(String, MatchScore)> {
    let name_key = normalize_skill_key(skill_name);
    let mut scored: Vec<(String, MatchScore)> = entries
        .iter()
        .filter_map(|path| {
            let key = entry_name_key(path);
            let path_key = normalize_skill_key(path);

            if key == name_key {
                Some((path.clone(), MatchScore::Exact))
            } else if path_key.contains(&name_key) {
                Some((path.clone(), MatchScore::Contains))
            } else {
                None
            }
        })
        .collect();

    scored.sort_by_key(|(path, score)| (*score, path_preference(path, entity_type), path.clone()));
    scored
}

fn path_preference(path: &str, entity_type: &str) -> u8 {
    let preferred_prefix = match entity_type {
        "agent" => "agents/",
        _ => "skills/",
    };
    if path.starts_with(preferred_prefix) {
        0
    } else if path.starts_with('.') {
        2
    } else {
        1
    }
}

fn entry_name_key(path: &str) -> String {
    let tail = path.rsplit('/').next().unwrap_or(path);
    let key = tail.strip_suffix(".md").unwrap_or(tail);
    normalize_skill_key(key)
}

fn normalize_skill_key(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_sep = false;

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !out.is_empty() && !last_was_sep {
            out.push('-');
            last_was_sep = true;
        }
    }

    if out.ends_with('-') {
        out.pop();
    }

    out
}

fn probe_common_skill_paths(
    client: &dyn HttpClient,
    owner_repo: &str,
    skill_name: &str,
) -> Option<String> {
    let slug = normalize_skill_key(skill_name);
    if slug.is_empty() {
        return None;
    }

    for ref_ in ["main", "master"] {
        let gh = GithubFetch {
            client,
            owner_repo,
            ref_,
        };

        if let Some(candidate) = common_skill_file_candidates(&slug)
            .into_iter()
            .find(|candidate| fetch_github_file(&gh, candidate).is_ok())
        {
            return Some(entry_path_from_github_path(&candidate));
        }
    }

    None
}

fn common_skill_file_candidates(slug: &str) -> [String; 5] {
    [
        format!("skills/{slug}/SKILL.md"),
        format!("{slug}/SKILL.md"),
        format!("skills/{slug}.md"),
        format!("{slug}.md"),
        "SKILL.md".to_string(),
    ]
}

fn canonical_owner_repo(
    client: &dyn HttpClient,
    owner_repo: &str,
) -> Result<Option<String>, SkillfileError> {
    let url = format!("https://api.github.com/repos/{owner_repo}");
    let Some(text) = client.get_json(&url)? else {
        return Ok(None);
    };
    let data: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        SkillfileError::Network(format!("invalid repository response for {owner_repo}: {e}"))
    })?;
    Ok(data["full_name"].as_str().map(ToString::to_string))
}

fn try_canonical_resolution(
    client: &dyn HttpClient,
    owner_repo: &str,
    skill_name: &str,
) -> Result<(Vec<String>, Option<String>), SkillfileError> {
    let Some(canonical) = canonical_owner_repo(client, owner_repo)? else {
        return Ok((Vec::new(), None));
    };
    let md_files = try_list_repo_skill_entries(client, &canonical)?;
    let path = if md_files.is_empty() {
        probe_common_skill_paths(client, &canonical, skill_name)
    } else {
        None
    };
    Ok((md_files, path))
}

#[cfg(debug_assertions)]
pub fn run_search_path_resolution_regression() -> Result<(), SkillfileError> {
    let client = FakeSearchPathClient::new();
    let query = SearchPathQuery {
        owner_repo: "paramchoudhary/resumeskills",
        skill_name: "linkedin profile optimizer",
        entity_type: "skill",
    };
    let (_, resolved) = discover_skill_path(&client, &query)?;
    emit_regression_resolution(resolved.as_deref())
}

#[cfg(debug_assertions)]
fn emit_regression_resolution(resolved: Option<&str>) -> Result<(), SkillfileError> {
    match resolved {
        Some("skills/linkedin-profile-optimizer") => {
            println!("skills/linkedin-profile-optimizer");
            Ok(())
        }
        Some(other) => Err(SkillfileError::Install(format!(
            "resolved wrong path: {other}"
        ))),
        None => Err(SkillfileError::Install(
            "failed to auto-resolve search path".into(),
        )),
    }
}

#[cfg(debug_assertions)]
struct FakeSearchPathClient {
    json: std::collections::HashMap<String, Option<String>>,
}

#[cfg(debug_assertions)]
impl FakeSearchPathClient {
    fn new() -> Self {
        Self {
            json: regression_fixture_json(),
        }
    }
}

#[cfg(debug_assertions)]
impl HttpClient for FakeSearchPathClient {
    fn get_bytes(&self, _url: &str) -> Result<Vec<u8>, SkillfileError> {
        Err(SkillfileError::Network("unexpected raw fetch".into()))
    }

    fn get_json(&self, url: &str) -> Result<Option<String>, SkillfileError> {
        Ok(self.json.get(url).cloned().flatten())
    }

    fn post_json(&self, _url: &str, _body: &str) -> Result<Vec<u8>, SkillfileError> {
        Err(SkillfileError::Network("unexpected post".into()))
    }
}

#[cfg(debug_assertions)]
fn regression_fixture_json() -> std::collections::HashMap<String, Option<String>> {
    use std::collections::HashMap;

    let mut json = HashMap::new();
    json.insert(
        "https://api.github.com/repos/paramchoudhary/resumeskills/git/trees/main?recursive=1"
            .to_string(),
        None,
    );
    json.insert(
        "https://api.github.com/repos/paramchoudhary/resumeskills/git/trees/master?recursive=1"
            .to_string(),
        None,
    );
    json.insert(
        "https://api.github.com/repos/paramchoudhary/resumeskills".to_string(),
        Some(
            serde_json::json!({
                "full_name": "Paramchoudhary/ResumeSkills"
            })
            .to_string(),
        ),
    );
    json.insert(
        "https://api.github.com/repos/Paramchoudhary/ResumeSkills/git/trees/main?recursive=1"
            .to_string(),
        Some(
            serde_json::json!({
                "tree": [
                    {"type": "blob", "path": ".agents/skills/linkedin-profile-optimizer/SKILL.md"},
                    {"type": "blob", "path": ".claude/skills/linkedin-profile-optimizer/SKILL.md"},
                    {"type": "blob", "path": "skills/linkedin-profile-optimizer/SKILL.md"}
                ]
            })
            .to_string(),
        ),
    );
    json
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
    use std::collections::HashMap;

    use skillfile_sources::registry::SearchResult;

    struct SearchPathClient {
        json: HashMap<String, Result<Option<String>, String>>,
        bytes: HashMap<String, Vec<u8>>,
    }

    impl SearchPathClient {
        fn new() -> Self {
            Self {
                json: HashMap::new(),
                bytes: HashMap::new(),
            }
        }

        fn with_json(mut self, url: &str, body: Option<serde_json::Value>) -> Self {
            self.json
                .insert(url.to_string(), Ok(body.map(|value| value.to_string())));
            self
        }

        fn with_bytes(mut self, url: &str, body: &[u8]) -> Self {
            self.bytes.insert(url.to_string(), body.to_vec());
            self
        }

        fn with_json_error(mut self, url: &str, message: &str) -> Self {
            self.json.insert(url.to_string(), Err(message.to_string()));
            self
        }
    }

    impl HttpClient for SearchPathClient {
        fn get_bytes(&self, url: &str) -> Result<Vec<u8>, SkillfileError> {
            match self.bytes.get(url) {
                Some(bytes) => Ok(bytes.clone()),
                None => Err(SkillfileError::Network(format!(
                    "unexpected raw fetch in test: {url}"
                ))),
            }
        }

        fn get_json(&self, url: &str) -> Result<Option<String>, SkillfileError> {
            self.json
                .get(url)
                .cloned()
                .unwrap_or(Ok(None))
                .map_err(SkillfileError::Network)
        }

        fn post_json(&self, _url: &str, _body: &str) -> Result<Vec<u8>, SkillfileError> {
            Err(SkillfileError::Network("unexpected post".into()))
        }
    }

    #[test]
    fn normalize_skill_key_collapses_separators() {
        assert_eq!(
            normalize_skill_key("LinkedIn Profile_Optimizer"),
            "linkedin-profile-optimizer"
        );
    }

    #[test]
    fn rank_by_name_matches_space_separated_skill_name_to_hyphenated_path() {
        let entries = vec![
            "skills/linkedin-profile-optimizer".to_string(),
            "skills/resume-tailor".to_string(),
        ];

        let ranked = rank_by_name_for_entity(&entries, "linkedin profile optimizer", "skill");

        assert_eq!(
            ranked.first(),
            Some(&(
                "skills/linkedin-profile-optimizer".to_string(),
                MatchScore::Exact,
            ))
        );
    }

    #[test]
    fn rank_by_name_prefers_skills_dir_over_hidden_mirrors_for_skill() {
        let entries = vec![
            ".agents/skills/linkedin-profile-optimizer".to_string(),
            "skills/linkedin-profile-optimizer".to_string(),
        ];

        let ranked = rank_by_name_for_entity(&entries, "linkedin profile optimizer", "skill");

        assert_eq!(
            ranked.first(),
            Some(&(
                "skills/linkedin-profile-optimizer".to_string(),
                MatchScore::Exact,
            ))
        );
    }

    #[test]
    fn rank_by_name_prefers_agents_dir_over_hidden_mirrors_for_agent() {
        let entries = vec![
            ".claude/agents/code-reviewer.md".to_string(),
            "agents/code-reviewer.md".to_string(),
        ];

        let ranked = rank_by_name_for_entity(&entries, "code reviewer", "agent");

        assert_eq!(
            ranked.first(),
            Some(&("agents/code-reviewer.md".to_string(), MatchScore::Exact))
        );
    }

    #[test]
    fn common_skill_file_candidates_cover_standard_layouts() {
        let candidates = common_skill_file_candidates("linkedin-profile-optimizer");

        assert_eq!(candidates[0], "skills/linkedin-profile-optimizer/SKILL.md");
        assert_eq!(candidates[4], "SKILL.md");
    }

    #[test]
    fn canonical_owner_repo_parses_full_name() {
        let client = SearchPathClient::new().with_json(
            "https://api.github.com/repos/paramchoudhary/resumeskills",
            Some(serde_json::json!({
                "full_name": "Paramchoudhary/ResumeSkills"
            })),
        );

        assert_eq!(
            canonical_owner_repo(&client, "paramchoudhary/resumeskills")
                .unwrap()
                .as_deref(),
            Some("Paramchoudhary/ResumeSkills")
        );
    }

    #[test]
    fn canonical_owner_repo_propagates_rate_limit_error() {
        let client = SearchPathClient::new().with_json_error(
            "https://api.github.com/repos/example/repo",
            "HTTP 403 fetching repository - you may be rate-limited",
        );

        let error = canonical_owner_repo(&client, "example/repo").unwrap_err();

        assert!(error.to_string().contains("rate-limited"), "{error}");
    }

    #[test]
    fn discover_skill_path_propagates_rate_limit_error() {
        let client = SearchPathClient::new().with_json_error(
            "https://api.github.com/repos/example/repo/git/trees/main?recursive=1",
            "HTTP 403 fetching tree - you may be rate-limited",
        );
        let query = SearchPathQuery {
            owner_repo: "example/repo",
            skill_name: "example",
            entity_type: "skill",
        };

        let error = discover_skill_path(&client, &query).unwrap_err();

        assert!(error.to_string().contains("rate-limited"), "{error}");
    }

    #[test]
    fn discover_skill_path_keeps_missing_repo_as_manual_fallback() {
        let client = SearchPathClient::new()
            .with_json(
                "https://api.github.com/repos/example/missing/git/trees/main?recursive=1",
                None,
            )
            .with_json(
                "https://api.github.com/repos/example/missing/git/trees/master?recursive=1",
                None,
            )
            .with_json("https://api.github.com/repos/example/missing", None);
        let query = SearchPathQuery {
            owner_repo: "example/missing",
            skill_name: "example",
            entity_type: "skill",
        };

        let (candidates, resolved) = discover_skill_path(&client, &query).unwrap();

        assert!(candidates.is_empty());
        assert!(resolved.is_none());
    }

    #[test]
    fn discover_skill_path_uses_canonical_repo_when_original_name_is_stale() {
        let client = SearchPathClient::new()
            .with_json(
                "https://api.github.com/repos/paramchoudhary/resumeskills/git/trees/main?recursive=1",
                None,
            )
            .with_json(
                "https://api.github.com/repos/paramchoudhary/resumeskills/git/trees/master?recursive=1",
                None,
            )
            .with_json(
                "https://api.github.com/repos/paramchoudhary/resumeskills",
                Some(serde_json::json!({
                    "full_name": "Paramchoudhary/ResumeSkills"
                })),
            )
            .with_json(
                "https://api.github.com/repos/Paramchoudhary/ResumeSkills/git/trees/main?recursive=1",
                Some(serde_json::json!({
                    "tree": [
                        {"type": "blob", "path": ".agents/skills/linkedin-profile-optimizer/SKILL.md"},
                        {"type": "blob", "path": "skills/linkedin-profile-optimizer/SKILL.md"}
                    ]
                })),
            );
        let query = SearchPathQuery {
            owner_repo: "paramchoudhary/resumeskills",
            skill_name: "linkedin profile optimizer",
            entity_type: "skill",
        };

        let (candidates, resolved) = discover_skill_path(&client, &query).unwrap();

        assert!(candidates.is_empty());
        assert_eq!(
            resolved.as_deref(),
            Some("skills/linkedin-profile-optimizer")
        );
    }

    #[test]
    fn discover_skill_path_probes_common_layout_when_tree_api_returns_empty() {
        let client = SearchPathClient::new()
            .with_json(
                "https://api.github.com/repos/paramchoudhary/resumeskills/git/trees/main?recursive=1",
                None,
            )
            .with_json(
                "https://api.github.com/repos/paramchoudhary/resumeskills/git/trees/master?recursive=1",
                None,
            )
            .with_json(
                "https://api.github.com/repos/paramchoudhary/resumeskills",
                Some(serde_json::json!({
                    "full_name": "Paramchoudhary/ResumeSkills"
                })),
            )
            .with_json(
                "https://api.github.com/repos/Paramchoudhary/ResumeSkills/git/trees/main?recursive=1",
                None,
            )
            .with_json(
                "https://api.github.com/repos/Paramchoudhary/ResumeSkills/git/trees/master?recursive=1",
                None,
            )
            .with_bytes(
                "https://raw.githubusercontent.com/paramchoudhary/resumeskills/main/skills/linkedin-profile-optimizer/SKILL.md",
                b"# skill",
            );
        let query = SearchPathQuery {
            owner_repo: "paramchoudhary/resumeskills",
            skill_name: "linkedin profile optimizer",
            entity_type: "skill",
        };

        let (candidates, resolved) = discover_skill_path(&client, &query).unwrap();

        assert!(candidates.is_empty());
        assert_eq!(
            resolved.as_deref(),
            Some("skills/linkedin-profile-optimizer")
        );
    }

    #[test]
    fn discover_skill_path_returns_filtered_candidates_without_exact_match() {
        let client = SearchPathClient::new().with_json(
            "https://api.github.com/repos/example/repo/git/trees/main?recursive=1",
            Some(serde_json::json!({
                "tree": [
                    {"type": "blob", "path": "skills/linkedin-profile-helper/SKILL.md"},
                    {"type": "blob", "path": "skills/linkedin-profile-writer/SKILL.md"},
                    {"type": "blob", "path": "skills/resume-tailor/SKILL.md"}
                ]
            })),
        );
        let query = SearchPathQuery {
            owner_repo: "example/repo",
            skill_name: "linkedin profile",
            entity_type: "skill",
        };

        let (candidates, resolved) = discover_skill_path(&client, &query).unwrap();

        assert!(resolved.is_none());
        assert_eq!(
            candidates,
            vec![
                "skills/linkedin-profile-helper".to_string(),
                "skills/linkedin-profile-writer".to_string(),
            ]
        );
    }

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
        let ranked = rank_by_name_for_entity(&entries, "kubernetes-specialist", "skill");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0, "skills/kubernetes-specialist");
        assert_eq!(ranked[0].1, MatchScore::Exact);
    }

    #[test]
    fn rank_exact_single_file() {
        // Single-file entry: stem (without .md) matches skill name.
        let entries = paths(&["skills/kubernetes-specialist.md", "skills/docker-helper.md"]);
        let ranked = rank_by_name_for_entity(&entries, "kubernetes-specialist", "skill");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0, "skills/kubernetes-specialist.md");
        assert_eq!(ranked[0].1, MatchScore::Exact);
    }

    #[test]
    fn rank_exact_case_insensitive() {
        let entries = paths(&["skills/Kubernetes-Specialist", "skills/other"]);
        let ranked = rank_by_name_for_entity(&entries, "kubernetes-specialist", "skill");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].1, MatchScore::Exact);
    }

    #[test]
    fn rank_contains_match() {
        let entries = paths(&["skills/advanced-kubernetes-specialist-v2", "skills/docker"]);
        let ranked = rank_by_name_for_entity(&entries, "kubernetes-specialist", "skill");
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
        let ranked = rank_by_name_for_entity(&entries, "kubernetes-specialist", "skill");
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].1, MatchScore::Exact);
        assert_eq!(ranked[0].0, "skills/kubernetes-specialist");
        assert_eq!(ranked[1].1, MatchScore::Contains);
    }

    #[test]
    fn rank_no_matches_returns_empty() {
        let entries = paths(&["skills/docker", "skills/python", "skills/rust.md"]);
        let ranked = rank_by_name_for_entity(&entries, "kubernetes-specialist", "skill");
        assert!(ranked.is_empty());
    }

    #[test]
    fn rank_dot_entry_never_matches() {
        // "." is the root SKILL.md — should not match a specific name.
        let entries = paths(&["."]);
        let ranked = rank_by_name_for_entity(&entries, "some-skill", "skill");
        assert!(ranked.is_empty());
    }

    #[test]
    fn rank_empty_entries_returns_empty() {
        let ranked = rank_by_name_for_entity(&[], "anything", "skill");
        assert!(ranked.is_empty());
    }

    #[test]
    fn rank_contains_matches_parent_dir() {
        // Name appears in a parent dir segment.
        let entries = paths(&["kubernetes-specialist/references", "unrelated/thing.md"]);
        let ranked = rank_by_name_for_entity(&entries, "kubernetes-specialist", "skill");
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
        let ranked = rank_by_name_for_entity(&entries, "kubernetes-specialist", "skill");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0, "skills/kubernetes-specialist");
        assert_eq!(ranked[0].1, MatchScore::Exact);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn emit_regression_resolution_errors_on_wrong_path() {
        let err = emit_regression_resolution(Some("skills/wrong")).unwrap_err();
        assert!(err.to_string().contains("resolved wrong path"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn emit_regression_resolution_errors_when_path_is_missing() {
        let err = emit_regression_resolution(None).unwrap_err();
        assert!(err.to_string().contains("failed to auto-resolve"));
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
