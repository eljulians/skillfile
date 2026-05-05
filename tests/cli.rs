/// CLI command tests: invoke the compiled `skillfile` binary against
/// local fixtures. No network, no GitHub token. Deterministic.
/// If a test here fails, we broke a command.
///
/// Run with: cargo test -p skillfile-functional-tests --test cli
use std::path::Path;

use predicates::prelude::*;
use skillfile_functional_tests::{sf, skillfile_cmd};

fn normalize_separators(text: &str) -> String {
    text.replace('\\', "/")
}

#[cfg(unix)]
fn write_fake_gh(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    let gh = dir.join("gh");
    std::fs::write(&gh, "#!/bin/sh\nexit 1\n").unwrap();
    let perms = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&gh, perms).unwrap();
}

#[cfg(windows)]
fn write_fake_gh(dir: &Path) {
    std::fs::write(dir.join("gh.cmd"), "@echo off\r\nexit /b 1\r\n").unwrap();
}

fn fake_gh_path(root: &Path) -> std::ffi::OsString {
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    write_fake_gh(&bin_dir);
    let current = std::env::var_os("PATH").unwrap_or_default();
    let paths = std::iter::once(bin_dir).chain(std::env::split_paths(&current));
    std::env::join_paths(paths).unwrap()
}

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
        .stdout(predicate::str::starts_with("skillfile "));
}

#[test]
fn completions_zsh_outputs_dynamic_registration() {
    skillfile_cmd()
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "_clap_dynamic_completer_skillfile",
        ))
        .stdout(predicate::str::contains("COMPLETE=\"zsh\""));
}

#[test]
fn complete_env_zsh_suggests_entry_names() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "local  skill  browser  skills/browser.md\n\
         local  agent  reviewer  agents/reviewer.md\n",
    )
    .unwrap();

    sf(dir.path())
        .args(["--", "skillfile", "remove"])
        .env("COMPLETE", "zsh")
        .env("_CLAP_COMPLETE_INDEX", "2")
        .assert()
        .success()
        .stdout(predicate::str::contains("browser"))
        .stdout(predicate::str::contains("reviewer"));
}

#[test]
fn no_args_exits_nonzero() {
    skillfile_cmd()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

#[test]
fn github_auth_test_detects_config_file_token() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "github_token = \"ghp_from_config\"\n").unwrap();

    sf(dir.path())
        .arg("__github-auth-test")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .env("SKILLFILE_CONFIG_PATH", &config_path)
        .env("PATH", fake_gh_path(dir.path()))
        .assert()
        .success()
        .stdout(predicate::str::contains("available"));
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

#[test]
fn init_fails_without_tty() {
    let dir = tempfile::tempdir().unwrap();
    sf(dir.path())
        .arg("init")
        .env("CI", "true")
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

    let output = sf(dir.path()).arg("validate").output().unwrap();
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    let stderr = std::str::from_utf8(&output.stderr).unwrap();

    assert!(output.status.success(), "validate should succeed: {stderr}");
    assert_eq!(stderr, "", "validate should not write to stderr");
    assert_eq!(
        stdout, "Skillfile OK — 2 entries, 1 install target\n",
        "unexpected validate stdout"
    );
    assert!(
        !stdout.contains("\u{1b}["),
        "captured validate stdout should stay plain text: {stdout:?}"
    );
}

#[test]
fn validate_junie_platform() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  junie  local\n\
         local  skill  test-skill  skills/test.md\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    std::fs::write(dir.path().join("skills/test.md"), "# Test\n").unwrap();

    let output = sf(dir.path()).arg("validate").assert();
    output.success().stdout(predicate::str::contains(
        "Skillfile OK — 1 entry, 1 install target",
    ));
}

#[test]
fn validate_error_output_is_plain_text_when_captured() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  unknown-platform  global\n",
    )
    .unwrap();

    let output = sf(dir.path()).arg("validate").output().unwrap();
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    let stderr = std::str::from_utf8(&output.stderr).unwrap();

    assert!(
        !output.status.success(),
        "validate should fail for an unknown platform"
    );
    assert_eq!(stdout, "", "error path should not write to stdout");
    assert!(
        stderr.contains("error: unknown platform: 'unknown-platform'"),
        "unexpected validate stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("\u{1b}["),
        "captured validate stderr should stay plain text: {stderr:?}"
    );
}

