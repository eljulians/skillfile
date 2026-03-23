//! Integration tests for `cmd_unpin` and `cmd_resolve`.
//!
//! These tests exercise multi-module orchestration:
//! manifest parsing → patch/conflict operations → filesystem state changes.
//!
//! They use real cross-crate calls (`skillfile_core::conflict::write_conflict`,
//! `skillfile_core::patch::write_patch`, `skillfile_core::lock::write_lock`)
//! to set up state, then call into the CLI crate to verify the full flow.

use std::collections::BTreeMap;
use std::path::Path;

use skillfile::commands::pin::cmd_unpin;
use skillfile::commands::resolve::cmd_resolve;
use skillfile_core::conflict::write_conflict;
use skillfile_core::lock::write_lock;
use skillfile_core::models::{ConflictState, EntityType, LockEntry, SourceFields};
use skillfile_core::patch::{dir_patch_path, patch_path, write_dir_patch, write_patch};

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn make_agent_entry(name: &str) -> skillfile_core::models::Entry {
    skillfile_core::models::Entry {
        entity_type: EntityType::Agent,
        name: name.to_string(),
        source: SourceFields::Github {
            owner_repo: "owner/repo".into(),
            path_in_repo: format!("agents/{name}.md"),
            ref_: "main".into(),
        },
    }
}

fn make_skill_dir_entry(name: &str) -> skillfile_core::models::Entry {
    skillfile_core::models::Entry {
        entity_type: EntityType::Skill,
        name: name.to_string(),
        source: SourceFields::Github {
            owner_repo: "owner/repo".into(),
            // No `.md` extension → is_dir_entry() returns true
            path_in_repo: format!("skills/{name}"),
            ref_: "main".into(),
        },
    }
}

fn write_manifest(dir: &Path, content: &str) {
    std::fs::write(dir.join("Skillfile"), content).unwrap();
}

fn make_lock_json(name: &str, entity_type: &str) -> BTreeMap<String, LockEntry> {
    let mut map = BTreeMap::new();
    map.insert(
        format!("github/{entity_type}/{name}"),
        LockEntry {
            sha: "abc123def456abc123def456abc123def456abc1".into(),
            raw_url: format!("https://raw.githubusercontent.com/owner/repo/abc123/{name}.md"),
        },
    );
    map
}

fn write_agent_cache(dir: &Path, name: &str, content: &str) {
    let cache_dir = dir.join(".skillfile/cache/agents").join(name);
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(cache_dir.join(format!("{name}.md")), content).unwrap();
}

/// Write a file into a directory entry's vendor cache.
/// `cache_dir` must be `.skillfile/cache/skills/<name>` (already resolved by caller).
fn write_skill_dir_cache(cache_dir: &Path, filename: &str, content: &str) {
    std::fs::create_dir_all(cache_dir).unwrap();
    std::fs::write(cache_dir.join(filename), content).unwrap();
}

fn write_installed_agent(dir: &Path, name: &str, content: &str) {
    let agents_dir = dir.join(".claude/agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join(format!("{name}.md")), content).unwrap();
}

/// Write a file into an installed skill directory.
/// `skill_dir` must be `.claude/skills/<name>` (already resolved by caller).
fn write_installed_skill_dir(skill_dir: &Path, filename: &str, content: &str) {
    std::fs::create_dir_all(skill_dir).unwrap();
    std::fs::write(skill_dir.join(filename), content).unwrap();
}

fn read_installed_agent(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(".claude/agents").join(format!("{name}.md"))).unwrap()
}

fn agent_manifest(name: &str) -> String {
    format!("install  claude-code  local\ngithub  agent  owner/repo  agents/{name}.md\n")
}

fn skill_dir_manifest(name: &str) -> String {
    format!("install  claude-code  local\ngithub  skill  owner/repo  skills/{name}\n")
}

fn make_conflict_state(entry: &str, entity_type: EntityType) -> ConflictState {
    ConflictState {
        entry: entry.to_string(),
        entity_type,
        old_sha: "a".repeat(40),
        new_sha: "b".repeat(40),
    }
}

// ---------------------------------------------------------------------------
// cmd_unpin tests
// ---------------------------------------------------------------------------

