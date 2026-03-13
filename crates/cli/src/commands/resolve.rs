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

fn three_way_merge(
    base: &str,
    theirs: &str,
    yours: &str,
    filename: &str,
) -> Result<(String, bool), SkillfileError> {
    use std::io::Write;

    let tmpdir = tempfile::tempdir()
        .map_err(|e| SkillfileError::Manifest(format!("failed to create temp dir: {e}")))?;

    let base_f = tmpdir.path().join(format!("base_{filename}"));
    let theirs_f = tmpdir.path().join(format!("theirs_{filename}"));
    let yours_f = tmpdir.path().join(format!("yours_{filename}"));

    std::fs::File::create(&base_f)
        .and_then(|mut f| f.write_all(base.as_bytes()))
        .map_err(|e| SkillfileError::Manifest(format!("failed to write temp file: {e}")))?;
    std::fs::File::create(&theirs_f)
        .and_then(|mut f| f.write_all(theirs.as_bytes()))
        .map_err(|e| SkillfileError::Manifest(format!("failed to write temp file: {e}")))?;
    std::fs::File::create(&yours_f)
        .and_then(|mut f| f.write_all(yours.as_bytes()))
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
    Ok((merged, has_conflicts))
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

    let result = parse_manifest(&repo_root.join(MANIFEST_NAME))?;
    let installed = installed_path(entry, &result.manifest, repo_root)?;

    // Reconstruct "yours" from stored patch applied to base upstream
    let yours = if has_patch(entry, repo_root) {
        let patch_text = read_patch(entry, repo_root)?;
        apply_patch_pure(&base, &patch_text)?
    } else {
        if !installed.exists() {
            return Err(SkillfileError::Manifest(format!(
                "'{}' is not installed at {}",
                entry.name,
                installed.display()
            )));
        }
        std::fs::read_to_string(&installed)?
    };

    progress!("  merging ...");
    let (mut merged, has_conflicts) = three_way_merge(&base, &theirs, &yours, &filename)?;

    if has_conflicts {
        eprintln!(
            "\nConflicts detected in '{}'. Opening in editor to resolve...\n  Save and close when done.",
            entry.name
        );
        merged = open_in_editor(&merged, &filename)?;
        if merged.contains("<<<<<<<") {
            eprintln!(
                "error: conflict markers still present — resolve all conflicts and try again"
            );
            return Ok(());
        }
    } else {
        progress!("  clean merge — no conflicts in '{}'", entry.name);
    }

    // Write merged result to installed path
    std::fs::write(&installed, &merged)?;

    // Regenerate patch: diff between new upstream and merged result
    let patch_text = generate_patch(&theirs, &merged, &filename);
    if !patch_text.is_empty() {
        write_patch(entry, &patch_text, repo_root)?;
        progress!("  updated .skillfile/patches/ for '{}'", entry.name);
    } else {
        remove_patch(entry, repo_root)?;
        progress!(
            "  merged result matches upstream — removed pin for '{}'",
            entry.name
        );
    }

    clear_conflict(repo_root)?;
    println!(
        "\nResolved. Run `skillfile install` to deploy '{}'.",
        entry.name
    );
    Ok(())
}

/// Merge each file using three-way merge. Returns the merged results and whether
/// any file had conflicts requiring manual resolution. Returns `Ok(None)` when
/// conflict markers remain after editor interaction (signals early exit to caller).
fn merge_all_files(
    all_filenames: &[String],
    base_files: &FileMap,
    theirs_files: &FileMap,
    entry: &skillfile_core::models::Entry,
    installed: &std::collections::HashMap<String, std::path::PathBuf>,
    repo_root: &Path,
) -> Result<Option<(FileMap, bool)>, SkillfileError> {
    let mut merged_results = FileMap::new();
    let mut any_conflict = false;

    for filename in all_filenames {
        let base = base_files.get(filename).map(|s| s.as_str()).unwrap_or("");
        let theirs = theirs_files.get(filename).map(|s| s.as_str()).unwrap_or("");

        // Reconstruct "yours" from stored patch + base
        let p = dir_patch_path(entry, filename, repo_root);
        let yours = if p.exists() {
            let patch_text = std::fs::read_to_string(&p)?;
            apply_patch_pure(base, &patch_text)?
        } else {
            match installed.get(filename) {
                Some(inst_path) if inst_path.exists() => std::fs::read_to_string(inst_path)?,
                _ => base.to_string(),
            }
        };

        let (mut merged, has_conflicts) = three_way_merge(base, theirs, &yours, filename)?;

        if has_conflicts {
            any_conflict = true;
            eprintln!("\n  Conflicts in '{filename}'. Opening in editor...");
            merged = open_in_editor(&merged, filename)?;
            if merged.contains("<<<<<<<") {
                eprintln!(
                    "error: conflict markers still present in '{filename}' — resolve and try again"
                );
                return Ok(None);
            }
        } else {
            progress!("  {filename}: clean merge");
        }

        merged_results.insert(filename.clone(), merged);
    }

    Ok(Some((merged_results, any_conflict)))
}

