use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use skillfile_core::conflict::{read_conflict, write_conflict};
use skillfile_core::error::SkillfileError;
use skillfile_core::lock::{lock_key, read_lock};
use skillfile_core::models::{
    short_sha, ConflictState, EntityType, Entry, InstallOptions, InstallTarget, Manifest,
    SourceFields,
};
use skillfile_core::parser::{parse_manifest, MANIFEST_NAME};
use skillfile_core::patch::{
    apply_patch_pure, dir_patch_path, generate_patch, has_patch, patch_path, patches_root,
    read_patch, relative_file_key, remove_dir_patch, remove_patch, text_content_eq, walkdir,
    write_dir_patch, write_patch,
};
use skillfile_core::progress;
use skillfile_sources::strategy::{content_file, is_cached_dir_entry, is_dir_entry};
use skillfile_sources::sync::{cmd_sync, vendor_dir_for};

use crate::adapter::{ensure_no_symlink_components, DeployRequest, DirInstallMode};
use crate::paths::source_path;
use crate::target::ResolvedInstallTarget;

static SNAPSHOT_COUNTER: AtomicU64 = AtomicU64::new(0);
static FLAT_NAME_PROBE_COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Patch application helpers
// ---------------------------------------------------------------------------

fn to_patch_conflict(err: &SkillfileError, entry_name: &str) -> SkillfileError {
    SkillfileError::PatchConflict {
        message: err.to_string(),
        entry_name: entry_name.to_string(),
    }
}

struct PatchCtx<'a> {
    entry: &'a Entry,
    repo_root: &'a Path,
}

/// Rebase a patch file against a new cache: write the updated patch or remove it
/// if the upstream content already equals the patched result.
fn rebase_single_patch(
    ctx: &PatchCtx<'_>,
    source: &Path,
    patched: &str,
) -> Result<(), SkillfileError> {
    let cache_text = std::fs::read_to_string(source)?;
    let new_patch = generate_patch(&cache_text, patched, &format!("{}.md", ctx.entry.name));
    if new_patch.is_empty() {
        remove_patch(ctx.entry, ctx.repo_root)?;
    } else {
        write_patch(ctx.entry, &new_patch, ctx.repo_root)?;
    }
    Ok(())
}

/// Apply stored patch (if any) to a single installed file, then rebase the patch
/// against the new cache content so status comparisons remain correct.
fn apply_single_file_patch(
    ctx: &PatchCtx<'_>,
    dest: &Path,
    source: &Path,
) -> Result<(), SkillfileError> {
    if !has_patch(ctx.entry, ctx.repo_root) {
        return Ok(());
    }
    let patch_text = read_patch(ctx.entry, ctx.repo_root)?;
    let original = std::fs::read_to_string(dest)?;
    let patched = apply_patch_pure(&original, &patch_text)
        .map_err(|e| to_patch_conflict(&e, &ctx.entry.name))?;
    std::fs::write(dest, &patched)?;

    // Rebase: regenerate patch against new cache so `diff` shows accurate deltas.
    rebase_single_patch(ctx, source, &patched)
}

/// Apply per-file patches to all installed files of a directory entry.
/// Rebases each patch against the new cache content after applying.
fn apply_dir_patches(
    ctx: &PatchCtx<'_>,
    installed_files: &HashMap<String, PathBuf>,
    source_dir: &Path,
) -> Result<(), SkillfileError> {
    let patches_dir = patches_root(ctx.repo_root)
        .join(ctx.entry.entity_type.dir_name())
        .join(&ctx.entry.name);
    if !patches_dir.is_dir() {
        return Ok(());
    }

    let patch_files: Vec<PathBuf> = walkdir(&patches_dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "patch"))
        .collect();

    for patch_file in patch_files {
        let Some(rel) = relative_file_key(&patches_dir, &patch_file)
            .as_deref()
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
            .map_err(|e| to_patch_conflict(&e, &ctx.entry.name))?;
        std::fs::write(target, &patched)?;

        // Rebase: regenerate patch against new cache content.
        let cache_file = source_dir.join(&rel);
        if !cache_file.exists() {
            continue;
        }
        let cache_text = std::fs::read_to_string(&cache_file)?;
        let new_patch = generate_patch(&cache_text, &patched, &rel);
        if new_patch.is_empty() {
            std::fs::remove_file(&patch_file)?;
            continue;
        }
        let canonical_patch = dir_patch_path(ctx.entry, &rel, ctx.repo_root);
        write_dir_patch(&canonical_patch, &new_patch)?;
        if patch_file != canonical_patch {
            std::fs::remove_file(&patch_file)?;
        }
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
        Ok(expected) if text_content_eq(installed_text, &expected) => true, // no new edits
        Err(_) => true, // cache inconsistent — preserve
        Ok(_) => false, // additional edits — fall through
    }
}

fn should_skip_pin(ctx: &PatchCtx<'_>, cache_text: &str, installed_text: &str) -> bool {
    if !has_patch(ctx.entry, ctx.repo_root) {
        return false;
    }
    let Ok(pt) = read_patch(ctx.entry, ctx.repo_root) else {
        return false;
    };
    patch_already_covers(&pt, cache_text, installed_text)
}

fn divergent_auto_pin_error(entry_name: &str, labels: &[String]) -> SkillfileError {
    SkillfileError::Install(format!(
        "'{entry_name}' has divergent edits across install targets: {} — reconcile them before running `skillfile install --update`",
        labels.join(", ")
    ))
}

struct SingleInstalledVariant {
    label: String,
    content: String,
}

fn installed_single_file_variants(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<Vec<SingleInstalledVariant>, SkillfileError> {
    let mut variants = Vec::new();
    for target in &manifest.install_targets {
        let resolved = ResolvedInstallTarget::from_target(target)?;
        if !resolved.supports(entry.entity_type) {
            continue;
        }
        let path = resolved.installed_path(entry, repo_root);
        if ensure_no_symlink_components(&path).is_err() || !path.exists() {
            continue;
        }
        variants.push(SingleInstalledVariant {
            label: target.to_string(),
            content: std::fs::read_to_string(path)?,
        });
    }
    Ok(variants)
}

fn representative_single_file_content(
    entry_name: &str,
    cache_text: &str,
    variants: &[SingleInstalledVariant],
) -> Result<Option<String>, SkillfileError> {
    let modified: Vec<&SingleInstalledVariant> = variants
        .iter()
        .filter(|variant| !text_content_eq(&variant.content, cache_text))
        .collect();
    if modified.is_empty() {
        return Ok(None);
    }
    let representative = &modified[0].content;
    if modified
        .iter()
        .any(|variant| !text_content_eq(&variant.content, representative))
    {
        let labels: Vec<String> = modified
            .iter()
            .map(|variant| variant.label.clone())
            .collect();
        return Err(divergent_auto_pin_error(entry_name, &labels));
    }
    Ok(Some(representative.clone()))
}

struct AutoPinSingleCtx<'a> {
    entry: &'a Entry,
    manifest: &'a Manifest,
    repo_root: &'a Path,
    cache_file: &'a Path,
}

fn auto_pin_single_file_entry(ctx: &AutoPinSingleCtx<'_>) -> Result<(), SkillfileError> {
    let cache_text = std::fs::read_to_string(ctx.cache_file)?;
    let variants = installed_single_file_variants(ctx.entry, ctx.manifest, ctx.repo_root)?;
    let Some(installed_text) =
        representative_single_file_content(&ctx.entry.name, &cache_text, &variants)?
    else {
        return Ok(());
    };

    let patch_ctx = PatchCtx {
        entry: ctx.entry,
        repo_root: ctx.repo_root,
    };
    if should_skip_pin(&patch_ctx, &cache_text, &installed_text) {
        return Ok(());
    }

    let patch_text = generate_patch(
        &cache_text,
        &installed_text,
        &format!("{}.md", ctx.entry.name),
    );
    if !patch_text.is_empty() && write_patch(ctx.entry, &patch_text, ctx.repo_root).is_ok() {
        progress!(
            "  {}: local changes auto-saved to .skillfile/patches/",
            ctx.entry.name
        );
    }
    Ok(())
}

fn auto_pin_entry(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    if entry.source_type() == "local" {
        return Ok(());
    }

    let Ok(locked) = read_lock(repo_root) else {
        return Ok(());
    };
    let key = lock_key(entry);
    if !locked.contains_key(&key) {
        return Ok(());
    }

    let vdir = vendor_dir_for(entry, repo_root);

    if is_cached_dir_entry(entry, &vdir) {
        return auto_pin_dir_entry(entry, manifest, repo_root);
    }

    let cf = content_file(entry);
    if cf.is_empty() {
        return Ok(());
    }
    let cache_file = vdir.join(&cf);
    if !cache_file.exists() {
        return Ok(());
    }
    auto_pin_single_file_entry(&AutoPinSingleCtx {
        entry,
        manifest,
        repo_root,
        cache_file: &cache_file,
    })
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

fn load_cache_files(vdir: &Path) -> BTreeMap<String, PathBuf> {
    walkdir(vdir)
        .into_iter()
        .filter(|cache_file| cache_file.file_name().is_none_or(|name| name != ".meta"))
        .filter_map(|cache_file| {
            let filename = relative_file_key(vdir, &cache_file)?;
            Some((filename, cache_file))
        })
        .collect()
}

struct DirInstalledVariant {
    label: String,
    files: HashMap<String, PathBuf>,
}

type DirModifiedMap = BTreeMap<String, String>;

fn installed_dir_variants(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<Vec<DirInstalledVariant>, SkillfileError> {
    let mut variants = Vec::new();
    for target in &manifest.install_targets {
        let resolved = ResolvedInstallTarget::from_target(target)?;
        if !resolved.supports(entry.entity_type) {
            continue;
        }
        let files = resolved.installed_dir_files(entry, repo_root);
        if files.is_empty() {
            continue;
        }
        variants.push(DirInstalledVariant {
            label: target.to_string(),
            files,
        });
    }
    Ok(variants)
}

fn modified_dir_content(
    cache_files: &BTreeMap<String, PathBuf>,
    variant: &DirInstalledVariant,
) -> Result<DirModifiedMap, SkillfileError> {
    let mut modified = BTreeMap::new();
    for (filename, cache_file) in cache_files {
        let Some(installed_path) = variant.files.get(filename).filter(|path| path.exists()) else {
            continue;
        };
        let cache_text = std::fs::read_to_string(cache_file)?;
        let installed_text = std::fs::read_to_string(installed_path)?;
        if !text_content_eq(&installed_text, &cache_text) {
            modified.insert(filename.clone(), installed_text);
        }
    }
    Ok(modified)
}

fn dir_modified_content_eq(left: &DirModifiedMap, right: &DirModifiedMap) -> bool {
    left.len() == right.len()
        && left.iter().all(|(filename, content)| {
            right
                .get(filename)
                .is_some_and(|other| text_content_eq(content, other))
        })
}

fn representative_dir_changes(
    entry_name: &str,
    cache_files: &BTreeMap<String, PathBuf>,
    variants: &[DirInstalledVariant],
) -> Result<Option<DirModifiedMap>, SkillfileError> {
    let mut modified = Vec::new();
    for variant in variants {
        let changed = modified_dir_content(cache_files, variant)?;
        if !changed.is_empty() {
            modified.push((variant.label.clone(), changed));
        }
    }
    if modified.is_empty() {
        return Ok(None);
    }
    let representative = &modified[0].1;
    if modified
        .iter()
        .any(|(_, changed)| !dir_modified_content_eq(changed, representative))
    {
        let labels: Vec<String> = modified.iter().map(|(label, _)| label.clone()).collect();
        return Err(divergent_auto_pin_error(entry_name, &labels));
    }
    Ok(Some(representative.clone()))
}

struct WriteDirPatchesCtx<'a> {
    entry: &'a Entry,
    repo_root: &'a Path,
    cache_files: &'a BTreeMap<String, PathBuf>,
    representative: &'a DirModifiedMap,
}

fn write_auto_pin_dir_patches(ctx: &WriteDirPatchesCtx<'_>) -> Result<Vec<String>, SkillfileError> {
    let mut pinned = Vec::new();
    for (filename, cache_file) in ctx.cache_files {
        let Some(installed_text) = ctx.representative.get(filename) else {
            remove_dir_patch(ctx.entry, filename, ctx.repo_root)?;
            continue;
        };

        let cache_text = std::fs::read_to_string(cache_file)?;
        let patch_path = dir_patch_path(ctx.entry, filename, ctx.repo_root);
        if dir_patch_already_matches(&patch_path, &cache_text, installed_text) {
            continue;
        }

        let patch_text = generate_patch(&cache_text, installed_text, filename);
        if patch_text.is_empty() {
            continue;
        }

        write_dir_patch(&patch_path, &patch_text)?;
        pinned.push(filename.clone());
    }
    Ok(pinned)
}

fn auto_pin_dir_entry(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    let vdir = &vendor_dir_for(entry, repo_root);
    if !vdir.is_dir() {
        return Ok(());
    }
    let installed = installed_dir_variants(entry, manifest, repo_root)?;
    if installed.is_empty() {
        return Ok(());
    }
    let cache_files = load_cache_files(vdir);
    let Some(representative) = representative_dir_changes(&entry.name, &cache_files, &installed)?
    else {
        return Ok(());
    };
    let pinned = write_auto_pin_dir_patches(&WriteDirPatchesCtx {
        entry,
        repo_root,
        cache_files: &cache_files,
        representative: &representative,
    })?;

    if !pinned.is_empty() {
        progress!(
            "  {}: local changes auto-saved to .skillfile/patches/ ({})",
            entry.name,
            pinned.join(", ")
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Core install entry point
// ---------------------------------------------------------------------------

pub struct InstallCtx<'a> {
    pub repo_root: &'a Path,
    pub opts: Option<&'a InstallOptions>,
}

/// Why an install target was skipped instead of being updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallSkipReason {
    UnknownAdapter,
    UnsupportedEntity,
    MissingSource,
    NothingDeployed,
    DryRun,
}

/// Outcome of attempting to deploy one entry to one install target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed,
    Skipped(InstallSkipReason),
}

pub fn install_entry(
    entry: &Entry,
    target: &InstallTarget,
    ctx: &InstallCtx<'_>,
) -> Result<(), SkillfileError> {
    let _ = install_entry_with_outcome(entry, target, ctx)?;
    Ok(())
}

fn install_failure(entry: &Entry, target: &InstallTarget, detail: &str) -> SkillfileError {
    SkillfileError::Install(format!(
        "failed to install '{}' to {target}: {detail}",
        entry.name
    ))
}

struct PathSnapshot {
    live_path: PathBuf,
    snapshot_path: Option<PathBuf>,
}

#[derive(Default)]
pub struct InstallSnapshot {
    scratch_dir: Option<PathBuf>,
    paths: Vec<PathSnapshot>,
    preserve_scratch: Cell<bool>,
}

impl Drop for InstallSnapshot {
    fn drop(&mut self) {
        if self.preserve_scratch.get() {
            return;
        }
        if let Some(path) = &self.scratch_dir {
            let _ = std::fs::remove_dir_all(path);
            remove_empty_dir(path.parent());
        }
    }
}

fn remove_empty_dir(path: Option<&Path>) {
    let Some(path) = path else {
        return;
    };
    if path
        .read_dir()
        .is_ok_and(|mut entries| entries.next().is_none())
    {
        let _ = std::fs::remove_dir(path);
    }
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    let file_type = metadata.file_type();
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt as _;

        if file_type.is_symlink_dir() {
            return std::fs::remove_dir(path);
        }
    }
    if file_type.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path, _is_dir: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path, is_dir: bool) -> std::io::Result<()> {
    if is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

#[cfg(windows)]
fn symlink_is_dir(path: &Path) -> std::io::Result<bool> {
    use std::os::windows::fs::FileTypeExt as _;

    Ok(std::fs::symlink_metadata(path)?
        .file_type()
        .is_symlink_dir())
}

#[cfg(windows)]
fn copy_symlink(source: &Path, dest: &Path) -> std::io::Result<()> {
    let target = std::fs::read_link(source)?;
    create_symlink(&target, dest, symlink_is_dir(source)?)
}

#[cfg(not(windows))]
fn copy_symlink(source: &Path, dest: &Path) -> std::io::Result<()> {
    let target = std::fs::read_link(source)?;
    create_symlink(&target, dest, false)
}

fn copy_path(source: &Path, dest: &Path) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        copy_symlink(source, dest)
    } else if metadata.is_dir() {
        copy_snapshot_dir(source, dest)
    } else {
        std::fs::copy(source, dest).map(|_| ())
    }
}

