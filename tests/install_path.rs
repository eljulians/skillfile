use skillfile_functional_tests::sf;

const LOCKED_SHA: &str = "abc123def456abc123def456abc123def456abc1";
const UPSTREAM_SKILL: &str = "# Release Skill\n\nUpstream content.\n";

fn write_cached_remote_skill(root: &std::path::Path, name: &str) {
    std::fs::write(
        root.join("Skillfile"),
        format!(
            "install-path  \"custom target\"  skill  \"./custom targets/skills\"\n\
             github  skill  {name}  owner/repo  skills/{name}.md  main\n"
        ),
    )
    .unwrap();

    let lock = serde_json::json!({
        format!("github/skill/{name}"): {
            "sha": LOCKED_SHA,
            "raw_url": format!(
                "https://raw.githubusercontent.com/owner/repo/{LOCKED_SHA}/skills/{name}.md"
            )
        }
    });
    std::fs::write(
        root.join("Skillfile.lock"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    let cache_dir = root.join(format!(".skillfile/cache/skills/{name}"));
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(cache_dir.join(format!("{name}.md")), UPSTREAM_SKILL).unwrap();
    std::fs::write(
        cache_dir.join(".meta"),
        format!(r#"{{"sha":"{LOCKED_SHA}"}}"#),
    )
    .unwrap();
}

fn command_stdout(root: &std::path::Path, args: &[&str]) -> String {
    let output = sf(root).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{} failed:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn format_validate_and_install(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    command_stdout(root, &["validate"]);
    command_stdout(root, &["format"]);
    command_stdout(root, &["validate"]);

    let manifest = std::fs::read_to_string(root.join("Skillfile")).unwrap();
    assert!(manifest.contains("install-path  'custom target'  skill  './custom targets/skills'"));

    command_stdout(root, &["install"]);
    let installed = root.join(format!("custom targets/skills/{name}/SKILL.md"));
    assert_eq!(std::fs::read_to_string(&installed).unwrap(), UPSTREAM_SKILL);
    installed
}

fn assert_install_is_observable(root: &std::path::Path, name: &str) {
    let info = command_stdout(root, &["info", name]);
    let installed = info
        .lines()
        .find_map(|line| line.split_once("Installed:").map(|(_, path)| path.trim()))
        .expect("info output must contain an Installed path");
    let installed_suffix = std::path::Path::new("custom targets")
        .join("skills")
        .join(name)
        .join("SKILL.md");
    assert!(
        std::path::Path::new(installed).ends_with(installed_suffix),
        "unexpected info output:\n{info}"
    );

    let status = command_stdout(root, &["status"]);
    assert!(status.contains(name));
    assert!(!status.contains("[modified]"));
}

fn assert_diff_and_pin_lifecycle(root: &std::path::Path, name: &str, installed: &std::path::Path) {
    std::fs::write(
        installed,
        format!("{UPSTREAM_SKILL}\n## Custom edit\n\nRelease validation.\n"),
    )
    .unwrap();

    assert!(command_stdout(root, &["status"]).contains("[modified]"));
    let diff = command_stdout(root, &["diff", name]);
    assert!(diff.contains("Custom edit"));
    assert!(diff.contains("custom target"));

    command_stdout(root, &["pin", name]);
    let patch = root.join(format!(".skillfile/patches/skills/{name}.patch"));
    assert!(std::fs::read_to_string(&patch)
        .unwrap()
        .contains("Release validation."));

    std::fs::remove_file(installed).unwrap();
    command_stdout(root, &["install"]);
    assert!(std::fs::read_to_string(installed)
        .unwrap()
        .contains("Release validation."));

    command_stdout(root, &["unpin", name]);
    assert!(!patch.exists());
    assert_eq!(std::fs::read_to_string(installed).unwrap(), UPSTREAM_SKILL);
}

#[test]
fn install_path_deploys_skill_nested_and_agent_flat() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::write(root.join("skills/demo-skill.md"), "# Demo Skill\n").unwrap();
    std::fs::write(root.join("agents/demo-agent.md"), "# Demo Agent\n").unwrap();

    std::fs::write(
        root.join("Skillfile"),
        "install-path  custom-skill  skill  ./out/skills\n\
         install-path  custom-agent  agent  ./out/agents\n\
         local  skill  demo-skill  skills/demo-skill.md\n\
         local  agent  demo-agent  agents/demo-agent.md\n",
    )
    .unwrap();

    sf(root).arg("install").assert().success();

    let deployed_skill = root.join("out/skills/demo-skill/SKILL.md");
    assert_eq!(
        std::fs::read_to_string(&deployed_skill).unwrap(),
        "# Demo Skill\n"
    );

    let deployed_agent = root.join("out/agents/demo-agent.md");
    assert_eq!(
        std::fs::read_to_string(&deployed_agent).unwrap(),
        "# Demo Agent\n"
    );

    assert!(!root.join("out/skills/demo-agent/SKILL.md").exists());
    assert!(!root.join("out/agents/demo-skill.md").exists());
}

#[test]
fn install_path_expands_home() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let home = tempfile::tempdir().unwrap();

    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::write(root.join("skills/home-skill.md"), "# Home Skill\n").unwrap();

    std::fs::write(
        root.join("Skillfile"),
        "install-path  home-target  skill  ~/custom-skills\n\
         local  skill  home-skill  skills/home-skill.md\n",
    )
    .unwrap();

    sf(root)
        .env("HOME", home.path())
        .arg("install")
        .assert()
        .success();

    let deployed_skill = home.path().join("custom-skills/home-skill/SKILL.md");
    assert_eq!(
        std::fs::read_to_string(&deployed_skill).unwrap(),
        "# Home Skill\n"
    );
}

#[test]
fn install_path_expands_bare_home_marker() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let home = tempfile::tempdir().unwrap();

    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::write(root.join("skills/home-skill.md"), "# Home Skill\n").unwrap();
    std::fs::write(
        root.join("Skillfile"),
        "install-path  home-target  skill  ~\n\
         local  skill  home-skill  skills/home-skill.md\n",
    )
    .unwrap();

    sf(root)
        .env("HOME", home.path())
        .arg("install")
        .assert()
        .success();

    assert!(home.path().join("home-skill/SKILL.md").exists());
    assert!(!home.path().join("~/home-skill/SKILL.md").exists());
}

#[test]
fn install_path_supports_release_lifecycle_for_cached_remote_skill() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let name = "release-skill";
    write_cached_remote_skill(root, name);

    let installed = format_validate_and_install(root, name);
    assert_install_is_observable(root, name);
    assert_diff_and_pin_lifecycle(root, name, &installed);
}

