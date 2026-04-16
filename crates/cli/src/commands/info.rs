use std::path::Path;

use skillfile_core::error::SkillfileError;
use skillfile_core::lock::{lock_key, read_lock};
use skillfile_core::models::{short_sha, Entry, Manifest, SourceFields};
use skillfile_core::parser::MANIFEST_NAME;
use skillfile_core::patch::{has_dir_patch, has_patch, patch_path, patches_root};
use skillfile_deploy::paths::installed_paths;
use skillfile_sources::strategy::{content_file, is_dir_entry};
use skillfile_sources::sync::vendor_dir_for;

use super::status::is_modified_local;

fn format_source(entry: &Entry) -> Vec<(&'static str, String)> {
    match &entry.source {
        SourceFields::Github {
            owner_repo,
            path_in_repo,
            ref_,
        } => vec![
            ("Source", format!("github ({owner_repo})")),
            ("Path", path_in_repo.clone()),
            ("Ref", ref_.clone()),
        ],
        SourceFields::Local { path } => vec![("Source", format!("local ({path})"))],
        SourceFields::Url { url } => vec![("Source", format!("url ({url})"))],
    }
}

fn format_lock_status(entry: &Entry, repo_root: &Path) -> String {
    let locked = match read_lock(repo_root) {
        Ok(l) => l,
        Err(e) => return format!("unknown ({e})"),
    };
    let key = lock_key(entry);
    match locked.get(&key) {
        Some(lock_entry) => {
            let sha = short_sha(&lock_entry.sha);
            format!("sha {sha}")
        }
        None => "no".to_string(),
    }
}

fn format_pinned_status(entry: &Entry, repo_root: &Path) -> String {
    if has_patch(entry, repo_root) {
        format!("yes ({})", patch_path(entry, repo_root).display())
    } else if has_dir_patch(entry, repo_root) {
        let dir = patches_root(repo_root)
            .join(entry.entity_type.dir_name())
            .join(&entry.name);
        format!("yes ({})", dir.display())
    } else {
        "no".to_string()
    }
}

