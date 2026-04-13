use std::collections::BTreeMap;
use std::path::Path;

use skillfile_core::error::SkillfileError;
use skillfile_core::lock::{lock_key, read_lock};
use skillfile_core::models::Entry;
use skillfile_core::parser::{find_entry_in, parse_manifest, MANIFEST_NAME};
use skillfile_deploy::install::install_entry;
use skillfile_sources::strategy::{content_file, is_dir_entry};
use skillfile_sources::sync::vendor_dir_for;

use crate::commands::installed_variants::{installed_dir_variants, installed_single_file_variants};
use crate::commands::multi_target::{
    divergent_targets_message, modified_dir_variants, modified_single_file_variants,
};
use crate::patch::{
    dir_patch_path, generate_patch, has_dir_patch, has_patch, remove_all_dir_patches,
    remove_dir_patch, remove_patch, walkdir, write_dir_patch, write_patch,
};

struct PinCtx<'a> {
    entry: &'a Entry,
    repo_root: &'a Path,
    dry_run: bool,
}

struct DirPinPlan<'a> {
    cache_files: BTreeMap<String, std::path::PathBuf>,
    representative: &'a crate::commands::multi_target::DirContentMap,
}

struct SinglePinPlan<'a> {
    cache_text: &'a str,
    installed: &'a [crate::commands::installed_variants::SingleFileVariant],
}

#[derive(Clone, Copy)]
struct DirPatchInput<'a> {
    filename: &'a str,
    original_text: &'a str,
    installed_text: &'a str,
}

fn process_dir_file_text(
    ctx: &PinCtx<'_>,
    input: &DirPatchInput<'_>,
) -> Result<Option<String>, SkillfileError> {
    let patch_text = generate_patch(input.original_text, input.installed_text, input.filename);

    if patch_text.is_empty() {
        if !ctx.dry_run {
            remove_dir_patch(ctx.entry, input.filename, ctx.repo_root)?;
        }
        return Ok(None);
    }
    if !ctx.dry_run {
        write_dir_patch(
            &dir_patch_path(ctx.entry, input.filename, ctx.repo_root),
            &patch_text,
        )?;
    }
    Ok(Some(input.filename.to_string()))
}

fn load_cache_files(vdir: &Path) -> BTreeMap<String, std::path::PathBuf> {
    walkdir(vdir)
        .into_iter()
        .filter(|cache_file| cache_file.file_name().is_some_and(|name| name != ".meta"))
        .filter_map(|cache_file| {
            let filename = cache_file
                .strip_prefix(vdir)
                .ok()
                .and_then(|path| path.to_str())
                .map(str::to_string)?;
            Some((filename, cache_file))
        })
        .collect()
}

fn representative_dir_changes<'a>(
    entry_name: &str,
    modified: &'a [(String, crate::commands::multi_target::DirContentMap)],
) -> Result<&'a crate::commands::multi_target::DirContentMap, SkillfileError> {
    let labels: Vec<String> = modified.iter().map(|(label, _)| label.clone()).collect();
    let representative = &modified[0].1;
    if modified
        .iter()
        .any(|(_, changed)| changed != representative)
    {
        return Err(divergent_targets_message(entry_name, &labels));
    }
    Ok(representative)
}

fn apply_dir_pin_changes(
    ctx: &PinCtx<'_>,
    plan: DirPinPlan<'_>,
) -> Result<Vec<String>, SkillfileError> {
    let mut pinned = Vec::new();

    for (filename, cache_file) in plan.cache_files {
        if let Some(installed_text) = plan.representative.get(&filename) {
            let original_text = std::fs::read_to_string(&cache_file)?;
            let input = DirPatchInput {
                filename: &filename,
                original_text: &original_text,
                installed_text,
            };
            let pinned_file = process_dir_file_text(ctx, &input)?;
            pinned.extend(pinned_file);
            continue;
        }
        if !ctx.dry_run {
            remove_dir_patch(ctx.entry, &filename, ctx.repo_root)?;
        }
    }

    Ok(pinned)
}

