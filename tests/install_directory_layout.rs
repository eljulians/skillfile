/// Integration tests for directory-normalized skill deployment.
///
/// These tests verify that all skills (both single-file and directory-based)
/// deploy to a consistent directory layout format, as per the upstream plan:
/// - Single-file entries deploy as: ./.claude/skills/<name>/SKILL.md
/// - Directory entries deploy as: ./.claude/skills/<name>/...
///
/// This normalization ensures compatibility with tools like Copilot and Claude Code
/// that expect directory-based skill layouts.

use skillfile_functional_tests::sf;

// ---------------------------------------------------------------------------
// Test 1: Single-file local entry deploys as directory
// ---------------------------------------------------------------------------

#[test]
fn install_local_file_entry_as_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Create local skill file
    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::write(root.join("skills/my-skill.md"), "# My Skill\n").unwrap();

    std::fs::write(
        root.join("Skillfile"),
        "install  claude-code  local\n\
         local  skill  my-skill  skills/my-skill.md\n",
    )
    .unwrap();

    sf(root).arg("install").assert().success();

    // Single-file local entry MUST deploy as directory structure
    let deployed_skill = root.join(".claude/skills/my-skill/SKILL.md");
    assert!(
        deployed_skill.exists(),
        "local file entry must deploy as my-skill/SKILL.md"
    );
    assert_eq!(std::fs::read_to_string(&deployed_skill).unwrap(), "# My Skill\n");

    // Flat .md file must NOT exist
    assert!(
        !root.join(".claude/skills/my-skill.md").exists(),
        "flat .md file must not exist for file-based local entry"
    );
}

// ---------------------------------------------------------------------------
// Test 2: Both Claude and Copilot platforms deploy as directories
// ---------------------------------------------------------------------------

#[test]
fn install_normalizes_both_platforms_to_directories() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Create local skill file
    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::write(root.join("skills/test.md"), "# Test\n").unwrap();

    std::fs::write(
        root.join("Skillfile"),
        "install  claude-code  local\n\
         install  copilot  local\n\
         local  skill  test  skills/test.md\n",
    )
    .unwrap();

    sf(root).arg("install").assert().success();

    // Both platforms normalize to directory structure
    assert!(root.join(".claude/skills/test/SKILL.md").exists());
    assert!(root.join(".github/skills/test/SKILL.md").exists());
}

// ---------------------------------------------------------------------------
// Test 3: Local directory entry still works with new normalization
// ---------------------------------------------------------------------------

#[test]
fn install_local_dir_entry_with_normalization() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Create a local skill directory with multiple files
    let skill_dir = root.join("skills/my-local-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "# My Local Skill\n\nMain content.\n",
    )
    .unwrap();
    std::fs::write(skill_dir.join("extra.md"), "# Extra\n\nBonus content.\n").unwrap();

    std::fs::write(
        root.join("Skillfile"),
        "install  claude-code  local\n\
         local  skill  my-local-skill  skills/my-local-skill\n",
    )
    .unwrap();

    sf(root).arg("install").assert().success();

    // Directory entry still deploys as nested directory
    let deployed_dir = root.join(".claude/skills/my-local-skill");
    assert!(deployed_dir.is_dir());
    assert_eq!(
        std::fs::read_to_string(deployed_dir.join("SKILL.md")).unwrap(),
        "# My Local Skill\n\nMain content.\n"
    );
    assert_eq!(
        std::fs::read_to_string(deployed_dir.join("extra.md")).unwrap(),
        "# Extra\n\nBonus content.\n"
    );

    // Must NOT create spurious .md file
    assert!(!root.join(".claude/skills/my-local-skill.md").exists());
}

// ---------------------------------------------------------------------------
// Test 4: Mixed local file and directory entries normalize consistently
// ---------------------------------------------------------------------------

#[test]
fn install_mixed_local_entries_normalize_consistently() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Create a file entry
    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::write(root.join("skills/file-skill.md"), "# File Skill\n").unwrap();

    // Create a directory entry
    let dir_skill = root.join("skills/dir-skill");
    std::fs::create_dir_all(&dir_skill).unwrap();
    std::fs::write(dir_skill.join("SKILL.md"), "# Dir Skill\n").unwrap();

    std::fs::write(
        root.join("Skillfile"),
        "install  claude-code  local\n\
         local  skill  file-skill  skills/file-skill.md\n\
         local  skill  dir-skill  skills/dir-skill\n",
    )
    .unwrap();

    sf(root).arg("install").assert().success();

    // Both should be directories
    assert!(
        root.join(".claude/skills/file-skill/SKILL.md").exists(),
        "file entry should normalize to directory layout"
    );
    assert!(
        root.join(".claude/skills/dir-skill/SKILL.md").exists(),
        "dir entry should exist in directory layout"
    );

    // Neither should have flat .md files
    assert!(!root.join(".claude/skills/file-skill.md").exists());
    assert!(!root.join(".claude/skills/dir-skill.md").exists());
}

// ---------------------------------------------------------------------------
// Test 5: Re-installing over a legacy flat file removes the old orphan
// ---------------------------------------------------------------------------
//
// Simulates a project that was previously managed by an older version of
// skillfile which deployed skills as flat .md files (e.g. .claude/skills/my-skill.md).
// After upgrading and re-running `skillfile install`, the old flat file must be
// removed so agents that scan the directory broadly do not load both versions.

#[test]
fn install_removes_orphan_flat_file_on_migration() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Create local skill source
    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::write(root.join("skills/my-skill.md"), "# My Skill\n").unwrap();

    std::fs::write(
        root.join("Skillfile"),
        "install  claude-code  local\n\
         local  skill  my-skill  skills/my-skill.md\n",
    )
    .unwrap();

    // Pre-create the legacy flat file as if placed by an older skillfile version
    std::fs::create_dir_all(root.join(".claude/skills")).unwrap();
    std::fs::write(
        root.join(".claude/skills/my-skill.md"),
        "# My Skill (old flat version)\n",
    )
    .unwrap();

    sf(root).arg("install").assert().success();

    // New directory-layout file must be present
    assert!(
        root.join(".claude/skills/my-skill/SKILL.md").exists(),
        "skill must be deployed as directory layout"
    );

    // Legacy flat file must have been removed to prevent duplicate loading
    assert!(
        !root.join(".claude/skills/my-skill.md").exists(),
        "legacy flat .md file must be removed on migration"
    );
}