#[test]
fn validate_parse_warnings_fail_as_errors() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Skillfile"), "svn  skill  bad\n").unwrap();

    let output = sf(dir.path()).arg("validate").output().unwrap();
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    let stderr = std::str::from_utf8(&output.stderr).unwrap();

    assert!(
        !output.status.success(),
        "validate should fail for malformed manifest lines"
    );
    assert_eq!(stdout, "", "error path should not write to stdout");
    assert!(
        stderr.contains("error: line 1: unknown source type 'svn', skipping"),
        "unexpected validate stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("\u{1b}["),
        "captured validate stderr should stay plain text: {stderr:?}"
    );
}

#[test]
fn validate_duplicate_name_reported_once() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    std::fs::write(dir.path().join("skills/a.md"), "# A\n").unwrap();
    std::fs::write(dir.path().join("skills/b.md"), "# B\n").unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "local  skill  dup  skills/a.md\n\
         local  skill  dup  skills/b.md\n",
    )
    .unwrap();

    let output = sf(dir.path()).arg("validate").output().unwrap();
    let stderr = std::str::from_utf8(&output.stderr).unwrap();

    assert!(
        !output.status.success(),
        "validate should fail on duplicate names"
    );
    assert_eq!(
        stderr.matches("duplicate").count(),
        1,
        "duplicate name should be reported once, got:\n{stderr}"
    );
    assert!(
        stderr.contains("error: duplicate name 'dup'"),
        "unexpected validate stderr:\n{stderr}"
    );
}

#[test]
fn status_output_is_plain_text_when_captured() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n\
         local  skill  foo  skills/foo.md\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    std::fs::write(dir.path().join("skills/foo.md"), "# Foo\n").unwrap();

    let output = sf(dir.path()).arg("status").output().unwrap();
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    let stderr = std::str::from_utf8(&output.stderr).unwrap();

    assert!(output.status.success(), "status should succeed: {stderr}");
    assert_eq!(stderr, "", "status should not write to stderr");
    assert!(
        stdout.contains("foo   local"),
        "unexpected status stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("1 skill"),
        "status summary should include the skill count:\n{stdout}"
    );
    assert!(
        stdout.contains("Install targets: claude-code (local)"),
        "status summary should include the manifest install target:\n{stdout}"
    );
    assert!(
        !stdout.contains("\u{1b}["),
        "captured status stdout should stay plain text: {stdout:?}"
    );
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
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    std::fs::write(dir.path().join("skills/test.md"), "# Test Skill\n").unwrap();
    std::fs::write(dir.path().join("Skillfile"), "# empty\n").unwrap();

    sf(dir.path())
        .env(
            "SKILLFILE_CONFIG_PATH",
            dir.path().join("missing-config.toml"),
        )
        .args([
            "add",
            "local",
            "skill",
            "skills/test.md",
            "--name",
            "my-new-skill",
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

fn write_multi_target_skill_fixture(dir: &Path, name: &str) {
    let manifest = format!(
        "install  claude-code  local\n\
         install  copilot  local\n\
         github  skill  {name}  owner/repo  skills/{name}.md  main\n"
    );
    std::fs::write(dir.join("Skillfile"), manifest).unwrap();

    let lock_json = serde_json::json!({
        format!("github/skill/{name}"): {
            "sha": "abc123def456abc123def456abc123def456abc1",
            "raw_url": format!("https://raw.githubusercontent.com/owner/repo/abc123/skills/{name}.md")
        }
    });
    std::fs::write(
        dir.join("Skillfile.lock"),
        serde_json::to_string_pretty(&lock_json).unwrap(),
    )
    .unwrap();

    let vdir = dir.join(format!(".skillfile/cache/skills/{name}"));
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(
        vdir.join(format!("{name}.md")),
        "# Skill\n\nUpstream content.\n",
    )
    .unwrap();
    std::fs::write(
        vdir.join(".meta"),
        r#"{"sha":"abc123def456abc123def456abc123def456abc1"}"#,
    )
    .unwrap();

    for platform_dir in [".claude/skills", ".github/skills"] {
        let installed_dir = dir.join(platform_dir).join(name);
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(
            installed_dir.join("SKILL.md"),
            "# Skill\n\nUpstream content.\n",
        )
        .unwrap();
    }
}

struct RemoteSkillFixture<'a> {
    name: &'a str,
    installed_text: Option<&'a str>,
    patch_text: Option<&'a str>,
}

fn write_remote_skill_fixture(dir: &Path, fixture: &RemoteSkillFixture<'_>) {
    let RemoteSkillFixture {
        name,
        installed_text,
        patch_text,
    } = fixture;
    let manifest = format!(
        "install  claude-code  local\n\
         github  skill  {name}  owner/repo  skills/{name}.md  main\n"
    );
    std::fs::write(dir.join("Skillfile"), manifest).unwrap();

    let lock_json = serde_json::json!({
        format!("github/skill/{name}"): {
            "sha": "abc123def456abc123def456abc123def456abc1",
            "raw_url": format!("https://raw.githubusercontent.com/owner/repo/abc123/skills/{name}.md")
        }
    });
    std::fs::write(
        dir.join("Skillfile.lock"),
        serde_json::to_string_pretty(&lock_json).unwrap(),
    )
    .unwrap();

    let vdir = dir.join(format!(".skillfile/cache/skills/{name}"));
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(
        vdir.join(format!("{name}.md")),
        "# Skill\n\nUpstream content.\n",
    )
    .unwrap();
    std::fs::write(
        vdir.join(".meta"),
        r#"{"sha":"abc123def456abc123def456abc123def456abc1"}"#,
    )
    .unwrap();

    if let Some(text) = installed_text {
        let installed_dir = dir.join(".claude/skills").join(name);
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(installed_dir.join("SKILL.md"), text).unwrap();
    }

    if let Some(text) = patch_text {
        let patch_dir = dir.join(".skillfile/patches/skills");
        std::fs::create_dir_all(&patch_dir).unwrap();
        std::fs::write(patch_dir.join(format!("{name}.patch")), text).unwrap();
    }
}

fn write_info_lock_pin_cache_fixture(dir: &Path) {
    write_remote_skill_fixture(
        dir,
        &RemoteSkillFixture {
            name: "info-skill",
            installed_text: Some(
                "# Skill\n\nUpstream content.\n\n## Custom Section\n\nAdded by user.\n",
            ),
            patch_text: Some(
                "--- a/info-skill.md\n+++ b/info-skill.md\n@@ -1,3 +1,7 @@\n # Skill\n \n Upstream content.\n+\n+## Custom Section\n+\n+Added by user.\n",
            ),
        },
    );
}

fn output_line<'a>(stdout: &'a str, label: &str) -> &'a str {
    stdout
        .lines()
        .find(|line| line.contains(label))
        .unwrap_or_else(|| panic!("missing {label} line:\n{stdout}"))
}

