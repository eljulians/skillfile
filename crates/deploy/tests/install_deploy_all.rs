/// Integration tests for the `deploy_all` orchestration path through `cmd_install`.
///
/// These tests span multiple modules: install -> patch application ->
/// conflict detection -> lock writing. They use the public `cmd_install` API
/// and cross-crate helpers (`skillfile_core::conflict::read_conflict`,
/// `skillfile_core::lock::write_lock`, etc.) because that is exactly what makes
/// them integration tests rather than unit tests.
use std::collections::BTreeMap;
use std::path::Path;

use skillfile_core::lock::write_lock;
use skillfile_core::models::{EntityType, LockEntry};
use skillfile_deploy::install::{cmd_install, CmdInstallOpts};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// A 40-hex-character fake SHA that looks realistic in error messages.
const FAKE_SHA: &str = "abcdef1234567890abcdef1234567890abcdef12";

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// All the paths derived from a tempdir that the fixtures need to write to.
struct Dirs {
    root: TempDir,
}

impl Dirs {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }
}

/// Write a minimal `Skillfile` with one github skill entry and one install target.
///
/// Entry: `github skill my-skill owner/repo skills/my-skill.md`
/// Target: `install claude-code local`
fn write_github_skillfile(root: &Path) {
    let content =
        "github skill my-skill owner/repo skills/my-skill.md\ninstall claude-code local\n";
    std::fs::write(root.join("Skillfile"), content).unwrap();
}

/// Write a `Skillfile` with an unknown adapter name so `deploy_all` skips it.
fn write_unknown_adapter_skillfile(root: &Path) {
    let content = "local skill my-skill skills/my-skill.md\ninstall unknown-platform-xyz local\n";
    std::fs::write(root.join("Skillfile"), content).unwrap();
    let source = root.join("skills/my-skill.md");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(source, "# My Skill\n").unwrap();
}

/// Write the vendor cache and `.meta` for a single-file GitHub skill entry.
///
/// The `.meta` SHA is set to `FAKE_SHA` so sync sees the cache as current.
fn write_github_cache(root: &Path) {
    let cache_dir = root.join(".skillfile/cache/skills/my-skill");
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(
        cache_dir.join("my-skill.md"),
        "# Original\n\nUpstream content.\n",
    )
    .unwrap();
    let meta = serde_json::json!({
        "source_type": "github",
        "owner_repo": "owner/repo",
        "path_in_repo": "skills/my-skill.md",
        "ref": "main",
        "sha": FAKE_SHA,
        "raw_url": format!("https://raw.githubusercontent.com/owner/repo/{FAKE_SHA}/skills/my-skill.md"),
    });
    std::fs::write(
        cache_dir.join(".meta"),
        serde_json::to_string_pretty(&meta).unwrap() + "\n",
    )
    .unwrap();
}

/// Write `Skillfile.lock` so sync treats the cache as up-to-date.
fn write_github_lock(root: &Path) {
    let mut locked = BTreeMap::new();
    locked.insert(
        "github/skill/my-skill".to_string(),
        LockEntry {
            sha: FAKE_SHA.to_string(),
            raw_url: format!(
                "https://raw.githubusercontent.com/owner/repo/{FAKE_SHA}/skills/my-skill.md"
            ),
        },
    );
    write_lock(root, &locked).unwrap();
}

/// Write a patch that cannot apply to the cached content.
///
/// The patch expects a line `"Expected original line.\n"` which is absent from
/// the cache, so `apply_patch_pure` will return a conflict error.
fn write_conflicting_patch(root: &Path) {
    let patch_dir = root.join(".skillfile/patches/skills");
    std::fs::create_dir_all(&patch_dir).unwrap();
    let bad_patch = concat!(
        "--- a/my-skill.md\n",
        "+++ b/my-skill.md\n",
        "@@ -1 +1 @@\n",
        "-Expected original line.\n",
        "+My local customization.\n",
    );
    std::fs::write(patch_dir.join("my-skill.patch"), bad_patch).unwrap();
}

/// Build the default (non-update, non-dry-run) `CmdInstallOpts` with no extra targets.
fn default_opts() -> CmdInstallOpts<'static> {
    CmdInstallOpts {
        dry_run: false,
        update: false,
        extra_targets: None,
    }
}

/// Set up a tempdir wired for a patch-conflict scenario and return it.
///
/// Writes Skillfile, cache, lock, and a conflicting patch. Calling `cmd_install`
/// on this fixture will trigger a `PatchConflict` which `deploy_all` converts
/// into an `Install` error and writes `.skillfile/conflict`.
fn setup_conflict_fixture() -> Dirs {
    let dirs = Dirs::new();
    let root = dirs.path();
    write_github_skillfile(root);
    write_github_cache(root);
    write_github_lock(root);
    write_conflicting_patch(root);
    dirs
}