fn format_installed_paths(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<Vec<String>, SkillfileError> {
    let paths = installed_paths(entry, manifest, repo_root)?;
    let existing: Vec<String> = paths
        .iter()
        .filter(|p| p.exists())
        .map(|p| p.display().to_string())
        .collect();
    if existing.is_empty() {
        Ok(vec!["(not installed)".to_string()])
    } else {
        Ok(existing)
    }
}

fn format_cache_path(entry: &Entry, repo_root: &Path) -> String {
    let vdir = vendor_dir_for(entry, repo_root);
    if is_dir_entry(entry) {
        if vdir.is_dir() {
            vdir.display().to_string()
        } else {
            format!("{} (not cached)", vdir.display())
        }
    } else {
        let cf = content_file(entry);
        if cf.is_empty() {
            return "(no cache file)".to_string();
        }
        let cache_file = vdir.join(&cf);
        if cache_file.exists() {
            cache_file.display().to_string()
        } else {
            format!("{} (not cached)", cache_file.display())
        }
    }
}

pub fn cmd_info(name: &str, repo_root: &Path) -> Result<(), SkillfileError> {
    let manifest_path = repo_root.join(MANIFEST_NAME);
    if !manifest_path.exists() {
        return Err(SkillfileError::Manifest(format!(
            "{MANIFEST_NAME} not found in {}. Create one and run `skillfile init`.",
            repo_root.display()
        )));
    }

    let manifest = crate::config::parse_and_resolve(&manifest_path)?;

    let entry = manifest
        .entries
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| SkillfileError::Manifest(format!("entry '{name}' not found in Skillfile")))?;

    let label_w = 12;

    println!("{:>label_w$}  {}", "Name:", entry.name);
    println!("{:>label_w$}  {}", "Type:", entry.entity_type);

    for (label, value) in format_source(entry) {
        println!("{:>label_w$}  {value}", format!("{label}:"));
    }

    println!("{:>label_w$}  {}", "Locked:", format_lock_status(entry, repo_root));
    println!(
        "{:>label_w$}  {}",
        "Pinned:",
        format_pinned_status(entry, repo_root)
    );
    println!(
        "{:>label_w$}  {}",
        "Modified:",
        if is_modified_local(entry, &manifest, repo_root) {
            "yes"
        } else {
            "no"
        }
    );

    let installed = format_installed_paths(entry, &manifest, repo_root)?;
    for (i, path) in installed.iter().enumerate() {
        let label = if i == 0 { "Installed:" } else { "" };
        println!("{label:>label_w$}  {path}");
    }

    println!(
        "{:>label_w$}  {}",
        "Cache:",
        format_cache_path(entry, repo_root)
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use skillfile_core::models::{EntityType, SourceFields};

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::write(dir.join(MANIFEST_NAME), content).unwrap();
    }

    fn write_lock(dir: &Path, data: &serde_json::Value) {
        std::fs::write(
            dir.join("Skillfile.lock"),
            serde_json::to_string_pretty(data).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn info_entry_not_found() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  foo  skills/foo.md\n");
        let result = cmd_info("nonexistent", dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn info_no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_info("foo", dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn info_local_entry() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("skills/foo.md");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "# Foo").unwrap();
        write_manifest(dir.path(), "local  skill  foo  skills/foo.md\n");
        // Should succeed without error
        cmd_info("foo", dir.path()).unwrap();
    }

    #[test]
    fn info_github_entry_locked() {
        let dir = tempfile::tempdir().unwrap();
        let sha = "87321636a1c666283d8f17398b45c2644395044b";
        write_manifest(
            dir.path(),
            "github  agent  my-agent  owner/repo  agents/agent.md  main\n",
        );
        write_lock(
            dir.path(),
            &serde_json::json!({"github/agent/my-agent": {"sha": sha, "raw_url": "https://example.com"}}),
        );
        cmd_info("my-agent", dir.path()).unwrap();
    }

    #[test]
    fn info_github_entry_unlocked() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "github  agent  my-agent  owner/repo  agents/agent.md  main\n",
        );
        cmd_info("my-agent", dir.path()).unwrap();
    }

    #[test]
    fn format_source_github() {
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "browser".into(),
            source: SourceFields::Github {
                owner_repo: "anthropics/skills".into(),
                path_in_repo: "skills/browser.md".into(),
                ref_: "main".into(),
            },
        };
        let fields = format_source(&entry);
        assert_eq!(fields.len(), 3);
        assert!(fields[0].1.contains("anthropics/skills"));
        assert_eq!(fields[1].1, "skills/browser.md");
        assert_eq!(fields[2].1, "main");
    }

    #[test]
    fn format_source_local() {
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "foo".into(),
            source: SourceFields::Local {
                path: "skills/foo.md".into(),
            },
        };
        let fields = format_source(&entry);
        assert_eq!(fields.len(), 1);
        assert!(fields[0].1.contains("skills/foo.md"));
    }

    #[test]
    fn format_source_url() {
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "my-agent".into(),
            source: SourceFields::Url {
                url: "https://example.com/agent.md".into(),
            },
        };
        let fields = format_source(&entry);
        assert_eq!(fields.len(), 1);
        assert!(fields[0].1.contains("https://example.com/agent.md"));
    }

    #[test]
    fn lock_status_locked() {
        let dir = tempfile::tempdir().unwrap();
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "my-agent".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "agents/agent.md".into(),
                ref_: "main".into(),
            },
        };
        write_lock(
            dir.path(),
            &serde_json::json!({"github/agent/my-agent": {"sha": sha, "raw_url": "https://example.com"}}),
        );
        let status = format_lock_status(&entry, dir.path());
        assert!(status.contains("abcdef123456"));
    }

    #[test]
    fn lock_status_unlocked() {
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "my-agent".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "agents/agent.md".into(),
                ref_: "main".into(),
            },
        };
        let status = format_lock_status(&entry, dir.path());
        assert_eq!(status, "no");
    }

    #[test]
    fn pinned_status_not_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "foo".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "skills/foo.md".into(),
                ref_: "main".into(),
            },
        };
        assert_eq!(format_pinned_status(&entry, dir.path()), "no");
    }

    #[test]
    fn pinned_status_single_file_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let patches_dir = dir.path().join(".skillfile/patches/skills");
        std::fs::create_dir_all(&patches_dir).unwrap();
        std::fs::write(patches_dir.join("foo.patch"), "patch").unwrap();

        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "foo".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "skills/foo.md".into(),
                ref_: "main".into(),
            },
        };
        let status = format_pinned_status(&entry, dir.path());
        assert!(status.starts_with("yes"), "expected 'yes ...', got: {status}");
        assert!(
            status.contains(".skillfile/patches/skills/foo.patch"),
            "expected patch path, got: {status}"
        );
    }

    #[test]
    fn pinned_status_dir_entry_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let patches_dir = dir.path().join(".skillfile/patches/skills/my-dir");
        std::fs::create_dir_all(&patches_dir).unwrap();
        std::fs::write(patches_dir.join("tool.md.patch"), "patch").unwrap();

        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "my-dir".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "skills/my-dir".into(),
                ref_: "main".into(),
            },
        };
        let status = format_pinned_status(&entry, dir.path());
        assert!(status.starts_with("yes"), "expected 'yes ...', got: {status}");
        assert!(
            status.contains(".skillfile/patches/skills/my-dir"),
            "expected dir patch path, got: {status}"
        );
    }

    #[test]
    fn cache_path_single_file_cached() {
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "foo".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "skills/foo.md".into(),
                ref_: "main".into(),
            },
        };
        let vdir = dir.path().join(".skillfile/cache/skills/foo");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("foo.md"), "content").unwrap();

        let path = format_cache_path(&entry, dir.path());
        assert!(
            path.contains("foo.md") && !path.contains("not cached"),
            "expected cached path, got: {path}"
        );
    }

    #[test]
    fn cache_path_single_file_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "foo".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "skills/foo.md".into(),
                ref_: "main".into(),
            },
        };
        let path = format_cache_path(&entry, dir.path());
        assert!(
            path.contains("not cached"),
            "expected 'not cached', got: {path}"
        );
    }

    #[test]
    fn cache_path_local_entry() {
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "foo".into(),
            source: SourceFields::Local {
                path: "skills/foo.md".into(),
            },
        };
        let path = format_cache_path(&entry, dir.path());
        assert_eq!(path, "(no cache file)");
    }

    #[test]
    fn installed_paths_no_targets() {
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "foo".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "skills/foo.md".into(),
                ref_: "main".into(),
            },
        };
        let manifest = Manifest {
            entries: vec![entry.clone()],
            install_targets: vec![],
        };
        let paths = format_installed_paths(&entry, &manifest, dir.path()).unwrap();
        assert_eq!(paths, vec!["(not installed)"]);
    }

    #[test]
    fn lock_status_corrupt_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Skillfile.lock"), "not json").unwrap();
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "my-agent".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "agents/agent.md".into(),
                ref_: "main".into(),
            },
        };
        let status = format_lock_status(&entry, dir.path());
        assert!(
            status.starts_with("unknown"),
            "expected 'unknown ...', got: {status}"
        );
    }
}