fn assert_output_contains(stdout: &str, label: &str, value: &str) {
    let line = output_line(stdout, label);
    assert!(
        line.contains(value),
        "{label} line missing {value}:\n{stdout}"
    );
}

fn assert_info_lock_pin_cache_output(stdout: &str) {
    let stdout = normalize_separators(stdout);

    assert_output_contains(&stdout, "Name:", "info-skill");
    assert_output_contains(&stdout, "Type:", "skill");
    assert_output_contains(&stdout, "Source:", "github (owner/repo)");
    assert_output_contains(&stdout, "Path:", "skills/info-skill.md");
    assert_output_contains(&stdout, "Ref:", "main");
    assert_output_contains(&stdout, "Locked:", "abc123d");
    assert_output_contains(
        &stdout,
        "Pinned:",
        ".skillfile/patches/skills/info-skill.patch",
    );
    assert_output_contains(&stdout, "Installed:", ".claude/skills/info-skill/SKILL.md");
    assert_output_contains(
        &stdout,
        "Cache:",
        ".skillfile/cache/skills/info-skill/info-skill.md",
    );

    let installed_line = output_line(&stdout, "Installed:");
    assert!(
        !installed_line.contains("(not installed)"),
        "info output must show the installed path as present:\n{stdout}"
    );

    let modified_line = output_line(&stdout, "Modified:");
    assert!(
        modified_line.trim_end().ends_with("no"),
        "pinned fixture should report modified=no:\n{stdout}"
    );
}

#[test]
fn install_first_run_shows_platform_hint() {
    let dir = tempfile::tempdir().unwrap();
    write_local_manifest(dir.path());

    // Sanity check: cache must not exist yet.
    assert!(
        !dir.path().join(".skillfile/cache").exists(),
        "cache dir should not exist in fresh tempdir"
    );

    let output = sf(dir.path())
        .arg("install")
        .output()
        .expect("failed to execute");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "install should succeed: {stderr}");
    assert!(
        stderr.contains("Configured platforms: claude-code (local)"),
        "first install should show platform hint, got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("skillfile init"),
        "first install should suggest init, got stderr:\n{stderr}"
    );
}

#[test]
fn install_second_run_no_platform_hint() {
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

// ---------------------------------------------------------------------------
// add github bulk: CLI flag parsing
// ---------------------------------------------------------------------------

#[test]
fn add_github_bulk_no_interactive_flag_accepted() {
    // Verify the --no-interactive flag is parsed without error.
    // The actual discovery will fail (no network), but the flag should be accepted.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Skillfile"), "# empty\n").unwrap();

    let output = sf(dir.path())
        .args([
            "add",
            "github",
            "skill",
            "owner/repo",
            "skills/",
            "--no-interactive",
        ])
        .timeout(std::time::Duration::from_secs(10))
        .output()
        .expect("failed to execute");

    // The command will fail because there's no network/mock, but the flag
    // should be accepted (no "unrecognized option" error).
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--no-interactive should be accepted, got: {stderr}"
    );
}

