use std::path::Path;

use skillfile_core::error::SkillfileError;
use skillfile_core::lock::{lock_key, read_lock};
use skillfile_core::models::Entry;
use skillfile_core::parser::{find_entry_in, parse_manifest, MANIFEST_NAME};
use skillfile_deploy::install::install_entry;
use skillfile_deploy::paths::{installed_dir_files, installed_path};
use skillfile_sources::strategy::{content_file, is_dir_entry};
use skillfile_sources::sync::vendor_dir_for;

use crate::patch::{
    dir_patch_path, generate_patch, has_dir_patch, has_patch, remove_all_dir_patches,
    remove_dir_patch, remove_patch, walkdir, write_dir_patch, write_patch,
};

struct PinCtx<'a> {
    entry: &'a Entry,
    repo_root: &'a Path,
    dry_run: bool,
}

struct DirFileRef<'a> {
    cache: &'a std::path::Path,
    installed: &'a std::path::Path,
    filename: &'a str,
}

/// Process a single file in a dir entry: generate a patch and write or remove it.
/// Returns the filename if the file was pinned (patch is non-empty), or `None`.
fn process_dir_file(
    ctx: &PinCtx<'_>,
    file: &DirFileRef<'_>,
) -> Result<Option<String>, SkillfileError> {
    let original_text = std::fs::read_to_string(file.cache)?;
    let inst_text = std::fs::read_to_string(file.installed)?;
    let patch_text = generate_patch(&original_text, &inst_text, file.filename);

    if patch_text.is_empty() {
        if !ctx.dry_run {
            remove_dir_patch(ctx.entry, file.filename, ctx.repo_root)?;
        }
        return Ok(None);
    }
    if !ctx.dry_run {
        write_dir_patch(
            &dir_patch_path(ctx.entry, file.filename, ctx.repo_root),
            &patch_text,
        )?;
    }
    Ok(Some(file.filename.to_string()))
}

fn pin_dir_entry(entry: &Entry, repo_root: &Path, dry_run: bool) -> Result<String, SkillfileError> {
    let vdir = vendor_dir_for(entry, repo_root);
    if !vdir.is_dir() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not cached — run `skillfile install` first",
            entry.name
        )));
    }

    let manifest = crate::config::parse_and_resolve(&repo_root.join(MANIFEST_NAME))?;
    let installed = installed_dir_files(entry, &manifest, repo_root)?;
    if installed.is_empty() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not installed — run `skillfile install` first",
            entry.name
        )));
    }

    let pin_ctx = PinCtx {
        entry,
        repo_root,
        dry_run,
    };
    let mut pinned: Vec<String> = Vec::new();

    for cache_file in walkdir(&vdir) {
        if cache_file.file_name().is_some_and(|n| n == ".meta") {
            continue;
        }
        let filename = cache_file
            .strip_prefix(&vdir)
            .ok()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_string();
        if filename.is_empty() {
            continue;
        }
        let Some(inst_path) = installed.get(&filename) else {
            continue;
        };
        if !inst_path.exists() {
            continue;
        }
        if let Some(f) = process_dir_file(
            &pin_ctx,
            &DirFileRef {
                cache: &cache_file,
                installed: inst_path,
                filename: &filename,
            },
        )? {
            pinned.push(f);
        }
    }

    let prefix = if dry_run { "Would pin" } else { "Pinned" };
    if pinned.is_empty() {
        Ok(format!(
            "'{}' matches upstream — nothing to pin",
            entry.name
        ))
    } else {
        Ok(format!("{prefix} '{}' ({})", entry.name, pinned.join(", ")))
    }
}

