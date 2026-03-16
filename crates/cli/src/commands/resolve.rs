use std::path::Path;
use std::process::Command;

use skillfile_core::conflict::{clear_conflict, read_conflict};
use skillfile_core::error::SkillfileError;
use skillfile_core::models::{short_sha, ConflictState};
use skillfile_core::parser::{find_entry_in, parse_manifest, MANIFEST_NAME};
use skillfile_core::progress;
use skillfile_deploy::paths::{installed_dir_files, installed_path};
use skillfile_sources::strategy::is_dir_entry;
use skillfile_sources::sync::{fetch_dir_at_sha, fetch_file_at_sha};

use crate::patch::{
    apply_patch_pure, dir_patch_path, generate_patch, has_patch, read_patch,
    remove_all_dir_patches, remove_patch, write_dir_patch, write_patch,
};

/// Map of filename to file content used during dir-entry merge operations.
type FileMap = std::collections::HashMap<String, String>;

/// Three versions to merge.
struct MergeInput<'a> {
    base: &'a str,
    theirs: &'a str,
    yours: &'a str,
}

/// Result of a three-way merge.
struct MergeResult {
    merged: String,
    has_conflicts: bool,
}

/// Bundles entry + repo root for single-file resolve operations.
struct ResolveEntryCtx<'a> {
    entry: &'a skillfile_core::models::Entry,
    repo_root: &'a Path,
}

/// Context for dir-entry merge and write-back operations.
struct DirMergeCtx<'a> {
    entry: &'a skillfile_core::models::Entry,
    installed: &'a std::collections::HashMap<String, std::path::PathBuf>,
    repo_root: &'a Path,
}

fn three_way_merge(input: &MergeInput<'_>, filename: &str) -> Result<MergeResult, SkillfileError> {
    use std::io::Write;

    let tmpdir = tempfile::tempdir()
        .map_err(|e| SkillfileError::Manifest(format!("failed to create temp dir: {e}")))?;

    let base_f = tmpdir.path().join(format!("base_{filename}"));
    let theirs_f = tmpdir.path().join(format!("theirs_{filename}"));
    let yours_f = tmpdir.path().join(format!("yours_{filename}"));

    std::fs::File::create(&base_f)
        .and_then(|mut f| f.write_all(input.base.as_bytes()))
        .map_err(|e| SkillfileError::Manifest(format!("failed to write temp file: {e}")))?;
    std::fs::File::create(&theirs_f)
        .and_then(|mut f| f.write_all(input.theirs.as_bytes()))
        .map_err(|e| SkillfileError::Manifest(format!("failed to write temp file: {e}")))?;
    std::fs::File::create(&yours_f)
        .and_then(|mut f| f.write_all(input.yours.as_bytes()))
        .map_err(|e| SkillfileError::Manifest(format!("failed to write temp file: {e}")))?;

    let output = Command::new("git")
        .args([
            "merge-file",
            "-p",
            "--diff3",
            &yours_f.to_string_lossy(),
            &base_f.to_string_lossy(),
            &theirs_f.to_string_lossy(),
        ])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SkillfileError::Install(
                    "`git` not found — install git to use `skillfile resolve`".into(),
                )
            } else {
                SkillfileError::Install(format!("git merge-file failed: {e}"))
            }
        })?;

    // exit 0 = clean merge, >0 = conflicts, <0 = error (would be negative but Command uses i32)
    let has_conflicts = !output.status.success();
    let merged = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(MergeResult {
        merged,
        has_conflicts,
    })
}

fn open_in_editor(content: &str, filename: &str) -> Result<String, SkillfileError> {
    use std::io::Write;

    let editor = std::env::var("MERGETOOL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());

    let tmp = tempfile::Builder::new()
        .suffix(&format!("_{filename}"))
        .tempfile()
        .map_err(|e| SkillfileError::Manifest(format!("failed to create temp file: {e}")))?;

    tmp.as_file()
        .write_all(content.as_bytes())
        .map_err(|e| SkillfileError::Manifest(format!("failed to write temp file: {e}")))?;
    // Persist to prevent deletion before editor opens
    let path = tmp.into_temp_path();

    Command::new(&editor)
        .arg(path.as_os_str())
        .status()
        .map_err(|e| SkillfileError::Install(format!("failed to open editor '{editor}': {e}")))?;

    let result = std::fs::read_to_string(&path)
        .map_err(|e| SkillfileError::Manifest(format!("failed to read temp file: {e}")))?;
    Ok(result)
}

/// Reconstruct the user's version ("yours") for a single-file entry.
fn reconstruct_yours_single(
    ctx: &ResolveEntryCtx<'_>,
    base: &str,
    installed: &std::path::Path,
) -> Result<String, SkillfileError> {
    if has_patch(ctx.entry, ctx.repo_root) {
        let patch_text = read_patch(ctx.entry, ctx.repo_root)?;
        return apply_patch_pure(base, &patch_text);
    }
    if !installed.exists() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not installed at {}",
            ctx.entry.name,
            installed.display()
        )));
    }
    Ok(std::fs::read_to_string(installed)?)
}