#[test]
fn add_github_normal_path_no_bulk() {
    // A path NOT ending with / should route to normal add (not bulk discovery).
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Skillfile"), "# empty\n").unwrap();

    // With no install targets configured, direct add returns early after
    // appending the entry. Bulk discovery would fail before printing this.
    let output = sf(dir.path())
        .env(
            "SKILLFILE_CONFIG_PATH",
            dir.path().join("missing-config.toml"),
        )
        .args(["add", "github", "skill", "owner/repo", "skills/SKILL.md"])
        .timeout(std::time::Duration::from_secs(10))
        .output()
        .expect("failed to execute");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "normal add path should succeed without install targets: {stderr}"
    );
    assert!(
        stdout.contains("Added: github  skill  owner/repo  skills/SKILL.md"),
        "normal add path should print the added entry, got: {stdout}"
    );
    assert!(
        stdout.contains("No install targets configured yet"),
        "normal add path should take the direct add path, got: {stdout}"
    );
    assert!(
        !stderr.contains("no skills found under"),
        "normal add path must not route into bulk discovery, got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// add wizard: CLI routing
// ---------------------------------------------------------------------------

#[test]
fn add_wizard_without_tty_fails() {
    // `skillfile add` with no subcommand and no TTY should fail
    // with a message pointing the user to explicit subcommands.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Skillfile"), "# empty\n").unwrap();

    let output = sf(dir.path())
        .args(["add"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("failed to execute");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("interactive wizard requires a terminal")
            || stderr.contains("skillfile add github|local|url"),
        "bare `add` without TTY should give guidance, got: {stderr}"
    );
}

#[test]
fn add_local_subcommand_works() {
    // `skillfile add github ...` should still route to the explicit handler,
    // not the wizard. Regression check for the Option<AddSource> change.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Skillfile"), "# empty\n").unwrap();

    let output = sf(dir.path())
        .args(["add", "local", "skill", "skills/test.md"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("failed to execute");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Added:"),
        "explicit add local should still work, got: {stdout}"
    );
}

#[test]
fn add_prints_no_targets_message_when_none_configured() {
    // When no install targets are in the Skillfile, `add` should tell the user
    // what happened and what to do next.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Skillfile"), "# empty\n").unwrap();

    let output = sf(dir.path())
        .env(
            "SKILLFILE_CONFIG_PATH",
            dir.path().join("missing-config.toml"),
        )
        .args(["add", "local", "skill", "skills/test.md"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("failed to execute");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No install targets configured yet"),
        "should mention no install targets, got: {stdout}"
    );
    assert!(
        stdout.contains("skillfile init"),
        "should point to `skillfile init`, got: {stdout}"
    );
}

#[test]
fn add_uses_config_backed_install_targets() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    std::fs::write(dir.path().join("skills/test.md"), "# Test Skill\n").unwrap();
    std::fs::write(dir.path().join("Skillfile"), "# empty\n").unwrap();

    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        "[[install]]\nplatform = \"claude-code\"\nscope = \"local\"\n",
    )
    .unwrap();

    let output = sf(dir.path())
        .env("SKILLFILE_CONFIG_PATH", &config_path)
        .args(["add", "local", "skill", "skills/test.md"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("failed to execute");

    assert!(
        output.status.success(),
        "add should succeed with config-backed targets: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Installed to: claude-code (local)"),
        "config-backed targets should be used for install, got: {stdout}"
    );
    assert!(
        !stdout.contains("No install targets configured yet"),
        "config-backed targets must suppress the no-targets message, got: {stdout}"
    );
    assert!(
        dir.path().join(".claude/skills/test/SKILL.md").exists(),
        "add should install into the config-backed target"
    );

    let skillfile = std::fs::read_to_string(dir.path().join("Skillfile")).unwrap();
    assert!(
        skillfile.contains("local  skill  skills/test.md"),
        "add should still persist the entry in Skillfile"
    );
}

#[test]
fn add_reports_only_targets_that_were_updated() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("agents")).unwrap();
    std::fs::write(dir.path().join("agents/test.md"), "# Test Agent\n").unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n\
         install  codex  local\n",
    )
    .unwrap();

    let output = sf(dir.path())
        .args(["add", "local", "agent", "agents/test.md"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("failed to execute");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Installed to: claude-code (local)"),
        "should report the supported target, got: {stdout}"
    );
    assert!(
        stdout.contains("Skipped: codex (local) [unsupported agent]"),
        "should report skipped unsupported targets, got: {stdout}"
    );
    assert!(
        !stdout.contains("Installed to: claude-code (local), codex (local)"),
        "must not claim unsupported targets were installed, got: {stdout}"
    );
    assert!(
        !dir.path().join(".skillfile/tmp").exists(),
        "successful add should not leave repo-local transaction scratch dirs behind"
    );
}

#[test]
fn add_reports_when_no_configured_platforms_were_updated() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("agents")).unwrap();
    std::fs::write(dir.path().join("agents/test.md"), "# Test Agent\n").unwrap();
    std::fs::write(dir.path().join("Skillfile"), "install  codex  local\n").unwrap();

    let output = sf(dir.path())
        .args(["add", "local", "agent", "agents/test.md"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("failed to execute");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No configured platforms were updated."),
        "should report that all configured targets were skipped, got: {stdout}"
    );
    assert!(
        stdout.contains("Skipped: codex (local) [unsupported agent]"),
        "should explain why nothing was updated, got: {stdout}"
    );
    assert!(
        !stdout.contains("Installed to:"),
        "must not print an install-success summary when nothing was updated, got: {stdout}"
    );
}