/// Verify that `cmd_unpin` removes the patch file and restores the installed
/// agent file to upstream content.
#[test]
fn unpin_removes_single_file_patch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write_manifest(root, &agent_manifest("my-agent"));
    write_lock(root, &make_lock_json("my-agent", "agent")).unwrap();

    let upstream = "# My Agent\n\nUpstream content.\n";
    let modified = "# My Agent\n\nModified content.\n";

    write_agent_cache(root, "my-agent", upstream);
    write_installed_agent(root, "my-agent", modified);

    let entry = make_agent_entry("my-agent");
    let dummy_patch = "--- a/my-agent.md\n+++ b/my-agent.md\n@@ -2 +2 @@\n-Upstream\n+Modified\n";
    write_patch(&entry, dummy_patch, root).unwrap();

    assert!(
        patch_path(&entry, root).exists(),
        "patch must exist before unpin"
    );

    cmd_unpin("my-agent", root).unwrap();

    assert!(
        !patch_path(&entry, root).exists(),
        "patch file must be removed after unpin"
    );
    assert_eq!(
        read_installed_agent(root, "my-agent"),
        upstream,
        "installed file must be restored to upstream content"
    );
}

/// Verify that `cmd_unpin` removes all patch files for a directory entry.
#[test]
fn unpin_dir_entry_removes_all_patches() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write_manifest(root, &skill_dir_manifest("my-skill"));
    write_lock(root, &make_lock_json("my-skill", "skill")).unwrap();

    let upstream = "# SKILL.md\n\nUpstream.\n";
    let upstream_b = "# Context.md\n\nUpstream B.\n";

    let cache_dir = root.join(".skillfile/cache/skills/my-skill");
    write_skill_dir_cache(&cache_dir, "SKILL.md", upstream);
    write_skill_dir_cache(&cache_dir, "context.md", upstream_b);
    let install_dir = root.join(".claude/skills/my-skill");
    write_installed_skill_dir(&install_dir, "SKILL.md", "# SKILL.md\n\nEdited.\n");
    write_installed_skill_dir(&install_dir, "context.md", "# Context.md\n\nEdited B.\n");

    let entry = make_skill_dir_entry("my-skill");
    let dummy = "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-Up\n+Ed\n";
    let patch_a = dir_patch_path(&entry, "SKILL.md", root);
    let patch_b = dir_patch_path(&entry, "context.md", root);
    write_dir_patch(&patch_a, dummy).unwrap();
    write_dir_patch(&patch_b, dummy).unwrap();

    assert!(patch_a.exists(), "SKILL.md patch must exist before unpin");
    assert!(patch_b.exists(), "context.md patch must exist before unpin");

    cmd_unpin("my-skill", root).unwrap();

    assert!(
        !patch_a.exists(),
        "SKILL.md patch must be removed after unpin"
    );
    assert!(
        !patch_b.exists(),
        "context.md patch must be removed after unpin"
    );
}

/// `cmd_unpin` on an entry with no patches must succeed without error.
#[test]
fn unpin_not_pinned_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write_manifest(root, &agent_manifest("clean-agent"));
    write_lock(root, &make_lock_json("clean-agent", "agent")).unwrap();

    let result = cmd_unpin("clean-agent", root);
    assert!(
        result.is_ok(),
        "unpin of unpinned entry must succeed: {result:?}"
    );
}

// ---------------------------------------------------------------------------
// cmd_resolve tests
// ---------------------------------------------------------------------------

/// `cmd_resolve` with `abort=true` must clear the conflict file.
#[test]
fn resolve_abort_clears_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write_manifest(root, "");
    let state = make_conflict_state("some-entry", EntityType::Skill);
    write_conflict(root, &state).unwrap();

    assert!(
        skillfile_core::conflict::has_conflict(root),
        "conflict file must exist before abort"
    );

    cmd_resolve(None, true, root).unwrap();

    assert!(
        !skillfile_core::conflict::has_conflict(root),
        "conflict file must be cleared after abort"
    );
}

/// `cmd_resolve` for an entry not present in the manifest must return an error.
#[test]
fn resolve_entry_not_in_manifest_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Manifest has "real-entry" but the conflict references "ghost"
    write_manifest(
        root,
        "install  claude-code  local\ngithub  skill  owner/repo  skills/real-entry.md\n",
    );
    let state = make_conflict_state("ghost", EntityType::Skill);
    write_conflict(root, &state).unwrap();

    let result = cmd_resolve(Some("ghost"), false, root);
    assert!(
        result.is_err(),
        "must error when entry is absent from manifest"
    );
}