/// Apply conflict resolution: open editor when needed, return resolved text.
/// Returns `Ok(None)` when conflict markers remain after editing.
fn resolve_conflicts_or_clean(
    result: MergeResult,
    entry_name: &str,
    filename: &str,
) -> Result<Option<String>, SkillfileError> {
    if !result.has_conflicts {
        progress!("  clean merge — no conflicts in '{entry_name}'");
        return Ok(Some(result.merged));
    }
    eprintln!(
        "\nConflicts detected in '{entry_name}'. Opening in editor to resolve...\n  Save and close when done."
    );
    let resolved = open_in_editor(&result.merged, filename)?;
    if resolved.contains("<<<<<<<") {
        eprintln!("error: conflict markers still present — resolve all conflicts and try again");
        return Ok(None);
    }
    Ok(Some(resolved))
}

fn resolve_single_file(
    entry: &skillfile_core::models::Entry,
    conflict: &ConflictState,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    let filename = format!("{}.md", entry.name);
    let client = skillfile_sources::http::UreqClient::new();

    progress!(
        "  fetching upstream at old sha={} (common ancestor) ...",
        short_sha(&conflict.old_sha)
    );
    let base = fetch_file_at_sha(&client, entry, &conflict.old_sha)?;
    progress!("done");

    progress!(
        "  fetching upstream at new sha={} ...",
        short_sha(&conflict.new_sha)
    );
    let theirs = fetch_file_at_sha(&client, entry, &conflict.new_sha)?;
    progress!("done");

    let manifest = crate::config::parse_and_resolve(&repo_root.join(MANIFEST_NAME))?;
    let installed = installed_path(entry, &manifest, repo_root)?;
    let ctx = ResolveEntryCtx { entry, repo_root };
    let yours = reconstruct_yours_single(&ctx, &base, &installed)?;

    progress!("  merging ...");
    let input = MergeInput {
        base: &base,
        theirs: &theirs,
        yours: &yours,
    };
    let result = three_way_merge(&input, &filename)?;
    let Some(merged) = resolve_conflicts_or_clean(result, &entry.name, &filename)? else {
        return Ok(());
    };

    std::fs::write(&installed, &merged)?;

    let patch_text = generate_patch(&theirs, &merged, &filename);
    if patch_text.is_empty() {
        remove_patch(entry, repo_root)?;
        progress!(
            "  merged result matches upstream — removed pin for '{}'",
            entry.name
        );
    } else {
        write_patch(entry, &patch_text, repo_root)?;
        progress!("  updated .skillfile/patches/ for '{}'", entry.name);
    }

    clear_conflict(repo_root)?;
    println!(
        "\nResolved. Run `skillfile install` to deploy '{}'.",
        entry.name
    );
    Ok(())
}

/// Upstream base and new versions for dir-entry merge.
struct UpstreamVersions<'a> {
    filenames: &'a [String],
    base: &'a FileMap,
    theirs: &'a FileMap,
}

/// Merge each file using three-way merge. Returns the merged results and whether
/// any file had conflicts requiring manual resolution. Returns `Ok(None)` when
/// conflict markers remain after editor interaction (signals early exit to caller).
fn merge_all_files(
    upstream: &UpstreamVersions<'_>,
    ctx: &DirMergeCtx<'_>,
) -> Result<Option<(FileMap, bool)>, SkillfileError> {
    let mut merged_results = FileMap::new();
    let mut any_conflict = false;

    for filename in upstream.filenames {
        let base = upstream
            .base
            .get(filename)
            .map_or("", std::string::String::as_str);
        let theirs = upstream
            .theirs
            .get(filename)
            .map_or("", std::string::String::as_str);

        // Reconstruct "yours" from stored patch + base
        let p = dir_patch_path(ctx.entry, filename, ctx.repo_root);
        let yours = if p.exists() {
            let patch_text = std::fs::read_to_string(&p)?;
            apply_patch_pure(base, &patch_text)?
        } else {
            match ctx.installed.get(filename) {
                Some(inst_path) if inst_path.exists() => std::fs::read_to_string(inst_path)?,
                _ => base.to_string(),
            }
        };

        let input = MergeInput {
            base,
            theirs,
            yours: &yours,
        };
        let result = three_way_merge(&input, filename)?;
        if result.has_conflicts {
            any_conflict = true;
            eprintln!("\n  Conflicts in '{filename}'. Opening in editor...");
        }
        let Some(merged) = resolve_conflicts_or_clean(result, filename, filename)? else {
            return Ok(None);
        };

        merged_results.insert(filename.clone(), merged);
    }

    Ok(Some((merged_results, any_conflict)))
}