#[test]
fn add_missing_source_does_not_claim_install_success() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n",
    )
    .unwrap();

    let output = sf(dir.path())
        .args(["add", "local", "skill", "skills/missing.md"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("failed to execute");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("No configured platforms were updated."),
        "should not report install success when the source is missing, got: {stdout}"
    );
    assert!(
        stdout.contains("Skipped: claude-code (local) [source missing]"),
        "should summarize the skipped target, got: {stdout}"
    );
    assert!(
        !stdout.contains("Installed to:"),
        "must not claim install success for a missing source, got: {stdout}"
    );
    assert!(
        stderr.contains("warning: source missing"),
        "missing source warning should still be emitted, got: {stderr}"
    );
}

#[test]
fn add_rolls_back_when_install_target_path_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    std::fs::write(dir.path().join("skills/test.md"), "# Test Skill\n").unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".claude"), "not a directory\n").unwrap();

    let output = sf(dir.path())
        .args(["add", "local", "skill", "skills/test.md"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("failed to execute");

    assert!(
        !output.status.success(),
        "command should fail when install target is blocked"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains("Added:"),
        "failed add must not print a success banner, got: {stdout}"
    );
    assert!(
        stderr.contains("Rolled back: removed 'test' from Skillfile"),
        "rollback message should be emitted, got: {stderr}"
    );
    assert!(
        stderr.contains("failed to install 'test' to claude-code (local)"),
        "install failure should be surfaced, got: {stderr}"
    );

    assert_eq!(
        std::fs::read_to_string(dir.path().join("Skillfile")).unwrap(),
        "install  claude-code  local\n"
    );
    assert!(
        !dir.path().join("Skillfile.lock").exists(),
        "lock file should be removed on rollback"
    );
    assert!(
        !dir.path().join(".skillfile/cache/skills/test").exists(),
        "cache dir should be removed on rollback"
    );
    assert!(
        !dir.path().join(".skillfile/tmp").exists(),
        "rollback should not leave repo-local transaction scratch dirs behind"
    );
}

#[test]
fn add_removes_earlier_platform_files_when_later_target_fails() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    std::fs::write(dir.path().join("skills/test.md"), "# Test Skill\n").unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n\
         install  cursor  local\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".cursor"), "not a directory\n").unwrap();

    let output = sf(dir.path())
        .args(["add", "local", "skill", "skills/test.md"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("failed to execute");

    assert!(
        !output.status.success(),
        "command should fail when a later target is blocked"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains("Added:"),
        "rolled-back add must not print a success banner, got: {stdout}"
    );
    assert!(
        stderr.contains("Rolled back: removed 'test' from Skillfile"),
        "rollback should still be announced, got: {stderr}"
    );
    assert!(
        !dir.path().join(".claude/skills/test/SKILL.md").exists(),
        "files written to earlier targets must be removed on rollback"
    );
    assert!(
        !dir.path().join(".skillfile/tmp").exists(),
        "rolled-back add should not leave repo-local transaction scratch dirs behind"
    );
}

#[test]
fn add_restores_legacy_flat_file_when_later_target_fails() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    std::fs::create_dir_all(dir.path().join(".claude/skills")).unwrap();
    std::fs::write(dir.path().join("skills/foo.md"), "# New Foo\n").unwrap();
    std::fs::write(dir.path().join(".claude/skills/foo.md"), "# Legacy Foo\n").unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n\
         install  copilot  local\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".github"), "not a directory\n").unwrap();

    let output = sf(dir.path())
        .args(["add", "local", "skill", "skills/foo.md", "--name", "foo"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("failed to execute");

    assert!(
        !output.status.success(),
        "command should fail when a later target is blocked"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Rolled back: removed 'foo' from Skillfile"),
        "rollback should be announced, got: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".claude/skills/foo.md")).unwrap(),
        "# Legacy Foo\n",
        "rollback must restore adapter migration side effects"
    );
    assert!(
        !dir.path().join(".claude/skills/foo/SKILL.md").exists(),
        "new nested install must be removed on rollback"
    );
}

#[test]
fn install_fails_when_nested_target_path_is_blocked_without_update() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    std::fs::write(dir.path().join("skills/test.md"), "# Test Skill\n").unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n\
         local  skill  test  skills/test.md\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".claude"), "not a directory\n").unwrap();

    let output = sf(dir.path())
        .arg("install")
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("failed to execute");

    assert!(
        !output.status.success(),
        "default install should fail when the target path is blocked"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to install 'test' to claude-code (local)"),
        "install failure should be surfaced, got: {stderr}"
    );
    assert!(
        !stdout.contains("Done."),
        "failed install must not report success, got: {stdout}"
    );
    assert!(
        !dir.path().join(".claude/skills/test/SKILL.md").exists(),
        "blocked install must not leave a deployed file behind"
    );
}

