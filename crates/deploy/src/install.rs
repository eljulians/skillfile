use std::collections::HashMap;
use std::path::{Path, PathBuf};

use skillfile_core::conflict::{read_conflict, write_conflict};
use skillfile_core::error::SkillfileError;
use skillfile_core::lock::{lock_key, read_lock};
use skillfile_core::models::{
    short_sha, ConflictState, Entry, InstallOptions, InstallTarget, Manifest,
};
use skillfile_core::parser::{parse_manifest, MANIFEST_NAME};
use skillfile_core::patch::{
    apply_patch_pure, dir_patch_path, generate_patch, has_patch, patches_root, read_patch,
    remove_patch, walkdir, write_dir_patch, write_patch,
};
use skillfile_core::progress;
use skillfile_sources::strategy::{content_file, is_dir_entry};
use skillfile_sources::sync::{cmd_sync, vendor_dir_for};

use crate::adapter::adapters;
use crate::paths::{installed_dir_files, installed_path, source_path};

// ---------------------------------------------------------------------------
// Patch application helpers
// ---------------------------------------------------------------------------

/// Convert a patch application error into PatchConflict for the given entry.
fn to_patch_conflict(err: &SkillfileError, entry_name: &str) -> SkillfileError {
    SkillfileError::PatchConflict {
        message: err.to_string(),
        entry_name: entry_name.to_string(),
    }
}

/// Rebase a patch file against a new cache: write the updated patch or remove it
/// if the upstream content already equals the patched result.
#[allow(clippy::too_many_arguments)]
fn rebase_single_patch(
    entry: &Entry,
    source: &Path,
    patched: &str,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    let cache_text = std::fs::read_to_string(source)?;
    let new_patch = generate_patch(&cache_text, patched, &format!("{}.md", entry.name));
    if new_patch.is_empty() {
        remove_patch(entry, repo_root)?;
    } else {
        write_patch(entry, &new_patch, repo_root)?;
    }
    Ok(())
}

/// Apply stored patch (if any) to a single installed file, then rebase the patch
/// against the new cache content so status comparisons remain correct.
#[allow(clippy::too_many_arguments)]
fn apply_single_file_patch(
    entry: &Entry,
    dest: &Path,
    source: &Path,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    if !has_patch(entry, repo_root) {
        return Ok(());
    }
    let patch_text = read_patch(entry, repo_root)?;
    let original = std::fs::read_to_string(dest)?;
    let patched =
        apply_patch_pure(&original, &patch_text).map_err(|e| to_patch_conflict(&e, &entry.name))?;
    std::fs::write(dest, &patched)?;

    // Rebase: regenerate patch against new cache so `diff` shows accurate deltas.
    rebase_single_patch(entry, source, &patched, repo_root)
}

/// Rebase a single dir-entry patch file; remove it if the diff becomes empty.
///
/// `rel` is the relative path key, `patch_file` the `.patch` file to update or
/// remove, `source_dir` the new cache dir, and `patched` the content after
/// applying the old patch.
#[allow(clippy::too_many_arguments)]
fn rebase_dir_patch(
    entry: &Entry,
    rel: &str,
    patch_file: &Path,
    source_dir: &Path,
    patched: &str,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    let cache_file = source_dir.join(rel);
    if !cache_file.exists() {
        return Ok(());
    }
    let cache_text = std::fs::read_to_string(&cache_file)?;
    let new_patch = generate_patch(&cache_text, patched, rel);
    if new_patch.is_empty() {
        std::fs::remove_file(patch_file)?;
    } else {
        write_dir_patch(entry, rel, &new_patch, repo_root)?;
    }
    Ok(())
}

/// Apply per-file patches to all installed files of a directory entry.
/// Rebases each patch against the new cache content after applying.
#[allow(clippy::too_many_arguments)]
fn apply_dir_patches(
    entry: &Entry,
    installed_files: &HashMap<String, PathBuf>,
    source_dir: &Path,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    let patches_dir = patches_root(repo_root)
        .join(entry.entity_type.dir_name())
        .join(&entry.name);
    if !patches_dir.is_dir() {
        return Ok(());
    }

    let patch_files: Vec<PathBuf> = walkdir(&patches_dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "patch"))
        .collect();

    for patch_file in patch_files {
        let Some(rel) = patch_file
            .strip_prefix(&patches_dir)
            .ok()
            .and_then(|p| p.to_str())
            .and_then(|s| s.strip_suffix(".patch"))
            .map(str::to_string)
        else {
            continue;
        };

        let Some(target) = installed_files.get(&rel).filter(|p| p.exists()) else {
            continue;
        };

        let patch_text = std::fs::read_to_string(&patch_file)?;
        let original = std::fs::read_to_string(target)?;
        let patched = apply_patch_pure(&original, &patch_text)
            .map_err(|e| to_patch_conflict(&e, &entry.name))?;
        std::fs::write(target, &patched)?;

        rebase_dir_patch(entry, &rel, &patch_file, source_dir, &patched, repo_root)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Auto-pin helpers (used by install --update)
// ---------------------------------------------------------------------------

/// Check whether applying `patch_text` to `cache_text` reproduces `installed_text`.
///
/// Returns `true` when the patch already describes the installed content (no re-pin
/// needed), or when the patch is inconsistent with the cache (preserve without
/// clobbering). Returns `false` when the installed content has edits beyond what
/// the patch captures.
fn patch_already_covers(patch_text: &str, cache_text: &str, installed_text: &str) -> bool {
    match apply_patch_pure(cache_text, patch_text) {
        Ok(expected) if installed_text == expected => true, // no new edits
        Err(_) => true,                                     // cache inconsistent — preserve
        Ok(_) => false,                                     // additional edits — fall through
    }
}

/// Return `true` if the stored patch already describes the installed content,
/// meaning no re-pin is needed.
#[allow(clippy::too_many_arguments)]
fn should_skip_pin(
    entry: &Entry,
    repo_root: &Path,
    cache_text: &str,
    installed_text: &str,
) -> bool {
    if !has_patch(entry, repo_root) {
        return false;
    }
    let Ok(pt) = read_patch(entry, repo_root) else {
        return false;
    };
    patch_already_covers(&pt, cache_text, installed_text)
}

/// Compare installed vs cache; write patch if they differ. Silent on missing prerequisites.
fn auto_pin_entry(entry: &Entry, manifest: &Manifest, repo_root: &Path) {
    if entry.source_type() == "local" {
        return;
    }

    let Ok(locked) = read_lock(repo_root) else {
        return;
    };
    let key = lock_key(entry);
    if !locked.contains_key(&key) {
        return;
    }

    let vdir = vendor_dir_for(entry, repo_root);

    if is_dir_entry(entry) {
        auto_pin_dir_entry(entry, manifest, repo_root, &vdir);
        return;
    }

    let cf = content_file(entry);
    if cf.is_empty() {
        return;
    }
    let cache_file = vdir.join(&cf);
    if !cache_file.exists() {
        return;
    }

    let Ok(dest) = installed_path(entry, manifest, repo_root) else {
        return;
    };
    if !dest.exists() {
        return;
    }

    let Ok(cache_text) = std::fs::read_to_string(&cache_file) else {
        return;
    };
    let Ok(installed_text) = std::fs::read_to_string(&dest) else {
        return;
    };

    // If already pinned, check if stored patch still describes the installed content exactly.
    if should_skip_pin(entry, repo_root, &cache_text, &installed_text) {
        return;
    }

    let patch_text = generate_patch(&cache_text, &installed_text, &format!("{}.md", entry.name));
    if !patch_text.is_empty() && write_patch(entry, &patch_text, repo_root).is_ok() {
        progress!(
            "  {}: local changes auto-saved to .skillfile/patches/",
            entry.name
        );
    }
}

/// Check a single cache file against its installed counterpart and generate/update
/// the patch if the user has modified the installed copy. Returns the filename if
/// a new patch was written.
struct AutoPinCtx<'a> {
    vdir: &'a Path,
    entry: &'a Entry,
    installed: &'a HashMap<String, PathBuf>,
    repo_root: &'a Path,
}

