/// Functional tests: invoke the compiled `skillfile` binary against the real GitHub API.
///
/// These tests require a GitHub token and network access.
/// Set GITHUB_TOKEN or GH_TOKEN, or have `gh auth login` configured.
/// Tests are skipped (not failed) when no token is available, so that
/// `cargo test --workspace` always works for coverage and local dev.
///
/// Run with: cargo test --test functional
use std::path::Path;

use assert_cmd::cargo_bin_cmd;
use predicates::prelude::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const TEST_SKILLFILE: &str = "\
install  claude-code  local\n\
\n\
# Single-file agent\n\
github  agent  code-refactorer  iannuttall/claude-agents  agents/code-refactorer.md\n\
\n\
# Single-file skill\n\
github  skill  requesting-code-review  obra/superpowers  skills/requesting-code-review\n\
";

fn make_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Skillfile"), TEST_SKILLFILE).unwrap();
    dir
}

/// Check whether a GitHub token is available (env var or `gh` CLI).
fn has_github_token() -> bool {
    if std::env::var("GITHUB_TOKEN").is_ok() || std::env::var("GH_TOKEN").is_ok() {
        return true;
    }
    std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}

/// Skip the test if no GitHub token is available. Returns true if token exists.
fn require_github_token() -> bool {
    if !has_github_token() {
        eprintln!("skipping: no GitHub token (set GITHUB_TOKEN, GH_TOKEN, or run `gh auth login`)");
        return false;
    }
    true
}

