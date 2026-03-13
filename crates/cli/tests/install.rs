/// Integration tests for `skillfile install` (local-only, no network).
///
/// Run with: cargo test --test install
use std::path::Path;

use assert_cmd::cargo_bin_cmd;
use predicates::prelude::*;

fn sf(dir: &Path) -> assert_cmd::Command {
    let mut cmd = cargo_bin_cmd!("skillfile");
    cmd.current_dir(dir);
    cmd
}

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

// ---------------------------------------------------------------------------
// Clone flow — platform hint on first install
// ---------------------------------------------------------------------------

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