fn copy_snapshot_dir(source: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        copy_path(&source_path, &dest_path)?;
    }
    Ok(())
}

fn snapshot_scratch_dir(repo_root: &Path) -> std::io::Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let seq = SNAPSHOT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = repo_root.join(".skillfile").join("tmp").join(format!(
        "install-snapshot-{}-{stamp}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn capture_path_snapshot(
    live_path: PathBuf,
    scratch_dir: &Path,
    index: usize,
) -> std::io::Result<PathSnapshot> {
    let snapshot_path = if live_path.exists() || live_path.is_symlink() {
        let snapshot_path = scratch_dir.join(index.to_string());
        copy_path(&live_path, &snapshot_path)?;
        Some(snapshot_path)
    } else {
        None
    };
    Ok(PathSnapshot {
        live_path,
        snapshot_path,
    })
}

impl InstallSnapshot {
    fn capture(repo_root: &Path, paths: Vec<PathBuf>) -> Result<Self, SkillfileError> {
        let mut seen = HashSet::new();
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter(|path| seen.insert(path.clone()))
            .collect();
        if paths.is_empty() {
            return Ok(Self::default());
        }

        let scratch_dir = snapshot_scratch_dir(repo_root)?;
        let mut snapshot = Self {
            scratch_dir: Some(scratch_dir.clone()),
            paths: Vec::new(),
            preserve_scratch: Cell::new(false),
        };
        for (index, path) in paths.into_iter().enumerate() {
            snapshot
                .paths
                .push(capture_path_snapshot(path, &scratch_dir, index)?);
        }
        Ok(snapshot)
    }

    pub fn restore(&self) -> Result<(), SkillfileError> {
        for snapshot in self.paths.iter().rev() {
            self.restore_path(snapshot)?;
        }
        Ok(())
    }

    fn scratch_dir(&self) -> Option<&Path> {
        self.scratch_dir.as_deref()
    }

    fn restore_path(&self, snapshot: &PathSnapshot) -> Result<(), SkillfileError> {
        let result = restore_path_snapshot(snapshot);
        if result.is_err() {
            self.preserve_scratch.set(true);
        }
        result.map_err(SkillfileError::from)
    }
}

fn restore_path_snapshot(snapshot: &PathSnapshot) -> std::io::Result<()> {
    remove_path(&snapshot.live_path)?;
    let Some(snapshot_path) = &snapshot.snapshot_path else {
        return Ok(());
    };
    copy_path(snapshot_path, &snapshot.live_path)
}

struct InstallValidationCtx<'a> {
    entry: &'a Entry,
    target: &'a InstallTarget,
    repo_root: &'a Path,
    source: &'a Path,
    resolved_target: ResolvedInstallTarget<'a>,
    is_dir: bool,
    opts: &'a InstallOptions,
}

struct InstallPlan {
    expected: HashMap<String, PathBuf>,
    existing_before: HashSet<String>,
}

struct ValidatedInstall {
    installed: HashMap<String, PathBuf>,
    outcome: InstallOutcome,
}

fn flat_expected_paths(source: &Path, target_dir: &Path) -> HashMap<String, PathBuf> {
    walkdir(source)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .filter_map(|path| {
            let name = path.file_name()?;
            let key = relative_file_key(source, &path)?;
            Some((key, target_dir.join(name)))
        })
        .collect()
}

fn nested_expected_paths(source: &Path, dest_root: &Path) -> HashMap<String, PathBuf> {
    walkdir(source)
        .into_iter()
        .filter(|path| path.file_name().is_none_or(|name| name != ".meta"))
        .filter_map(|path| {
            let rel = path.strip_prefix(source).ok()?;
            let key = relative_file_key(source, &path)?;
            Some((key, dest_root.join(rel)))
        })
        .collect()
}

fn planned_install_paths(ctx: &InstallValidationCtx<'_>) -> HashMap<String, PathBuf> {
    let target_dir = ctx
        .resolved_target
        .target_dir(ctx.entry.entity_type, ctx.repo_root);
    if !ctx.is_dir {
        let key = format!("{}.md", ctx.entry.name);
        return HashMap::from([(
            key,
            ctx.resolved_target.installed_path(ctx.entry, ctx.repo_root),
        )]);
    }

    match ctx.resolved_target.dir_mode(ctx.entry.entity_type) {
        Some(DirInstallMode::Flat) => flat_expected_paths(ctx.source, &target_dir),
        _ => nested_expected_paths(ctx.source, &target_dir.join(&ctx.entry.name)),
    }
}

fn patch_effect_path(ctx: &InstallValidationCtx<'_>) -> PathBuf {
    if ctx.is_dir {
        patches_root(ctx.repo_root)
            .join(ctx.entry.entity_type.dir_name())
            .join(&ctx.entry.name)
    } else {
        patch_path(ctx.entry, ctx.repo_root)
    }
}

fn install_effect_paths(ctx: &InstallValidationCtx<'_>) -> Vec<PathBuf> {
    let target_dir = ctx
        .resolved_target
        .target_dir(ctx.entry.entity_type, ctx.repo_root);
    let mut paths = if !ctx.is_dir {
        vec![ctx.resolved_target.installed_path(ctx.entry, ctx.repo_root)]
    } else if ctx.resolved_target.dir_mode(ctx.entry.entity_type) == Some(DirInstallMode::Flat) {
        flat_expected_paths(ctx.source, &target_dir)
            .into_values()
            .collect()
    } else {
        vec![target_dir.join(&ctx.entry.name)]
    };

    if matches!(ctx.target, InstallTarget::Platform { .. })
        && ctx.resolved_target.dir_mode(ctx.entry.entity_type) != Some(DirInstallMode::Flat)
    {
        paths.push(target_dir.join(format!("{}.md", ctx.entry.name)));
    }
    paths.push(patch_effect_path(ctx));
    paths
}

fn ensure_safe_install_effect_paths(ctx: &InstallValidationCtx<'_>) -> Result<(), SkillfileError> {
    for path in install_effect_paths(ctx) {
        if let Err(error) = ensure_no_symlink_components(&path) {
            return Err(install_failure(ctx.entry, ctx.target, &error.to_string()));
        }
    }
    Ok(())
}

fn ensure_existing_safe_dir(path: &Path) -> io::Result<()> {
    // The shared guard permits OS-managed top-level aliases such as macOS /var.
    ensure_no_symlink_components(path)?;
    if std::fs::metadata(path)?.is_dir() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{} is not a safe directory",
            path.display()
        )))
    }
}

fn create_missing_target_dirs(target: &Path, created: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut missing = Vec::new();
    let mut cursor = target;
    while !cursor.as_os_str().is_empty() {
        match std::fs::symlink_metadata(cursor) {
            Ok(_) => {
                ensure_existing_safe_dir(cursor)?;
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(cursor.to_path_buf());
                cursor = cursor
                    .parent()
                    .ok_or_else(|| io::Error::other("target has no parent directory"))?;
            }
            Err(error) => return Err(error),
        }
    }

    for dir in missing.into_iter().rev() {
        ensure_no_symlink_components(&dir)?;
        match std::fs::create_dir(&dir) {
            Ok(()) => created.push(dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                ensure_existing_safe_dir(&dir)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn probe_marker_visible(path: &Path, expected: &[u8]) -> io::Result<bool> {
    match std::fs::read(path) {
        Ok(actual) if actual == expected => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected validation marker at {}", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn reserve_flat_probe_stage(target: &Path) -> io::Result<(PathBuf, String)> {
    for _ in 0..32 {
        let counter = FLAT_NAME_PROBE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!(".skillfile-CaSe-{}-{nanos}-{counter}", std::process::id());
        let stage = target.join(&name);
        let Err(error) = std::fs::create_dir(&stage) else {
            return Ok((stage, name));
        };
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "cannot reserve a flat destination validation directory",
    ))
}

fn record_probe_cleanup_error(path: &Path, result: io::Result<()>, errors: &mut Vec<String>) {
    if let Err(error) = result {
        if error.kind() != io::ErrorKind::NotFound {
            errors.push(format!("{}: {error}", path.display()));
        }
    }
}

fn read_probe_source_index(path: &Path, current: usize) -> io::Result<usize> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected validation entry",
        ));
    }
    let first = std::fs::read_to_string(path)?
        .parse::<usize>()
        .map_err(|_| io::Error::other("invalid validation source index"))?;
    if first >= current {
        return Err(io::Error::other("invalid validation source index"));
    }
    Ok(first)
}