#[test]
fn install_cleans_up_partial_flat_write_failures() {
    let dir = tempfile::tempdir().unwrap();
    let agent_dir = dir.path().join("agents/core-dev");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("alpha.md"), "# Alpha\n").unwrap();
    std::fs::write(agent_dir.join("beta.md"), "# Beta\n").unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n\
         local  agent  core-dev  agents/core-dev\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join(".claude/agents/alpha.md")).unwrap();

    let output = sf(dir.path())
        .arg("install")
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("failed to execute");

    assert!(
        !output.status.success(),
        "install should fail when only part of a flat directory can be deployed"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to install 'core-dev' to claude-code (local)"),
        "partial flat failure should be surfaced, got: {stderr}"
    );
    assert!(
        dir.path().join(".claude/agents/alpha.md").is_dir(),
        "the pre-existing blocking path should remain untouched"
    );
    assert!(
        !dir.path().join(".claude/agents/beta.md").exists(),
        "newly written files must be cleaned up after partial failure"
    );
}

#[cfg(unix)]
#[test]
fn install_update_restores_existing_nested_dir_when_copy_fails() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let source_dir = dir.path().join("skills/foo");
    let dest_dir = dir.path().join(".claude/skills/foo");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(&dest_dir).unwrap();
    std::fs::write(source_dir.join("SKILL.md"), "# New Foo\n").unwrap();
    symlink("missing-target.md", source_dir.join("dangling.md")).unwrap();
    std::fs::write(dest_dir.join("SKILL.md"), "# Old Foo\n").unwrap();
    std::fs::write(dest_dir.join("keep.md"), "# Keep\n").unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n\
         local  skill  foo  skills/foo\n",
    )
    .unwrap();

    let output = sf(dir.path())
        .args(["install", "--update"])
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("failed to execute");

    assert!(
        !output.status.success(),
        "install --update should fail when source copy fails"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to install 'foo' to claude-code (local)"),
        "install failure should be surfaced, got: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(dest_dir.join("SKILL.md")).unwrap(),
        "# Old Foo\n"
    );
    assert_eq!(
        std::fs::read_to_string(dest_dir.join("keep.md")).unwrap(),
        "# Keep\n"
    );
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

    // Single-file entry: now normalized to directory structure
    let simple_file = dir.path().join(".claude/skills/simple/SKILL.md");
    assert!(
        simple_file.exists(),
        "single-file entry must be normalized to directory structure"
    );
    assert_eq!(
        std::fs::read_to_string(&simple_file).unwrap(),
        "# Simple Skill\n"
    );
    // Flat .md file must not exist
    assert!(
        !dir.path().join(".claude/skills/simple.md").exists(),
        "flat .md file must not exist for normalized single-file entry"
    );
}

#[test]
fn info_shows_missing_secondary_target() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_multi_target_skill_fixture(root, "info-skill");

    std::fs::remove_file(root.join(".github/skills/info-skill/SKILL.md")).unwrap();

    let output = sf(root).args(["info", "info-skill"]).output().unwrap();

    assert!(output.status.success());

    let stdout = normalize_separators(std::str::from_utf8(&output.stdout).unwrap());
    assert!(stdout.contains(".claude/skills/info-skill/SKILL.md"));
    assert!(stdout.contains(".github/skills/info-skill/SKILL.md (not installed)"));
}

