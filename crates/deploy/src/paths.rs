use std::collections::HashMap;
use std::path::{Path, PathBuf};

use skillfile_core::error::SkillfileError;
use skillfile_core::models::{EntityType, Entry, Manifest, SourceFields};
use skillfile_core::patch::walkdir;
use skillfile_sources::strategy::{content_file, is_cached_dir_entry, meta_sha};
use skillfile_sources::sync::vendor_dir_for;

use crate::adapter::{adapters, ensure_no_symlink_components, AdapterScope};
use crate::target::ResolvedInstallTarget;

pub fn resolve_target_dir(
    adapter_name: &str,
    entity_type: EntityType,
    ctx: &AdapterScope<'_>,
) -> Result<PathBuf, SkillfileError> {
    let a = adapters()
        .get(adapter_name)
        .ok_or_else(|| SkillfileError::Manifest(format!("unknown adapter '{adapter_name}'")))?;
    Ok(a.target_dir(entity_type, ctx))
}

/// Installed path for a single-file entry (first install target).
pub fn installed_path(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<PathBuf, SkillfileError> {
    let target = first_supporting_target(entry, manifest)?;
    let path = target.installed_path(entry, repo_root);
    ensure_no_symlink_components(&path)
        .map_err(|error| SkillfileError::Install(error.to_string()))?;
    Ok(path)
}

#[must_use]
pub fn is_safe_installed_path(path: &Path) -> bool {
    ensure_no_symlink_components(path).is_ok()
}

/// Installed paths for a single-file entry across all install targets.
pub fn installed_paths(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<Vec<PathBuf>, SkillfileError> {
    let mut paths = Vec::new();
    for target in &manifest.install_targets {
        let target = resolved_target(target)?;
        if !target.supports(entry.entity_type) {
            continue;
        }
        let path = target.installed_path(entry, repo_root);
        if is_safe_installed_path(&path) {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// Installed files for a directory entry (first install target).
pub fn installed_dir_files(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<HashMap<String, PathBuf>, SkillfileError> {
    let target = first_supporting_target(entry, manifest)?;
    Ok(target.installed_dir_files(entry, repo_root))
}

/// Installed file maps for a directory entry across all install targets.
pub fn installed_dir_file_sets(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<Vec<HashMap<String, PathBuf>>, SkillfileError> {
    let mut file_sets = Vec::new();
    for target in &manifest.install_targets {
        let target = resolved_target(target)?;
        if !target.supports(entry.entity_type) {
            continue;
        }
        file_sets.push(target.installed_dir_files(entry, repo_root));
    }
    Ok(file_sets)
}

#[must_use]
pub fn source_path(entry: &Entry, repo_root: &Path) -> Option<PathBuf> {
    match &entry.source {
        SourceFields::Local { path } => Some(repo_root.join(path)),
        SourceFields::Github { .. } | SourceFields::Gitlab { .. } | SourceFields::Url { .. } => {
            source_path_remote(entry, repo_root)
        }
    }
}

fn source_path_remote(entry: &Entry, repo_root: &Path) -> Option<PathBuf> {
    let vdir = vendor_dir_for(entry, repo_root);
    if is_cached_dir_entry(entry, &vdir) {
        remote_dir_cache_is_complete(entry, &vdir).then_some(vdir)
    } else {
        let filename = content_file(entry);
        (!filename.is_empty()).then(|| vdir.join(filename))
    }
}

fn remote_dir_cache_is_complete(entry: &Entry, vdir: &Path) -> bool {
    if meta_sha(vdir).is_none() {
        return false;
    }
    match entry.entity_type {
        EntityType::Skill => std::fs::read_dir(vdir).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_file())
                    && entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"))
            })
        }),
        EntityType::Agent => walkdir(vdir)
            .into_iter()
            .any(|path| path.extension().is_some_and(|extension| extension == "md")),
    }
}

fn first_supporting_target<'a>(
    entry: &Entry,
    manifest: &'a Manifest,
) -> Result<ResolvedInstallTarget<'a>, SkillfileError> {
    if manifest.install_targets.is_empty() {
        return Err(SkillfileError::Manifest(
            "no install targets configured — run `skillfile install` first".into(),
        ));
    }
    for target in &manifest.install_targets {
        let resolved = resolved_target(target)?;
        if resolved.supports(entry.entity_type) {
            return Ok(resolved);
        }
    }
    Err(SkillfileError::Manifest(format!(
        "no install target supports {} '{}'",
        entry.entity_type, entry.name
    )))
}