fn describe_probe_collision(basename: &OsString, first: &str, second: &str) -> String {
    let mut sources = [first.to_owned(), second.to_owned()];
    sources.sort();
    format!("{:?} from {sources:?}", basename.to_string_lossy())
}

struct FlatDestinationProbe<'a> {
    target: &'a Path,
    stage: Option<PathBuf>,
    files: Vec<PathBuf>,
    created_dirs: Vec<PathBuf>,
}

impl Drop for FlatDestinationProbe<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

impl<'a> FlatDestinationProbe<'a> {
    fn new(target: &'a Path) -> Self {
        Self {
            target,
            stage: None,
            files: Vec::new(),
            created_dirs: Vec::new(),
        }
    }

    fn prepare(&mut self) -> io::Result<()> {
        ensure_no_symlink_components(self.target)?;
        create_missing_target_dirs(self.target, &mut self.created_dirs)?;
        let (stage, name) = reserve_flat_probe_stage(self.target)?;
        self.stage = Some(stage);
        self.calibrate(&name)
    }

    fn calibrate(&mut self, stage_name: &str) -> io::Result<()> {
        let stage = self.stage.as_ref().expect("validation directory exists");
        let marker_name = ".skillfile-MaRkEr";
        let marker = stage.join(marker_name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)?;
        self.files.push(marker.clone());
        file.write_all(stage_name.as_bytes())?;
        drop(file);

        // A direct child can inherit different case rules from its parent.
        let parent_alias = self
            .target
            .join(stage_name.replacen("CaSe", "cAsE", 1))
            .join(marker_name);
        let parent_insensitive = probe_marker_visible(&parent_alias, stage_name.as_bytes())?;
        let stage_insensitive =
            probe_marker_visible(&stage.join(".skillfile-mArKeR"), stage_name.as_bytes())?;
        if parent_insensitive != stage_insensitive {
            return Err(io::Error::other(
                "cannot reliably validate target directory filename behavior",
            ));
        }
        std::fs::remove_file(&marker)?;
        self.files.pop();
        Ok(())
    }

    fn stage_name(&mut self, index: usize, basename: &OsString) -> io::Result<Option<usize>> {
        let stage = self.stage.as_ref().expect("validation directory exists");
        let path = stage.join(basename);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                self.files.push(path);
                file.write_all(index.to_string().as_bytes())?;
                Ok(None)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                read_probe_source_index(&path, index).map(Some)
            }
            Err(error) => Err(error),
        }
    }

    fn cleanup_stage(&self, errors: &mut Vec<String>) {
        let Some(stage) = &self.stage else {
            return;
        };
        if let Err(error) = ensure_no_symlink_components(stage) {
            errors.push(format!("{}: {error}", stage.display()));
            return;
        }
        for file in self.files.iter().rev() {
            record_probe_cleanup_error(file, std::fs::remove_file(file), errors);
        }
        record_probe_cleanup_error(stage, std::fs::remove_dir(stage), errors);
    }

    fn cleanup_created_dirs(&self, errors: &mut Vec<String>) {
        for dir in self.created_dirs.iter().rev() {
            match ensure_no_symlink_components(dir) {
                Ok(()) => record_probe_cleanup_error(dir, std::fs::remove_dir(dir), errors),
                Err(error) => errors.push(format!("{}: {error}", dir.display())),
            }
        }
    }

    fn cleanup(&mut self) -> io::Result<()> {
        let mut errors = Vec::new();
        self.cleanup_stage(&mut errors);
        self.cleanup_created_dirs(&mut errors);
        if errors.is_empty() {
            self.stage = None;
            self.files.clear();
            self.created_dirs.clear();
            Ok(())
        } else {
            Err(io::Error::other(errors.join("; ")))
        }
    }
}

fn find_flat_probe_collision(
    probe: &mut FlatDestinationProbe<'_>,
    names: &[(OsString, String)],
) -> io::Result<Option<String>> {
    for (index, (basename, source)) in names.iter().enumerate() {
        let Some(first) = probe.stage_name(index, basename)? else {
            continue;
        };
        return Ok(Some(describe_probe_collision(
            basename,
            &names[first].1,
            source,
        )));
    }
    Ok(None)
}

fn validate_actual_flat_names(
    target: &InstallTarget,
    target_dir: &Path,
    names: &[(OsString, String)],
) -> Result<(), SkillfileError> {
    let mut probe = FlatDestinationProbe::new(target_dir);
    let outcome = probe
        .prepare()
        .and_then(|()| find_flat_probe_collision(&mut probe, names));
    let cleanup = probe.cleanup();
    let failure = match outcome {
        Ok(Some(collision)) => Some(format!(
            "failed to install to {target}: duplicate flat destination filename(s): {collision}"
        )),
        Ok(None) => None,
        Err(error) => Some(format!(
            "failed to install to {target}: cannot validate flat destinations in {}: {error}",
            target_dir.display()
        )),
    };
    match (failure, cleanup) {
        (None, Ok(())) => Ok(()),
        (Some(message), Ok(())) => Err(SkillfileError::Install(message)),
        (None, Err(error)) => Err(SkillfileError::Install(format!(
            "failed to install to {target}: validation cleanup failed: {error}"
        ))),
        (Some(message), Err(error)) => Err(SkillfileError::Install(format!(
            "{message}; validation cleanup failed: {error}"
        ))),
    }
}

struct FlatValidationCtx<'a> {
    entries: &'a [Entry],
    target: &'a InstallTarget,
    repo_root: &'a Path,
    dry_run: bool,
}

fn collect_flat_destinations(
    ctx: &FlatValidationCtx<'_>,
) -> Result<BTreeMap<PathBuf, Vec<String>>, SkillfileError> {
    let Ok(resolved) = ResolvedInstallTarget::from_target(ctx.target) else {
        return Ok(BTreeMap::new()); // Unknown built-in targets are skipped.
    };
    let mut sources_by_destination: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();

    for entry in ctx.entries {
        if !resolved.supports(entry.entity_type)
            || resolved.dir_mode(entry.entity_type) != Some(DirInstallMode::Flat)
        {
            continue;
        }
        let Some(source) = source_path(entry, ctx.repo_root) else {
            continue;
        };
        ensure_no_symlink_components(&source)
            .map_err(|error| install_failure(entry, ctx.target, &error.to_string()))?;
        if !source.exists() {
            continue;
        }

        let paths = if is_dir_entry(entry) || source.is_dir() {
            flat_expected_paths(
                &source,
                &resolved.target_dir(entry.entity_type, ctx.repo_root),
            )
        } else {
            let name = source
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            HashMap::from([(name, resolved.installed_path(entry, ctx.repo_root))])
        };
        for (relative, destination) in paths {
            sources_by_destination
                .entry(destination)
                .or_default()
                .push(format!("{}:{relative}", entry.name));
        }
    }
    Ok(sources_by_destination)
}

fn literal_flat_collisions(sources_by_destination: &BTreeMap<PathBuf, Vec<String>>) -> Vec<String> {
    sources_by_destination
        .iter()
        .filter_map(|(destination, sources)| {
            if sources.len() < 2 {
                return None;
            }
            let mut sources = sources.clone();
            sources.sort();
            let basename = destination
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            Some(format!("{basename:?} from {sources:?}"))
        })
        .collect()
}

fn validate_actual_flat_groups(
    target: &InstallTarget,
    sources_by_destination: BTreeMap<PathBuf, Vec<String>>,
) -> Result<(), SkillfileError> {
    let mut names_by_target: BTreeMap<PathBuf, Vec<(OsString, String)>> = BTreeMap::new();
    for (destination, sources) in sources_by_destination {
        let Some((parent, basename)) = destination.parent().zip(destination.file_name()) else {
            continue;
        };
        for source in sources {
            names_by_target
                .entry(parent.to_path_buf())
                .or_default()
                .push((basename.to_os_string(), source));
        }
    }
    for (target_dir, mut names) in names_by_target {
        if names.len() < 2 {
            continue;
        }
        names.sort();
        validate_actual_flat_names(target, &target_dir, &names)?;
    }
    Ok(())
}

fn ensure_unique_flat_target_destinations(
    ctx: &FlatValidationCtx<'_>,
) -> Result<(), SkillfileError> {
    let sources_by_destination = collect_flat_destinations(ctx)?;
    let collisions = literal_flat_collisions(&sources_by_destination);
    if !collisions.is_empty() {
        return Err(SkillfileError::Install(format!(
            "failed to install to {}: duplicate flat destination filename(s): {}",
            ctx.target,
            collisions.join("; ")
        )));
    }
    if ctx.dry_run {
        return Ok(());
    }
    validate_actual_flat_groups(ctx.target, sources_by_destination)
}

/// Check all entries for each target before deployment can change installed files.
pub fn ensure_unique_flat_install_destinations(
    manifest: &Manifest,
    repo_root: &Path,
    dry_run: bool,
) -> Result<(), SkillfileError> {
    for target in &manifest.install_targets {
        ensure_unique_flat_target_destinations(&FlatValidationCtx {
            entries: &manifest.entries,
            target,
            repo_root,
            dry_run,
        })?;
    }
    Ok(())
}

pub fn capture_install_snapshot(
    entry: &Entry,
    targets: &[InstallTarget],
    repo_root: &Path,
) -> Result<InstallSnapshot, SkillfileError> {
    let mut paths = Vec::new();
    for target in targets {
        paths.extend(install_effect_paths_for_target(entry, target, repo_root));
    }
    InstallSnapshot::capture(repo_root, paths)
}

fn install_effect_paths_for_target(
    entry: &Entry,
    target: &InstallTarget,
    repo_root: &Path,
) -> Vec<PathBuf> {
    let Ok(resolved_target) = ResolvedInstallTarget::from_target(target) else {
        return Vec::new();
    };
    if !resolved_target.supports(entry.entity_type) {
        return Vec::new();
    }
    let Some(source) = source_path(entry, repo_root) else {
        return Vec::new();
    };
    if ensure_no_symlink_components(&source).is_err() || !source.exists() {
        return Vec::new();
    }
    let default_opts = InstallOptions::default();
    let validation_ctx = InstallValidationCtx {
        entry,
        target,
        repo_root,
        source: &source,
        resolved_target,
        is_dir: is_dir_entry(entry) || source.is_dir(),
        opts: &default_opts,
    };
    install_effect_paths(&validation_ctx)
}

fn build_install_plan(ctx: &InstallValidationCtx<'_>) -> InstallPlan {
    let expected = planned_install_paths(ctx);
    let existing_before = expected
        .iter()
        .filter_map(|(key, path)| path.is_file().then_some(key.clone()))
        .collect();
    InstallPlan {
        expected,
        existing_before,
    }
}

fn cleanup_created_files(plan: &InstallPlan, installed: &HashMap<String, PathBuf>) {
    for (key, path) in installed {
        if plan.existing_before.contains(key) {
            continue;
        }
        let _ = std::fs::remove_file(path);
    }
}

fn validate_installed_files(
    ctx: &InstallValidationCtx<'_>,
    plan: &InstallPlan,
    installed: HashMap<String, PathBuf>,
) -> Result<ValidatedInstall, SkillfileError> {
    let reported_count = installed.len();
    let existing = installed
        .into_iter()
        .filter(|(_, path)| path.is_file())
        .collect::<HashMap<_, _>>();

    if existing.len() != reported_count {
        return Err(install_failure(
            ctx.entry,
            ctx.target,
            "adapter reported installed files that do not exist on disk",
        ));
    }

    let expected = plan.expected.len();
    if expected == 0 {
        return Ok(ValidatedInstall {
            installed: existing,
            outcome: InstallOutcome::Skipped(InstallSkipReason::NothingDeployed),
        });
    }

    let present = plan.expected.values().filter(|path| path.is_file()).count();
    if present == 0 {
        cleanup_created_files(plan, &existing);
        return Err(install_failure(
            ctx.entry,
            ctx.target,
            "no files were written to the target platform directory",
        ));
    }

    if present < expected {
        cleanup_created_files(plan, &existing);
        return Err(install_failure(
            ctx.entry,
            ctx.target,
            &format!("only {present} of {expected} expected file(s) were written"),
        ));
    }

    let outcome = if ctx.opts.overwrite || plan.existing_before.len() != expected {
        InstallOutcome::Installed
    } else {
        InstallOutcome::Skipped(InstallSkipReason::NothingDeployed)
    };
    Ok(ValidatedInstall {
        installed: existing,
        outcome,
    })
}