fn pin_entry(entry: &Entry, repo_root: &Path, dry_run: bool) -> Result<String, SkillfileError> {
    if entry.source_type() == "local" {
        return Ok(format!("'{}' is a local entry — skipped", entry.name));
    }

    let locked = read_lock(repo_root)?;
    let key = lock_key(entry);
    if !locked.contains_key(&key) {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not locked — run `skillfile install` first",
            entry.name
        )));
    }

    if is_dir_entry(entry) {
        return pin_dir_entry(entry, repo_root, dry_run);
    }

    // Single-file entry
    let vdir = vendor_dir_for(entry, repo_root);
    let cf = content_file(entry);
    let cache_file = if cf.is_empty() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not cached — run `skillfile install` first",
            entry.name
        )));
    } else {
        vdir.join(&cf)
    };

    if !cache_file.exists() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not cached — run `skillfile install` first",
            entry.name
        )));
    }

    let manifest = crate::config::parse_and_resolve(&repo_root.join(MANIFEST_NAME))?;
    let dest = installed_path(entry, &manifest, repo_root)?;
    if !dest.exists() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not installed — run `skillfile install` first",
            entry.name
        )));
    }

    let label = format!("{}.md", entry.name);
    let cache_text = std::fs::read_to_string(&cache_file)?;
    let dest_text = std::fs::read_to_string(&dest)?;
    let patch_text = generate_patch(&cache_text, &dest_text, &label);

    if patch_text.is_empty() {
        return Ok(format!(
            "'{}' matches upstream — nothing to pin",
            entry.name
        ));
    }

    if !dry_run {
        write_patch(entry, &patch_text, repo_root)?;
    }
    let prefix = if dry_run { "Would pin" } else { "Pinned" };
    Ok(format!("{prefix} '{}'", entry.name))
}

pub fn cmd_pin(name: &str, repo_root: &Path, dry_run: bool) -> Result<(), SkillfileError> {
    let manifest_path = repo_root.join(MANIFEST_NAME);
    let result = parse_manifest(&manifest_path)?;
    let entry = find_entry_in(name, &result.manifest)?;

    let status = pin_entry(entry, repo_root, dry_run)?;

    if status.starts_with("Pinned") {
        println!("{status} — customisations saved to .skillfile/patches/");
    } else if status.starts_with("Would pin") {
        println!("{status} [dry-run]");
    } else {
        println!("{status}");
    }

    Ok(())
}