#[test]
fn install_path_rejects_unquoted_extra_fields() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install-path custom skill ./my skills\n",
    )
    .unwrap();

    let output = sf(dir.path()).arg("validate").output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "validate unexpectedly succeeded");
    assert!(
        stderr.contains("install-path line needs exactly: tool-name entity-type path"),
        "unexpected validation error:\n{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn install_path_refuses_symlinked_target_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();

    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::write(root.join("skills/escape.md"), "# Must not escape\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("linked-target")).unwrap();
    std::fs::write(
        root.join("Skillfile"),
        "install-path custom skill ./linked-target\n\
         local skill escape skills/escape.md\n",
    )
    .unwrap();

    let output = sf(root).arg("install").output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "install unexpectedly succeeded");
    assert!(
        stderr.contains("refusing to traverse symlink"),
        "unexpected install error:\n{stderr}"
    );
    assert!(!outside.path().join("escape/SKILL.md").exists());
}

#[cfg(unix)]
#[test]
fn install_path_refuses_symlinked_nested_skill_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();

    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::create_dir_all(root.join("out/skills")).unwrap();
    std::fs::write(root.join("skills/escape.md"), "# Must not escape\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("out/skills/escape")).unwrap();
    std::fs::write(
        root.join("Skillfile"),
        "install-path custom skill ./out/skills\n\
         local skill escape skills/escape.md\n",
    )
    .unwrap();

    let output = sf(root).arg("install").output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "install unexpectedly succeeded");
    assert!(
        stderr.contains("refusing to traverse symlink"),
        "unexpected install error:\n{stderr}"
    );
    assert!(!outside.path().join("SKILL.md").exists());
}

#[cfg(unix)]
#[test]
fn install_path_refuses_symlinked_single_file_source_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();

    std::fs::create_dir_all(root.join("skills")).unwrap();
    let outside_file = outside.path().join("secret.md");
    std::fs::write(&outside_file, "SECRET OUTSIDE SOURCE\n").unwrap();
    std::os::unix::fs::symlink(&outside_file, root.join("skills/escape.md")).unwrap();
    std::fs::write(
        root.join("Skillfile"),
        "install-path custom skill ./out/skills\n\
         local skill escape skills/escape.md\n",
    )
    .unwrap();

    let output = sf(root).arg("install").output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "install unexpectedly succeeded");
    assert!(
        stderr.contains("refusing to traverse symlink"),
        "unexpected install error:\n{stderr}"
    );
    assert!(!root.join("out/skills/escape/SKILL.md").exists());
}