fn restore_on_install_error(snapshot: &InstallSnapshot, error: SkillfileError) -> SkillfileError {
    match snapshot.restore() {
        Ok(()) => error,
        Err(rollback_error) => {
            let snapshot_hint = snapshot.scratch_dir().map_or_else(String::new, |path| {
                format!("; rollback snapshot kept at {}", path.display())
            });
            let rollback_detail = format!(
                "rollback failed: {rollback_error}{snapshot_hint}; target may need manual cleanup"
            );
            match error {
                SkillfileError::PatchConflict {
                    message,
                    entry_name,
                } => SkillfileError::PatchConflict {
                    message: format!("{message}; {rollback_detail}"),
                    entry_name,
                },
                other => SkillfileError::Install(format!("{other}; {rollback_detail}")),
            }
        }
    }
}

fn deploy_and_patch_entry(
    validation_ctx: &InstallValidationCtx<'_>,
    plan: &InstallPlan,
) -> Result<InstallOutcome, SkillfileError> {
    let installed = validation_ctx
        .resolved_target
        .adapter()
        .deploy_entry(&DeployRequest {
            entry: validation_ctx.entry,
            source: validation_ctx.source,
            scope: validation_ctx.resolved_target.scope(),
            repo_root: validation_ctx.repo_root,
            opts: validation_ctx.opts,
        });

    if validation_ctx.opts.dry_run {
        return Ok(InstallOutcome::Skipped(InstallSkipReason::DryRun));
    }
    let validated = validate_installed_files(validation_ctx, plan, installed)?;
    if let InstallOutcome::Skipped(reason) = validated.outcome {
        return Ok(InstallOutcome::Skipped(reason));
    }

    let patch_ctx = PatchCtx {
        entry: validation_ctx.entry,
        repo_root: validation_ctx.repo_root,
    };
    if validation_ctx.is_dir {
        apply_dir_patches(&patch_ctx, &validated.installed, validation_ctx.source)?;
    } else {
        let key = format!("{}.md", validation_ctx.entry.name);
        if let Some(dest) = validated.installed.get(&key) {
            apply_single_file_patch(&patch_ctx, dest, validation_ctx.source)?;
        }
    }

    Ok(InstallOutcome::Installed)
}

/// Returns `Err(PatchConflict)` if a stored patch fails to apply cleanly.
pub fn install_entry_with_outcome(
    entry: &Entry,
    target: &InstallTarget,
    ctx: &InstallCtx<'_>,
) -> Result<InstallOutcome, SkillfileError> {
    let default_opts = InstallOptions::default();
    let opts = ctx.opts.unwrap_or(&default_opts);

    let Ok(resolved_target) = ResolvedInstallTarget::from_target(target) else {
        return Ok(InstallOutcome::Skipped(InstallSkipReason::UnknownAdapter));
    };

    if !resolved_target.supports(entry.entity_type) {
        return Ok(InstallOutcome::Skipped(
            InstallSkipReason::UnsupportedEntity,
        ));
    }

    let Some(source) = source_path(entry, ctx.repo_root) else {
        eprintln!("  warning: source missing for {}, skipping", entry.name);
        return Ok(InstallOutcome::Skipped(InstallSkipReason::MissingSource));
    };
    ensure_no_symlink_components(&source)
        .map_err(|error| install_failure(entry, target, &error.to_string()))?;
    if !source.exists() {
        eprintln!("  warning: source missing for {}, skipping", entry.name);
        return Ok(InstallOutcome::Skipped(InstallSkipReason::MissingSource));
    }

    let is_dir = is_dir_entry(entry) || source.is_dir();
    let validation_ctx = InstallValidationCtx {
        entry,
        target,
        repo_root: ctx.repo_root,
        source: &source,
        resolved_target,
        is_dir,
        opts,
    };
    ensure_unique_flat_target_destinations(&FlatValidationCtx {
        entries: std::slice::from_ref(entry),
        target,
        repo_root: ctx.repo_root,
        dry_run: opts.dry_run,
    })?;
    ensure_safe_install_effect_paths(&validation_ctx)?;
    let plan = build_install_plan(&validation_ctx);
    let snapshot = if opts.dry_run {
        InstallSnapshot::default()
    } else {
        InstallSnapshot::capture(ctx.repo_root, install_effect_paths(&validation_ctx))?
    };
    deploy_and_patch_entry(&validation_ctx, &plan)
        .map_err(|error| restore_on_install_error(&snapshot, error))
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

struct LockMaps<'a> {
    locked: &'a std::collections::BTreeMap<String, skillfile_core::models::LockEntry>,
    old_locked: &'a std::collections::BTreeMap<String, skillfile_core::models::LockEntry>,
}

struct DeployCtx<'a> {
    repo_root: &'a Path,
    opts: &'a InstallOptions,
    maps: LockMaps<'a>,
}