pub fn cmd_unpin(name: &str, repo_root: &Path) -> Result<(), SkillfileError> {
    let manifest = crate::config::parse_and_resolve(&repo_root.join(MANIFEST_NAME))?;
    let entry = find_entry_in(name, &manifest)?;

    let single = has_patch(entry, repo_root);
    let directory = has_dir_patch(entry, repo_root);

    if !single && !directory {
        println!("'{name}' is not pinned");
        return Ok(());
    }

    if single {
        remove_patch(entry, repo_root)?;
    }
    if directory {
        remove_all_dir_patches(entry, repo_root)?;
    }

    // Restore pristine upstream from vendor cache immediately.
    for target in &manifest.install_targets {
        install_entry(
            entry,
            target,
            &skillfile_deploy::install::InstallCtx {
                repo_root,
                opts: None,
            },
        )?;
    }

    println!("Unpinned '{name}' — restored to upstream version");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use skillfile_core::models::{EntityType, InstallTarget, Scope, SourceFields};

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::write(dir.join(MANIFEST_NAME), content).unwrap();
    }

    fn write_lock(dir: &Path, content: &str) {
        std::fs::write(dir.join("Skillfile.lock"), content).unwrap();
    }

    fn github_entry_skill(name: &str, path_in_repo: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: name.to_string(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: path_in_repo.to_string(),
                ref_: "main".into(),
            },
        }
    }

    fn make_lock_json(name: &str, entity_type: &str) -> String {
        format!(
            r#"{{
  "github/{entity_type}/{name}": {{
    "sha": "abc123def456",
    "raw_url": "https://raw.githubusercontent.com/owner/repo/abc123/test.md"
  }}
}}"#
        )
    }

    #[test]
    fn pin_no_manifest_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_pin("foo", dir.path(), false);
        assert!(result.is_err());
    }

    #[test]
    fn pin_local_entry_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");

        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "foo".into(),
            source: SourceFields::Local {
                path: "skills/foo.md".into(),
            },
        };
        let result = pin_entry(&entry, dir.path(), false).unwrap();
        assert!(result.contains("local entry — skipped"));
    }

    #[test]
    fn pin_not_locked_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "github  skill  owner/repo  agents/test.md\n");
        write_lock(dir.path(), "{}");
        let entry = github_entry_skill("test", "agents/test.md");
        let result = pin_entry(&entry, dir.path(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not locked"));
    }

    #[test]
    fn pin_not_cached_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "github  skill  owner/repo  skills/test.md\n");
        write_lock(dir.path(), &make_lock_json("test", "skill"));
        let entry = github_entry_skill("test", "skills/test.md");
        let result = pin_entry(&entry, dir.path(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not cached"));
    }

    #[test]
    fn pin_not_installed_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  owner/repo  skills/test.md\n",
        );
        write_lock(dir.path(), &make_lock_json("test", "skill"));

        // Create cache but not installed
        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("test.md"), "upstream content\n").unwrap();

        let entry = github_entry_skill("test", "skills/test.md");
        let result = pin_entry(&entry, dir.path(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not installed"));
    }

    #[test]
    fn pin_matches_upstream_nothing_to_pin() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  owner/repo  skills/test.md\n",
        );
        write_lock(dir.path(), &make_lock_json("test", "skill"));

        // Create cache
        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        let content = "# Test Skill\n\nSome content.\n";
        std::fs::write(vdir.join("test.md"), content).unwrap();

        // Create installed = same as cache
        let installed_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(installed_dir.join("test.md"), content).unwrap();

        let entry = github_entry_skill("test", "skills/test.md");
        let result = pin_entry(&entry, dir.path(), false).unwrap();
        assert!(result.contains("matches upstream — nothing to pin"));
    }

    #[test]
    fn pin_captures_edits() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  owner/repo  skills/test.md\n",
        );
        write_lock(dir.path(), &make_lock_json("test", "skill"));

        // Create cache
        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("test.md"), "# Test\n\nOriginal content.\n").unwrap();

        // Create installed (modified)
        let installed_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(
            installed_dir.join("test.md"),
            "# Test\n\nModified content.\n",
        )
        .unwrap();

        let entry = github_entry_skill("test", "skills/test.md");
        let result = pin_entry(&entry, dir.path(), false).unwrap();
        assert!(result.contains("Pinned 'test'"));

        // Check patch was written
        let patch_path = dir.path().join(".skillfile/patches/skills/test.patch");
        assert!(patch_path.exists());
        let patch_text = std::fs::read_to_string(&patch_path).unwrap();
        assert!(patch_text.contains("-Original content."));
        assert!(patch_text.contains("+Modified content."));
    }

    #[test]
    fn pin_dry_run_does_not_write_patch() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  owner/repo  skills/test.md\n",
        );
        write_lock(dir.path(), &make_lock_json("test", "skill"));

        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("test.md"), "original\n").unwrap();

        let installed_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(installed_dir.join("test.md"), "modified\n").unwrap();

        let entry = github_entry_skill("test", "skills/test.md");
        let result = pin_entry(&entry, dir.path(), true).unwrap();
        assert!(result.contains("Would pin 'test'"));

        // No patch written
        let patch_path = dir.path().join(".skillfile/patches/skills/test.patch");
        assert!(!patch_path.exists());
    }

    #[test]
    fn unpin_not_pinned_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "github  skill  owner/repo  skills/test.md\n");

        // cmd_unpin uses find_entry_in, which needs the manifest parsed
        // Just check has_patch/has_dir_patch return false
        let entry = github_entry_skill("test", "skills/test.md");
        assert!(!has_patch(&entry, dir.path()));
        assert!(!has_dir_patch(&entry, dir.path()));
    }

    #[test]
    fn unpin_removes_patch() {
        let dir = tempfile::tempdir().unwrap();
        let entry = github_entry_skill("test", "skills/test.md");

        // Write a patch
        write_patch(&entry, "some patch content", dir.path()).unwrap();
        assert!(has_patch(&entry, dir.path()));

        remove_patch(&entry, dir.path()).unwrap();
        assert!(!has_patch(&entry, dir.path()));
    }

    #[test]
    fn pin_dir_entry_not_cached_errors() {
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "lang-pro".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "agents/lang-pro".into(), // dir entry
                ref_: "main".into(),
            },
        };
        write_manifest(dir.path(), "");
        let result = pin_dir_entry(&entry, dir.path(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not cached"));
    }

    #[test]
    fn cmd_pin_entry_not_found_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");
        let result = cmd_pin("nonexistent", dir.path(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("nonexistent"));
    }

    // Helper: ensure target is set up for installed_path to work
    fn _make_install_target() -> InstallTarget {
        InstallTarget {
            adapter: "claude-code".into(),
            scope: Scope::Local,
        }
    }

    // -----------------------------------------------------------------------
    // cmd_pin() output formatting
    // -----------------------------------------------------------------------

    // Exercises the "Pinned '...' — customisations saved to .skillfile/patches/"
    // branch in cmd_pin by going through a full single-file github skill scenario.
    #[test]
    fn cmd_pin_prints_pinned_message_when_edits_exist() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  owner/repo  skills/myskill.md\n",
        );
        write_lock(dir.path(), &make_lock_json("myskill", "skill"));

        // Vendor cache
        let vdir = dir.path().join(".skillfile/cache/skills/myskill");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("myskill.md"), "# MySkill\n\nOriginal.\n").unwrap();

        // Installed (modified)
        let inst_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("myskill.md"), "# MySkill\n\nModified.\n").unwrap();

        // cmd_pin itself returns Ok — we can't capture stdout in unit tests easily,
        // but we can verify it returns Ok and the patch was written (proving the
        // "Pinned" branch was reached).
        let result = cmd_pin("myskill", dir.path(), false);
        assert!(result.is_ok(), "cmd_pin must return Ok when edits exist");

        let patch_path = dir.path().join(".skillfile/patches/skills/myskill.patch");
        assert!(patch_path.exists(), "patch file must be written");
    }

    // Exercises the "Would pin '...' [dry-run]" branch.
    #[test]
    fn cmd_pin_dry_run_prints_would_pin_message() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  owner/repo  skills/myskill.md\n",
        );
        write_lock(dir.path(), &make_lock_json("myskill", "skill"));

        let vdir = dir.path().join(".skillfile/cache/skills/myskill");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("myskill.md"), "original\n").unwrap();

        let inst_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("myskill.md"), "modified\n").unwrap();

        // dry_run = true — "Would pin" branch
        let result = cmd_pin("myskill", dir.path(), true);
        assert!(result.is_ok(), "cmd_pin dry-run must return Ok");

        // No patch must have been written
        let patch_path = dir.path().join(".skillfile/patches/skills/myskill.patch");
        assert!(!patch_path.exists(), "dry-run must not write a patch file");
    }

    // Exercises the fallthrough "other status" branch ("'...' matches upstream").
    #[test]
    fn cmd_pin_prints_nothing_to_pin_when_identical() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  owner/repo  skills/myskill.md\n",
        );
        write_lock(dir.path(), &make_lock_json("myskill", "skill"));

        let content = "# MySkill\n\nSame content.\n";
        let vdir = dir.path().join(".skillfile/cache/skills/myskill");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("myskill.md"), content).unwrap();

        let inst_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("myskill.md"), content).unwrap();

        let result = cmd_pin("myskill", dir.path(), false);
        assert!(result.is_ok(), "cmd_pin must return Ok when nothing to pin");
    }

    // -----------------------------------------------------------------------
    // pin_dir_entry() — happy path
    // -----------------------------------------------------------------------

    // Skills with claude-code use Nested dir mode: installed at .claude/skills/<name>/
    // Only modified files get patches written; unmodified files do not.
    #[test]
    fn pin_dir_entry_writes_patch_only_for_modified_files() {
        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        write_manifest(
            dir.path(),
            &format!(
                "install  claude-code  local\ngithub  skill  {name}  owner/repo  skills/{name}\n"
            ),
        );
        write_lock(dir.path(), &make_lock_json(name, "skill"));

        // Vendor cache — two files
        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), "# Lang Pro\n\nOriginal.\n").unwrap();
        std::fs::write(vdir.join("extra.md"), "# Extra\n\nUnchanged.\n").unwrap();

        // Installed dir (Nested mode for skills)
        let inst_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&inst_dir).unwrap();
        // SKILL.md modified, extra.md unchanged
        std::fs::write(inst_dir.join("SKILL.md"), "# Lang Pro\n\nModified.\n").unwrap();
        std::fs::write(inst_dir.join("extra.md"), "# Extra\n\nUnchanged.\n").unwrap();

        let entry = Entry {
            entity_type: EntityType::Skill,
            name: name.to_string(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: format!("skills/{name}"),
                ref_: "main".into(),
            },
        };

        let result = pin_dir_entry(&entry, dir.path(), false).unwrap();
        assert!(
            result.contains("Pinned"),
            "status must start with 'Pinned': {result}"
        );
        assert!(result.contains(name), "status must contain entry name");
        assert!(
            result.contains("SKILL.md"),
            "status must list modified file"
        );

        // Patch written for modified file
        let skill_patch = dir
            .path()
            .join(format!(".skillfile/patches/skills/{name}/SKILL.md.patch"));
        assert!(skill_patch.exists(), "patch must be written for SKILL.md");
        let patch_text = std::fs::read_to_string(&skill_patch).unwrap();
        assert!(
            patch_text.contains("-Original."),
            "patch must remove old line"
        );
        assert!(patch_text.contains("+Modified."), "patch must add new line");

        // No patch for unmodified file
        let extra_patch = dir
            .path()
            .join(format!(".skillfile/patches/skills/{name}/extra.md.patch"));
        assert!(
            !extra_patch.exists(),
            "patch must NOT be written for unchanged file"
        );
    }

    #[test]
    fn pin_dir_entry_dry_run_writes_no_patches() {
        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        write_manifest(
            dir.path(),
            &format!(
                "install  claude-code  local\ngithub  skill  {name}  owner/repo  skills/{name}\n"
            ),
        );
        write_lock(dir.path(), &make_lock_json(name, "skill"));

        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), "# Lang Pro\n\nOriginal.\n").unwrap();

        let inst_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), "# Lang Pro\n\nModified.\n").unwrap();

        let entry = Entry {
            entity_type: EntityType::Skill,
            name: name.to_string(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: format!("skills/{name}"),
                ref_: "main".into(),
            },
        };

        let result = pin_dir_entry(&entry, dir.path(), true).unwrap();
        assert!(
            result.contains("Would pin"),
            "dry-run status must start with 'Would pin': {result}"
        );

        let patch_dir = dir.path().join(format!(".skillfile/patches/skills/{name}"));
        assert!(
            !patch_dir.exists(),
            "dry-run must not create patches directory"
        );
    }

    #[test]
    fn pin_dir_entry_all_identical_returns_nothing_to_pin() {
        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        write_manifest(
            dir.path(),
            &format!(
                "install  claude-code  local\ngithub  skill  {name}  owner/repo  skills/{name}\n"
            ),
        );
        write_lock(dir.path(), &make_lock_json(name, "skill"));

        let content = "# Identical\n\nSame content everywhere.\n";

        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), content).unwrap();

        let inst_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), content).unwrap();

        let entry = Entry {
            entity_type: EntityType::Skill,
            name: name.to_string(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: format!("skills/{name}"),
                ref_: "main".into(),
            },
        };

        let result = pin_dir_entry(&entry, dir.path(), false).unwrap();
        assert!(
            result.contains("matches upstream"),
            "must report nothing to pin when all files identical: {result}"
        );
    }

    // -----------------------------------------------------------------------
    // pin_dir_entry() — installed not found
    // -----------------------------------------------------------------------

    // Vendor cache exists but no installed files (installed dir does not exist).
    #[test]
    fn pin_dir_entry_installed_not_found_errors() {
        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        write_manifest(
            dir.path(),
            &format!(
                "install  claude-code  local\ngithub  skill  {name}  owner/repo  skills/{name}\n"
            ),
        );
        write_lock(dir.path(), &make_lock_json(name, "skill"));

        // Vendor cache exists
        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), "# Lang Pro\n").unwrap();

        // Installed dir does NOT exist → installed_dir_files returns empty map
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: name.to_string(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: format!("skills/{name}"),
                ref_: "main".into(),
            },
        };

        let result = pin_dir_entry(&entry, dir.path(), false);
        assert!(result.is_err(), "must error when installed files not found");
        assert!(
            result.unwrap_err().to_string().contains("not installed"),
            "error must say 'not installed'"
        );
    }

    // -----------------------------------------------------------------------
    // cmd_pin() with dir entry (full flow through cmd_pin → pin_dir_entry)
    // -----------------------------------------------------------------------

    #[test]
    fn cmd_pin_dir_entry_happy_path() {
        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        write_manifest(
            dir.path(),
            &format!(
                "install  claude-code  local\ngithub  skill  {name}  owner/repo  skills/{name}\n"
            ),
        );
        write_lock(dir.path(), &make_lock_json(name, "skill"));

        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), "# Lang Pro\n\nOriginal.\n").unwrap();

        let inst_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), "# Lang Pro\n\nCustomised.\n").unwrap();

        let result = cmd_pin(name, dir.path(), false);
        assert!(
            result.is_ok(),
            "cmd_pin must succeed for dir entry: {result:?}"
        );

        let skill_patch = dir
            .path()
            .join(format!(".skillfile/patches/skills/{name}/SKILL.md.patch"));
        assert!(skill_patch.exists(), "patch must be written for SKILL.md");
    }

    #[test]
    fn cmd_pin_dir_entry_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        write_manifest(
            dir.path(),
            &format!(
                "install  claude-code  local\ngithub  skill  {name}  owner/repo  skills/{name}\n"
            ),
        );
        write_lock(dir.path(), &make_lock_json(name, "skill"));

        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), "# Lang Pro\n\nOriginal.\n").unwrap();

        let inst_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), "# Lang Pro\n\nCustomised.\n").unwrap();

        let result = cmd_pin(name, dir.path(), true);
        assert!(result.is_ok(), "cmd_pin dry-run must succeed: {result:?}");

        let patch_dir = dir.path().join(format!(".skillfile/patches/skills/{name}"));
        assert!(!patch_dir.exists(), "dry-run must not write any patches");
    }

    // -----------------------------------------------------------------------
    // cmd_unpin() full flow
    // -----------------------------------------------------------------------

    // Full unpin flow: patch exists → patch is removed → upstream is restored from cache.
    #[test]
    fn cmd_unpin_full_flow_removes_patch_and_restores_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let name = "myskill";

        // Manifest with install target so install_entry can restore the file.
        write_manifest(
            dir.path(),
            &format!(
                "install  claude-code  local\ngithub  skill  {name}  owner/repo  skills/{name}.md\n"
            ),
        );
        write_lock(dir.path(), &make_lock_json(name, "skill"));

        let upstream_content = "# MySkill\n\nUpstream content.\n";
        let modified_content = "# MySkill\n\nUser-modified content.\n";

        // Vendor cache (upstream)
        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join(format!("{name}.md")), upstream_content).unwrap();

        // Installed file (user-modified)
        let inst_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join(format!("{name}.md")), modified_content).unwrap();

        // Write a patch that represents the user's edits
        let entry = github_entry_skill(name, &format!("skills/{name}.md"));
        let patch_text =
            crate::patch::generate_patch(upstream_content, modified_content, &format!("{name}.md"));
        write_patch(&entry, &patch_text, dir.path()).unwrap();
        assert!(
            has_patch(&entry, dir.path()),
            "patch must exist before unpin"
        );

        // Execute unpin
        let result = cmd_unpin(name, dir.path());
        assert!(result.is_ok(), "cmd_unpin must succeed: {result:?}");

        // Patch must be removed
        assert!(
            !has_patch(&entry, dir.path()),
            "patch must be removed after unpin"
        );

        // Installed file must be restored to upstream content
        let installed_after = std::fs::read_to_string(inst_dir.join(format!("{name}.md"))).unwrap();
        assert_eq!(
            installed_after, upstream_content,
            "installed file must match upstream after unpin"
        );
    }

    // Unpin when not pinned: no error, no change.
    #[test]
    fn cmd_unpin_not_pinned_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  owner/repo  skills/myskill.md\n",
        );

        let result = cmd_unpin("myskill", dir.path());
        assert!(result.is_ok(), "cmd_unpin of unpinned entry must return Ok");
    }

    // Full unpin flow for a dir entry: all dir patches are removed and
    // upstream files are restored from the vendor cache (Nested mode).
    #[test]
    fn cmd_unpin_dir_entry_removes_all_dir_patches_and_restores() {
        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        write_manifest(
            dir.path(),
            &format!(
                "install  claude-code  local\ngithub  skill  {name}  owner/repo  skills/{name}\n"
            ),
        );
        write_lock(dir.path(), &make_lock_json(name, "skill"));

        let upstream_skill = "# Lang Pro\n\nUpstream SKILL.\n";
        let modified_skill = "# Lang Pro\n\nUser modified SKILL.\n";
        let upstream_extra = "# Extra\n\nUpstream.\n";

        // Vendor cache
        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("SKILL.md"), upstream_skill).unwrap();
        std::fs::write(vdir.join("extra.md"), upstream_extra).unwrap();

        // Installed dir (modified)
        let inst_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), modified_skill).unwrap();
        std::fs::write(inst_dir.join("extra.md"), upstream_extra).unwrap();

        // Write dir patches
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: name.to_string(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: format!("skills/{name}"),
                ref_: "main".into(),
            },
        };
        let patch_text = crate::patch::generate_patch(upstream_skill, modified_skill, "SKILL.md");
        write_dir_patch(&dir_patch_path(&entry, "SKILL.md", dir.path()), &patch_text).unwrap();
        assert!(
            has_dir_patch(&entry, dir.path()),
            "dir patch must exist before unpin"
        );

        let result = cmd_unpin(name, dir.path());
        assert!(result.is_ok(), "cmd_unpin must succeed: {result:?}");

        // Dir patches must be removed
        assert!(
            !has_dir_patch(&entry, dir.path()),
            "dir patches must be removed after unpin"
        );

        // Installed SKILL.md must be restored to upstream
        let skill_after = std::fs::read_to_string(inst_dir.join("SKILL.md")).unwrap();
        assert_eq!(
            skill_after, upstream_skill,
            "SKILL.md must be restored to upstream after unpin"
        );
    }

    // cmd_unpin errors when name not found in manifest.
    #[test]
    fn cmd_unpin_entry_not_found_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");
        let result = cmd_unpin("nonexistent", dir.path());
        assert!(result.is_err(), "cmd_unpin must error for unknown entry");
        assert!(
            result.unwrap_err().to_string().contains("nonexistent"),
            "error must mention the entry name"
        );
    }

    // -----------------------------------------------------------------------
    // pin_entry() — dir entry routing
    // -----------------------------------------------------------------------

    // pin_entry routes to pin_dir_entry when is_dir_entry returns true.
    // A github entry with path_in_repo lacking ".md" is a dir entry.
    #[test]
    fn pin_entry_routes_to_pin_dir_entry_for_dir_entries() {
        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        write_manifest(
            dir.path(),
            &format!(
                "install  claude-code  local\ngithub  skill  {name}  owner/repo  skills/{name}\n"
            ),
        );
        write_lock(dir.path(), &make_lock_json(name, "skill"));

        // No vendor cache → pin_dir_entry will error with "not cached"
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: name.to_string(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: format!("skills/{name}"),
                ref_: "main".into(),
            },
        };

        let result = pin_entry(&entry, dir.path(), false);
        // Must error with "not cached" (from pin_dir_entry), confirming routing
        assert!(
            result.is_err(),
            "must error when cache missing for dir entry"
        );
        assert!(
            result.unwrap_err().to_string().contains("not cached"),
            "error must say 'not cached'"
        );
    }

    // -----------------------------------------------------------------------
    // pin_dir_entry() — .meta file in cache is skipped
    // -----------------------------------------------------------------------

    // Verify that the .meta file in the vendor cache dir is not treated as a
    // content file and does not generate a spurious patch.
    #[test]
    fn pin_dir_entry_skips_meta_file() {
        let dir = tempfile::tempdir().unwrap();
        let name = "lang-pro";

        write_manifest(
            dir.path(),
            &format!(
                "install  claude-code  local\ngithub  skill  {name}  owner/repo  skills/{name}\n"
            ),
        );
        write_lock(dir.path(), &make_lock_json(name, "skill"));

        let vdir = dir.path().join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        // .meta present alongside content file
        std::fs::write(vdir.join(".meta"), r#"{"sha":"abc123"}"#).unwrap();
        let content = "# Lang Pro\n\nSame.\n";
        std::fs::write(vdir.join("SKILL.md"), content).unwrap();

        let inst_dir = dir.path().join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), content).unwrap();

        let entry = Entry {
            entity_type: EntityType::Skill,
            name: name.to_string(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: format!("skills/{name}"),
                ref_: "main".into(),
            },
        };

        let result = pin_dir_entry(&entry, dir.path(), false).unwrap();
        assert!(
            result.contains("matches upstream"),
            "must report nothing to pin when only .meta differs: {result}"
        );

        // No patch for .meta
        let meta_patch = dir
            .path()
            .join(format!(".skillfile/patches/skills/{name}/.meta.patch"));
        assert!(!meta_patch.exists(), ".meta must never produce a patch");
    }
}