#[test]
fn info_shows_lock_pin_and_cache_details() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_info_lock_pin_cache_fixture(root);

    let output = sf(root).args(["info", "info-skill"]).output().unwrap();

    assert!(output.status.success());
    assert_info_lock_pin_cache_output(std::str::from_utf8(&output.stdout).unwrap());
}

#[test]
fn info_reports_modified_when_pinned_entry_has_extra_edits() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_info_lock_pin_cache_fixture(root);

    let installed = root.join(".claude/skills/info-skill/SKILL.md");
    std::fs::write(
        &installed,
        "# Skill\n\nUpstream content.\n\n## Custom Section\n\nAdded by user.\nExtra drift.\n",
    )
    .unwrap();

    let output = sf(root).args(["info", "info-skill"]).output().unwrap();

    assert!(output.status.success());

    let stdout = normalize_separators(std::str::from_utf8(&output.stdout).unwrap());
    let modified_line = stdout
        .lines()
        .find(|line| line.contains("Modified:"))
        .unwrap();
    assert!(
        modified_line.trim_end().ends_with("yes"),
        "info should report pinned extra edits as modified:\n{stdout}"
    );
}

#[test]
fn info_shows_flat_dir_installed_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let agent_dir = root.join("agents/my-agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("agent.md"), "# Agent\n").unwrap();
    std::fs::write(agent_dir.join("notes.md"), "# Notes\n").unwrap();

    std::fs::write(
        root.join("Skillfile"),
        "install  claude-code  local\n\
         local  agent  my-agent  agents/my-agent\n",
    )
    .unwrap();

    sf(root).arg("install").assert().success();

    let output = sf(root).args(["info", "my-agent"]).output().unwrap();

    assert!(output.status.success());

    let stdout = normalize_separators(std::str::from_utf8(&output.stdout).unwrap());
    assert!(stdout.contains(".claude/agents/agent.md"));
    assert!(stdout.contains(".claude/agents/notes.md"));
    assert!(!stdout.contains("(not installed)"));
}

#[test]
fn status_reports_remote_entry_as_not_installed_when_files_are_missing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_remote_skill_fixture(
        root,
        &RemoteSkillFixture {
            name: "remote-skill",
            installed_text: None,
            patch_text: None,
        },
    );

    let output = sf(root).arg("status").output().unwrap();

    assert!(output.status.success());

    let stdout = normalize_separators(std::str::from_utf8(&output.stdout).unwrap());
    assert!(
        stdout.contains("remote-skill"),
        "status output must include the entry name:\n{stdout}"
    );
    assert!(
        stdout.contains("[not installed]"),
        "status must surface missing installs for locked remote entries:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// remove (direct golden path)
// ---------------------------------------------------------------------------

/// Remove an entry: Skillfile line gone, lock entry gone, cache cleaned.
#[test]
fn remove_clears_entry_lock_and_cache() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Create a local skill
    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::write(root.join("skills/foo.md"), "# Foo\n").unwrap();
    std::fs::write(
        root.join("Skillfile"),
        "install  claude-code  local\nlocal  skill  foo  skills/foo.md\n",
    )
    .unwrap();

    sf(root).arg("install").assert().success();
    assert!(
        root.join(".claude/skills/foo/SKILL.md").exists(),
        "single-file skill entry must be deployed as directory"
    );

    sf(root).args(["remove", "foo"]).assert().success();

    let text = std::fs::read_to_string(root.join("Skillfile")).unwrap();
    assert!(!text.contains("foo"), "entry should be gone from Skillfile");
    assert!(
        !root.join("Skillfile.lock").exists()
            || !std::fs::read_to_string(root.join("Skillfile.lock"))
                .unwrap()
                .contains("foo"),
        "lock should not contain the removed entry"
    );
}

// ---------------------------------------------------------------------------
// diff (golden path)
// ---------------------------------------------------------------------------