fn pin_single_file_content(
    ctx: &PinCtx<'_>,
    plan: &SinglePinPlan<'_>,
) -> Result<String, SkillfileError> {
    let modified = modified_single_file_variants(plan.cache_text, plan.installed);
    if modified.is_empty() {
        return Ok(format!(
            "'{}' matches upstream — nothing to pin",
            ctx.entry.name
        ));
    }

    let representative = &modified[0].content;
    if modified
        .iter()
        .any(|variant| variant.content != *representative)
    {
        let labels: Vec<String> = modified
            .iter()
            .map(|variant| variant.label.clone())
            .collect();
        return Err(divergent_targets_message(&ctx.entry.name, &labels));
    }

    let patch_text = generate_patch(
        plan.cache_text,
        representative,
        &format!("{}.md", ctx.entry.name),
    );
    if !ctx.dry_run {
        write_patch(ctx.entry, &patch_text, ctx.repo_root)?;
    }

    let prefix = if ctx.dry_run { "Would pin" } else { "Pinned" };
    Ok(format!("{prefix} '{}'", ctx.entry.name))
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
    let installed = installed_dir_variants(entry, &manifest, repo_root);
    if installed.is_empty() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not installed — run `skillfile install` first",
            entry.name
        )));
    }

    let cache_files = load_cache_files(&vdir);
    let modified = modified_dir_variants(&cache_files, &installed)?;
    if modified.is_empty() {
        return Ok(format!(
            "'{}' matches upstream — nothing to pin",
            entry.name
        ));
    }
    let representative = representative_dir_changes(&entry.name, &modified)?;
    let pin_ctx = PinCtx {
        entry,
        repo_root,
        dry_run,
    };
    let pinned = apply_dir_pin_changes(
        &pin_ctx,
        DirPinPlan {
            cache_files,
            representative,
        },
    )?;

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
    let installed = installed_single_file_variants(entry, &manifest, repo_root)?;
    if installed.is_empty() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not installed — run `skillfile install` first",
            entry.name
        )));
    }

    let cache_text = std::fs::read_to_string(&cache_file)?;
    let pin_ctx = PinCtx {
        entry,
        repo_root,
        dry_run,
    };
    pin_single_file_content(
        &pin_ctx,
        &SinglePinPlan {
            cache_text: &cache_text,
            installed: &installed,
        },
    )
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
    use skillfile_core::models::{EntityType, SourceFields};

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
        let installed_dir = dir.path().join(".claude/skills/test");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(installed_dir.join("SKILL.md"), content).unwrap();

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
        let installed_dir = dir.path().join(".claude/skills/test");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(
            installed_dir.join("SKILL.md"),
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

        let installed_dir = dir.path().join(".claude/skills/test");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(installed_dir.join("SKILL.md"), "modified\n").unwrap();

        let entry = github_entry_skill("test", "skills/test.md");
        let result = pin_entry(&entry, dir.path(), true).unwrap();
        assert!(result.contains("Would pin 'test'"));

        // No patch written
        let patch_path = dir.path().join(".skillfile/patches/skills/test.patch");
        assert!(!patch_path.exists());
    }

    #[test]
    fn pin_entry_uses_second_target_when_first_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\n\
             install  copilot  local\n\
             github  skill  owner/repo  skills/test.md\n",
        );
        write_lock(dir.path(), &make_lock_json("test", "skill"));

        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("test.md"), "original\n").unwrap();

        let first_target = dir.path().join(".claude/skills/test");
        std::fs::create_dir_all(&first_target).unwrap();
        std::fs::write(first_target.join("SKILL.md"), "original\n").unwrap();

        let second_target = dir.path().join(".github/skills/test");
        std::fs::create_dir_all(&second_target).unwrap();
        std::fs::write(second_target.join("SKILL.md"), "modified\n").unwrap();

        let entry = github_entry_skill("test", "skills/test.md");
        let result = pin_entry(&entry, dir.path(), false).unwrap();
        assert!(result.contains("Pinned 'test'"));

        let patch_path = dir.path().join(".skillfile/patches/skills/test.patch");
        let patch_text = std::fs::read_to_string(&patch_path).unwrap();
        assert!(patch_text.contains("+modified"));
    }

    #[test]
    fn pin_entry_errors_on_divergent_multi_target_edits() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\n\
             install  copilot  local\n\
             github  skill  owner/repo  skills/test.md\n",
        );
        write_lock(dir.path(), &make_lock_json("test", "skill"));

        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("test.md"), "original\n").unwrap();

        let first_target = dir.path().join(".claude/skills/test");
        std::fs::create_dir_all(&first_target).unwrap();
        std::fs::write(first_target.join("SKILL.md"), "modified one\n").unwrap();

        let second_target = dir.path().join(".github/skills/test");
        std::fs::create_dir_all(&second_target).unwrap();
        std::fs::write(second_target.join("SKILL.md"), "modified two\n").unwrap();

        let entry = github_entry_skill("test", "skills/test.md");
        let result = pin_entry(&entry, dir.path(), false);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("divergent edits across install targets"));
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
        let inst_dir = dir.path().join(".claude/skills/myskill");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), "# MySkill\n\nModified.\n").unwrap();

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

        let inst_dir = dir.path().join(".claude/skills/myskill");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), "modified\n").unwrap();

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

        let inst_dir = dir.path().join(".claude/skills/myskill");
        std::fs::create_dir_all(&inst_dir).unwrap();
        std::fs::write(inst_dir.join("SKILL.md"), content).unwrap();

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