/// Return `true` if the dir-entry patch file at `patch_path` already describes
/// the transition from `cache_text` to `installed_text`.
fn dir_patch_already_matches(patch_path: &Path, cache_text: &str, installed_text: &str) -> bool {
    if !patch_path.exists() {
        return false;
    }
    let Ok(pt) = std::fs::read_to_string(patch_path) else {
        return false;
    };
    patch_already_covers(&pt, cache_text, installed_text)
}

fn try_auto_pin_file(cache_file: &Path, ctx: &AutoPinCtx<'_>) -> Option<String> {
    if cache_file.file_name().is_some_and(|n| n == ".meta") {
        return None;
    }
    let filename = cache_file
        .strip_prefix(ctx.vdir)
        .ok()?
        .to_str()?
        .to_string();
    let inst_path = match ctx.installed.get(&filename) {
        Some(p) if p.exists() => p,
        _ => return None,
    };

    let cache_text = std::fs::read_to_string(cache_file).ok()?;
    let installed_text = std::fs::read_to_string(inst_path).ok()?;

    // Check if stored dir patch still matches
    let p = dir_patch_path(ctx.entry, &filename, ctx.repo_root);
    if dir_patch_already_matches(&p, &cache_text, &installed_text) {
        return None;
    }

    let patch_text = generate_patch(&cache_text, &installed_text, &filename);
    if !patch_text.is_empty()
        && write_dir_patch(ctx.entry, &filename, &patch_text, ctx.repo_root).is_ok()
    {
        Some(filename)
    } else {
        None
    }
}

/// Auto-pin each modified file in a directory entry's installed copy.
#[allow(clippy::too_many_arguments)]
fn auto_pin_dir_entry(entry: &Entry, manifest: &Manifest, repo_root: &Path, vdir: &Path) {
    if !vdir.is_dir() {
        return;
    }
    let Ok(installed) = installed_dir_files(entry, manifest, repo_root) else {
        return;
    };
    if installed.is_empty() {
        return;
    }

    let ctx = AutoPinCtx {
        vdir,
        entry,
        installed: &installed,
        repo_root,
    };
    let pinned: Vec<String> = walkdir(vdir)
        .into_iter()
        .filter_map(|f| try_auto_pin_file(&f, &ctx))
        .collect();

    if !pinned.is_empty() {
        progress!(
            "  {}: local changes auto-saved to .skillfile/patches/ ({})",
            entry.name,
            pinned.join(", ")
        );
    }
}

// ---------------------------------------------------------------------------
// Core install entry point
// ---------------------------------------------------------------------------

/// Deploy one entry to its installed path via the platform adapter.
///
/// The adapter owns all platform-specific logic (target dirs, flat vs. nested).
/// This function handles cross-cutting concerns: source resolution,
/// missing-source warnings, and patch application.
///
/// Returns `Err(PatchConflict)` if a stored patch fails to apply cleanly.
#[allow(clippy::too_many_arguments)]
pub fn install_entry(
    entry: &Entry,
    target: &InstallTarget,
    repo_root: &Path,
    opts: Option<&InstallOptions>,
) -> Result<(), SkillfileError> {
    let default_opts = InstallOptions::default();
    let opts = opts.unwrap_or(&default_opts);

    let all_adapters = adapters();
    let Some(adapter) = all_adapters.get(&target.adapter) else {
        return Ok(());
    };

    if !adapter.supports(entry.entity_type) {
        return Ok(());
    }

    let source = match source_path(entry, repo_root) {
        Some(p) if p.exists() => p,
        _ => {
            eprintln!("  warning: source missing for {}, skipping", entry.name);
            return Ok(());
        }
    };

    let is_dir = is_dir_entry(entry) || source.is_dir();
    let installed = adapter.deploy_entry(entry, &source, target.scope, repo_root, opts);

    if !installed.is_empty() && !opts.dry_run {
        apply_entry_patches(entry, &installed, &source, is_dir, repo_root)?;
    }

    Ok(())
}

