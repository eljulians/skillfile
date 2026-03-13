/// CLI tests: invoke the compiled `skillfile` binary against local-only
/// operations (no network, no GitHub token needed).
///
/// Run with: cargo test -p skillfile-functional-tests --test cli
use std::path::Path;

use predicates::prelude::*;
use skillfile_functional_tests::{sf, skillfile_cmd};

// ---------------------------------------------------------------------------
// Smoke tests (binary boots up)
// ---------------------------------------------------------------------------

#[test]
fn help_flag_exits_zero() {
    skillfile_cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Tool-agnostic AI skill & agent manager",
        ));
}

#[test]
fn version_flag_exits_zero() {
    skillfile_cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("skillfile"));
}

#[test]
fn no_args_exits_nonzero() {
    skillfile_cmd()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

#[test]
fn init_fails_without_tty() {
    let dir = tempfile::tempdir().unwrap();
    sf(dir.path())
        .arg("init")
        .write_stdin("") // force piped stdin so is_terminal() returns false
        .timeout(std::time::Duration::from_secs(5))
        .assert()
        .failure()
        .stderr(predicate::str::contains("interactive terminal"));
}

// ---------------------------------------------------------------------------
// validate, format
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
fn format_golden_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n\
         github  skill  zebra  owner/repo  skills/z.md\n\
         github  skill  alpha  owner/repo  skills/a.md\n",
    )
    .unwrap();

    sf(dir.path()).arg("format").assert().success();

    let text = std::fs::read_to_string(dir.path().join("Skillfile")).unwrap();
    let entry_lines: Vec<&str> = text.lines().filter(|l| l.starts_with("github")).collect();
    assert!(entry_lines[0].contains("alpha"), "alpha should be first");
    assert!(entry_lines[1].contains("zebra"), "zebra should be second");
}

// ---------------------------------------------------------------------------
// add, remove
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// install (local-only)
// ---------------------------------------------------------------------------

fn write_local_manifest(dir: &Path) {
    std::fs::write(
        dir.join("Skillfile"),
        "install  claude-code  local\n\
         local  skill  my-skill  skills/my-skill.md\n",
    )
    .unwrap();

    std::fs::create_dir_all(dir.join("skills")).unwrap();
    std::fs::write(dir.join("skills/my-skill.md"), "# My Skill\n").unwrap();
}

#[test]
fn first_run_shows_platform_hint() {
    let dir = tempfile::tempdir().unwrap();
    write_local_manifest(dir.path());

    // No .skillfile/cache yet → should show configured platforms and init hint.
    sf(dir.path())
        .arg("install")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "Configured platforms: claude-code (local)",
        ))
        .stderr(predicate::str::contains("skillfile init"));
}

#[test]
fn second_run_no_platform_hint() {
    let dir = tempfile::tempdir().unwrap();
    write_local_manifest(dir.path());

    // First install creates .skillfile/cache.
    sf(dir.path()).arg("install").assert().success();

    // Second install: cache exists → no platform hint.
    sf(dir.path())
        .arg("install")
        .assert()
        .success()
        .stderr(predicate::str::contains("Configured platforms:").not());
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