/// Write merged results to installed paths and update patch files.
fn write_merged_results(
    merged_results: &FileMap,
    theirs_files: &FileMap,
    entry: &skillfile_core::models::Entry,
    installed: &std::collections::HashMap<String, std::path::PathBuf>,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    remove_all_dir_patches(entry, repo_root)?;
    let mut pinned: Vec<String> = Vec::new();
    for (filename, merged_text) in merged_results {
        let theirs = theirs_files.get(filename).map(|s| s.as_str()).unwrap_or("");
        if let Some(inst_path) = installed.get(filename) {
            std::fs::write(inst_path, merged_text)?;
        }
        let patch_text = generate_patch(theirs, merged_text, filename);
        if !patch_text.is_empty() {
            write_dir_patch(entry, filename, &patch_text, repo_root)?;
            pinned.push(filename.clone());
        }
    }

    if !pinned.is_empty() {
        progress!(
            "  updated .skillfile/patches/ for '{}' ({})",
            entry.name,
            pinned.join(", ")
        );
    } else {
        progress!(
            "  merged result matches upstream — no pin needed for '{}'",
            entry.name
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

    let result = parse_manifest(&repo_root.join(MANIFEST_NAME))?;
    let installed = installed_dir_files(entry, &result.manifest, repo_root)?;
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

    let Some((merged_results, any_conflict)) = merge_all_files(
        &all_filenames,
        &base_files,
        &theirs_files,
        entry,
        &installed,
        repo_root,
    )?
    else {
        return Ok(());
    };

    if !any_conflict {
        progress!("  all files merged cleanly in '{}'", entry.name);
    }

    write_merged_results(&merged_results, &theirs_files, entry, &installed, repo_root)?;

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
    use skillfile_core::models::ConflictState;

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::write(dir.join(MANIFEST_NAME), content).unwrap();
    }

    fn make_conflict(entry: &str, entity_type: &str) -> ConflictState {
        ConflictState {
            entry: entry.to_string(),
            entity_type: entity_type.to_string(),
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
        let conflict = make_conflict("test", "skill");
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
        let conflict = make_conflict("other-entry", "skill");
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
        let base = "line1\nline2\n";
        let theirs = "line1\nline2\nline3-theirs\n";
        let yours = "line0-yours\nline1\nline2\n";
        let (merged, has_conflicts) = three_way_merge(base, theirs, yours, "test.md").unwrap();
        assert!(!has_conflicts, "expected clean merge");
        assert!(
            merged.contains("line3-theirs"),
            "merged missing theirs change"
        );
        assert!(
            merged.contains("line0-yours"),
            "merged missing yours change"
        );
    }

    #[test]
    fn three_way_merge_conflict() {
        // Both sides modify the same line — conflict expected
        let base = "line1\n";
        let theirs = "THEIRS\n";
        let yours = "YOURS\n";
        let (merged, has_conflicts) = three_way_merge(base, theirs, yours, "test.md").unwrap();
        assert!(has_conflicts, "expected conflict");
        assert!(merged.contains("<<<<<<<"), "expected conflict markers");
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
        let (merged, has_conflicts) =
            three_way_merge(base, theirs, &reconstructed, "test.md").unwrap();
        assert!(!has_conflicts);
        assert_eq!(merged, yours);
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
        let conflict = make_conflict("nonexistent", "skill");
        write_conflict(dir.path(), &conflict).unwrap();
        let result = cmd_resolve(Some("nonexistent"), false, dir.path());
        assert!(result.is_err(), "expected error for entry not in manifest");
    }
}