/// Write merged results to installed paths and update patch files.
fn write_merged_results(
    merged_results: &FileMap,
    theirs_files: &FileMap,
    ctx: &DirMergeCtx<'_>,
) -> Result<(), SkillfileError> {
    remove_all_dir_patches(ctx.entry, ctx.repo_root)?;
    let mut pinned: Vec<String> = Vec::new();
    for (filename, merged_text) in merged_results {
        let theirs = theirs_files
            .get(filename)
            .map_or("", std::string::String::as_str);
        if let Some(inst_path) = ctx.installed.get(filename) {
            std::fs::write(inst_path, merged_text)?;
        }
        let patch_text = generate_patch(theirs, merged_text, filename);
        if !patch_text.is_empty() {
            write_dir_patch(
                &dir_patch_path(ctx.entry, filename, ctx.repo_root),
                &patch_text,
            )?;
            pinned.push(filename.clone());
        }
    }

    if pinned.is_empty() {
        progress!(
            "  merged result matches upstream — no pin needed for '{}'",
            ctx.entry.name
        );
    } else {
        progress!(
            "  updated .skillfile/patches/ for '{}' ({})",
            ctx.entry.name,
            pinned.join(", ")
        );
    }
    Ok(())
}

fn resolve_dir_entry(
    entry: &skillfile_core::models::Entry,
    conflict: &ConflictState,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    let client = skillfile_sources::http::UreqClient::new();

    progress!(
        "  fetching upstream at old sha={} (common ancestor) ...",
        short_sha(&conflict.old_sha)
    );
    let base_files = fetch_dir_at_sha(&client, entry, &conflict.old_sha)?;
    progress!("done");

    progress!(
        "  fetching upstream at new sha={} ...",
        short_sha(&conflict.new_sha)
    );
    let theirs_files = fetch_dir_at_sha(&client, entry, &conflict.new_sha)?;
    progress!("done");

    let manifest = crate::config::parse_and_resolve(&repo_root.join(MANIFEST_NAME))?;
    let installed = installed_dir_files(entry, &manifest, repo_root)?;
    if installed.is_empty() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not installed",
            entry.name
        )));
    }

    let mut all_filenames: Vec<String> = base_files
        .keys()
        .chain(theirs_files.keys())
        .cloned()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    all_filenames.sort();

    let dir_ctx = DirMergeCtx {
        entry,
        installed: &installed,
        repo_root,
    };

    let upstream = UpstreamVersions {
        filenames: &all_filenames,
        base: &base_files,
        theirs: &theirs_files,
    };
    let Some((merged_results, any_conflict)) = merge_all_files(&upstream, &dir_ctx)? else {
        return Ok(());
    };

    if !any_conflict {
        progress!("  all files merged cleanly in '{}'", entry.name);
    }

    write_merged_results(&merged_results, &theirs_files, &dir_ctx)?;

    clear_conflict(repo_root)?;
    println!(
        "\nResolved. Run `skillfile install` to deploy '{}'.",
        entry.name
    );
    Ok(())
}