/// Dispatch to the correct patch-application strategy for a deployed entry.
#[allow(clippy::too_many_arguments)]
fn apply_entry_patches(
    entry: &Entry,
    installed: &HashMap<String, PathBuf>,
    source: &Path,
    is_dir: bool,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    if is_dir {
        apply_dir_patches(entry, installed, source, repo_root)
    } else {
        let key = format!("{}.md", entry.name);
        if let Some(dest) = installed.get(&key) {
            apply_single_file_patch(entry, dest, source, repo_root)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Precondition check
// ---------------------------------------------------------------------------

fn check_preconditions(manifest: &Manifest, repo_root: &Path) -> Result<(), SkillfileError> {
    if manifest.install_targets.is_empty() {
        return Err(SkillfileError::Manifest(
            "No install targets configured. Run `skillfile init` first.".into(),
        ));
    }

    if let Some(conflict) = read_conflict(repo_root)? {
        return Err(SkillfileError::Install(format!(
            "pending conflict for '{}' — \
             run `skillfile diff {}` to review, \
             or `skillfile resolve {}` to merge",
            conflict.entry, conflict.entry, conflict.entry
        )));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Deploy all entries, handling patch conflicts
// ---------------------------------------------------------------------------

/// Format a SHA transition hint for conflict error messages.
fn sha_transition_hint(old_sha: &str, new_sha: &str) -> String {
    if !old_sha.is_empty() && !new_sha.is_empty() && old_sha != new_sha {
        format!(
            "\n  upstream: {} \u{2192} {}",
            short_sha(old_sha),
            short_sha(new_sha)
        )
    } else {
        String::new()
    }
}

/// Shared lock maps threaded through `deploy_all` and `handle_patch_conflict`.
struct LockMaps<'a> {
    locked: &'a std::collections::BTreeMap<String, skillfile_core::models::LockEntry>,
    old_locked: &'a std::collections::BTreeMap<String, skillfile_core::models::LockEntry>,
}

/// Handle a patch conflict during deployment: write conflict state and return an error.
#[allow(clippy::too_many_arguments)]
fn handle_patch_conflict(
    entry: &Entry,
    entry_name: &str,
    repo_root: &Path,
    maps: &LockMaps<'_>,
) -> Result<(), SkillfileError> {
    let key = lock_key(entry);
    let old_sha = maps
        .old_locked
        .get(&key)
        .map(|l| l.sha.clone())
        .unwrap_or_default();
    let new_sha = maps
        .locked
        .get(&key)
        .map_or_else(|| old_sha.clone(), |l| l.sha.clone());

    write_conflict(
        repo_root,
        &ConflictState {
            entry: entry_name.to_string(),
            entity_type: entry.entity_type,
            old_sha: old_sha.clone(),
            new_sha: new_sha.clone(),
        },
    )?;

    let sha_info = sha_transition_hint(&old_sha, &new_sha);
    Err(SkillfileError::Install(format!(
        "upstream changes to '{entry_name}' conflict with your customisations.{sha_info}\n\
         Your pinned edits could not be applied to the new upstream version.\n\
         Run `skillfile diff {entry_name}` to review what changed upstream.\n\
         Run `skillfile resolve {entry_name}` when ready to merge.\n\
         Run `skillfile resolve --abort` to discard the conflict and keep the old version."
    )))
}

/// Install one entry and translate a `PatchConflict` into a full conflict error.
#[allow(clippy::too_many_arguments)]
fn install_entry_or_conflict(
    entry: &Entry,
    target: &InstallTarget,
    repo_root: &Path,
    opts: &InstallOptions,
    maps: &LockMaps<'_>,
) -> Result<(), SkillfileError> {
    match install_entry(entry, target, repo_root, Some(opts)) {
        Ok(()) => Ok(()),
        Err(SkillfileError::PatchConflict { entry_name, .. }) => {
            handle_patch_conflict(entry, &entry_name, repo_root, maps)
        }
        Err(e) => Err(e),
    }
}

#[allow(clippy::too_many_arguments)]
fn deploy_all(
    manifest: &Manifest,
    repo_root: &Path,
    opts: &InstallOptions,
    locked: &std::collections::BTreeMap<String, skillfile_core::models::LockEntry>,
    old_locked: &std::collections::BTreeMap<String, skillfile_core::models::LockEntry>,
) -> Result<(), SkillfileError> {
    let mode = if opts.dry_run { " [dry-run]" } else { "" };
    let all_adapters = adapters();
    let maps = LockMaps { locked, old_locked };

    for target in &manifest.install_targets {
        if !all_adapters.contains(&target.adapter) {
            eprintln!("warning: unknown platform '{}', skipping", target.adapter);
            continue;
        }
        progress!(
            "Installing for {} ({}){mode}...",
            target.adapter,
            target.scope
        );
        for entry in &manifest.entries {
            install_entry_or_conflict(entry, target, repo_root, opts, &maps)?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// cmd_install
// ---------------------------------------------------------------------------

/// Populate `manifest.install_targets` from `extra_targets` when the manifest
/// has none of its own. Emits a progress message when targets are injected.
fn apply_extra_targets(manifest: &mut Manifest, extra_targets: Option<&[InstallTarget]>) {
    let Some(targets) = extra_targets else {
        return;
    };
    if !targets.is_empty() {
        progress!("Using platform targets from personal config (Skillfile has no install lines).");
    }
    manifest.install_targets = targets.to_vec();
}

/// Parse the manifest, apply extra targets if the Skillfile has none, and
/// return the parsed manifest. Extracted to reduce cognitive complexity of
/// `cmd_install`.
fn load_manifest(
    repo_root: &Path,
    extra_targets: Option<&[InstallTarget]>,
) -> Result<Manifest, SkillfileError> {
    let manifest_path = repo_root.join(MANIFEST_NAME);
    if !manifest_path.exists() {
        return Err(SkillfileError::Manifest(format!(
            "{MANIFEST_NAME} not found in {}. Create one and run `skillfile init`.",
            repo_root.display()
        )));
    }

    let result = parse_manifest(&manifest_path)?;
    for w in &result.warnings {
        eprintln!("{w}");
    }
    let mut manifest = result.manifest;

    // If the Skillfile has no install targets, fall back to caller-provided targets
    // (e.g. from user-global config).
    if manifest.install_targets.is_empty() {
        apply_extra_targets(&mut manifest, extra_targets);
    }

    Ok(manifest)
}

/// Run the auto-pin pass over all entries when `--update` is requested.
fn auto_pin_all(manifest: &Manifest, repo_root: &Path) {
    for entry in &manifest.entries {
        auto_pin_entry(entry, manifest, repo_root);
    }
}

/// Print the first-install hint listing the configured platforms.
fn print_first_install_hint(manifest: &Manifest) {
    let platforms: Vec<String> = manifest
        .install_targets
        .iter()
        .map(|t| format!("{} ({})", t.adapter, t.scope))
        .collect();
    progress!("  Configured platforms: {}", platforms.join(", "));
    progress!("  Run `skillfile init` to add or change platforms.");
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_install(
    repo_root: &Path,
    dry_run: bool,
    update: bool,
    extra_targets: Option<&[InstallTarget]>,
) -> Result<(), SkillfileError> {
    let manifest = load_manifest(repo_root, extra_targets)?;

    check_preconditions(&manifest, repo_root)?;

    // Detect first install (cache dir absent → fresh clone or first run).
    let cache_dir = repo_root.join(".skillfile").join("cache");
    let first_install = !cache_dir.exists();

    // Read old locked state before sync (used for SHA context in conflict messages).
    let old_locked = read_lock(repo_root).unwrap_or_default();

    // Auto-pin local edits before re-fetching upstream (--update only).
    if update && !dry_run {
        auto_pin_all(&manifest, repo_root);
    }

    // Ensure cache dir exists (used as first-install marker and by sync).
    if !dry_run {
        std::fs::create_dir_all(&cache_dir)?;
    }

    // Fetch any missing or stale entries.
    cmd_sync(repo_root, dry_run, None, update)?;

    // Read new locked state (written by sync).
    let locked = read_lock(repo_root).unwrap_or_default();

    // Deploy to all configured platform targets.
    let opts = InstallOptions {
        dry_run,
        overwrite: update,
    };
    deploy_all(&manifest, repo_root, &opts, &locked, &old_locked)?;

    if !dry_run {
        progress!("Done.");

        // On first install, show configured platforms and hint about `init`.
        // Helps the clone scenario: user clones a repo with a Skillfile targeting
        // platforms they may not use, and needs to know how to add theirs.
        if first_install {
            print_first_install_hint(&manifest);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use skillfile_core::models::{EntityType, Entry, InstallTarget, Scope, SourceFields};

    fn make_agent_entry(name: &str) -> Entry {
        Entry {
            entity_type: EntityType::Agent,
            name: name.into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "agents/agent.md".into(),
                ref_: "main".into(),
            },
        }
    }

    fn make_local_entry(name: &str, path: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: name.into(),
            source: SourceFields::Local { path: path.into() },
        }
    }

    fn make_target(adapter: &str, scope: Scope) -> InstallTarget {
        InstallTarget {
            adapter: adapter.into(),
            scope,
        }
    }

    // -- install_entry: local source --

    #[test]
    fn install_local_entry_copy() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("skills/my-skill.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# My Skill").unwrap();

        let entry = make_local_entry("my-skill", "skills/my-skill.md");
        let target = make_target("claude-code", Scope::Local);
        install_entry(&entry, &target, dir.path(), None).unwrap();

        let dest = dir.path().join(".claude/skills/my-skill.md");
        assert!(dest.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# My Skill");
    }

    #[test]
    fn install_local_dir_entry_copy() {
        let dir = tempfile::tempdir().unwrap();
        // Local source is a directory (not a .md file)
        let source_dir = dir.path().join("skills/python-testing");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("SKILL.md"), "# Python Testing").unwrap();
        std::fs::write(source_dir.join("examples.md"), "# Examples").unwrap();

        let entry = make_local_entry("python-testing", "skills/python-testing");
        let target = make_target("claude-code", Scope::Local);
        install_entry(&entry, &target, dir.path(), None).unwrap();

        // Must be deployed as a directory (nested mode), not as a single .md file
        let dest = dir.path().join(".claude/skills/python-testing");
        assert!(dest.is_dir(), "local dir entry must deploy as directory");
        assert_eq!(
            std::fs::read_to_string(dest.join("SKILL.md")).unwrap(),
            "# Python Testing"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("examples.md")).unwrap(),
            "# Examples"
        );
        // Must NOT create a .md file at the target
        assert!(
            !dir.path().join(".claude/skills/python-testing.md").exists(),
            "should not create python-testing.md for a dir source"
        );
    }

    #[test]
    fn install_entry_dry_run_no_write() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("skills/my-skill.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# My Skill").unwrap();

        let entry = make_local_entry("my-skill", "skills/my-skill.md");
        let target = make_target("claude-code", Scope::Local);
        let opts = InstallOptions {
            dry_run: true,
            ..Default::default()
        };
        install_entry(&entry, &target, dir.path(), Some(&opts)).unwrap();

        let dest = dir.path().join(".claude/skills/my-skill.md");
        assert!(!dest.exists());
    }

    #[test]
    fn install_entry_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("skills/my-skill.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# New content").unwrap();

        let dest_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&dest_dir).unwrap();
        let dest = dest_dir.join("my-skill.md");
        std::fs::write(&dest, "# Old content").unwrap();

        let entry = make_local_entry("my-skill", "skills/my-skill.md");
        let target = make_target("claude-code", Scope::Local);
        install_entry(&entry, &target, dir.path(), None).unwrap();

        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# New content");
    }

    // -- install_entry: github (vendored) source --

    #[test]
    fn install_github_entry_copy() {
        let dir = tempfile::tempdir().unwrap();
        let vdir = dir.path().join(".skillfile/cache/agents/my-agent");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("agent.md"), "# Agent").unwrap();

        let entry = make_agent_entry("my-agent");
        let target = make_target("claude-code", Scope::Local);
        install_entry(&entry, &target, dir.path(), None).unwrap();

        let dest = dir.path().join(".claude/agents/my-agent.md");
        assert!(dest.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# Agent");
    }

    #[test]
    fn install_github_dir_entry_copy() {
        let dir = tempfile::tempdir().unwrap();
        let vdir = dir.path().join(".skillfile/cache/skills/python-pro");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), "# Python Pro").unwrap();
        std::fs::write(vdir.join("examples.md"), "# Examples").unwrap();

        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "python-pro".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "skills/python-pro".into(),
                ref_: "main".into(),
            },
        };
        let target = make_target("claude-code", Scope::Local);
        install_entry(&entry, &target, dir.path(), None).unwrap();

        let dest = dir.path().join(".claude/skills/python-pro");
        assert!(dest.is_dir());
        assert_eq!(
            std::fs::read_to_string(dest.join("SKILL.md")).unwrap(),
            "# Python Pro"
        );
    }

    #[test]
    fn install_agent_dir_entry_explodes_to_individual_files() {
        let dir = tempfile::tempdir().unwrap();
        let vdir = dir.path().join(".skillfile/cache/agents/core-dev");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("backend-developer.md"), "# Backend").unwrap();
        std::fs::write(vdir.join("frontend-developer.md"), "# Frontend").unwrap();
        std::fs::write(vdir.join(".meta"), "{}").unwrap();

        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "core-dev".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "categories/core-dev".into(),
                ref_: "main".into(),
            },
        };
        let target = make_target("claude-code", Scope::Local);
        install_entry(&entry, &target, dir.path(), None).unwrap();

        let agents_dir = dir.path().join(".claude/agents");
        assert_eq!(
            std::fs::read_to_string(agents_dir.join("backend-developer.md")).unwrap(),
            "# Backend"
        );
        assert_eq!(
            std::fs::read_to_string(agents_dir.join("frontend-developer.md")).unwrap(),
            "# Frontend"
        );
        // No "core-dev" directory should exist — flat mode
        assert!(!agents_dir.join("core-dev").exists());
    }

    #[test]
    fn install_entry_missing_source_warns() {
        let dir = tempfile::tempdir().unwrap();
        let entry = make_agent_entry("my-agent");
        let target = make_target("claude-code", Scope::Local);

        // Should return Ok without error — just a warning
        install_entry(&entry, &target, dir.path(), None).unwrap();
    }

    // -- Patch application during install --

    #[test]
    fn install_applies_existing_patch() {
        let dir = tempfile::tempdir().unwrap();

        // Set up cache
        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("test.md"), "# Test\n\nOriginal.\n").unwrap();

        // Write a patch
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "skills/test.md".into(),
                ref_: "main".into(),
            },
        };
        let patch_text = skillfile_core::patch::generate_patch(
            "# Test\n\nOriginal.\n",
            "# Test\n\nModified.\n",
            "test.md",
        );
        skillfile_core::patch::write_patch(&entry, &patch_text, dir.path()).unwrap();

        let target = make_target("claude-code", Scope::Local);
        install_entry(&entry, &target, dir.path(), None).unwrap();

        let dest = dir.path().join(".claude/skills/test.md");
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "# Test\n\nModified.\n"
        );
    }

    #[test]
    fn install_patch_conflict_returns_error() {
        let dir = tempfile::tempdir().unwrap();

        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        // Cache has completely different content from what the patch expects
        std::fs::write(vdir.join("test.md"), "totally different\ncontent\n").unwrap();

        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "skills/test.md".into(),
                ref_: "main".into(),
            },
        };
        // Write a patch that expects a line that doesn't exist
        let bad_patch =
            "--- a/test.md\n+++ b/test.md\n@@ -1 +1 @@\n-expected_original_line\n+modified\n";
        skillfile_core::patch::write_patch(&entry, bad_patch, dir.path()).unwrap();

        // Deploy the entry
        let installed_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(
            installed_dir.join("test.md"),
            "totally different\ncontent\n",
        )
        .unwrap();

        let target = make_target("claude-code", Scope::Local);
        let result = install_entry(&entry, &target, dir.path(), None);
        assert!(result.is_err());
        // Should be a PatchConflict error
        matches!(result.unwrap_err(), SkillfileError::PatchConflict { .. });
    }

    // -- Multi-adapter --

    #[test]
    fn install_local_skill_gemini_cli() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("skills/my-skill.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# My Skill").unwrap();

        let entry = make_local_entry("my-skill", "skills/my-skill.md");
        let target = make_target("gemini-cli", Scope::Local);
        install_entry(&entry, &target, dir.path(), None).unwrap();

        let dest = dir.path().join(".gemini/skills/my-skill.md");
        assert!(dest.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# My Skill");
    }

    #[test]
    fn install_local_skill_codex() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("skills/my-skill.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# My Skill").unwrap();

        let entry = make_local_entry("my-skill", "skills/my-skill.md");
        let target = make_target("codex", Scope::Local);
        install_entry(&entry, &target, dir.path(), None).unwrap();

        let dest = dir.path().join(".codex/skills/my-skill.md");
        assert!(dest.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# My Skill");
    }

    #[test]
    fn codex_skips_agent_entries() {
        let dir = tempfile::tempdir().unwrap();
        let entry = make_agent_entry("my-agent");
        let target = make_target("codex", Scope::Local);
        install_entry(&entry, &target, dir.path(), None).unwrap();

        assert!(!dir.path().join(".codex").exists());
    }

    #[test]
    fn install_github_agent_gemini_cli() {
        let dir = tempfile::tempdir().unwrap();
        let vdir = dir.path().join(".skillfile/cache/agents/my-agent");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("agent.md"), "# Agent").unwrap();

        let entry = make_agent_entry("my-agent");
        let target = make_target("gemini-cli", Scope::Local);
        install_entry(
            &entry,
            &target,
            dir.path(),
            Some(&InstallOptions::default()),
        )
        .unwrap();

        let dest = dir.path().join(".gemini/agents/my-agent.md");
        assert!(dest.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# Agent");
    }

    #[test]
    fn install_skill_multi_adapter() {
        for adapter in &["claude-code", "gemini-cli", "codex"] {
            let dir = tempfile::tempdir().unwrap();
            let source_file = dir.path().join("skills/my-skill.md");
            std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
            std::fs::write(&source_file, "# Multi Skill").unwrap();

            let entry = make_local_entry("my-skill", "skills/my-skill.md");
            let target = make_target(adapter, Scope::Local);
            install_entry(&entry, &target, dir.path(), None).unwrap();

            let prefix = match *adapter {
                "claude-code" => ".claude",
                "gemini-cli" => ".gemini",
                "codex" => ".codex",
                _ => unreachable!(),
            };
            let dest = dir.path().join(format!("{prefix}/skills/my-skill.md"));
            assert!(dest.exists(), "Failed for adapter {adapter}");
            assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# Multi Skill");
        }
    }

    // -- cmd_install --

    #[test]
    fn cmd_install_no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_install(dir.path(), false, false, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn cmd_install_no_install_targets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Skillfile"),
            "local  skill  foo  skills/foo.md\n",
        )
        .unwrap();

        let result = cmd_install(dir.path(), false, false, None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No install targets"));
    }

    #[test]
    fn cmd_install_extra_targets_fallback() {
        let dir = tempfile::tempdir().unwrap();
        // Skillfile with entries but NO install lines.
        std::fs::write(
            dir.path().join("Skillfile"),
            "local  skill  foo  skills/foo.md\n",
        )
        .unwrap();
        let source_file = dir.path().join("skills/foo.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# Foo").unwrap();

        // Pass extra targets — should be used as fallback.
        let targets = vec![make_target("claude-code", Scope::Local)];
        cmd_install(dir.path(), false, false, Some(&targets)).unwrap();

        let dest = dir.path().join(".claude/skills/foo.md");
        assert!(
            dest.exists(),
            "extra_targets must be used when Skillfile has none"
        );
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# Foo");
    }

    #[test]
    fn cmd_install_skillfile_targets_win_over_extra() {
        let dir = tempfile::tempdir().unwrap();
        // Skillfile WITH install lines.
        std::fs::write(
            dir.path().join("Skillfile"),
            "install  claude-code  local\nlocal  skill  foo  skills/foo.md\n",
        )
        .unwrap();
        let source_file = dir.path().join("skills/foo.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# Foo").unwrap();

        // Pass extra targets for gemini-cli — should be IGNORED (Skillfile wins).
        let targets = vec![make_target("gemini-cli", Scope::Local)];
        cmd_install(dir.path(), false, false, Some(&targets)).unwrap();

        // claude-code (from Skillfile) should be deployed.
        assert!(dir.path().join(".claude/skills/foo.md").exists());
        // gemini-cli (from extra_targets) should NOT be deployed.
        assert!(!dir.path().join(".gemini").exists());
    }

    #[test]
    fn cmd_install_dry_run_no_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Skillfile"),
            "install  claude-code  local\nlocal  skill  foo  skills/foo.md\n",
        )
        .unwrap();
        let source_file = dir.path().join("skills/foo.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# Foo").unwrap();

        cmd_install(dir.path(), true, false, None).unwrap();

        assert!(!dir.path().join(".claude").exists());
    }

    #[test]
    fn cmd_install_deploys_to_multiple_adapters() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Skillfile"),
            "install  claude-code  local\n\
             install  gemini-cli  local\n\
             install  codex  local\n\
             local  skill  foo  skills/foo.md\n\
             local  agent  bar  agents/bar.md\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("skills")).unwrap();
        std::fs::write(dir.path().join("skills/foo.md"), "# Foo").unwrap();
        std::fs::create_dir_all(dir.path().join("agents")).unwrap();
        std::fs::write(dir.path().join("agents/bar.md"), "# Bar").unwrap();

        cmd_install(dir.path(), false, false, None).unwrap();

        // skill deployed to all three adapters
        assert!(dir.path().join(".claude/skills/foo.md").exists());
        assert!(dir.path().join(".gemini/skills/foo.md").exists());
        assert!(dir.path().join(".codex/skills/foo.md").exists());

        // agent deployed to claude-code and gemini-cli but NOT codex
        assert!(dir.path().join(".claude/agents/bar.md").exists());
        assert!(dir.path().join(".gemini/agents/bar.md").exists());
        assert!(!dir.path().join(".codex/agents").exists());
    }

    #[test]
    fn cmd_install_pending_conflict_blocks() {
        use skillfile_core::conflict::write_conflict;
        use skillfile_core::models::ConflictState;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Skillfile"),
            "install  claude-code  local\nlocal  skill  foo  skills/foo.md\n",
        )
        .unwrap();

        write_conflict(
            dir.path(),
            &ConflictState {
                entry: "foo".into(),
                entity_type: EntityType::Skill,
                old_sha: "aaa".into(),
                new_sha: "bbb".into(),
            },
        )
        .unwrap();

        let result = cmd_install(dir.path(), false, false, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("pending conflict"));
    }

    // -----------------------------------------------------------------------
    // Helpers shared by the new tests below
    // -----------------------------------------------------------------------

    /// Build a single-file github skill Entry.
    fn make_skill_entry(name: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: name.into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: format!("skills/{name}.md"),
                ref_: "main".into(),
            },
        }
    }

    /// Build a directory github skill Entry (path_in_repo has no `.md` suffix).
    fn make_dir_skill_entry(name: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: name.into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: format!("skills/{name}"),
                ref_: "main".into(),
            },
        }
    }

    /// Write a minimal Skillfile + Skillfile.lock for a single single-file github skill.
    fn setup_github_skill_repo(dir: &std::path::Path, name: &str, cache_content: &str) {
        use skillfile_core::lock::write_lock;
        use skillfile_core::models::LockEntry;
        use std::collections::BTreeMap;

        // Manifest
        std::fs::write(
            dir.join("Skillfile"),
            format!("install  claude-code  local\ngithub  skill  {name}  owner/repo  skills/{name}.md\n"),
        )
        .unwrap();

        // Lock file — use write_lock so we don't need serde_json directly.
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();
        locked.insert(
            format!("github/skill/{name}"),
            LockEntry {
                sha: "abc123def456abc123def456abc123def456abc123".into(),
                raw_url: format!(
                    "https://raw.githubusercontent.com/owner/repo/abc123def456/skills/{name}.md"
                ),
            },
        );
        write_lock(dir, &locked).unwrap();

        // Vendor cache
        let vdir = dir.join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join(format!("{name}.md")), cache_content).unwrap();
    }

    // -----------------------------------------------------------------------
    // auto_pin_entry — single-file entry
    // -----------------------------------------------------------------------

    #[test]
    fn auto_pin_entry_local_is_skipped() {
        let dir = tempfile::tempdir().unwrap();

        // Local entry: auto_pin should be a no-op.
        let entry = make_local_entry("my-skill", "skills/my-skill.md");
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        // Provide installed file that differs from source — pin should NOT fire.
        let skills_dir = dir.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("my-skill.md"), "# Original\n").unwrap();

        auto_pin_entry(&entry, &manifest, dir.path());

        // No patch must have been written.
        assert!(
            !skillfile_core::patch::has_patch(&entry, dir.path()),
            "local entry must never be pinned"
        );
    }

    #[test]
    fn auto_pin_entry_missing_lock_is_skipped() {
        let dir = tempfile::tempdir().unwrap();

        let entry = make_skill_entry("test");
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        // No Skillfile.lock — should silently return without panicking.
        auto_pin_entry(&entry, &manifest, dir.path());

        assert!(!skillfile_core::patch::has_patch(&entry, dir.path()));
    }

    #[test]
    fn auto_pin_entry_missing_lock_key_is_skipped() {
        use skillfile_core::lock::write_lock;
        use skillfile_core::models::LockEntry;
        use std::collections::BTreeMap;

        let dir = tempfile::tempdir().unwrap();

        // Lock exists but for a different entry.
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();
        locked.insert(
            "github/skill/other".into(),
            LockEntry {
                sha: "aabbcc".into(),
                raw_url: "https://example.com/other.md".into(),
            },
        );
        write_lock(dir.path(), &locked).unwrap();

        let entry = make_skill_entry("test");
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        auto_pin_entry(&entry, &manifest, dir.path());

        assert!(!skillfile_core::patch::has_patch(&entry, dir.path()));
    }

    #[test]
    fn auto_pin_entry_writes_patch_when_installed_differs() {
        let dir = tempfile::tempdir().unwrap();
        let name = "my-skill";

        let cache_content = "# My Skill\n\nOriginal content.\n";
        let installed_content = "# My Skill\n\nUser-modified content.\n";

        setup_github_skill_repo(dir.path(), name, cache_content);

        // Place a modified installed file.
        let installed_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(installed_dir.join(format!("{name}.md")), installed_content).unwrap();

        let entry = make_skill_entry(name);
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        auto_pin_entry(&entry, &manifest, dir.path());

        assert!(
            skillfile_core::patch::has_patch(&entry, dir.path()),
            "patch should be written when installed differs from cache"
        );

        // The stored patch should round-trip: applying it to cache gives installed.
        let patch_text = skillfile_core::patch::read_patch(&entry, dir.path()).unwrap();
        let result = skillfile_core::patch::apply_patch_pure(cache_content, &patch_text).unwrap();
        assert_eq!(result, installed_content);
    }

    #[test]
    fn auto_pin_entry_no_repin_when_patch_already_describes_installed() {
        let dir = tempfile::tempdir().unwrap();
        let name = "my-skill";

        let cache_content = "# My Skill\n\nOriginal.\n";
        let installed_content = "# My Skill\n\nModified.\n";

        setup_github_skill_repo(dir.path(), name, cache_content);

        let entry = make_skill_entry(name);
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        // Pre-write the correct patch (cache → installed).
        let patch_text = skillfile_core::patch::generate_patch(
            cache_content,
            installed_content,
            &format!("{name}.md"),
        );
        skillfile_core::patch::write_patch(&entry, &patch_text, dir.path()).unwrap();

        // Write installed file that matches what the patch produces.
        let installed_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(installed_dir.join(format!("{name}.md")), installed_content).unwrap();

        // Record mtime of patch so we can detect if it changed.
        let patch_path = skillfile_core::patch::patch_path(&entry, dir.path());
        let mtime_before = std::fs::metadata(&patch_path).unwrap().modified().unwrap();

        // Small sleep so that any write would produce a different mtime.
        std::thread::sleep(std::time::Duration::from_millis(20));

        auto_pin_entry(&entry, &manifest, dir.path());

        let mtime_after = std::fs::metadata(&patch_path).unwrap().modified().unwrap();

        assert_eq!(
            mtime_before, mtime_after,
            "patch must not be rewritten when already up to date"
        );
    }

    #[test]
    fn auto_pin_entry_repins_when_installed_has_additional_edits() {
        let dir = tempfile::tempdir().unwrap();
        let name = "my-skill";

        let cache_content = "# My Skill\n\nOriginal.\n";
        let old_installed = "# My Skill\n\nFirst edit.\n";
        let new_installed = "# My Skill\n\nFirst edit.\n\nSecond edit.\n";

        setup_github_skill_repo(dir.path(), name, cache_content);

        let entry = make_skill_entry(name);
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        // Stored patch reflects the old installed state.
        let old_patch = skillfile_core::patch::generate_patch(
            cache_content,
            old_installed,
            &format!("{name}.md"),
        );
        skillfile_core::patch::write_patch(&entry, &old_patch, dir.path()).unwrap();

        // But the actual installed file has further edits.
        let installed_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(installed_dir.join(format!("{name}.md")), new_installed).unwrap();

        auto_pin_entry(&entry, &manifest, dir.path());

        // The patch should now reflect the new installed content.
        let new_patch = skillfile_core::patch::read_patch(&entry, dir.path()).unwrap();
        let result = skillfile_core::patch::apply_patch_pure(cache_content, &new_patch).unwrap();
        assert_eq!(
            result, new_installed,
            "updated patch must describe the latest installed content"
        );
    }

    // -----------------------------------------------------------------------
    // auto_pin_dir_entry
    // -----------------------------------------------------------------------

    #[test]
    fn auto_pin_dir_entry_writes_per_file_patches() {
        use skillfile_core::lock::write_lock;
        use skillfile_core::models::LockEntry;
        use std::collections::BTreeMap;

        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        // Manifest + lock (dir entry)
        std::fs::write(
            dir.path().join("Skillfile"),
            format!(
                "install  claude-code  local\ngithub  skill  {name}  owner/repo  skills/{name}\n"
            ),
        )
        .unwrap();
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();
        locked.insert(
            format!("github/skill/{name}"),
            LockEntry {
                sha: "deadbeefdeadbeefdeadbeef".into(),
                raw_url: format!("https://example.com/{name}"),
            },
        );
        write_lock(dir.path(), &locked).unwrap();

        // Vendor cache with two files.
        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), "# Lang Pro\n\nOriginal.\n").unwrap();
        std::fs::write(vdir.join("examples.md"), "# Examples\n\nOriginal.\n").unwrap();

        // Installed dir (nested mode for skills).
        let inst_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), "# Lang Pro\n\nModified.\n").unwrap();
        std::fs::write(inst_dir.join("examples.md"), "# Examples\n\nOriginal.\n").unwrap();

        let entry = make_dir_skill_entry(name);
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        auto_pin_entry(&entry, &manifest, dir.path());

        // Patch for the modified file should exist.
        let skill_patch = skillfile_core::patch::dir_patch_path(&entry, "SKILL.md", dir.path());
        assert!(skill_patch.exists(), "patch for SKILL.md must be written");

        // Patch for the unmodified file should NOT exist.
        let examples_patch =
            skillfile_core::patch::dir_patch_path(&entry, "examples.md", dir.path());
        assert!(
            !examples_patch.exists(),
            "patch for examples.md must not be written (content unchanged)"
        );
    }

    #[test]
    fn auto_pin_dir_entry_skips_when_vendor_dir_missing() {
        use skillfile_core::lock::write_lock;
        use skillfile_core::models::LockEntry;
        use std::collections::BTreeMap;

        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        // Write lock so we don't bail out there.
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();
        locked.insert(
            format!("github/skill/{name}"),
            LockEntry {
                sha: "abc".into(),
                raw_url: "https://example.com".into(),
            },
        );
        write_lock(dir.path(), &locked).unwrap();

        let entry = make_dir_skill_entry(name);
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        // No vendor dir — must silently return without panicking.
        auto_pin_entry(&entry, &manifest, dir.path());

        assert!(!skillfile_core::patch::has_dir_patch(&entry, dir.path()));
    }

    #[test]
    fn auto_pin_dir_entry_no_repin_when_patch_already_matches() {
        use skillfile_core::lock::write_lock;
        use skillfile_core::models::LockEntry;
        use std::collections::BTreeMap;

        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        let cache_content = "# Lang Pro\n\nOriginal.\n";
        let modified = "# Lang Pro\n\nModified.\n";

        // Write lock.
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();
        locked.insert(
            format!("github/skill/{name}"),
            LockEntry {
                sha: "abc".into(),
                raw_url: "https://example.com".into(),
            },
        );
        write_lock(dir.path(), &locked).unwrap();

        // Vendor cache.
        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), cache_content).unwrap();

        // Installed dir.
        let inst_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), modified).unwrap();

        let entry = make_dir_skill_entry(name);
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        // Pre-write the correct patch.
        let patch_text = skillfile_core::patch::generate_patch(cache_content, modified, "SKILL.md");
        skillfile_core::patch::write_dir_patch(&entry, "SKILL.md", &patch_text, dir.path())
            .unwrap();

        let patch_path = skillfile_core::patch::dir_patch_path(&entry, "SKILL.md", dir.path());
        let mtime_before = std::fs::metadata(&patch_path).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));

        auto_pin_entry(&entry, &manifest, dir.path());

        let mtime_after = std::fs::metadata(&patch_path).unwrap().modified().unwrap();

        assert_eq!(
            mtime_before, mtime_after,
            "dir patch must not be rewritten when already up to date"
        );
    }

    // -----------------------------------------------------------------------
    // apply_dir_patches
    // -----------------------------------------------------------------------

    #[test]
    fn apply_dir_patches_applies_patch_and_rebases() {
        let dir = tempfile::tempdir().unwrap();

        // Old upstream → user's installed version (what the stored patch records).
        let cache_content = "# Skill\n\nOriginal.\n";
        let installed_content = "# Skill\n\nModified.\n";
        // New upstream has a different body line but same structure.
        let new_cache_content = "# Skill\n\nOriginal v2.\n";
        // After rebase, the rebased patch encodes the diff from new_cache to installed.
        // Applying that rebased patch to new_cache must yield installed_content.
        let expected_rebased_to_new_cache = installed_content;

        let entry = make_dir_skill_entry("lang-pro");

        // Create patch dir with a valid patch (old cache → installed).
        let patch_text =
            skillfile_core::patch::generate_patch(cache_content, installed_content, "SKILL.md");
        skillfile_core::patch::write_dir_patch(&entry, "SKILL.md", &patch_text, dir.path())
            .unwrap();

        // Installed file starts at cache content (patch not yet applied).
        let inst_dir = dir.path().join(".claude/skills/lang-pro");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), cache_content).unwrap();

        // New cache (simulates upstream update).
        let new_cache_dir = dir.path().join(".skillfile/cache/skills/lang-pro");
        std::fs::create_dir_all(&new_cache_dir).unwrap();
        std::fs::write(new_cache_dir.join("SKILL.md"), new_cache_content).unwrap();

        // Build the installed_files map as deploy_all would.
        let mut installed_files = std::collections::HashMap::new();
        installed_files.insert("SKILL.md".to_string(), inst_dir.join("SKILL.md"));

        apply_dir_patches(&entry, &installed_files, &new_cache_dir, dir.path()).unwrap();

        // The installed file should have the original patch applied.
        let installed_after = std::fs::read_to_string(inst_dir.join("SKILL.md")).unwrap();
        assert_eq!(installed_after, installed_content);

        // The stored patch must now describe the diff from new_cache to installed_content.
        // Applying the rebased patch to new_cache must reproduce installed_content.
        let rebased_patch = std::fs::read_to_string(skillfile_core::patch::dir_patch_path(
            &entry,
            "SKILL.md",
            dir.path(),
        ))
        .unwrap();
        let rebase_result =
            skillfile_core::patch::apply_patch_pure(new_cache_content, &rebased_patch).unwrap();
        assert_eq!(
            rebase_result, expected_rebased_to_new_cache,
            "rebased patch applied to new_cache must reproduce installed_content"
        );
    }

    #[test]
    fn apply_dir_patches_removes_patch_when_rebase_yields_empty_diff() {
        let dir = tempfile::tempdir().unwrap();

        // The "new" cache content IS the patched content — patch becomes a no-op.
        let original = "# Skill\n\nOriginal.\n";
        let modified = "# Skill\n\nModified.\n";
        // New upstream == modified, so after applying patch the result equals new cache.
        let new_cache = modified; // upstream caught up

        let entry = make_dir_skill_entry("lang-pro");

        let patch_text = skillfile_core::patch::generate_patch(original, modified, "SKILL.md");
        skillfile_core::patch::write_dir_patch(&entry, "SKILL.md", &patch_text, dir.path())
            .unwrap();

        // Installed file starts at original (patch not yet applied).
        let inst_dir = dir.path().join(".claude/skills/lang-pro");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), original).unwrap();

        let new_cache_dir = dir.path().join(".skillfile/cache/skills/lang-pro");
        std::fs::create_dir_all(&new_cache_dir).unwrap();
        std::fs::write(new_cache_dir.join("SKILL.md"), new_cache).unwrap();

        let mut installed_files = std::collections::HashMap::new();
        installed_files.insert("SKILL.md".to_string(), inst_dir.join("SKILL.md"));

        apply_dir_patches(&entry, &installed_files, &new_cache_dir, dir.path()).unwrap();

        // Patch file must be removed (rebase produced empty diff).
        let patch_path = skillfile_core::patch::dir_patch_path(&entry, "SKILL.md", dir.path());
        assert!(
            !patch_path.exists(),
            "patch file must be removed when rebase yields empty diff"
        );
    }

    #[test]
    fn apply_dir_patches_no_op_when_no_patches_dir() {
        let dir = tempfile::tempdir().unwrap();

        // No patches directory at all.
        let entry = make_dir_skill_entry("lang-pro");
        let installed_files = std::collections::HashMap::new();
        let source_dir = dir.path().join(".skillfile/cache/skills/lang-pro");
        std::fs::create_dir_all(&source_dir).unwrap();

        // Must succeed without error.
        apply_dir_patches(&entry, &installed_files, &source_dir, dir.path()).unwrap();
    }

    // -----------------------------------------------------------------------
    // apply_single_file_patch — rebase removes patch when result equals cache
    // -----------------------------------------------------------------------

    #[test]
    fn apply_single_file_patch_removes_patch_when_rebase_is_empty() {
        let dir = tempfile::tempdir().unwrap();

        let original = "# Skill\n\nOriginal.\n";
        let modified = "# Skill\n\nModified.\n";
        // New cache == modified: after rebase, new_patch is empty → patch removed.
        let new_cache = modified;

        let entry = make_skill_entry("test");

        // Write patch.
        let patch_text = skillfile_core::patch::generate_patch(original, modified, "test.md");
        skillfile_core::patch::write_patch(&entry, &patch_text, dir.path()).unwrap();

        // Set up vendor cache (the "new" version).
        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        let source = vdir.join("test.md");
        std::fs::write(&source, new_cache).unwrap();

        // Installed file is the original (patch not yet applied).
        let installed_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&installed_dir).unwrap();
        let dest = installed_dir.join("test.md");
        std::fs::write(&dest, original).unwrap();

        apply_single_file_patch(&entry, &dest, &source, dir.path()).unwrap();

        // The installed file must be the patched (== new cache) result.
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), modified);

        // Patch file must have been removed.
        assert!(
            !skillfile_core::patch::has_patch(&entry, dir.path()),
            "patch must be removed when new cache already matches patched content"
        );
    }

    #[test]
    fn apply_single_file_patch_rewrites_patch_after_rebase() {
        let dir = tempfile::tempdir().unwrap();

        // Old upstream, user edit, new upstream (different body — no overlap with user edit).
        let original = "# Skill\n\nOriginal.\n";
        let modified = "# Skill\n\nModified.\n";
        let new_cache = "# Skill\n\nOriginal v2.\n";
        // The rebase stores generate_patch(new_cache, modified).
        // Applying that to new_cache must reproduce `modified`.
        let expected_rebased_result = modified;

        let entry = make_skill_entry("test");

        let patch_text = skillfile_core::patch::generate_patch(original, modified, "test.md");
        skillfile_core::patch::write_patch(&entry, &patch_text, dir.path()).unwrap();

        // New vendor cache (upstream updated).
        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        let source = vdir.join("test.md");
        std::fs::write(&source, new_cache).unwrap();

        // Installed still at original content (patch not applied yet).
        let installed_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&installed_dir).unwrap();
        let dest = installed_dir.join("test.md");
        std::fs::write(&dest, original).unwrap();

        apply_single_file_patch(&entry, &dest, &source, dir.path()).unwrap();

        // Installed must now be the patched content.
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), modified);

        // The patch is rebased: generate_patch(new_cache, modified).
        // Applying the rebased patch to new_cache must reproduce modified.
        assert!(
            skillfile_core::patch::has_patch(&entry, dir.path()),
            "rebased patch must still exist (new_cache != modified)"
        );
        let rebased = skillfile_core::patch::read_patch(&entry, dir.path()).unwrap();
        let result = skillfile_core::patch::apply_patch_pure(new_cache, &rebased).unwrap();
        assert_eq!(
            result, expected_rebased_result,
            "rebased patch applied to new_cache must reproduce installed content"
        );
    }

    // -----------------------------------------------------------------------
    // check_preconditions
    // -----------------------------------------------------------------------

    #[test]
    fn check_preconditions_no_targets_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![],
            install_targets: vec![],
        };
        let result = check_preconditions(&manifest, dir.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No install targets"));
    }

    #[test]
    fn check_preconditions_pending_conflict_returns_error() {
        use skillfile_core::conflict::write_conflict;
        use skillfile_core::models::ConflictState;

        let dir = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        write_conflict(
            dir.path(),
            &ConflictState {
                entry: "my-skill".into(),
                entity_type: EntityType::Skill,
                old_sha: "aaa".into(),
                new_sha: "bbb".into(),
            },
        )
        .unwrap();

        let result = check_preconditions(&manifest, dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("pending conflict"));
    }

    #[test]
    fn check_preconditions_ok_with_target_and_no_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };
        check_preconditions(&manifest, dir.path()).unwrap();
    }

    // -----------------------------------------------------------------------
    // deploy_all — PatchConflict writes conflict state and returns Install error
    // -----------------------------------------------------------------------

    fn setup_deploy_all_conflict_scenario(
        dir: &std::path::Path,
        name: &str,
    ) -> (
        Entry,
        Manifest,
        std::collections::BTreeMap<String, skillfile_core::models::LockEntry>,
        std::collections::BTreeMap<String, skillfile_core::models::LockEntry>,
    ) {
        use skillfile_core::models::LockEntry;
        use std::collections::BTreeMap;

        // Vendor cache: content that cannot match the stored patch.
        let vdir = dir.join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(
            vdir.join(format!("{name}.md")),
            "totally different content\n",
        )
        .unwrap();

        // Write a patch that expects lines which don't exist.
        let entry = make_skill_entry(name);
        let bad_patch =
            "--- a/test.md\n+++ b/test.md\n@@ -1,1 +1,1 @@\n-expected_original_line\n+modified\n";
        skillfile_core::patch::write_patch(&entry, bad_patch, dir).unwrap();

        // Pre-create installed file.
        let inst_dir = dir.join(".claude/skills");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(
            inst_dir.join(format!("{name}.md")),
            "totally different content\n",
        )
        .unwrap();

        // Manifest.
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        let lock_key_str = format!("github/skill/{name}");
        let old_sha = "a".repeat(40);
        let new_sha = "b".repeat(40);

        let mut old_locked: BTreeMap<String, LockEntry> = BTreeMap::new();
        old_locked.insert(
            lock_key_str.clone(),
            LockEntry {
                sha: old_sha.clone(),
                raw_url: "https://example.com/old.md".into(),
            },
        );

        let mut new_locked: BTreeMap<String, LockEntry> = BTreeMap::new();
        new_locked.insert(
            lock_key_str,
            LockEntry {
                sha: new_sha.clone(),
                raw_url: "https://example.com/new.md".into(),
            },
        );

        (entry, manifest, new_locked, old_locked)
    }

    #[test]
    fn deploy_all_patch_conflict_writes_conflict_state() {
        use skillfile_core::conflict::{has_conflict, read_conflict};
        use skillfile_core::lock::write_lock;

        let dir = tempfile::tempdir().unwrap();
        let name = "test";

        let (_, manifest, new_locked, old_locked) =
            setup_deploy_all_conflict_scenario(dir.path(), name);

        write_lock(dir.path(), &new_locked).unwrap();

        let old_sha = old_locked
            .get(&format!("github/skill/{name}"))
            .map(|l| l.sha.clone())
            .unwrap_or_default();
        let new_sha = new_locked
            .get(&format!("github/skill/{name}"))
            .map(|l| l.sha.clone())
            .unwrap_or_default();

        let opts = InstallOptions {
            dry_run: false,
            overwrite: true,
        };

        let result = deploy_all(&manifest, dir.path(), &opts, &new_locked, &old_locked);

        // Must return an error.
        assert!(
            result.is_err(),
            "deploy_all must return Err on PatchConflict"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("conflict"),
            "error message must mention conflict: {err_msg}"
        );

        // Conflict state file must have been written.
        assert!(
            has_conflict(dir.path()),
            "conflict state file must be written after PatchConflict"
        );

        let conflict = read_conflict(dir.path()).unwrap().unwrap();
        assert_eq!(conflict.entry, name);
        assert_eq!(conflict.old_sha, old_sha);
        assert_eq!(conflict.new_sha, new_sha);
    }

    #[test]
    fn deploy_all_patch_conflict_error_message_contains_sha_context() {
        use skillfile_core::lock::write_lock;
        use skillfile_core::models::LockEntry;
        use std::collections::BTreeMap;

        let dir = tempfile::tempdir().unwrap();
        let name = "test";

        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join(format!("{name}.md")), "different\n").unwrap();

        let entry = make_skill_entry(name);
        let bad_patch =
            "--- a/test.md\n+++ b/test.md\n@@ -1,1 +1,1 @@\n-nonexistent_line\n+other\n";
        skillfile_core::patch::write_patch(&entry, bad_patch, dir.path()).unwrap();

        let inst_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join(format!("{name}.md")), "different\n").unwrap();

        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        let lock_key_str = format!("github/skill/{name}");
        let old_sha = "aabbccddeeff001122334455aabbccddeeff0011".to_string();
        let new_sha = "99887766554433221100ffeeddccbbaa99887766".to_string();

        let mut old_locked: BTreeMap<String, LockEntry> = BTreeMap::new();
        old_locked.insert(
            lock_key_str.clone(),
            LockEntry {
                sha: old_sha.clone(),
                raw_url: "https://example.com/old.md".into(),
            },
        );

        let mut new_locked: BTreeMap<String, LockEntry> = BTreeMap::new();
        new_locked.insert(
            lock_key_str,
            LockEntry {
                sha: new_sha.clone(),
                raw_url: "https://example.com/new.md".into(),
            },
        );

        write_lock(dir.path(), &new_locked).unwrap();

        let opts = InstallOptions {
            dry_run: false,
            overwrite: true,
        };

        let result = deploy_all(&manifest, dir.path(), &opts, &new_locked, &old_locked);
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();

        // The error must include the short-SHA arrow notation.
        assert!(
            err_msg.contains('\u{2192}'),
            "error message must contain the SHA arrow (→): {err_msg}"
        );
        // Must contain truncated SHAs.
        assert!(
            err_msg.contains(&old_sha[..12]),
            "error must contain old SHA prefix: {err_msg}"
        );
        assert!(
            err_msg.contains(&new_sha[..12]),
            "error must contain new SHA prefix: {err_msg}"
        );
    }

    #[test]
    fn deploy_all_patch_conflict_error_message_has_resolve_hints() {
        use skillfile_core::lock::write_lock;
        use skillfile_core::models::LockEntry;
        use std::collections::BTreeMap;

        let dir = tempfile::tempdir().unwrap();
        let name = "test";

        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join(format!("{name}.md")), "different\n").unwrap();

        let entry = make_skill_entry(name);
        let bad_patch =
            "--- a/test.md\n+++ b/test.md\n@@ -1,1 +1,1 @@\n-nonexistent_line\n+other\n";
        skillfile_core::patch::write_patch(&entry, bad_patch, dir.path()).unwrap();

        let inst_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join(format!("{name}.md")), "different\n").unwrap();

        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        let lock_key_str = format!("github/skill/{name}");
        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();
        locked.insert(
            lock_key_str,
            LockEntry {
                sha: "abc123".into(),
                raw_url: "https://example.com/test.md".into(),
            },
        );
        write_lock(dir.path(), &locked).unwrap();

        let opts = InstallOptions {
            dry_run: false,
            overwrite: true,
        };

        let result = deploy_all(
            &manifest,
            dir.path(),
            &opts,
            &locked,
            &BTreeMap::new(), // no old lock
        );
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("skillfile resolve"),
            "error must mention resolve command: {err_msg}"
        );
        assert!(
            err_msg.contains("skillfile diff"),
            "error must mention diff command: {err_msg}"
        );
        assert!(
            err_msg.contains("--abort"),
            "error must mention --abort: {err_msg}"
        );
    }

    #[test]
    fn deploy_all_unknown_platform_skips_gracefully() {
        use std::collections::BTreeMap;

        let dir = tempfile::tempdir().unwrap();

        // Manifest with an unknown adapter.
        let manifest = Manifest {
            entries: vec![],
            install_targets: vec![InstallTarget {
                adapter: "unknown-tool".into(),
                scope: Scope::Local,
            }],
        };

        let opts = InstallOptions {
            dry_run: false,
            overwrite: true,
        };

        // Must succeed even with unknown adapter (just warns).
        deploy_all(
            &manifest,
            dir.path(),
            &opts,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
    }
}
