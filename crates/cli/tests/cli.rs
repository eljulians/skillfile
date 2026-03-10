/// CLI integration tests: invoke the compiled `skillfile` binary against
/// local-only operations (no network, no GitHub token needed).
///
/// Run with: cargo test --test cli
use std::path::Path;

use assert_cmd::cargo_bin_cmd;
use predicates::prelude::*;

fn sf(dir: &Path) -> assert_cmd::Command {
    let mut cmd = cargo_bin_cmd!("skillfile");
    cmd.current_dir(dir);
    cmd
}

// ---------------------------------------------------------------------------
// Smoke tests (binary boots up)
// ---------------------------------------------------------------------------

#[test]
fn help_flag_exits_zero() {
    cargo_bin_cmd!("skillfile")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Tool-agnostic AI skill & agent manager",
        ));
}

#[test]
fn version_flag_exits_zero() {
    cargo_bin_cmd!("skillfile")
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("skillfile"));
}

#[test]
fn no_args_exits_nonzero() {
    cargo_bin_cmd!("skillfile")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

// ---------------------------------------------------------------------------
// Command tests (local operations, no network)
// ---------------------------------------------------------------------------

#[test]
fn validate_golden_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n\
         github  agent  code-refactorer  iannuttall/claude-agents  agents/code-refactorer.md\n\
         github  skill  requesting-code-review  obra/superpowers  skills/requesting-code-review\n",
    )
    .unwrap();

    sf(dir.path())
        .arg("validate")
        .assert()
        .success()
        .stderr(predicate::str::contains("error").not())
        .stdout(predicate::str::contains("error").not());
}

#[test]
fn sort_golden_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n\
         github  skill  zebra  owner/repo  skills/z.md\n\
         github  skill  alpha  owner/repo  skills/a.md\n",
    )
    .unwrap();

    sf(dir.path()).arg("sort").assert().success();

    let text = std::fs::read_to_string(dir.path().join("Skillfile")).unwrap();
    let entry_lines: Vec<&str> = text.lines().filter(|l| l.starts_with("github")).collect();
    assert!(entry_lines[0].contains("alpha"), "alpha should be first");
    assert!(entry_lines[1].contains("zebra"), "zebra should be second");
}

#[test]
fn add_then_remove() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Skillfile"), "# empty\n").unwrap();

    sf(dir.path())
        .args([
            "add",
            "github",
            "skill",
            "my-new-skill",
            "owner/repo",
            "skills/test.md",
        ])
        .assert()
        .success();

    let sf_text = std::fs::read_to_string(dir.path().join("Skillfile")).unwrap();
    assert!(
        sf_text.contains("my-new-skill"),
        "entry should be in Skillfile"
    );

    sf(dir.path())
        .args(["remove", "my-new-skill"])
        .assert()
        .success();

    let sf_text = std::fs::read_to_string(dir.path().join("Skillfile")).unwrap();
    assert!(!sf_text.contains("my-new-skill"), "entry should be removed");
}