fn resolved_target(
    target: &skillfile_core::models::InstallTarget,
) -> Result<ResolvedInstallTarget<'_>, SkillfileError> {
    ResolvedInstallTarget::from_target(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::AdapterScope;
    use skillfile_core::models::{EntityType, InstallTarget, Scope};

    #[test]
    fn resolve_target_dir_global() {
        let ctx = AdapterScope {
            scope: Scope::Global,
            repo_root: Path::new("/tmp"),
        };
        let result = resolve_target_dir("claude-code", EntityType::Agent, &ctx).unwrap();
        assert!(result.to_string_lossy().ends_with(".claude/agents"));
    }

    #[test]
    fn resolve_target_dir_local() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = AdapterScope {
            scope: Scope::Local,
            repo_root: tmp.path(),
        };
        let result = resolve_target_dir("claude-code", EntityType::Agent, &ctx).unwrap();
        assert_eq!(result, tmp.path().join(".claude/agents"));
    }

    #[test]
    fn installed_path_no_targets() {
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "a.md".into(),
                ref_: "main".into(),
            },
        };
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![],
        };
        let result = installed_path(&entry, &manifest, Path::new("/tmp"));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no install targets"));
    }

    #[test]
    fn installed_path_unknown_adapter() {
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "a.md".into(),
                ref_: "main".into(),
            },
        };
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![InstallTarget::platform("unknown", Scope::Global)],
        };
        let result = installed_path(&entry, &manifest, Path::new("/tmp"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown adapter"));
    }

    #[test]
    fn installed_path_returns_correct_path() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "a.md".into(),
                ref_: "main".into(),
            },
        };
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![InstallTarget::platform("claude-code", Scope::Local)],
        };
        let result = installed_path(&entry, &manifest, tmp.path()).unwrap();
        assert_eq!(result, tmp.path().join(".claude/agents/test.md"));
    }

    #[test]
    fn installed_paths_returns_all_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "skills/test.md".into(),
                ref_: "main".into(),
            },
        };
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![
                InstallTarget::platform("claude-code", Scope::Local),
                InstallTarget::platform("copilot", Scope::Local),
            ],
        };

        let result = installed_paths(&entry, &manifest, tmp.path()).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.contains(&tmp.path().join(".claude/skills/test/SKILL.md")));
        assert!(result.contains(&tmp.path().join(".github/skills/test/SKILL.md")));
    }

    #[test]
    fn installed_dir_files_no_targets() {
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "agents".into(),
                ref_: "main".into(),
            },
        };
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![],
        };
        let result = installed_dir_files(&entry, &manifest, Path::new("/tmp"));
        assert!(result.is_err());
    }

    #[test]
    fn installed_dir_files_skill_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "my-skill".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "skills".into(),
                ref_: "main".into(),
            },
        };
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![InstallTarget::platform("claude-code", Scope::Local)],
        };
        let skill_dir = tmp.path().join(".claude/skills/my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Skill\n").unwrap();

        let result = installed_dir_files(&entry, &manifest, tmp.path()).unwrap();
        assert!(result.contains_key("SKILL.md"));
    }

    #[test]
    fn installed_dir_file_sets_returns_all_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "my-skill".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "skills".into(),
                ref_: "main".into(),
            },
        };
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![
                InstallTarget::platform("claude-code", Scope::Local),
                InstallTarget::platform("copilot", Scope::Local),
            ],
        };
        let claude_dir = tmp.path().join(".claude/skills/my-skill");
        let copilot_dir = tmp.path().join(".github/skills/my-skill");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::create_dir_all(&copilot_dir).unwrap();
        std::fs::write(claude_dir.join("SKILL.md"), "# Skill\n").unwrap();
        std::fs::write(copilot_dir.join("SKILL.md"), "# Skill\n").unwrap();

        let result = installed_dir_file_sets(&entry, &manifest, tmp.path()).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|files| files.contains_key("SKILL.md")));
    }

    #[test]
    fn installed_dir_files_agent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "my-agents".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "agents".into(),
                ref_: "main".into(),
            },
        };
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![InstallTarget::platform("claude-code", Scope::Local)],
        };
        // Create vendor cache
        let vdir = tmp.path().join(".skillfile/cache/agents/my-agents");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("a.md"), "# A\n").unwrap();
        std::fs::write(vdir.join("b.md"), "# B\n").unwrap();
        // Create installed copies
        let agents_dir = tmp.path().join(".claude/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("a.md"), "# A\n").unwrap();
        std::fs::write(agents_dir.join("b.md"), "# B\n").unwrap();

        let result = installed_dir_files(&entry, &manifest, tmp.path()).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn source_path_local() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "test".into(),
            source: SourceFields::Local {
                path: "skills/test.md".into(),
            },
        };
        let result = source_path(&entry, tmp.path());
        assert_eq!(result, Some(tmp.path().join("skills/test.md")));
    }

    #[test]
    fn source_path_github_single() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "agents/test.md".into(),
                ref_: "main".into(),
            },
        };
        let vdir = tmp.path().join(".skillfile/cache/agents/test");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("test.md"), "# Test\n").unwrap();

        let result = source_path(&entry, tmp.path());
        assert_eq!(result, Some(vdir.join("test.md")));
    }

    #[test]
    fn source_path_remote_dir_requires_cached_content() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "skills/test".into(),
                ref_: "main".into(),
            },
        };
        let vdir = tmp.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::create_dir_all(vdir.join("scripts")).unwrap();
        std::fs::write(vdir.join("scripts/helper.py"), "pass\n").unwrap();

        assert_eq!(source_path(&entry, tmp.path()), None);

        std::fs::write(vdir.join(".meta"), r#"{"sha":"abc123"}"#).unwrap();
        assert_eq!(source_path(&entry, tmp.path()), None);

        std::fs::write(vdir.join("skill.md"), "# Test\n").unwrap();
        assert_eq!(source_path(&entry, tmp.path()), Some(vdir));
    }

    #[test]
    fn source_path_remote_agent_dir_requires_metadata_and_markdown() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "agents".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "agents".into(),
                ref_: "main".into(),
            },
        };
        let vdir = tmp.path().join(".skillfile/cache/agents/agents");
        std::fs::create_dir_all(vdir.join("nested")).unwrap();
        std::fs::write(vdir.join("nested/helper.py"), "pass\n").unwrap();

        assert_eq!(source_path(&entry, tmp.path()), None);

        std::fs::write(vdir.join(".meta"), r#"{"sha":"abc123"}"#).unwrap();
        assert_eq!(source_path(&entry, tmp.path()), None);

        std::fs::write(vdir.join("nested/agent.md"), "# Agent\n").unwrap();

        assert_eq!(source_path(&entry, tmp.path()), Some(vdir));
    }

    #[test]
    fn known_adapters_includes_claude_code() {
        // resolve_target_dir only succeeds for known adapters; a successful
        // call is sufficient proof that "claude-code" is registered.
        let ctx = AdapterScope {
            scope: Scope::Global,
            repo_root: Path::new("/tmp"),
        };
        assert!(resolve_target_dir("claude-code", EntityType::Skill, &ctx).is_ok());
    }

    #[test]
    fn known_adapters_includes_junie() {
        // resolve_target_dir only succeeds for known adapters; a successful
        // call is sufficient proof that "junie" is registered.
        let ctx = AdapterScope {
            scope: Scope::Global,
            repo_root: Path::new("/tmp"),
        };
        assert!(resolve_target_dir("junie", EntityType::Skill, &ctx).is_ok());
    }
}