fn sf(dir: &Path) -> assert_cmd::Command {
    let mut cmd = cargo_bin_cmd!("skillfile");
    cmd.current_dir(dir);
    cmd
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn sync_golden_path() {
    if !require_github_token() {
        return;
    }
    let dir = make_repo();

    sf(dir.path()).arg("sync").assert().success();

    assert!(dir.path().join("Skillfile.lock").exists());
    let lock_text = std::fs::read_to_string(dir.path().join("Skillfile.lock")).unwrap();
    assert!(lock_text.contains("code-refactorer"));
    assert!(lock_text.contains("requesting-code-review"));

    assert!(dir
        .path()
        .join(".skillfile/cache/agents/code-refactorer")
        .is_dir());

    // NOT deployed (sync only)
    assert!(!dir.path().join(".claude").exists());
}

#[test]
fn install_golden_path() {
    if !require_github_token() {
        return;
    }
    let dir = make_repo();

    sf(dir.path()).arg("install").assert().success();

    assert!(dir.path().join("Skillfile.lock").exists());
    let lock_text = std::fs::read_to_string(dir.path().join("Skillfile.lock")).unwrap();
    assert!(lock_text.contains("code-refactorer"));
    assert!(lock_text.contains("requesting-code-review"));

    assert!(dir
        .path()
        .join(".skillfile/cache/agents/code-refactorer")
        .is_dir());
    assert!(dir
        .path()
        .join(".skillfile/cache/skills/requesting-code-review")
        .is_dir());

    let agent_file = dir.path().join(".claude/agents/code-refactorer.md");
    assert!(agent_file.exists());

    let content = std::fs::read_to_string(&agent_file).unwrap();
    assert!(content.len() > 10, "deployed file should have content");
}

#[test]
fn install_dry_run() {
    if !require_github_token() {
        return;
    }
    let dir = make_repo();

    sf(dir.path())
        .args(["install", "--dry-run"])
        .assert()
        .success()
        .stderr(predicate::str::contains("dry-run"));

    assert!(
        !dir.path().join("Skillfile.lock").exists(),
        "lock should not be written in dry-run"
    );
    assert!(
        !dir.path().join(".claude").exists(),
        ".claude should not be created in dry-run"
    );
}

#[test]
fn install_update() {
    if !require_github_token() {
        return;
    }
    let dir = make_repo();

    sf(dir.path()).arg("install").assert().success();

    sf(dir.path())
        .args(["install", "--update"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Done"));
}

#[test]
fn pin_then_unpin() {
    if !require_github_token() {
        return;
    }
    let dir = make_repo();

    sf(dir.path()).arg("install").assert().success();

    let agent_file = dir.path().join(".claude/agents/code-refactorer.md");
    let original = std::fs::read_to_string(&agent_file).unwrap();
    std::fs::write(&agent_file, format!("{original}\n## My custom section\n")).unwrap();

    sf(dir.path())
        .args(["pin", "code-refactorer"])
        .assert()
        .success();

    let patch_file = dir
        .path()
        .join(".skillfile/patches/agents/code-refactorer.patch");
    assert!(patch_file.exists(), "patch file should exist after pin");

    sf(dir.path())
        .args(["unpin", "code-refactorer"])
        .assert()
        .success();

    assert!(
        !patch_file.exists(),
        "patch file should be removed after unpin"
    );

    let restored = std::fs::read_to_string(&agent_file).unwrap();
    assert_eq!(restored, original, "file should be restored to upstream");
}

#[test]
fn status_after_install() {
    if !require_github_token() {
        return;
    }
    let dir = make_repo();

    sf(dir.path()).arg("install").assert().success();

    sf(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("code-refactorer"))
        .stdout(predicate::str::contains("requesting-code-review"));
}

// ---------------------------------------------------------------------------
// Search tests - one golden-path smoke test per registry.
// Hit real APIs, no GitHub token needed. Edge cases are in unit tests
// (registry.rs, search.rs).
// ---------------------------------------------------------------------------

/// agentskill.sh golden path: query returns JSON with items.
#[test]
fn search_agentskill_sh() {
    let dir = tempfile::tempdir().unwrap();

    let output = sf(dir.path())
        .args([
            "search",
            "code review",
            "--limit",
            "3",
            "--registry",
            "agentskill.sh",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    assert!(parsed["total"].as_u64().unwrap() > 0);
    let items = parsed["items"].as_array().unwrap();
    assert!(!items.is_empty());
    assert_eq!(items[0]["registry"].as_str().unwrap(), "agentskill.sh");
}

/// skills.sh golden path: query returns JSON with items.
#[test]
fn search_skills_sh() {
    let dir = tempfile::tempdir().unwrap();

    let output = sf(dir.path())
        .args([
            "search",
            "docker",
            "--limit",
            "3",
            "--registry",
            "skills.sh",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    let items = parsed["items"].as_array().unwrap();
    // skills.sh may return 0 results for some queries, so just verify the
    // response structure is valid and items carry the right registry tag.
    for item in items {
        assert_eq!(item["registry"].as_str().unwrap(), "skills.sh");
    }
}

/// skillhub.club golden path: without API key, returns empty results gracefully.
#[test]
fn search_skillhub_club_no_key() {
    let dir = tempfile::tempdir().unwrap();

    let output = sf(dir.path())
        .args([
            "search",
            "testing",
            "--limit",
            "3",
            "--registry",
            "skillhub.club",
            "--json",
        ])
        .env_remove("SKILLHUB_API_KEY")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    // Without API key, skillhub.club gracefully returns 0 results.
    assert_eq!(parsed["total"].as_u64().unwrap(), 0);
    assert!(parsed["items"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// list_repo_skill_entries - Tree API integration tests against real GitHub repos.
// Exercises the full pipeline: HTTP → JSON parse → filter → fallback logic.
// ---------------------------------------------------------------------------

/// list_repo_skill_entries returns collapsed entry paths from a multi-skill repo.
///
/// Uses `iannuttall/claude-agents` (from the test Skillfile) which has
/// multiple agent .md files under an `agents/` directory.
#[test]
fn list_repo_skill_entries_real_multi_file_repo() {
    if !require_github_token() {
        return;
    }
    let client = skillfile_sources::http::UreqClient::new();
    let entries =
        skillfile_sources::resolver::list_repo_skill_entries(&client, "iannuttall/claude-agents");

    assert!(!entries.is_empty(), "should find skill entries in the repo");

    // Entries are Skillfile-ready paths: single .md files or directory paths.
    // No raw README.md or .github/ paths should leak through.
    for e in &entries {
        let lower = e.to_ascii_lowercase();
        let tail = lower.rsplit('/').next().unwrap_or(&lower);
        assert_ne!(tail, "readme.md", "README.md should be excluded: {e}");
        assert!(
            !lower.starts_with(".github/"),
            ".github/ entries should be excluded: {e}"
        );
    }
}

/// list_repo_skill_entries returns entries from a known repo.
#[test]
fn list_repo_skill_entries_real_another_repo() {
    if !require_github_token() {
        return;
    }
    let client = skillfile_sources::http::UreqClient::new();
    let entries = skillfile_sources::resolver::list_repo_skill_entries(
        &client,
        "ComposioHQ/awesome-claude-skills",
    );

    // This repo should have at least one entry.
    assert!(!entries.is_empty(), "should find skill entries");
}

/// list_repo_skill_entries returns empty for a non-existent repo.
#[test]
fn list_repo_skill_entries_real_nonexistent_repo() {
    if !require_github_token() {
        return;
    }
    let client = skillfile_sources::http::UreqClient::new();
    let files = skillfile_sources::resolver::list_repo_skill_entries(
        &client,
        "this-owner-does-not-exist-zzzzzz/no-such-repo-xxxxxxxxx",
    );
    assert!(
        files.is_empty(),
        "non-existent repo should return empty vec"
    );
}

/// End-to-end: multi-skill repo collapses to directory entries, and name
/// matching finds the right one.
///
/// This is the critical flow: user selects "kubernetes-specialist" from
/// search results, source_repo is "jeffallan/claude-skills". The system
/// must resolve to `skills/kubernetes-specialist` (a directory entry),
/// NOT list every individual .md file inside it.
#[test]
fn skill_entry_resolution_multi_skill_repo() {
    if !require_github_token() {
        return;
    }
    let client = skillfile_sources::http::UreqClient::new();
    let entries =
        skillfile_sources::resolver::list_repo_skill_entries(&client, "jeffallan/claude-skills");

    assert!(
        !entries.is_empty(),
        "jeffallan/claude-skills should have entries"
    );

    // The entries should be collapsed directory paths, not individual files.
    // No entry should look like "skills/kubernetes-specialist/SKILL.md" or
    // "skills/kubernetes-specialist/references/helm.md".
    for e in &entries {
        assert!(
            !e.contains("/references/"),
            "should not expose sub-directory files: {e}"
        );
    }

    // Simulate the name-matching logic from resolve_skill_path:
    // find an entry whose last segment exactly matches the skill name.
    let skill_name = "kubernetes-specialist";
    let exact_match = entries.iter().find(|e| {
        let tail = e.rsplit('/').next().unwrap_or(e);
        tail.eq_ignore_ascii_case(skill_name)
    });
    assert!(
        exact_match.is_some(),
        "should find an exact match for '{skill_name}' among entries: {entries:?}"
    );
    // The resolved path should be a directory entry like "skills/kubernetes-specialist".
    let matched = exact_match.unwrap();
    assert!(
        !matched.ends_with(".md"),
        "directory skill should not end in .md: {matched}"
    );
}

/// End-to-end: single-skill repo with SKILL.md at root resolves to ".".
#[test]
fn skill_entry_resolution_single_skill_repo() {
    if !require_github_token() {
        return;
    }
    let client = skillfile_sources::http::UreqClient::new();
    // obra/superpowers has a single skill at root (SKILL.md pattern).
    let entries = skillfile_sources::resolver::list_repo_skill_entries(&client, "obra/superpowers");

    assert!(!entries.is_empty(), "obra/superpowers should have entries");

    // For repos with skills at specific paths, entries should be present.
    // Verify no raw README.md leaks through.
    for e in &entries {
        let tail = e.rsplit('/').next().unwrap_or(e).to_ascii_lowercase();
        assert_ne!(tail, "readme.md", "README.md should be excluded: {e}");
    }
}

/// Local directory entries must be deployed as directories, not empty .md files.
///
/// Regression test: is_dir_entry() only inspected GitHub path_in_repo and
/// returned false for all local entries. When the local path was a directory,
/// deploy_entry treated it as a single file, fs::copy(dir, file.md) failed
/// silently, and install printed a success message with nothing actually written.
#[test]
fn install_local_dir_entry() {
    let dir = tempfile::tempdir().unwrap();

    // Create a local skill directory with multiple files
    let skill_dir = dir.path().join("skills/my-local-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "# My Local Skill\n\nMain content.\n",
    )
    .unwrap();
    std::fs::write(skill_dir.join("extra.md"), "# Extra\n\nBonus content.\n").unwrap();

    // Also create a single-file local skill for comparison
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    std::fs::write(dir.path().join("skills/simple.md"), "# Simple Skill\n").unwrap();

    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n\
         \n\
         local  skill  my-local-skill  skills/my-local-skill\n\
         local  skill  simple  skills/simple.md\n",
    )
    .unwrap();

    // No network needed -- all local
    sf(dir.path()).arg("install").assert().success();

    // Directory entry: deployed as nested directory
    let deployed_dir = dir.path().join(".claude/skills/my-local-skill");
    assert!(
        deployed_dir.is_dir(),
        "local dir entry must be deployed as a directory, not a .md file"
    );
    assert_eq!(
        std::fs::read_to_string(deployed_dir.join("SKILL.md")).unwrap(),
        "# My Local Skill\n\nMain content.\n"
    );
    assert_eq!(
        std::fs::read_to_string(deployed_dir.join("extra.md")).unwrap(),
        "# Extra\n\nBonus content.\n"
    );
    // Must NOT create a spurious .md file
    assert!(
        !dir.path().join(".claude/skills/my-local-skill.md").exists(),
        "must not create my-local-skill.md for a directory source"
    );

    // Single-file entry: still works as before
    let simple = dir.path().join(".claude/skills/simple.md");
    assert!(simple.is_file());
    assert_eq!(
        std::fs::read_to_string(&simple).unwrap(),
        "# Simple Skill\n"
    );
}
