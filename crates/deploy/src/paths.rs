use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use skillfile_core::error::SkillfileError;
use skillfile_core::models::{EntityType, Entry, Manifest, Scope, SourceFields};
use skillfile_sources::strategy::{content_file, is_dir_entry};
use skillfile_sources::sync::vendor_dir_for;

use crate::adapter::{adapters, AdapterScope, DirInstallMode, PlatformAdapter};

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

// ---------------------------------------------------------------------------
// Untracked file detection
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UntrackedKind {
    File,
    Directory,
}

impl UntrackedKind {
    #[must_use]
    pub fn suffix(self) -> &'static str {
        match self {
            Self::File => "",
            Self::Directory => "/",
        }
    }
}

pub struct UntrackedFile {
    pub entity_type: EntityType,
    pub path: PathBuf,
    pub kind: UntrackedKind,
}

fn covered_paths(manifest: &Manifest, repo_root: &Path) -> HashSet<PathBuf> {
    let mut covered = HashSet::new();
    for_each_local_adapter(manifest, repo_root, |adapter, ctx| {
        for entry in manifest
            .entries
            .iter()
            .filter(|e| adapter.supports(e.entity_type))
        {
            covered.extend(entry_covered_paths(adapter, ctx, entry));
        }
    });
    covered
}

fn entry_covered_paths(
    adapter: &dyn PlatformAdapter,
    ctx: &AdapterScope<'_>,
    entry: &Entry,
) -> Vec<PathBuf> {
    let installed = if is_dir_entry(entry) {
        adapter.target_dir(entry.entity_type, ctx).join(&entry.name)
    } else {
        adapter.installed_path(entry, ctx)
    };
    let mut paths = vec![installed];
    if let SourceFields::Local { path } = &entry.source {
        paths.push(ctx.repo_root.join(path));
    }
    paths
}