fn handle_patch_conflict(
    entry: &Entry,
    entry_name: &str,
    ctx: &DeployCtx<'_>,
) -> Result<(), SkillfileError> {
    let key = lock_key(entry);
    let old_sha = ctx
        .maps
        .old_locked
        .get(&key)
        .map(|l| l.sha.clone())
        .unwrap_or_default();
    let new_sha = ctx
        .maps
        .locked
        .get(&key)
        .map_or_else(|| old_sha.clone(), |l| l.sha.clone());

    write_conflict(
        ctx.repo_root,
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

fn append_rollback_detail(error: SkillfileError, patch_message: &str) -> SkillfileError {
    if !patch_message.contains("rollback failed") {
        return error;
    }
    match error {
        SkillfileError::Install(message) => {
            SkillfileError::Install(format!("{message}\nRollback warning: {patch_message}"))
        }
        other => other,
    }
}

fn install_entry_or_conflict(
    entry: &Entry,
    target: &InstallTarget,
    ctx: &DeployCtx<'_>,
) -> Result<(), SkillfileError> {
    let install_ctx = InstallCtx {
        repo_root: ctx.repo_root,
        opts: Some(ctx.opts),
    };
    match install_entry(entry, target, &install_ctx) {
        Ok(()) => Ok(()),
        Err(SkillfileError::PatchConflict {
            entry_name,
            message,
        }) => handle_patch_conflict(entry, &entry_name, ctx)
            .map_err(|error| append_rollback_detail(error, &message)),
        Err(e) => Err(e),
    }
}

fn deploy_all(manifest: &Manifest, ctx: &DeployCtx<'_>) -> Result<(), SkillfileError> {
    let mode = if ctx.opts.dry_run { " [dry-run]" } else { "" };

    ensure_unique_flat_install_destinations(manifest, ctx.repo_root, ctx.opts.dry_run)?;

    for target in &manifest.install_targets {
        if matches!(target, InstallTarget::Platform { .. })
            && ResolvedInstallTarget::from_target(target).is_err()
        {
            eprintln!(
                "warning: unknown platform '{}', skipping",
                target.platform_name()
            );
            continue;
        }
        progress!("Installing for {target}{mode}...");
        for entry in &manifest.entries {
            install_entry_or_conflict(entry, target, ctx)?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// cmd_install
// ---------------------------------------------------------------------------

fn apply_extra_targets(manifest: &mut Manifest, extra_targets: Option<&[InstallTarget]>) {
    let Some(targets) = extra_targets else {
        return;
    };
    if !targets.is_empty() {
        progress!("Using platform targets from personal config (Skillfile has no install lines).");
    }
    manifest.install_targets = targets.to_vec();
}

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

fn auto_pin_all(manifest: &Manifest, repo_root: &Path) -> Result<(), SkillfileError> {
    for entry in &manifest.entries {
        auto_pin_entry(entry, manifest, repo_root)?;
    }
    Ok(())
}

fn has_updatable_flat_agent_dir(manifest: &Manifest) -> bool {
    let has_updatable_dir = manifest.entries.iter().any(|entry| {
        if entry.entity_type != EntityType::Agent {
            return false;
        }
        match &entry.source {
            SourceFields::Github { path_in_repo, .. }
            | SourceFields::Gitlab { path_in_repo, .. } => {
                path_in_repo == "." || is_dir_entry(entry)
            }
            SourceFields::Local { .. } | SourceFields::Url { .. } => false,
        }
    });
    has_updatable_dir
        && manifest.install_targets.iter().any(|target| {
            ResolvedInstallTarget::from_target(target).is_ok_and(|resolved| {
                resolved.supports(EntityType::Agent)
                    && resolved.dir_mode(EntityType::Agent) == Some(DirInstallMode::Flat)
            })
        })
}

fn capture_update_state(
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<InstallSnapshot, SkillfileError> {
    // Sync can update every remote entry before a newly fetched agent collision is known.
    let mut paths = vec![repo_root.join("Skillfile.lock"), patches_root(repo_root)];
    paths.extend(
        manifest
            .entries
            .iter()
            .filter(|entry| !matches!(&entry.source, SourceFields::Local { .. }))
            .map(|entry| vendor_dir_for(entry, repo_root)),
    );
    InstallSnapshot::capture(repo_root, paths)
}

fn print_first_install_hint(manifest: &Manifest) {
    let platforms: Vec<String> = manifest
        .install_targets
        .iter()
        .map(ToString::to_string)
        .collect();
    progress!("  Configured platforms: {}", platforms.join(", "));
    progress!("  Run `skillfile init` to add or change platforms.");
}

pub struct CmdInstallOpts<'a> {
    pub dry_run: bool,
    pub update: bool,
    pub extra_targets: Option<&'a [InstallTarget]>,
}

pub fn cmd_install(repo_root: &Path, opts: &CmdInstallOpts<'_>) -> Result<(), SkillfileError> {
    cmd_install_with_sync(repo_root, opts, || {
        cmd_sync(&skillfile_sources::sync::SyncCmdOpts {
            repo_root,
            dry_run: opts.dry_run,
            entry_filter: None,
            update: opts.update,
            // The install body already printed manifest warnings.
            print_warnings: false,
        })
    })
}

fn cmd_install_with_sync(
    repo_root: &Path,
    opts: &CmdInstallOpts<'_>,
    sync: impl FnOnce() -> Result<(), SkillfileError>,
) -> Result<(), SkillfileError> {
    let manifest = load_manifest(repo_root, opts.extra_targets)?;

    check_preconditions(&manifest, repo_root)?;

    // Detect first install (cache dir absent → fresh clone or first run).
    let cache_dir = repo_root.join(".skillfile").join("cache");
    let first_install = !cache_dir.exists();

    // Read old locked state before sync (used for SHA context in conflict messages).
    let old_locked = read_lock(repo_root).unwrap_or_default();

    // Cached collisions are known before auto-pin; fetched sources need a second check.
    ensure_unique_flat_install_destinations(&manifest, repo_root, opts.dry_run)?;

    let update_snapshot = if opts.update && !opts.dry_run && has_updatable_flat_agent_dir(&manifest)
    {
        Some(capture_update_state(&manifest, repo_root)?)
    } else {
        None
    };

    // Auto-pin local edits before re-fetching upstream (--update only).
    if opts.update && !opts.dry_run {
        auto_pin_all(&manifest, repo_root)?;
    }

    // Ensure cache dir exists (used as first-install marker and by sync).
    if !opts.dry_run {
        std::fs::create_dir_all(&cache_dir)?;
    }

    // Fetch any missing or stale entries.
    let sync_result = sync();

    if let Err(error) = ensure_unique_flat_install_destinations(&manifest, repo_root, opts.dry_run)
    {
        let error = match sync_result {
            Ok(()) => error,
            Err(sync_error) => {
                SkillfileError::Install(format!("{error}; sync also failed: {sync_error}"))
            }
        };
        return Err(if let Some(snapshot) = &update_snapshot {
            restore_on_install_error(snapshot, error)
        } else {
            error
        });
    }

    // Read new locked state (written by sync).
    let locked = read_lock(repo_root).unwrap_or_default();

    // Deploy to all configured platform targets.
    let install_opts = InstallOptions {
        dry_run: opts.dry_run,
        overwrite: opts.update,
    };
    let deploy_ctx = DeployCtx {
        repo_root,
        opts: &install_opts,
        maps: LockMaps {
            locked: &locked,
            old_locked: &old_locked,
        },
    };
    deploy_all(&manifest, &deploy_ctx)?;
    sync_result?;

    if !opts.dry_run {
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
    use skillfile_core::models::{
        EntityType, Entry, InstallTarget, LockEntry, Scope, SourceFields,
    };
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn target_aliases_case_names(target: &Path) -> bool {
        let upper = target.join("Agent.md");
        let lower = target.join("agent.md");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&upper)
            .unwrap();
        let aliases = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lower)
        {
            Ok(_) => false,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => true,
            Err(error) => panic!("cannot probe target filename behavior: {error}"),
        };
        std::fs::remove_file(upper).unwrap();
        if !aliases {
            std::fs::remove_file(lower).unwrap();
        }
        aliases
    }

    #[test]
    fn flat_probe_cleanup_preserves_unowned_stage_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("agents");
        std::fs::create_dir(&target).unwrap();
        let mut probe = FlatDestinationProbe::new(&target);
        probe.prepare().unwrap();
        let stage = probe.stage.as_ref().unwrap().clone();
        let unowned = stage.join("unowned.md");
        std::fs::write(&unowned, "# Keep\n").unwrap();

        let error = probe.cleanup().unwrap_err();

        assert!(error.to_string().contains(&stage.display().to_string()));
        assert!(stage.exists());
        assert_eq!(std::fs::read_to_string(&unowned).unwrap(), "# Keep\n");
        std::fs::remove_file(&unowned).unwrap();
        probe.cleanup().unwrap();
        assert!(!stage.exists());
    }

    #[test]
    fn flat_probe_reports_both_sources_and_cleans_collision() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("agents");
        std::fs::create_dir(&target).unwrap();
        let existing = target.join("existing.md");
        std::fs::write(&existing, "# User content\n").unwrap();
        let names = [
            ("agent.md".into(), "frontend:agent.md".into()),
            ("agent.md".into(), "backend:agent.md".into()),
        ];

        let error =
            validate_actual_flat_names(&make_target("claude-code", Scope::Local), &target, &names)
                .unwrap_err()
                .to_string();

        assert!(error.contains("duplicate flat destination"), "{error}");
        assert!(
            error.contains(r#""agent.md" from ["backend:agent.md", "frontend:agent.md"]"#),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(existing).unwrap(),
            "# User content\n"
        );
        assert_eq!(std::fs::read_dir(target).unwrap().count(), 1);
    }

    // -----------------------------------------------------------------------
    // Fixture helpers — filesystem-only, no cross-crate function calls
    // -----------------------------------------------------------------------

    /// Return the path for a single-file entry patch.
    /// `.skillfile/patches/<type>s/<name>.patch`
    fn patch_fixture_path(dir: &Path, entry: &Entry) -> PathBuf {
        dir.join(".skillfile/patches")
            .join(entry.entity_type.dir_name())
            .join(format!("{}.patch", entry.name))
    }

    /// Return the path for a per-file patch within a directory entry.
    /// `.skillfile/patches/<type>s/<name>/<rel>.patch`
    fn dir_patch_fixture_path(dir: &Path, entry: &Entry, rel: &str) -> PathBuf {
        dir.join(".skillfile/patches")
            .join(entry.entity_type.dir_name())
            .join(&entry.name)
            .join(format!("{rel}.patch"))
    }

    /// Write a single-file patch fixture to the correct path.
    fn write_patch_fixture(dir: &Path, entry: &Entry, text: &str) {
        let p = patch_fixture_path(dir, entry);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// Write `Skillfile.lock` as JSON. Uses `serde_json` — no cross-crate call.
    fn write_lock_fixture(dir: &Path, locked: &BTreeMap<String, LockEntry>) {
        let json = serde_json::to_string_pretty(locked).unwrap();
        std::fs::write(dir.join("Skillfile.lock"), format!("{json}\n")).unwrap();
    }

    /// Write `.skillfile/conflict` JSON from a `ConflictState`.
    fn write_conflict_fixture(dir: &Path, state: &ConflictState) {
        let p = dir.join(".skillfile/conflict");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let json = serde_json::to_string_pretty(state).unwrap();
        std::fs::write(p, format!("{json}\n")).unwrap();
    }

    /// Return `true` if any `.patch` file exists under the directory-entry patch dir.
    fn has_dir_patch_fixture(dir: &Path, entry: &Entry) -> bool {
        let d = dir
            .join(".skillfile/patches")
            .join(entry.entity_type.dir_name())
            .join(&entry.name);
        if !d.is_dir() {
            return false;
        }
        std::fs::read_dir(&d).is_ok_and(|rd| {
            rd.filter_map(std::result::Result::ok)
                .any(|e| e.path().extension().is_some_and(|x| x == "patch"))
        })
    }

    // -----------------------------------------------------------------------
    // Entry and target builders
    // -----------------------------------------------------------------------

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
        InstallTarget::platform(adapter, scope)
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
        let outcome = install_entry_with_outcome(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();
        assert_eq!(outcome, InstallOutcome::Installed);

        let dest = dir.path().join(".claude/skills/my-skill/SKILL.md");
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
        install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();

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
        let outcome = install_entry_with_outcome(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: Some(&opts),
            },
        )
        .unwrap();
        assert_eq!(outcome, InstallOutcome::Skipped(InstallSkipReason::DryRun));

        let dest = dir.path().join(".claude/skills/my-skill/SKILL.md");
        assert!(!dest.exists());
    }

    #[test]
    fn install_entry_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("skills/my-skill.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# New content").unwrap();

        let dest_dir = dir.path().join(".claude/skills/my-skill");
        std::fs::create_dir_all(&dest_dir).unwrap();
        let dest = dest_dir.join("SKILL.md");
        std::fs::write(&dest, "# Old content").unwrap();

        let entry = make_local_entry("my-skill", "skills/my-skill.md");
        let target = make_target("claude-code", Scope::Local);
        install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();

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
        install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();

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
        std::fs::write(vdir.join(".meta"), r#"{"sha":"cached"}"#).unwrap();

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
        install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();

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
        std::fs::write(vdir.join(".meta"), r#"{"sha":"cached"}"#).unwrap();

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
        install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();

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
    fn install_entry_rejects_flat_collision_before_replacing_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("agents/team");
        for relative in ["backend/agent.md", "frontend/agent.md"] {
            let path = source.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("# {relative}\n")).unwrap();
        }
        let installed = dir.path().join(".claude/agents/agent.md");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, "# Existing\n").unwrap();
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "team".into(),
            source: SourceFields::Local {
                path: "agents/team".into(),
            },
        };
        let options = InstallOptions {
            dry_run: false,
            overwrite: true,
        };

        let error = install_entry_with_outcome(
            &entry,
            &make_target("claude-code", Scope::Local),
            &InstallCtx {
                repo_root: dir.path(),
                opts: Some(&options),
            },
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("duplicate flat destination"), "{error}");
        assert_eq!(std::fs::read_to_string(installed).unwrap(), "# Existing\n");
    }

    #[test]
    fn install_entry_missing_source_warns() {
        let dir = tempfile::tempdir().unwrap();
        let entry = make_agent_entry("my-agent");
        let target = make_target("claude-code", Scope::Local);

        let outcome = install_entry_with_outcome(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();
        assert_eq!(
            outcome,
            InstallOutcome::Skipped(InstallSkipReason::MissingSource)
        );
    }

    #[test]
    fn install_entry_unknown_adapter_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("skills/my-skill.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# My Skill").unwrap();

        let entry = make_local_entry("my-skill", "skills/my-skill.md");
        let target = make_target("unknown-adapter", Scope::Local);
        let outcome = install_entry_with_outcome(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();
        assert_eq!(
            outcome,
            InstallOutcome::Skipped(InstallSkipReason::UnknownAdapter)
        );
    }

    #[test]
    fn install_entry_errors_when_target_path_is_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("skills/my-skill.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# My Skill").unwrap();
        std::fs::write(dir.path().join(".claude"), "not a directory").unwrap();

        let entry = make_local_entry("my-skill", "skills/my-skill.md");
        let target = make_target("claude-code", Scope::Local);
        let result = install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        );
        assert!(matches!(
            result,
            Err(SkillfileError::Install(message))
                if message.contains("failed to install 'my-skill' to claude-code (local)")
        ));
    }

    #[test]
    fn install_snapshot_restores_previous_directory_contents() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("skills/foo");
        let dest_dir = dir.path().join(".claude/skills/foo");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();
        std::fs::write(source_dir.join("SKILL.md"), "# Source\n").unwrap();
        std::fs::write(dest_dir.join("SKILL.md"), "# Old\n").unwrap();

        let entry = make_local_entry("foo", "skills/foo");
        let target = make_target("claude-code", Scope::Local);
        let snapshot =
            capture_install_snapshot(&entry, std::slice::from_ref(&target), dir.path()).unwrap();

        std::fs::remove_dir_all(&dest_dir).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();
        std::fs::write(dest_dir.join("SKILL.md"), "# New\n").unwrap();

        snapshot.restore().unwrap();
        assert_eq!(
            std::fs::read_to_string(dest_dir.join("SKILL.md")).unwrap(),
            "# Old\n"
        );
    }

    #[test]
    fn install_snapshot_restores_legacy_flat_file_side_effect() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("skills/foo.md");
        let legacy_file = dir.path().join(".claude/skills/foo.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::create_dir_all(legacy_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# Source\n").unwrap();
        std::fs::write(&legacy_file, "# Legacy\n").unwrap();

        let entry = make_local_entry("foo", "skills/foo.md");
        let target = make_target("claude-code", Scope::Local);
        let snapshot =
            capture_install_snapshot(&entry, std::slice::from_ref(&target), dir.path()).unwrap();

        std::fs::remove_file(&legacy_file).unwrap();
        snapshot.restore().unwrap();
        assert_eq!(std::fs::read_to_string(&legacy_file).unwrap(), "# Legacy\n");
    }

    #[test]
    fn install_snapshot_restores_patch_file() {
        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("skills/foo.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# Source\n").unwrap();

        let entry = make_local_entry("foo", "skills/foo.md");
        let patch = patch_fixture_path(dir.path(), &entry);
        std::fs::create_dir_all(patch.parent().unwrap()).unwrap();
        std::fs::write(&patch, "old patch").unwrap();

        let target = make_target("claude-code", Scope::Local);
        let snapshot =
            capture_install_snapshot(&entry, std::slice::from_ref(&target), dir.path()).unwrap();
        std::fs::write(&patch, "new patch").unwrap();

        snapshot.restore().unwrap();
        assert_eq!(std::fs::read_to_string(&patch).unwrap(), "old patch");
    }

    #[cfg(unix)]
    #[test]
    fn install_entry_restores_existing_directory_when_copy_fails() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("skills/foo");
        let dest_dir = dir.path().join(".claude/skills/foo");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();
        std::fs::write(source_dir.join("SKILL.md"), "# New\n").unwrap();
        symlink("missing-target.md", source_dir.join("dangling.md")).unwrap();
        std::fs::write(dest_dir.join("SKILL.md"), "# Old\n").unwrap();
        std::fs::write(dest_dir.join("keep.md"), "# Keep\n").unwrap();

        let entry = make_local_entry("foo", "skills/foo");
        let target = make_target("claude-code", Scope::Local);
        let result = install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        );

        assert!(matches!(
            result,
            Err(SkillfileError::Install(message))
                if message.contains("failed to install 'foo' to claude-code (local)")
        ));
        assert_eq!(
            std::fs::read_to_string(dest_dir.join("SKILL.md")).unwrap(),
            "# Old\n"
        );
        assert_eq!(
            std::fs::read_to_string(dest_dir.join("keep.md")).unwrap(),
            "# Keep\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_snapshot_restores_symlink_as_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join(".claude/skills/foo.md");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink("target.md", &link).unwrap();
        let snapshot = InstallSnapshot::capture(dir.path(), vec![link.clone()]).unwrap();

        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, "# regular file\n").unwrap();

        snapshot.restore().unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("target.md")
        );
    }

    #[cfg(windows)]
    #[test]
    fn install_snapshot_restores_dangling_directory_symlink_kind() {
        use std::os::windows::fs::{symlink_dir, FileTypeExt as _};

        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join(".claude/skills/foo");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        if symlink_dir("missing-dir", &link).is_err() {
            return;
        }
        let snapshot = InstallSnapshot::capture(dir.path(), vec![link.clone()]).unwrap();

        remove_path(&link).unwrap();
        std::fs::write(&link, "# regular file\n").unwrap();

        snapshot.restore().unwrap();
        let file_type = std::fs::symlink_metadata(&link).unwrap().file_type();
        assert!(file_type.is_symlink_dir());
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("missing-dir")
        );
    }

    #[test]
    fn install_snapshot_keeps_scratch_dir_when_restore_fails() {
        let dir = tempfile::tempdir().unwrap();
        let live_path = dir.path().join("target/foo.md");
        std::fs::create_dir_all(live_path.parent().unwrap()).unwrap();
        std::fs::write(&live_path, "# old\n").unwrap();
        let snapshot = InstallSnapshot::capture(dir.path(), vec![live_path.clone()]).unwrap();
        let scratch_dir = snapshot.scratch_dir().unwrap().to_path_buf();

        std::fs::remove_dir_all(live_path.parent().unwrap()).unwrap();
        std::fs::write(live_path.parent().unwrap(), "parent is a file").unwrap();

        let result = snapshot.restore();
        assert!(result.is_err());
        drop(snapshot);
        assert!(scratch_dir.exists());

        std::fs::remove_dir_all(&scratch_dir).unwrap();
        remove_empty_dir(scratch_dir.parent());
    }

    #[test]
    fn patch_conflict_type_survives_rollback_failure() {
        let dir = tempfile::tempdir().unwrap();
        let live_path = dir.path().join("target/foo.md");
        std::fs::create_dir_all(live_path.parent().unwrap()).unwrap();
        std::fs::write(&live_path, "# old\n").unwrap();
        let snapshot = InstallSnapshot::capture(dir.path(), vec![live_path.clone()]).unwrap();
        let scratch_dir = snapshot.scratch_dir().unwrap().to_path_buf();

        std::fs::remove_dir_all(live_path.parent().unwrap()).unwrap();
        std::fs::write(live_path.parent().unwrap(), "parent is a file").unwrap();

        let error = restore_on_install_error(
            &snapshot,
            SkillfileError::PatchConflict {
                message: "patch failed".to_string(),
                entry_name: "foo".to_string(),
            },
        );
        assert!(matches!(
            error,
            SkillfileError::PatchConflict { ref message, ref entry_name }
                if entry_name == "foo"
                    && message.contains("patch failed")
                    && message.contains("rollback failed")
        ));
        drop(snapshot);

        std::fs::remove_dir_all(&scratch_dir).unwrap();
        remove_empty_dir(scratch_dir.parent());
    }

    // -- Patch application during install --

    #[test]
    fn install_applies_existing_patch() {
        let dir = tempfile::tempdir().unwrap();

        // Set up cache
        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("test.md"), "# Test\n\nOriginal.\n").unwrap();

        // Write a patch using filesystem fixture helper.
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "skills/test.md".into(),
                ref_: "main".into(),
            },
        };
        // Hand-written unified diff: "Original." → "Modified."
        let patch_text =
            "--- a/test.md\n+++ b/test.md\n@@ -1,3 +1,3 @@\n # Test\n \n-Original.\n+Modified.\n";
        write_patch_fixture(dir.path(), &entry, patch_text);

        let target = make_target("claude-code", Scope::Local);
        install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();

        let dest = dir.path().join(".claude/skills/test/SKILL.md");
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
        write_patch_fixture(dir.path(), &entry, bad_patch);

        // Deploy the entry
        let installed_dir = dir.path().join(".claude/skills/test");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(
            installed_dir.join("SKILL.md"),
            "totally different\ncontent\n",
        )
        .unwrap();

        let target = make_target("claude-code", Scope::Local);
        let result = install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        );
        assert!(result.is_err());
        // Should be a PatchConflict error
        matches!(result.unwrap_err(), SkillfileError::PatchConflict { .. });
    }

    #[test]
    fn install_patch_conflict_restores_previous_installed_file() {
        let dir = tempfile::tempdir().unwrap();

        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("test.md"), "# New upstream\n").unwrap();

        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "skills/test.md".into(),
                ref_: "main".into(),
            },
        };
        let bad_patch =
            "--- a/test.md\n+++ b/test.md\n@@ -1 +1 @@\n-expected_original_line\n+modified\n";
        write_patch_fixture(dir.path(), &entry, bad_patch);

        let dest = dir.path().join(".claude/skills/test/SKILL.md");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, "# Old installed\n").unwrap();

        let target = make_target("claude-code", Scope::Local);
        let result = install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        );

        assert!(matches!(result, Err(SkillfileError::PatchConflict { .. })));
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# Old installed\n");
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
        install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();

        let dest = dir.path().join(".gemini/skills/my-skill/SKILL.md");
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
        install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();

        let dest = dir.path().join(".codex/skills/my-skill/SKILL.md");
        assert!(dest.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# My Skill");
    }

    #[test]
    fn codex_skips_agent_entries() {
        let dir = tempfile::tempdir().unwrap();
        let entry = make_agent_entry("my-agent");
        let target = make_target("codex", Scope::Local);
        let outcome = install_entry_with_outcome(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();
        assert_eq!(
            outcome,
            InstallOutcome::Skipped(InstallSkipReason::UnsupportedEntity)
        );

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
            &InstallCtx {
                repo_root: dir.path(),
                opts: Some(&InstallOptions::default()),
            },
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
            install_entry(
                &entry,
                &target,
                &InstallCtx {
                    repo_root: dir.path(),
                    opts: None,
                },
            )
            .unwrap();

            let prefix = match *adapter {
                "claude-code" => ".claude",
                "gemini-cli" => ".gemini",
                "codex" => ".codex",
                _ => unreachable!(),
            };
            let dest = dir
                .path()
                .join(format!("{prefix}/skills/my-skill/SKILL.md"));
            assert!(dest.exists(), "Failed for adapter {adapter}");
            assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# Multi Skill");
        }
    }

    // -- cmd_install --

    #[test]
    fn cmd_install_no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_install(
            dir.path(),
            &CmdInstallOpts {
                dry_run: false,
                update: false,
                extra_targets: None,
            },
        );
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

        let result = cmd_install(
            dir.path(),
            &CmdInstallOpts {
                dry_run: false,
                update: false,
                extra_targets: None,
            },
        );
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
        cmd_install(
            dir.path(),
            &CmdInstallOpts {
                dry_run: false,
                update: false,
                extra_targets: Some(&targets),
            },
        )
        .unwrap();

        let dest = dir.path().join(".claude/skills/foo/SKILL.md");
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
        cmd_install(
            dir.path(),
            &CmdInstallOpts {
                dry_run: false,
                update: false,
                extra_targets: Some(&targets),
            },
        )
        .unwrap();

        // claude-code (from Skillfile) should be deployed.
        assert!(dir.path().join(".claude/skills/foo/SKILL.md").exists());
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

        cmd_install(
            dir.path(),
            &CmdInstallOpts {
                dry_run: true,
                update: false,
                extra_targets: None,
            },
        )
        .unwrap();

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

        cmd_install(
            dir.path(),
            &CmdInstallOpts {
                dry_run: false,
                update: false,
                extra_targets: None,
            },
        )
        .unwrap();

        // skill deployed to all three adapters
        assert!(dir.path().join(".claude/skills/foo/SKILL.md").exists());
        assert!(dir.path().join(".gemini/skills/foo/SKILL.md").exists());
        assert!(dir.path().join(".codex/skills/foo/SKILL.md").exists());

        // agent deployed to claude-code and gemini-cli but NOT codex
        assert!(dir.path().join(".claude/agents/bar.md").exists());
        assert!(dir.path().join(".gemini/agents/bar.md").exists());
        assert!(!dir.path().join(".codex/agents").exists());
    }

    #[test]
    fn cmd_install_pending_conflict_blocks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Skillfile"),
            "install  claude-code  local\nlocal  skill  foo  skills/foo.md\n",
        )
        .unwrap();

        write_conflict_fixture(
            dir.path(),
            &ConflictState {
                entry: "foo".into(),
                entity_type: EntityType::Skill,
                old_sha: "aaa".into(),
                new_sha: "bbb".into(),
            },
        );

        let result = cmd_install(
            dir.path(),
            &CmdInstallOpts {
                dry_run: false,
                update: false,
                extra_targets: None,
            },
        );
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
    fn setup_github_skill_repo(dir: &Path, name: &str, cache_content: &str) {
        // Manifest
        std::fs::write(
            dir.join("Skillfile"),
            format!(
                "install  claude-code  local\ngithub  skill  {name}  owner/repo  skills/{name}.md\n"
            ),
        )
        .unwrap();

        // Lock file via filesystem fixture (no cross-crate call).
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
        write_lock_fixture(dir, &locked);

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

        auto_pin_entry(&entry, &manifest, dir.path()).unwrap();

        // No patch must have been written.
        assert!(
            !patch_fixture_path(dir.path(), &entry).exists(),
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
        auto_pin_entry(&entry, &manifest, dir.path()).unwrap();

        assert!(!patch_fixture_path(dir.path(), &entry).exists());
    }

    #[test]
    fn auto_pin_entry_missing_lock_key_is_skipped() {
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
        write_lock_fixture(dir.path(), &locked);

        let entry = make_skill_entry("test");
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        auto_pin_entry(&entry, &manifest, dir.path()).unwrap();

        assert!(!patch_fixture_path(dir.path(), &entry).exists());
    }

    #[test]
    fn auto_pin_comparisons_treat_crlf_as_equivalent() {
        let patch = "--- a/test.md\n+++ b/test.md\n@@ -1 +1 @@\n-Original\n+Modified\n";
        assert!(patch_already_covers(patch, "Original\n", "Modified\r\n"));

        let variants = vec![
            SingleInstalledVariant {
                label: "claude-code (local)".into(),
                content: "Modified\n".into(),
            },
            SingleInstalledVariant {
                label: "cursor (local)".into(),
                content: "Modified\r\n".into(),
            },
        ];
        assert_eq!(
            representative_single_file_content("test", "Original\n", &variants).unwrap(),
            Some("Modified\n".into())
        );
    }

    #[test]
    fn auto_pin_preserves_malformed_patch() {
        assert!(patch_already_covers(
            "@@ invalid\n",
            "Original\n",
            "Modified\n"
        ));
    }

    #[test]
    fn auto_pin_dir_comparison_treats_crlf_as_equivalent() {
        let left = BTreeMap::from([("SKILL.md".into(), "Modified\n".into())]);
        let right = BTreeMap::from([("SKILL.md".into(), "Modified\r\n".into())]);

        assert!(dir_modified_content_eq(&left, &right));
    }

    #[test]
    fn auto_pin_entry_writes_patch_when_installed_differs() {
        let dir = tempfile::tempdir().unwrap();
        let name = "my-skill";

        let cache_content = "# My Skill\n\nOriginal content.\n";
        let installed_content = "# My Skill\n\nUser-modified content.\n";

        setup_github_skill_repo(dir.path(), name, cache_content);

        // Place a modified installed file.
        let installed_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(installed_dir.join("SKILL.md"), installed_content).unwrap();

        let entry = make_skill_entry(name);
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        auto_pin_entry(&entry, &manifest, dir.path()).unwrap();

        assert!(
            patch_fixture_path(dir.path(), &entry).exists(),
            "patch should be written when installed differs from cache"
        );

        // Verify the patch round-trips: reset the installed file to cache_content and
        // reinstall — the patch must produce installed_content.
        std::fs::write(installed_dir.join("SKILL.md"), cache_content).unwrap();
        let target = make_target("claude-code", Scope::Local);
        install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(installed_dir.join("SKILL.md")).unwrap(),
            installed_content,
        );
    }

    #[test]
    fn auto_pin_entry_uses_second_target_when_first_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        let name = "my-skill";

        let cache_content = "# My Skill\n\nOriginal content.\n";
        let installed_content = "# My Skill\n\nUser-modified content.\n";

        setup_github_skill_repo(dir.path(), name, cache_content);

        let first_installed_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&first_installed_dir).unwrap();
        std::fs::write(first_installed_dir.join("SKILL.md"), cache_content).unwrap();

        let second_installed_dir = dir.path().join(format!(".cursor/skills/{name}"));
        std::fs::create_dir_all(&second_installed_dir).unwrap();
        std::fs::write(second_installed_dir.join("SKILL.md"), installed_content).unwrap();

        let entry = make_skill_entry(name);
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![
                make_target("claude-code", Scope::Local),
                make_target("cursor", Scope::Local),
            ],
        };

        auto_pin_entry(&entry, &manifest, dir.path()).unwrap();

        std::fs::write(first_installed_dir.join("SKILL.md"), cache_content).unwrap();
        install_entry(
            &entry,
            &make_target("claude-code", Scope::Local),
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(first_installed_dir.join("SKILL.md")).unwrap(),
            installed_content,
            "auto-pin must preserve edits from a modified secondary target"
        );
    }

    #[test]
    fn auto_pin_entry_errors_on_divergent_multi_target_edits() {
        let dir = tempfile::tempdir().unwrap();
        let name = "my-skill";

        let cache_content = "# My Skill\n\nOriginal content.\n";
        setup_github_skill_repo(dir.path(), name, cache_content);

        let first_installed_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&first_installed_dir).unwrap();
        std::fs::write(
            first_installed_dir.join("SKILL.md"),
            "# My Skill\n\nClaude edit.\n",
        )
        .unwrap();

        let second_installed_dir = dir.path().join(format!(".cursor/skills/{name}"));
        std::fs::create_dir_all(&second_installed_dir).unwrap();
        std::fs::write(
            second_installed_dir.join("SKILL.md"),
            "# My Skill\n\nCursor edit.\n",
        )
        .unwrap();

        let entry = make_skill_entry(name);
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![
                make_target("claude-code", Scope::Local),
                make_target("cursor", Scope::Local),
            ],
        };

        let error = auto_pin_entry(&entry, &manifest, dir.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("divergent edits across install targets"),
            "unexpected error: {error}"
        );
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

        // Pre-write the correct patch (cache → installed) using the fixture helper.
        // Hand-written unified diff: "Original." → "Modified."
        let patch_text = "--- a/my-skill.md\n+++ b/my-skill.md\n@@ -1,3 +1,3 @@\n # My Skill\n \n-Original.\n+Modified.\n";
        write_patch_fixture(dir.path(), &entry, patch_text);

        // Write installed file that matches what the patch produces.
        let installed_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(installed_dir.join("SKILL.md"), installed_content).unwrap();

        // Record mtime of patch so we can detect if it changed.
        let patch_path = patch_fixture_path(dir.path(), &entry);
        let mtime_before = std::fs::metadata(&patch_path).unwrap().modified().unwrap();

        // Small sleep so that any write would produce a different mtime.
        std::thread::sleep(std::time::Duration::from_millis(20));

        auto_pin_entry(&entry, &manifest, dir.path()).unwrap();

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
        let new_installed = "# My Skill\n\nFirst edit.\n\nSecond edit.\n";

        setup_github_skill_repo(dir.path(), name, cache_content);

        let entry = make_skill_entry(name);
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        // Stored patch reflects the old installed state: "Original." → "First edit."
        let old_patch = "--- a/my-skill.md\n+++ b/my-skill.md\n@@ -1,3 +1,3 @@\n # My Skill\n \n-Original.\n+First edit.\n";
        write_patch_fixture(dir.path(), &entry, old_patch);

        // But the actual installed file has further edits.
        let installed_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(installed_dir.join("SKILL.md"), new_installed).unwrap();

        auto_pin_entry(&entry, &manifest, dir.path()).unwrap();

        // The patch was re-written to reflect new_installed. Verify by resetting the
        // installed file to cache_content and reinstalling — must yield new_installed.
        std::fs::write(installed_dir.join("SKILL.md"), cache_content).unwrap();
        let target = make_target("claude-code", Scope::Local);
        install_entry(
            &entry,
            &target,
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(installed_dir.join("SKILL.md")).unwrap(),
            new_installed,
            "updated patch must describe the latest installed content"
        );
    }

    // -----------------------------------------------------------------------
    // auto_pin_dir_entry
    // -----------------------------------------------------------------------

    #[test]
    fn auto_pin_dir_entry_writes_per_file_patches() {
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
        write_lock_fixture(dir.path(), &locked);

        // Vendor cache with two files.
        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), "# Lang Pro\n\nOriginal.\n").unwrap();
        std::fs::write(vdir.join(".meta"), r#"{"sha":"cached"}"#).unwrap();
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

        auto_pin_entry(&entry, &manifest, dir.path()).unwrap();

        // Patch for the modified file should exist.
        let skill_patch = dir_patch_fixture_path(dir.path(), &entry, "SKILL.md");
        assert!(skill_patch.exists(), "patch for SKILL.md must be written");

        // Patch for the unmodified file should NOT exist.
        let examples_patch = dir_patch_fixture_path(dir.path(), &entry, "examples.md");
        assert!(
            !examples_patch.exists(),
            "patch for examples.md must not be written (content unchanged)"
        );
    }

    #[test]
    fn auto_pin_dir_entry_normalizes_cache_file_keys() {
        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        let mut locked: BTreeMap<String, LockEntry> = BTreeMap::new();
        locked.insert(
            format!("github/skill/{name}"),
            LockEntry {
                sha: "deadbeefdeadbeefdeadbeef".into(),
                raw_url: format!("https://example.com/{name}"),
            },
        );
        write_lock_fixture(dir.path(), &locked);

        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        let cache_file = vdir.join(r"nested\file.md");
        std::fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
        std::fs::write(&cache_file, "# Original\n").unwrap();

        let installed_file = dir
            .path()
            .join(format!(".claude/skills/{name}/nested/file.md"));
        std::fs::create_dir_all(installed_file.parent().unwrap()).unwrap();
        std::fs::write(&installed_file, "# Modified\n").unwrap();

        let entry = make_dir_skill_entry(name);
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        auto_pin_entry(&entry, &manifest, dir.path()).unwrap();

        let patch = dir_patch_fixture_path(dir.path(), &entry, "nested/file.md");
        assert!(
            patch.exists(),
            "auto-pin must match cache and installed files through canonical keys"
        );
    }

    #[test]
    fn auto_pin_dir_entry_uses_second_target_when_first_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        std::fs::write(
            dir.path().join("Skillfile"),
            format!(
                "install  claude-code  local\ninstall  cursor  local\ngithub  skill  {name}  owner/repo  skills/{name}\n"
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
        write_lock_fixture(dir.path(), &locked);

        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), "# Lang Pro\n\nOriginal.\n").unwrap();
        std::fs::write(vdir.join(".meta"), r#"{"sha":"cached"}"#).unwrap();

        let first_inst_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&first_inst_dir).unwrap();
        std::fs::write(first_inst_dir.join("SKILL.md"), "# Lang Pro\n\nOriginal.\n").unwrap();

        let second_inst_dir = dir.path().join(format!(".cursor/skills/{name}"));
        std::fs::create_dir_all(&second_inst_dir).unwrap();
        std::fs::write(
            second_inst_dir.join("SKILL.md"),
            "# Lang Pro\n\nModified.\n",
        )
        .unwrap();

        let entry = make_dir_skill_entry(name);
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![
                make_target("claude-code", Scope::Local),
                make_target("cursor", Scope::Local),
            ],
        };

        auto_pin_entry(&entry, &manifest, dir.path()).unwrap();

        std::fs::write(first_inst_dir.join("SKILL.md"), "# Lang Pro\n\nOriginal.\n").unwrap();
        install_entry(
            &entry,
            &make_target("claude-code", Scope::Local),
            &InstallCtx {
                repo_root: dir.path(),
                opts: None,
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(first_inst_dir.join("SKILL.md")).unwrap(),
            "# Lang Pro\n\nModified.\n",
            "auto-pin must preserve dir-entry edits from a modified secondary target"
        );
    }

    #[test]
    fn auto_pin_dir_entry_skips_when_vendor_dir_missing() {
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
        write_lock_fixture(dir.path(), &locked);

        let entry = make_dir_skill_entry(name);
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        // No vendor dir — must silently return without panicking.
        auto_pin_entry(&entry, &manifest, dir.path()).unwrap();

        assert!(!has_dir_patch_fixture(dir.path(), &entry));
    }

    #[test]
    fn auto_pin_dir_entry_no_repin_when_patch_already_matches() {
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
        write_lock_fixture(dir.path(), &locked);

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

        // Pre-write the correct patch: "Original." → "Modified." for SKILL.md
        let patch_text = "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1,3 +1,3 @@\n # Lang Pro\n \n-Original.\n+Modified.\n";
        let dp = dir_patch_fixture_path(dir.path(), &entry, "SKILL.md");
        std::fs::create_dir_all(dp.parent().unwrap()).unwrap();
        std::fs::write(&dp, patch_text).unwrap();

        let patch_path = dir_patch_fixture_path(dir.path(), &entry, "SKILL.md");
        let mtime_before = std::fs::metadata(&patch_path).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));

        auto_pin_entry(&entry, &manifest, dir.path()).unwrap();

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

        // Create patch dir with a valid patch (old cache → installed): "Original." → "Modified."
        let patch_text = "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1,3 +1,3 @@\n # Skill\n \n-Original.\n+Modified.\n";
        let dp = dir_patch_fixture_path(dir.path(), &entry, "SKILL.md");
        std::fs::create_dir_all(dp.parent().unwrap()).unwrap();
        std::fs::write(&dp, patch_text).unwrap();

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

        apply_dir_patches(
            &PatchCtx {
                entry: &entry,
                repo_root: dir.path(),
            },
            &installed_files,
            &new_cache_dir,
        )
        .unwrap();

        // The installed file should have the original patch applied.
        let installed_after = std::fs::read_to_string(inst_dir.join("SKILL.md")).unwrap();
        assert_eq!(installed_after, installed_content);

        // The stored patch must now describe the diff from new_cache to installed_content.
        // Verify by resetting the installed file to new_cache and reinstalling — must
        // yield installed_content (== expected_rebased_to_new_cache).
        std::fs::write(inst_dir.join("SKILL.md"), new_cache_content).unwrap();
        let mut reinstall_files = std::collections::HashMap::new();
        reinstall_files.insert("SKILL.md".to_string(), inst_dir.join("SKILL.md"));
        apply_dir_patches(
            &PatchCtx {
                entry: &entry,
                repo_root: dir.path(),
            },
            &reinstall_files,
            &new_cache_dir,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(inst_dir.join("SKILL.md")).unwrap(),
            expected_rebased_to_new_cache,
            "rebased patch applied to new_cache must reproduce installed_content"
        );
    }

    #[test]
    fn apply_dir_patches_normalizes_patch_file_keys() {
        let dir = tempfile::tempdir().unwrap();
        let entry = make_dir_skill_entry("lang-pro");
        let patch_text = concat!(
            "--- a/nested/file.md\n",
            "+++ b/nested/file.md\n",
            "@@ -1 +1 @@\n",
            "-# Original\n",
            "+# Modified\n",
        );

        let patches_dir = dir.path().join(".skillfile/patches/skills/lang-pro");
        let platform_keyed_patch = patches_dir.join(r"nested\file.md.patch");
        std::fs::create_dir_all(platform_keyed_patch.parent().unwrap()).unwrap();
        std::fs::write(&platform_keyed_patch, patch_text).unwrap();

        let installed_file = dir.path().join(".claude/skills/lang-pro/nested/file.md");
        std::fs::create_dir_all(installed_file.parent().unwrap()).unwrap();
        std::fs::write(&installed_file, "# Original\n").unwrap();

        let source_dir = dir.path().join(".skillfile/cache/skills/lang-pro");
        let source_file = source_dir.join("nested/file.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# Original\n").unwrap();

        let installed_files =
            HashMap::from([("nested/file.md".to_string(), installed_file.clone())]);

        apply_dir_patches(
            &PatchCtx {
                entry: &entry,
                repo_root: dir.path(),
            },
            &installed_files,
            &source_dir,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(&installed_file).unwrap(),
            "# Modified\n"
        );
        let canonical_patch = dir_patch_fixture_path(dir.path(), &entry, "nested/file.md");
        assert!(canonical_patch.exists());
        if platform_keyed_patch != canonical_patch {
            assert!(
                !platform_keyed_patch.exists(),
                "rebasing must not leave a second non-canonical patch path"
            );
        }
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

        // Hand-written patch: "Original." → "Modified."
        let patch_text = "--- a/SKILL.md\n+++ b/SKILL.md\n@@ -1,3 +1,3 @@\n # Skill\n \n-Original.\n+Modified.\n";
        let dp = dir_patch_fixture_path(dir.path(), &entry, "SKILL.md");
        std::fs::create_dir_all(dp.parent().unwrap()).unwrap();
        std::fs::write(&dp, patch_text).unwrap();

        // Installed file starts at original (patch not yet applied).
        let inst_dir = dir.path().join(".claude/skills/lang-pro");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), original).unwrap();

        let new_cache_dir = dir.path().join(".skillfile/cache/skills/lang-pro");
        std::fs::create_dir_all(&new_cache_dir).unwrap();
        std::fs::write(new_cache_dir.join("SKILL.md"), new_cache).unwrap();

        let mut installed_files = std::collections::HashMap::new();
        installed_files.insert("SKILL.md".to_string(), inst_dir.join("SKILL.md"));

        apply_dir_patches(
            &PatchCtx {
                entry: &entry,
                repo_root: dir.path(),
            },
            &installed_files,
            &new_cache_dir,
        )
        .unwrap();

        // Patch file must be removed (rebase produced empty diff).
        let removed_patch = dir_patch_fixture_path(dir.path(), &entry, "SKILL.md");
        assert!(
            !removed_patch.exists(),
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
        apply_dir_patches(
            &PatchCtx {
                entry: &entry,
                repo_root: dir.path(),
            },
            &installed_files,
            &source_dir,
        )
        .unwrap();
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

        // Write patch using filesystem fixture: "Original." → "Modified."
        let patch_text =
            "--- a/test.md\n+++ b/test.md\n@@ -1,3 +1,3 @@\n # Skill\n \n-Original.\n+Modified.\n";
        write_patch_fixture(dir.path(), &entry, patch_text);

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

        apply_single_file_patch(
            &PatchCtx {
                entry: &entry,
                repo_root: dir.path(),
            },
            &dest,
            &source,
        )
        .unwrap();

        // The installed file must be the patched (== new cache) result.
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), modified);

        // Patch file must have been removed.
        assert!(
            !patch_fixture_path(dir.path(), &entry).exists(),
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

        // Hand-written patch: "Original." → "Modified."
        let patch_text =
            "--- a/test.md\n+++ b/test.md\n@@ -1,3 +1,3 @@\n # Skill\n \n-Original.\n+Modified.\n";
        write_patch_fixture(dir.path(), &entry, patch_text);

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

        apply_single_file_patch(
            &PatchCtx {
                entry: &entry,
                repo_root: dir.path(),
            },
            &dest,
            &source,
        )
        .unwrap();

        // Installed must now be the patched content.
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), modified);

        // The rebased patch must still exist (new_cache != modified).
        assert!(
            patch_fixture_path(dir.path(), &entry).exists(),
            "rebased patch must still exist (new_cache != modified)"
        );
        // Verify the rebased patch yields expected_rebased_result when applied to new_cache.
        // Reset dest to new_cache and call apply_single_file_patch again.
        std::fs::write(&dest, new_cache).unwrap();
        std::fs::write(&source, new_cache).unwrap();
        apply_single_file_patch(
            &PatchCtx {
                entry: &entry,
                repo_root: dir.path(),
            },
            &dest,
            &source,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            expected_rebased_result,
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
        let dir = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };

        write_conflict_fixture(
            dir.path(),
            &ConflictState {
                entry: "my-skill".into(),
                entity_type: EntityType::Skill,
                old_sha: "aaa".into(),
                new_sha: "bbb".into(),
            },
        );

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

    #[test]
    fn newly_fetched_flat_collision_keeps_installed_edit_and_auto_pin_patch() {
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "team".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "agents/team".into(),
                ref_: "main".into(),
            },
        };
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![make_target("claude-code", Scope::Local)],
        };
        let cache = dir.path().join(".skillfile/cache/agents/team");
        std::fs::create_dir_all(cache.join("backend")).unwrap();
        std::fs::write(cache.join("backend/agent.md"), "# Upstream\n").unwrap();
        std::fs::write(cache.join(".meta"), r#"{"sha":"old-sha"}"#).unwrap();
        let installed = dir.path().join(".claude/agents/agent.md");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, "# Local edit\n").unwrap();
        let locked = BTreeMap::from([(
            "github/agent/team".to_string(),
            LockEntry {
                sha: "old-sha".into(),
                raw_url: "https://example.invalid/team".into(),
            },
        )]);
        write_lock_fixture(dir.path(), &locked);

        auto_pin_all(&manifest, dir.path()).unwrap();
        let patch_path = dir_patch_fixture_path(dir.path(), &entry, "backend/agent.md");
        let patch = std::fs::read_to_string(&patch_path).unwrap();
        assert!(patch.contains("+# Local edit\n"));

        // A fetched revision adds a second source for the same flat destination.
        std::fs::create_dir_all(cache.join("frontend")).unwrap();
        std::fs::write(cache.join("frontend/agent.md"), "# New upstream\n").unwrap();
        std::fs::write(cache.join(".meta"), r#"{"sha":"new-sha"}"#).unwrap();
        let options = InstallOptions {
            dry_run: false,
            overwrite: true,
        };
        let error = deploy_all(
            &manifest,
            &DeployCtx {
                repo_root: dir.path(),
                opts: &options,
                maps: LockMaps {
                    locked: &locked,
                    old_locked: &locked,
                },
            },
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("duplicate flat destination"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&installed).unwrap(),
            "# Local edit\n"
        );
        assert_eq!(std::fs::read_to_string(patch_path).unwrap(), patch);
        assert!(!dir.path().join(".skillfile/conflict").exists());
    }

    fn update_collision_lock(sha: &str) -> BTreeMap<String, LockEntry> {
        ["github/agent/team", "github/skill/notes"]
            .into_iter()
            .map(|key| {
                (
                    key.to_string(),
                    LockEntry {
                        sha: sha.into(),
                        raw_url: format!("https://example.invalid/{key}/{sha}"),
                    },
                )
            })
            .collect()
    }

    fn write_update_collision_fixture(root: &Path) {
        std::fs::write(
            root.join("Skillfile"),
            "github agent team owner/repo agents/team\n\
             github skill notes owner/repo skills/notes.md\n\
             install claude-code local\n",
        )
        .unwrap();
        for (relative, content) in [
            (
                ".skillfile/cache/agents/team/backend/agent.md",
                "# Old upstream\n",
            ),
            (".skillfile/cache/agents/team/.meta", r#"{"sha":"old-sha"}"#),
            (".skillfile/cache/skills/notes/notes.md", "# Old skill\n"),
            (".claude/agents/agent.md", "# Local edit\n"),
            (
                ".skillfile/patches/agents/team/backend/agent.md.patch",
                "old patch marker\n",
            ),
        ] {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        write_lock_fixture(root, &update_collision_lock("old-sha"));
    }

    #[test]
    fn update_newly_fetched_flat_collision_restores_cache_lock_and_patches() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_update_collision_fixture(root);
        let old_lock = std::fs::read(root.join("Skillfile.lock")).unwrap();
        let cache = root.join(".skillfile/cache/agents/team");
        let patch = root.join(".skillfile/patches/agents/team/backend/agent.md.patch");

        let error = cmd_install_with_sync(
            root,
            &CmdInstallOpts {
                dry_run: false,
                update: true,
                extra_targets: None,
            },
            || {
                assert_ne!(
                    std::fs::read_to_string(&patch).unwrap(),
                    "old patch marker\n"
                );
                std::fs::write(cache.join("backend/agent.md"), "# New upstream\n").unwrap();
                std::fs::create_dir_all(cache.join("frontend")).unwrap();
                std::fs::write(cache.join("frontend/agent.md"), "# Duplicate\n").unwrap();
                std::fs::write(cache.join(".meta"), r#"{"sha":"new-sha"}"#).unwrap();
                std::fs::write(
                    root.join(".skillfile/cache/skills/notes/notes.md"),
                    "# New skill\n",
                )
                .unwrap();
                write_lock_fixture(root, &update_collision_lock("new-sha"));
                Ok(())
            },
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("duplicate flat destination"), "{error}");
        assert_eq!(
            std::fs::read_to_string(root.join(".claude/agents/agent.md")).unwrap(),
            "# Local edit\n"
        );
        assert_eq!(
            std::fs::read_to_string(&patch).unwrap(),
            "old patch marker\n"
        );
        assert_eq!(
            std::fs::read(root.join("Skillfile.lock")).unwrap(),
            old_lock
        );
        assert_eq!(
            std::fs::read_to_string(cache.join(".meta")).unwrap(),
            r#"{"sha":"old-sha"}"#
        );
        assert_eq!(
            std::fs::read_to_string(cache.join("backend/agent.md")).unwrap(),
            "# Old upstream\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".skillfile/cache/skills/notes/notes.md")).unwrap(),
            "# Old skill\n"
        );
        assert!(!cache.join("frontend/agent.md").exists());
        assert!(!root.join(".skillfile/conflict").exists());
    }

    #[test]
    fn update_collision_also_reports_sync_failure() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_update_collision_fixture(root);
        let old_lock = std::fs::read(root.join("Skillfile.lock")).unwrap();
        let cache = root.join(".skillfile/cache/agents/team");
        let patch = root.join(".skillfile/patches/agents/team/backend/agent.md.patch");
        let mut synced = false;

        let error = cmd_install_with_sync(
            root,
            &CmdInstallOpts {
                dry_run: false,
                update: true,
                extra_targets: None,
            },
            || {
                assert_ne!(
                    std::fs::read_to_string(&patch).unwrap(),
                    "old patch marker\n"
                );
                std::fs::write(cache.join("backend/agent.md"), "# New upstream\n").unwrap();
                std::fs::create_dir_all(cache.join("frontend")).unwrap();
                std::fs::write(cache.join("frontend/agent.md"), "# Duplicate\n").unwrap();
                std::fs::write(cache.join(".meta"), r#"{"sha":"new-sha"}"#).unwrap();
                write_lock_fixture(root, &update_collision_lock("new-sha"));
                assert_ne!(
                    std::fs::read(root.join("Skillfile.lock")).unwrap(),
                    old_lock
                );
                synced = true;
                Err(SkillfileError::Network("HTTP 403 fetching notes".into()))
            },
        )
        .unwrap_err()
        .to_string();

        assert!(synced, "sync must change state before rollback");
        assert!(error.contains("backend/agent.md"), "{error}");
        assert!(error.contains("frontend/agent.md"), "{error}");
        assert!(error.contains("HTTP 403 fetching notes"), "{error}");
        assert_update_collision_rollback(root, &old_lock, "agent.md");
    }

    #[test]
    fn update_case_alias_restores_state_and_keeps_sync_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let target = root.join(".claude/agents");
        std::fs::create_dir_all(&target).unwrap();
        if !target_aliases_case_names(&target) {
            return;
        }
        write_update_collision_fixture(root);
        let old_lock = std::fs::read(root.join("Skillfile.lock")).unwrap();
        let cache = root.join(".skillfile/cache/agents/team");

        let error = cmd_install_with_sync(
            root,
            &CmdInstallOpts {
                dry_run: false,
                update: true,
                extra_targets: None,
            },
            || {
                std::fs::create_dir_all(cache.join("frontend")).unwrap();
                std::fs::write(cache.join("frontend/Agent.md"), "# Alias\n").unwrap();
                write_lock_fixture(root, &update_collision_lock("new-sha"));
                Err(SkillfileError::Network("HTTP 403 fetching notes".into()))
            },
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("backend/agent.md"), "{error}");
        assert!(error.contains("frontend/Agent.md"), "{error}");
        assert!(error.contains("HTTP 403 fetching notes"), "{error}");
        assert_update_collision_rollback(root, &old_lock, "Agent.md");
    }

    fn assert_update_collision_rollback(root: &Path, old_lock: &[u8], new_name: &str) {
        let cache = root.join(".skillfile/cache/agents/team");
        assert_eq!(
            std::fs::read(root.join("Skillfile.lock")).unwrap(),
            old_lock
        );
        assert_eq!(
            std::fs::read_to_string(
                root.join(".skillfile/patches/agents/team/backend/agent.md.patch")
            )
            .unwrap(),
            "old patch marker\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".claude/agents/agent.md")).unwrap(),
            "# Local edit\n"
        );
        assert_eq!(
            std::fs::read_to_string(cache.join("backend/agent.md")).unwrap(),
            "# Old upstream\n"
        );
        assert_eq!(
            std::fs::read_to_string(cache.join(".meta")).unwrap(),
            r#"{"sha":"old-sha"}"#
        );
        assert!(!cache.join("frontend").join(new_name).exists());
    }

    #[test]
    fn update_without_remote_flat_agent_skips_collision_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Skillfile"),
            "local skill notes skills/notes.md\ninstall claude-code local\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("skills")).unwrap();
        std::fs::write(root.join("skills/notes.md"), "# Notes\n").unwrap();

        cmd_install_with_sync(
            root,
            &CmdInstallOpts {
                dry_run: false,
                update: true,
                extra_targets: None,
            },
            || {
                assert!(!root.join(".skillfile/tmp").exists());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join(".claude/skills/notes/SKILL.md")).unwrap(),
            "# Notes\n"
        );
    }
}