// ---------------------------------------------------------------------------
// install_patch_conflict_writes_conflict_state
// ---------------------------------------------------------------------------

/// `cmd_install` writes `.skillfile/conflict` when a patch fails to apply.
///
/// Flow: local cache is up-to-date (sync skips) -> adapter deploys cached
/// content -> `apply_single_file_patch` fails -> `handle_patch_conflict`
/// writes the conflict state file and returns `Err(Install(...))`.
#[test]
fn install_patch_conflict_writes_conflict_state() {
    let dirs = setup_conflict_fixture();
    let root = dirs.path();

    let result = cmd_install(root, &default_opts());

    assert!(result.is_err(), "expected conflict error");
    let conflict_file = root.join(".skillfile/conflict");
    assert!(
        conflict_file.exists(),
        "conflict file must be written on patch failure"
    );
    let state =
        skillfile_core::conflict::read_conflict(root).expect("conflict must be readable JSON");
    let state = state.expect("conflict state must be present");
    assert_eq!(state.entry, "my-skill");
    assert_eq!(state.entity_type, EntityType::Skill);
}

// ---------------------------------------------------------------------------
// install_patch_conflict_error_contains_sha
// ---------------------------------------------------------------------------

/// The error message produced on conflict identifies the entry by name.
///
/// The upstream-changes prefix `"upstream changes to 'my-skill'"` is the SHA
/// context: it tells the user which entry's upstream version introduced the
/// conflict. Verifying this string is present confirms the SHA context is
/// included in the error.
#[test]
fn install_patch_conflict_error_contains_sha() {
    let dirs = setup_conflict_fixture();
    let result = cmd_install(dirs.path(), &default_opts());

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("my-skill"),
        "error must name the conflicting entry; got: {err}"
    );
    assert!(
        err.contains("upstream"),
        "error must contain upstream context; got: {err}"
    );
}

// ---------------------------------------------------------------------------
// install_patch_conflict_error_has_resolve_hints
// ---------------------------------------------------------------------------

/// The conflict error message includes actionable `resolve` command hints.
///
/// Users must be able to recover from a conflict using the hints in the error
/// alone. The message must mention both `skillfile resolve` and
/// `skillfile resolve --abort`.
#[test]
fn install_patch_conflict_error_has_resolve_hints() {
    let dirs = setup_conflict_fixture();
    let result = cmd_install(dirs.path(), &default_opts());

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("resolve"),
        "error must include resolve hint; got: {err}"
    );
    assert!(
        err.contains("--abort"),
        "error must include resolve --abort hint; got: {err}"
    );
}

// ---------------------------------------------------------------------------
// install_unknown_platform_skips_gracefully
// ---------------------------------------------------------------------------

/// `cmd_install` does not return an error for an unknown adapter name.
///
/// `deploy_all` prints a warning and continues, so an unrecognised platform
/// must never cause a hard failure. This lets new platform names be added to a
/// Skillfile without breaking existing installs on machines that have an older
/// `skillfile` binary.
#[test]
fn install_unknown_platform_skips_gracefully() {
    let dirs = Dirs::new();
    write_unknown_adapter_skillfile(dirs.path());
    std::fs::create_dir_all(dirs.path().join(".skillfile/cache")).unwrap();

    let result = cmd_install(dirs.path(), &default_opts());

    assert!(
        result.is_ok(),
        "unknown adapter must not cause error; got: {:?}",
        result.unwrap_err()
    );
    assert!(
        !dirs.path().join(".skillfile/conflict").exists(),
        "no conflict file must be written for unknown adapter"
    );
}