#[cfg(unix)]
#[test]
fn install_path_refuses_symlinked_directory_source_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();

    let outside_skill = outside.path().join("escape");
    std::fs::create_dir_all(&outside_skill).unwrap();
    std::fs::write(outside_skill.join("SKILL.md"), "SECRET OUTSIDE SOURCE\n").unwrap();
    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::os::unix::fs::symlink(&outside_skill, root.join("skills/escape")).unwrap();
    std::fs::write(
        root.join("Skillfile"),
        "install-path custom skill ./out/skills\n\
         local skill escape skills/escape\n",
    )
    .unwrap();

    let output = sf(root).arg("install").output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "install unexpectedly succeeded");
    assert!(
        stderr.contains("refusing to traverse symlink"),
        "unexpected install error:\n{stderr}"
    );
    assert!(!root.join("out/skills/escape/SKILL.md").exists());
}

#[test]
fn install_path_preserves_user_managed_legacy_shaped_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::create_dir_all(root.join("out/skills")).unwrap();
    std::fs::write(root.join("skills/demo.md"), "# Managed skill\n").unwrap();
    std::fs::write(
        root.join("out/skills/demo.md"),
        "# User-managed flat file\n",
    )
    .unwrap();
    std::fs::write(
        root.join("Skillfile"),
        "install-path custom skill ./out/skills\n\
         local skill demo skills/demo.md\n",
    )
    .unwrap();

    sf(root).arg("install").assert().success();

    assert_eq!(
        std::fs::read_to_string(root.join("out/skills/demo.md")).unwrap(),
        "# User-managed flat file\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("out/skills/demo/SKILL.md")).unwrap(),
        "# Managed skill\n"
    );
}

#[test]
fn add_preserves_existing_explicit_target_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::create_dir_all(root.join("team-agents")).unwrap();
    std::fs::write(root.join("agents/reviewer.md"), "# Managed agent\n").unwrap();
    std::fs::write(
        root.join("team-agents/reviewer.md"),
        "# User-managed agent\n",
    )
    .unwrap();
    std::fs::write(
        root.join("Skillfile"),
        "install-path custom agent ./team-agents\n",
    )
    .unwrap();

    sf(root)
        .args(["add", "local", "agent", "agents/reviewer.md"])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(root.join("team-agents/reviewer.md")).unwrap(),
        "# User-managed agent\n"
    );
    assert!(std::fs::read_to_string(root.join("Skillfile"))
        .unwrap()
        .contains("local  agent  agents/reviewer.md"));
}

#[cfg(unix)]
#[test]
fn read_commands_do_not_follow_explicit_target_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let name = "release-skill";
    write_cached_remote_skill(root, name);
    command_stdout(root, &["install"]);

    let installed = root.join("custom targets/skills/release-skill/SKILL.md");
    let secret = root.join("outside-secret.txt");
    std::fs::write(&secret, "DO NOT DISCLOSE\n").unwrap();
    std::fs::remove_file(&installed).unwrap();
    std::os::unix::fs::symlink(&secret, &installed).unwrap();

    let status = command_stdout(root, &["status"]);
    assert!(status.contains("[not installed]"));

    for args in [["diff", name], ["pin", name]] {
        let output = sf(root).args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("DO NOT DISCLOSE"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("DO NOT DISCLOSE"));
    }
    assert!(!root
        .join(".skillfile/patches/skills/release-skill.patch")
        .exists());
}

#[cfg(unix)]
#[test]
fn install_path_preserves_legacy_shaped_sibling_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();

    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::create_dir_all(root.join("out/skills")).unwrap();
    std::fs::write(root.join("skills/demo.md"), "# Managed skill\n").unwrap();
    let outside_file = outside.path().join("user-managed.md");
    std::fs::write(&outside_file, "# User-managed flat file\n").unwrap();
    std::os::unix::fs::symlink(&outside_file, root.join("out/skills/demo.md")).unwrap();
    std::fs::write(
        root.join("Skillfile"),
        "install-path custom skill ./out/skills\n\
         local skill demo skills/demo.md\n",
    )
    .unwrap();

    sf(root).arg("install").assert().success();

    assert_eq!(
        std::fs::read_to_string(outside_file).unwrap(),
        "# User-managed flat file\n"
    );
    assert!(root.join("out/skills/demo.md").is_symlink());
    assert!(root.join("out/skills/demo/SKILL.md").exists());
}
