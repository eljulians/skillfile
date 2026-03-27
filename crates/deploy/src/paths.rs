use std::collections::HashMap;
use std::path::{Path, PathBuf};

use skillfile_core::error::SkillfileError;
use skillfile_core::models::{EntityType, Entry, Manifest, SourceFields};
use skillfile_sources::strategy::{content_file, is_dir_entry};
use skillfile_sources::sync::vendor_dir_for;

use crate::adapter::{adapters, AdapterScope, PlatformAdapter};

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
    let adapter = first_target(manifest)?;
    let ctx = AdapterScope {
        scope: manifest.install_targets[0].scope,
        repo_root,
    };
    Ok(adapter.installed_path(entry, &ctx))
}

/// Installed files for a directory entry (first install target).
pub fn installed_dir_files(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<HashMap<String, PathBuf>, SkillfileError> {
    let adapter = first_target(manifest)?;
    let ctx = AdapterScope {
        scope: manifest.install_targets[0].scope,
        repo_root,
    };
    Ok(adapter.installed_dir_files(entry, &ctx))
}

#[must_use]
pub fn source_path(entry: &Entry, repo_root: &Path) -> Option<PathBuf> {
    match &entry.source {
        SourceFields::Local { path } => Some(repo_root.join(path)),
        SourceFields::Github { .. } | SourceFields::Url { .. } => {
            source_path_remote(entry, repo_root)
        }
    }
}

fn source_path_remote(entry: &Entry, repo_root: &Path) -> Option<PathBuf> {
    let vdir = vendor_dir_for(entry, repo_root);
    if is_dir_entry(entry) {
        vdir.exists().then_some(vdir)
    } else {
        let filename = content_file(entry);
        (!filename.is_empty()).then(|| vdir.join(filename))
    }
}

fn first_target(manifest: &Manifest) -> Result<&'static dyn PlatformAdapter, SkillfileError> {
    if manifest.install_targets.is_empty() {
        return Err(SkillfileError::Manifest(
            "no install targets configured — run `skillfile install` first".into(),
        ));
    }
    let t = &manifest.install_targets[0];
    adapters()
        .get(&t.adapter)
        .ok_or_else(|| SkillfileError::Manifest(format!("unknown adapter '{}'", t.adapter)))
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
            install_targets: vec![InstallTarget {
                adapter: "unknown".into(),
                scope: Scope::Global,
            }],
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
            install_targets: vec![InstallTarget {
                adapter: "claude-code".into(),
                scope: Scope::Local,
            }],
        };
        let result = installed_path(&entry, &manifest, tmp.path()).unwrap();
        assert_eq!(result, tmp.path().join(".claude/agents/test.md"));
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
            install_targets: vec![InstallTarget {
                adapter: "claude-code".into(),
                scope: Scope::Local,
            }],
        };
        let skill_dir = tmp.path().join(".claude/skills/my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Skill\n").unwrap();

        let result = installed_dir_files(&entry, &manifest, tmp.path()).unwrap();
        assert!(result.contains_key("SKILL.md"));
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
            install_targets: vec![InstallTarget {
                adapter: "claude-code".into(),
                scope: Scope::Local,
            }],
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
    fn known_adapters_includes_claude_code() {
        // resolve_target_dir only succeeds for known adapters; a successful
        // call is sufficient proof that "claude-code" is registered.
        let ctx = AdapterScope {
            scope: Scope::Global,
            repo_root: Path::new("/tmp"),
        };
        assert!(resolve_target_dir("claude-code", EntityType::Skill, &ctx).is_ok());
    }
}