/// Diff a modified installed file against its vendor cache.
/// Uses an agent entry (flat deploy to `.claude/agents/<name>.md`).
/// Requires pre-populated cache + lock (no network).
#[test]
fn diff_shows_changes_between_cache_and_installed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let manifest = "install  claude-code  local\n\
                    github  agent  my-agent  owner/repo  agents/agent.md  main\n";
    std::fs::write(root.join("Skillfile"), manifest).unwrap();

    // Lock
    let lock_json = serde_json::json!({
        "github/agent/my-agent": {
            "sha": "abc123def456abc123def456abc123def456abc1",
            "raw_url": "https://raw.githubusercontent.com/owner/repo/abc123/agents/agent.md"
        }
    });
    std::fs::write(
        root.join("Skillfile.lock"),
        serde_json::to_string_pretty(&lock_json).unwrap(),
    )
    .unwrap();

    // Vendor cache
    let vdir = root.join(".skillfile/cache/agents/my-agent");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("agent.md"), "# Agent\n\nUpstream content.\n").unwrap();
    std::fs::write(
        vdir.join(".meta"),
        r#"{"sha":"abc123def456abc123def456abc123def456abc1"}"#,
    )
    .unwrap();

    // Installed (modified by user) — agents deploy flat
    let installed = root.join(".claude/agents");
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::write(
        installed.join("my-agent.md"),
        "# Agent\n\nUpstream content.\n\n## My Notes\n\nUser addition.\n",
    )
    .unwrap();

    sf(root)
        .args(["diff", "my-agent"])
        .assert()
        .success()
        .stdout(predicate::str::contains("User addition"));
}

#[test]
fn diff_surfaces_secondary_target_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_multi_target_skill_fixture(root, "multi-skill");

    std::fs::write(
        root.join(".github/skills/multi-skill/SKILL.md"),
        "# Skill\n\nUpstream content.\n\nSecondary target edit.\n",
    )
    .unwrap();

    sf(root)
        .args(["diff", "multi-skill"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Secondary target edit."))
        .stdout(predicate::str::contains("installed: copilot (local)"));
}

#[test]
fn pin_and_unpin_round_trip_secondary_target_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_multi_target_skill_fixture(root, "pin-skill");

    std::fs::write(
        root.join(".github/skills/pin-skill/SKILL.md"),
        "# Skill\n\nUpstream content.\n\nPinned from second target.\n",
    )
    .unwrap();

    sf(root).args(["pin", "pin-skill"]).assert().success();

    let patch_path = root.join(".skillfile/patches/skills/pin-skill.patch");
    assert!(patch_path.exists(), "pin should write a patch");
    assert!(
        std::fs::read_to_string(&patch_path)
            .unwrap()
            .contains("Pinned from second target."),
        "patch should capture second-target edit"
    );

    std::fs::remove_file(root.join(".claude/skills/pin-skill/SKILL.md")).unwrap();

    sf(root).arg("install").assert().success();
    assert!(
        std::fs::read_to_string(root.join(".claude/skills/pin-skill/SKILL.md"))
            .unwrap()
            .contains("Pinned from second target."),
        "install should apply the stored patch when redeploying the first target"
    );

    sf(root).args(["unpin", "pin-skill"]).assert().success();
    assert!(!patch_path.exists(), "unpin should remove patch");
    assert_eq!(
        std::fs::read_to_string(root.join(".claude/skills/pin-skill/SKILL.md")).unwrap(),
        "# Skill\n\nUpstream content.\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join(".github/skills/pin-skill/SKILL.md")).unwrap(),
        "# Skill\n\nUpstream content.\n"
    );
}

// ---------------------------------------------------------------------------
// resolve --abort (golden path)
// ---------------------------------------------------------------------------

/// Resolve --abort clears conflict state without modifying files.
#[test]
fn resolve_abort_clears_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    std::fs::write(
        root.join("Skillfile"),
        "github  skill  test  owner/repo  skills/test.md  main\n",
    )
    .unwrap();

    // Write conflict state manually
    let conflict_dir = root.join(".skillfile");
    std::fs::create_dir_all(&conflict_dir).unwrap();
    std::fs::write(
        conflict_dir.join("conflict"),
        r#"{"entry":"test","entity_type":"skill","old_sha":"aaa","new_sha":"bbb"}"#,
    )
    .unwrap();
    assert!(conflict_dir.join("conflict").exists());

    sf(root).args(["resolve", "--abort"]).assert().success();

    assert!(
        !conflict_dir.join("conflict").exists(),
        "conflict file should be cleared after --abort"
    );
}

// ---------------------------------------------------------------------------
// upgrade
// ---------------------------------------------------------------------------

/// `skillfile upgrade` is a thin wrapper for `install --update`.
/// With a local-only Skillfile and no network, it should succeed and behave
/// identically to `skillfile install --update`.
#[test]
fn upgrade_succeeds_on_local_manifest() {
    let dir = tempfile::tempdir().unwrap();
    write_local_manifest(dir.path());

    sf(dir.path()).arg("upgrade").assert().success();
}

/// `skillfile upgrade --dry-run` should exit zero and mention the dry-run
/// behaviour without modifying any files.
#[test]
fn upgrade_dry_run_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    write_local_manifest(dir.path());

    sf(dir.path())
        .args(["upgrade", "--dry-run"])
        .assert()
        .success();
}
