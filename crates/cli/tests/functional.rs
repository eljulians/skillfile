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