pub fn find_untracked(
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<Vec<UntrackedFile>, SkillfileError> {
    let covered = covered_paths(manifest, repo_root);
    let mut scanner = DirScanner::new(&covered, repo_root);
    for_each_local_adapter(manifest, repo_root, |adapter, ctx| {
        scanner.scan_adapter(adapter, ctx);
    });
    Ok(scanner.into_sorted())
}

fn for_each_local_adapter(
    manifest: &Manifest,
    repo_root: &Path,
    mut f: impl FnMut(&dyn PlatformAdapter, &AdapterScope<'_>),
) {
    let registry = adapters();
    for target in manifest
        .install_targets
        .iter()
        .filter(|t| t.scope == Scope::Local)
    {
        let Some(adapter) = registry.get(&target.adapter) else {
            continue;
        };
        let ctx = AdapterScope {
            scope: target.scope,
            repo_root,
        };
        f(adapter, &ctx);
    }
}

#[derive(Clone, Copy)]
struct ScanSpec {
    entity_type: EntityType,
    mode: DirInstallMode,
}

struct DirScanner<'a> {
    covered: &'a HashSet<PathBuf>,
    repo_root: &'a Path,
    seen: HashSet<PathBuf>,
    items: Vec<UntrackedFile>,
}

impl<'a> DirScanner<'a> {
    fn new(covered: &'a HashSet<PathBuf>, repo_root: &'a Path) -> Self {
        Self {
            covered,
            repo_root,
            seen: HashSet::new(),
            items: Vec::new(),
        }
    }

    fn scan_adapter(&mut self, adapter: &dyn PlatformAdapter, ctx: &AdapterScope<'_>) {
        let specs = EntityType::ALL
            .iter()
            .filter(|&&et| adapter.supports(et))
            .filter_map(|&et| {
                let dir = adapter.target_dir(et, ctx);
                let mode = adapter.dir_mode(et).unwrap_or(DirInstallMode::Nested);
                dir.is_dir().then_some((
                    dir,
                    ScanSpec {
                        entity_type: et,
                        mode,
                    },
                ))
            });
        for (dir, spec) in specs {
            scan_target_dir(&dir, spec, self);
        }
    }

    fn into_sorted(mut self) -> Vec<UntrackedFile> {
        self.items.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        self.items
    }
}

fn scan_target_dir(target_dir: &Path, spec: ScanSpec, scanner: &mut DirScanner<'_>) {
    let Ok(entries) = std::fs::read_dir(target_dir) else {
        return;
    };
    for dir_entry in entries.flatten() {
        let is_dir = dir_entry.file_type().is_ok_and(|ft| ft.is_dir());
        let path = dir_entry.path();
        let is_nested_dir = is_dir && spec.mode == DirInstallMode::Nested;
        if !is_nested_dir && !is_md_file(&path) {
            continue;
        }
        if scanner.covered.contains(&path) || !scanner.seen.insert(path.clone()) {
            continue;
        }
        let kind = if is_nested_dir {
            UntrackedKind::Directory
        } else {
            UntrackedKind::File
        };
        scanner.items.push(UntrackedFile {
            entity_type: spec.entity_type,
            path: relative_to(scanner.repo_root, &path),
            kind,
        });
    }
}

fn is_md_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

fn relative_to(base: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(base)
        .map_or_else(|_| path.to_path_buf(), Path::to_path_buf)
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

    // -- covered_paths / find_untracked tests --------------------------------

    fn local_target() -> InstallTarget {
        InstallTarget {
            adapter: "claude-code".into(),
            scope: Scope::Local,
        }
    }

    fn global_target() -> InstallTarget {
        InstallTarget {
            adapter: "claude-code".into(),
            scope: Scope::Global,
        }
    }

    fn github_skill(name: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: name.into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: format!("skills/{name}.md"),
                ref_: "main".into(),
            },
        }
    }

    fn github_skill_dir(name: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: name.into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: format!("skills/{name}"),
                ref_: "main".into(),
            },
        }
    }

    fn github_agent(name: &str) -> Entry {
        Entry {
            entity_type: EntityType::Agent,
            name: name.into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: format!("agents/{name}.md"),
                ref_: "main".into(),
            },
        }
    }

    fn local_skill(name: &str, path: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: name.into(),
            source: SourceFields::Local { path: path.into() },
        }
    }

    #[test]
    fn covered_single_file_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![github_skill("browser")],
            install_targets: vec![local_target()],
        };
        let paths = covered_paths(&manifest, tmp.path());
        assert!(paths.contains(&tmp.path().join(".claude/skills/browser.md")));
    }

    #[test]
    fn covered_dir_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![github_skill_dir("python-pro")],
            install_targets: vec![local_target()],
        };
        let paths = covered_paths(&manifest, tmp.path());
        assert!(paths.contains(&tmp.path().join(".claude/skills/python-pro")));
    }

    #[test]
    fn covered_local_entry_includes_source_path() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![local_skill("docker", ".claude/skills/docker")],
            install_targets: vec![local_target()],
        };
        let paths = covered_paths(&manifest, tmp.path());
        assert!(paths.contains(&tmp.path().join(".claude/skills/docker")));
    }

    #[test]
    fn covered_multiple_local_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![github_skill("browser")],
            install_targets: vec![
                local_target(),
                InstallTarget {
                    adapter: "cursor".into(),
                    scope: Scope::Local,
                },
            ],
        };
        let paths = covered_paths(&manifest, tmp.path());
        assert!(paths.contains(&tmp.path().join(".claude/skills/browser.md")));
        assert!(paths.contains(&tmp.path().join(".cursor/skills/browser.md")));
    }

    #[test]
    fn covered_skips_unsupported_entity() {
        let tmp = tempfile::tempdir().unwrap();
        // codex doesn't support agents
        let manifest = Manifest {
            entries: vec![github_agent("helper")],
            install_targets: vec![InstallTarget {
                adapter: "codex".into(),
                scope: Scope::Local,
            }],
        };
        let paths = covered_paths(&manifest, tmp.path());
        assert!(paths.is_empty());
    }

    #[test]
    fn covered_skips_global_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![github_skill("browser")],
            install_targets: vec![global_target()],
        };
        let paths = covered_paths(&manifest, tmp.path());
        assert!(paths.is_empty());
    }

    #[test]
    fn untracked_empty_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![],
            install_targets: vec![local_target()],
        };
        let result = find_untracked(&manifest, tmp.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn untracked_all_tracked() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![github_agent("helper")],
            install_targets: vec![local_target()],
        };
        // Create the installed file that IS tracked
        let agents = tmp.path().join(".claude/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("helper.md"), "# Helper").unwrap();

        let result = find_untracked(&manifest, tmp.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn untracked_agent_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![github_agent("helper")],
            install_targets: vec![local_target()],
        };
        let agents = tmp.path().join(".claude/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("helper.md"), "# tracked").unwrap();
        std::fs::write(agents.join("rogue.md"), "# rogue").unwrap();

        let result = find_untracked(&manifest, tmp.path()).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].path.ends_with("rogue.md"));
        assert_eq!(result[0].entity_type, EntityType::Agent);
        assert_eq!(result[0].kind, UntrackedKind::File);
    }

    #[test]
    fn untracked_skill_dir_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![github_skill_dir("python-pro")],
            install_targets: vec![local_target()],
        };
        let skills = tmp.path().join(".claude/skills");
        std::fs::create_dir_all(skills.join("python-pro")).unwrap();
        std::fs::create_dir_all(skills.join("docker")).unwrap();

        let result = find_untracked(&manifest, tmp.path()).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].path.ends_with("docker"));
        assert_eq!(result[0].kind, UntrackedKind::Directory);
    }

    #[test]
    fn untracked_extra_file_in_managed_dir_not_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![github_skill_dir("python-pro")],
            install_targets: vec![local_target()],
        };
        let skill_dir = tmp.path().join(".claude/skills/python-pro");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Skill").unwrap();
        std::fs::write(skill_dir.join("my-notes.md"), "# Notes").unwrap();

        let result = find_untracked(&manifest, tmp.path()).unwrap();
        assert!(
            result.is_empty(),
            "lenient: extra files in managed dir not flagged"
        );
    }

    #[test]
    fn untracked_non_md_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![],
            install_targets: vec![local_target()],
        };
        let agents = tmp.path().join(".claude/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("notes.txt"), "not a skill").unwrap();

        let result = find_untracked(&manifest, tmp.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn untracked_missing_target_dir_no_error() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![],
            install_targets: vec![local_target()],
        };
        // Don't create .claude/ at all
        let result = find_untracked(&manifest, tmp.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn untracked_global_target_not_scanned() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![],
            install_targets: vec![global_target()],
        };
        // Even with files in global dirs, nothing should be reported
        let result = find_untracked(&manifest, tmp.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn untracked_single_file_skill_in_skills_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![github_skill("browser")],
            install_targets: vec![local_target()],
        };
        let skills = tmp.path().join(".claude/skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("browser.md"), "# tracked").unwrap();
        std::fs::write(skills.join("rogue.md"), "# rogue").unwrap();

        let result = find_untracked(&manifest, tmp.path()).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].path.ends_with("rogue.md"));
        assert_eq!(result[0].entity_type, EntityType::Skill);
    }

    #[test]
    fn untracked_deduplicates_across_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![],
            install_targets: vec![local_target(), local_target()],
        };
        let agents = tmp.path().join(".claude/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("rogue.md"), "# rogue").unwrap();

        let result = find_untracked(&manifest, tmp.path()).unwrap();
        assert_eq!(result.len(), 1, "should deduplicate across targets");
    }
}