pub fn cmd_resolve(
    name: Option<&str>,
    abort: bool,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    if abort {
        let conflict = read_conflict(repo_root)?;
        match conflict {
            None => {
                println!("No pending conflict to abort.");
            }
            Some(c) => {
                clear_conflict(repo_root)?;
                println!(
                    "Conflict for '{}' cleared. Run `skillfile install` to continue.",
                    c.entry
                );
            }
        }
        return Ok(());
    }

    let name = name
        .ok_or_else(|| SkillfileError::Manifest("entry name required (unless --abort)".into()))?;

    let manifest_path = repo_root.join(MANIFEST_NAME);
    let result = parse_manifest(&manifest_path)?;
    let entry = find_entry_in(name, &result.manifest)?;

    let conflict = read_conflict(repo_root)?;
    let conflict = match conflict {
        None => {
            return Err(SkillfileError::Manifest(format!(
                "no pending conflict for '{name}' — \
                `skillfile resolve` is only available after a conflict is detected by `skillfile install --update`"
            )))
        }
        Some(c) if c.entry != name => {
            return Err(SkillfileError::Manifest(format!(
                "no pending conflict for '{name}' — \
                `skillfile resolve` is only available after a conflict is detected by `skillfile install --update`"
            )))
        }
        Some(c) => c,
    };

    if is_dir_entry(entry) {
        resolve_dir_entry(entry, &conflict, repo_root)
    } else {
        resolve_single_file(entry, &conflict, repo_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skillfile_core::conflict::write_conflict;
    use skillfile_core::models::{ConflictState, EntityType};

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::write(dir.join(MANIFEST_NAME), content).unwrap();
    }

    fn make_conflict(entry: &str, entity_type: EntityType) -> ConflictState {
        ConflictState {
            entry: entry.to_string(),
            entity_type,
            old_sha: "aaaaaa".to_string(),
            new_sha: "bbbbbb".to_string(),
        }
    }

    #[test]
    fn resolve_abort_no_conflict_noop() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let result = cmd_resolve(None, true, dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_abort_clears_conflict() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        let conflict = make_conflict("test", EntityType::Skill);
        write_conflict(dir.path(), &conflict).unwrap();

        cmd_resolve(None, true, dir.path()).unwrap();

        // Conflict should be cleared
        let c = read_conflict(dir.path()).unwrap();
        assert!(c.is_none());
    }

    #[test]
    fn resolve_no_conflict_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "github  skill  owner/repo  skills/test.md\n");
        let result = cmd_resolve(Some("test"), false, dir.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no pending conflict"));
    }

    #[test]
    fn resolve_wrong_entry_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "github  skill  owner/repo  skills/test.md\n");
        let conflict = make_conflict("other-entry", EntityType::Skill);
        write_conflict(dir.path(), &conflict).unwrap();

        let result = cmd_resolve(Some("test"), false, dir.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no pending conflict"));
    }

    #[test]
    fn resolve_no_manifest_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_resolve(Some("test"), false, dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn three_way_merge_clean() {
        // Simple clean merge: base->theirs adds a line, base->yours adds different line
        let input = MergeInput {
            base: "line1\nline2\n",
            theirs: "line1\nline2\nline3-theirs\n",
            yours: "line0-yours\nline1\nline2\n",
        };
        let result = three_way_merge(&input, "test.md").unwrap();
        assert!(!result.has_conflicts, "expected clean merge");
        assert!(
            result.merged.contains("line3-theirs"),
            "merged missing theirs change"
        );
        assert!(
            result.merged.contains("line0-yours"),
            "merged missing yours change"
        );
    }

    #[test]
    fn three_way_merge_conflict() {
        // Both sides modify the same line — conflict expected
        let input = MergeInput {
            base: "line1\n",
            theirs: "THEIRS\n",
            yours: "YOURS\n",
        };
        let result = three_way_merge(&input, "test.md").unwrap();
        assert!(result.has_conflicts, "expected conflict");
        assert!(
            result.merged.contains("<<<<<<<"),
            "expected conflict markers"
        );
    }

    #[test]
    fn apply_patch_round_trip_via_three_way() {
        // Test the full resolve workflow logic:
        // base -> yours (via patch) -> merge with theirs (same as base) -> result = yours
        let base = "original content\n";
        let yours = "modified content\n";
        let theirs = base; // upstream didn't change
        let patch = crate::patch::generate_patch(base, yours, "test.md");
        let reconstructed = apply_patch_pure(base, &patch).unwrap();
        assert_eq!(reconstructed, yours);
        let input = MergeInput {
            base,
            theirs,
            yours: &reconstructed,
        };
        let result = three_way_merge(&input, "test.md").unwrap();
        assert!(!result.has_conflicts);
        assert_eq!(result.merged, yours);
    }

    #[test]
    fn cmd_resolve_no_name_no_abort_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "github  skill  owner/repo  skills/test.md\n");
        let result = cmd_resolve(None, false, dir.path());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("entry name required"),
            "expected 'entry name required' in error message"
        );
    }

    #[test]
    fn cmd_resolve_entry_not_in_manifest_errors() {
        let dir = tempfile::tempdir().unwrap();
        // Manifest has "test" but we request "nonexistent"
        write_manifest(dir.path(), "github  skill  owner/repo  skills/test.md\n");
        let conflict = make_conflict("nonexistent", EntityType::Skill);
        write_conflict(dir.path(), &conflict).unwrap();
        let result = cmd_resolve(Some("nonexistent"), false, dir.path());
        assert!(result.is_err(), "expected error for entry not in manifest");
    }
}