#[test]
fn install_update_deploys_valid_cache_and_preserves_install_for_empty_cache() {
    let dirs = Dirs::new();
    let root = dirs.path();
    std::fs::write(
        root.join("Skillfile"),
        "github skill my-skill owner/repo skills/my-skill\n\
         github skill good-skill owner/repo skills/good-skill\n\
         install claude-code local\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join(".skillfile/cache/skills/my-skill")).unwrap();
    let good_cache = root.join(".skillfile/cache/skills/good-skill");
    std::fs::create_dir_all(&good_cache).unwrap();
    std::fs::write(good_cache.join("SKILL.md"), "# Deploy cached skill\n").unwrap();
    std::fs::write(good_cache.join(".meta"), r#"{"sha":"cached"}"#).unwrap();
    let installed = root.join(".claude/skills/my-skill/SKILL.md");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(&installed, "# Keep existing install\n").unwrap();
    std::fs::write(root.join("Skillfile.lock"), "not valid json").unwrap();

    let error = cmd_install(
        root,
        &CmdInstallOpts {
            dry_run: false,
            update: true,
            extra_targets: None,
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("lock"));
    assert_eq!(
        std::fs::read_to_string(installed).unwrap(),
        "# Keep existing install\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join(".claude/skills/good-skill/SKILL.md")).unwrap(),
        "# Deploy cached skill\n"
    );
}

#[test]
fn install_adds_missing_nested_files_without_overwriting_existing_files() {
    let dirs = Dirs::new();
    let root = dirs.path();
    std::fs::write(
        root.join("Skillfile"),
        "local skill my-skill skills/my-skill\ninstall claude-code local\n",
    )
    .unwrap();
    let source = root.join("skills/my-skill");
    std::fs::create_dir_all(source.join("scripts")).unwrap();
    std::fs::write(source.join("SKILL.md"), "# Upstream\n").unwrap();
    std::fs::write(source.join("scripts/new.py"), "print('new')\n").unwrap();
    let installed = root.join(".claude/skills/my-skill");
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::write(installed.join("SKILL.md"), "# Local edit\n").unwrap();

    cmd_install(root, &default_opts()).unwrap();

    assert_eq!(
        std::fs::read_to_string(installed.join("SKILL.md")).unwrap(),
        "# Local edit\n"
    );
    assert_eq!(
        std::fs::read_to_string(installed.join("scripts/new.py")).unwrap(),
        "print('new')\n"
    );
}

#[test]
fn install_rejects_duplicate_flat_agent_names_before_writing_target() {
    let dirs = Dirs::new();
    let root = dirs.path();
    std::fs::write(
        root.join("Skillfile"),
        "local agent team agents/team\ninstall claude-code local\n",
    )
    .unwrap();
    for source in ["backend/agent.md", "frontend/agent.md"] {
        let path = root.join("agents/team").join(source);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }

    let error = cmd_install(root, &default_opts()).unwrap_err().to_string();

    assert!(error.contains("agent.md"), "{error}");
    assert!(error.contains("backend/agent.md"), "{error}");
    assert!(error.contains("frontend/agent.md"), "{error}");
    assert!(!root.join(".claude/agents/agent.md").exists());
}

#[test]
fn update_rejects_cross_entry_flat_collision_without_replacing_local_edit() {
    let dirs = Dirs::new();
    let root = dirs.path();
    std::fs::write(
        root.join("Skillfile"),
        "local agent backend agents/backend\n\
         local agent frontend agents/frontend\n\
         install claude-code local\n",
    )
    .unwrap();
    for entry in ["backend", "frontend"] {
        let path = root.join("agents").join(entry).join("agent.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("# {entry}\n")).unwrap();
    }
    let installed = root.join(".claude/agents/agent.md");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(&installed, "# My local edit\n").unwrap();

    let error = cmd_install(
        root,
        &CmdInstallOpts {
            dry_run: false,
            update: true,
            extra_targets: None,
        },
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("backend:agent.md"), "{error}");
    assert!(error.contains("frontend:agent.md"), "{error}");
    assert_eq!(
        std::fs::read_to_string(installed).unwrap(),
        "# My local edit\n"
    );
}

#[test]
fn update_rejects_cached_flat_collision_before_auto_pin() {
    let dirs = Dirs::new();
    let root = dirs.path();
    std::fs::write(
        root.join("Skillfile"),
        "github agent team owner/repo agents/team\ninstall claude-code local\n",
    )
    .unwrap();
    let cache = root.join(".skillfile/cache/agents/team");
    for source in ["backend/agent.md", "frontend/agent.md"] {
        let path = cache.join(source);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("# {source}\n")).unwrap();
    }
    std::fs::write(cache.join(".meta"), r#"{"sha":"cached"}"#).unwrap();
    let locked = BTreeMap::from([(
        "github/agent/team".to_string(),
        LockEntry {
            sha: "cached".into(),
            raw_url: "https://example.invalid/team".into(),
        },
    )]);
    write_lock(root, &locked).unwrap();
    let installed = root.join(".claude/agents/agent.md");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(&installed, [0xff, 0xfe]).unwrap();

    let error = cmd_install(
        root,
        &CmdInstallOpts {
            dry_run: false,
            update: true,
            extra_targets: None,
        },
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("duplicate flat destination"), "{error}");
    assert_eq!(std::fs::read(installed).unwrap(), [0xff, 0xfe]);
    assert!(!root.join(".skillfile/patches").exists());
}

#[test]
fn install_rejects_flat_collision_between_file_and_directory_entries() {
    let dirs = Dirs::new();
    let root = dirs.path();
    std::fs::write(
        root.join("Skillfile"),
        "local agent agent agents/standalone.md\n\
         local agent team agents/team\n\
         install-path team-agents agent ./shared-agents\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("agents/team")).unwrap();
    std::fs::write(root.join("agents/standalone.md"), "# Standalone\n").unwrap();
    std::fs::write(root.join("agents/team/agent.md"), "# Team\n").unwrap();

    let error = cmd_install(root, &default_opts()).unwrap_err().to_string();

    assert!(error.contains("agent:standalone.md"), "{error}");
    assert!(error.contains("team:agent.md"), "{error}");
    assert!(!root.join("shared-agents/agent.md").exists());
}

fn target_aliases_agent_names(target: &Path) -> bool {
    // Probe the actual target directory rather than assuming an OS-wide case rule.
    let upper = target.join("Agent.md");
    let lower = target.join("agent.md");
    drop(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&upper)
            .unwrap(),
    );
    let names_alias = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lower)
    {
        Ok(file) => {
            drop(file);
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => true,
        Err(error) => panic!("cannot probe target filename behavior: {error}"),
    };
    std::fs::remove_file(&upper).unwrap();
    if !names_alias {
        std::fs::remove_file(&lower).unwrap();
    }
    eprintln!("actual target filename case alias: {names_alias}");
    names_alias
}

fn write_case_alias_agents(root: &Path) {
    std::fs::write(
        root.join("Skillfile"),
        "local agent team agents/team\ninstall claude-code local\n",
    )
    .unwrap();
    for (source, content) in [
        ("backend/Agent.md", "# Backend\n"),
        ("frontend/agent.md", "# Frontend\n"),
    ] {
        let path = root.join("agents/team").join(source);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
}

#[test]
fn install_rejects_flat_agent_names_that_alias_on_the_target_filesystem() {
    let dirs = Dirs::new();
    let root = dirs.path();
    let target = root.join(".claude/agents");
    std::fs::create_dir_all(&target).unwrap();
    let names_alias = target_aliases_agent_names(&target);
    let existing = target.join("existing.md");
    std::fs::write(&existing, "# User content\n").unwrap();
    write_case_alias_agents(root);

    let result = cmd_install(root, &default_opts());

    if names_alias {
        assert!(
            result.is_err(),
            "aliased names must be rejected before install"
        );
        let error = result.unwrap_err().to_string();
        assert!(error.contains("backend/Agent.md"), "{error}");
        assert!(error.contains("frontend/agent.md"), "{error}");
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
    } else {
        result.unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("Agent.md")).unwrap(),
            "# Backend\n"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("agent.md")).unwrap(),
            "# Frontend\n"
        );
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 3);
    }
    assert_eq!(
        std::fs::read_to_string(existing).unwrap(),
        "# User content\n"
    );
}

#[test]
fn install_cleans_new_target_after_case_alias_check() {
    let dirs = Dirs::new();
    let root = dirs.path();
    let target = root.join(".claude/agents");
    std::fs::create_dir_all(&target).unwrap();
    let names_alias = target_aliases_agent_names(&target);
    std::fs::remove_dir(&target).unwrap();
    std::fs::remove_dir(root.join(".claude")).unwrap();
    write_case_alias_agents(root);

    let result = cmd_install(root, &default_opts());

    if names_alias {
        let error = result.unwrap_err().to_string();
        assert!(error.contains("backend/Agent.md"), "{error}");
        assert!(error.contains("frontend/agent.md"), "{error}");
        assert!(!root.join(".claude").exists());
    } else {
        result.unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("Agent.md")).unwrap(),
            "# Backend\n"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("agent.md")).unwrap(),
            "# Frontend\n"
        );
        assert_eq!(std::fs::read_dir(target).unwrap().count(), 2);
    }
}

#[test]
fn dry_run_case_alias_check_does_not_create_target() {
    let dirs = Dirs::new();
    let root = dirs.path();
    write_case_alias_agents(root);

    cmd_install(
        root,
        &CmdInstallOpts {
            dry_run: true,
            ..default_opts()
        },
    )
    .unwrap();

    assert!(!root.join(".claude").exists());
}

#[test]
fn case_alias_validation_reports_non_directory_target_without_changing_it() {
    let dirs = Dirs::new();
    let root = dirs.path();
    let target = root.join(".claude/agents");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "# Existing target\n").unwrap();
    write_case_alias_agents(root);

    let error = cmd_install(root, &default_opts()).unwrap_err().to_string();

    assert!(
        error.contains("cannot validate flat destinations"),
        "{error}"
    );
    assert!(!error.contains("duplicate flat destination"), "{error}");
    assert_eq!(
        std::fs::read_to_string(target).unwrap(),
        "# Existing target\n"
    );
}
