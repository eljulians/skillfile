use std::collections::HashMap;
use std::path::Path;

use skillfile_core::error::SkillfileError;
use skillfile_core::lock::{lock_key, read_lock};
use skillfile_core::models::{short_sha, EntityType, Entry, LockEntry, Manifest, SourceFields};
use skillfile_core::parser::MANIFEST_NAME;
use skillfile_core::patch::{has_dir_patch, has_patch, walkdir};
use skillfile_deploy::paths::{installed_dir_files, installed_path};
use skillfile_sources::strategy::{content_file, is_dir_entry, meta_sha};
use skillfile_sources::sync::vendor_dir_for;

fn is_cache_file_modified(
    cache_file: &std::path::PathBuf,
    vdir: &std::path::PathBuf,
    installed: &HashMap<String, std::path::PathBuf>,
) -> Result<bool, ()> {
    let filename = cache_file
        .strip_prefix(vdir)
        .map_err(|_| ())?
        .to_string_lossy()
        .to_string();
    let inst_path = match installed.get(&filename) {
        Some(p) if p.exists() => p,
        _ => return Ok(false),
    };
    let cache_text = std::fs::read_to_string(cache_file).map_err(|_| ())?;
    let installed_text = std::fs::read_to_string(inst_path).map_err(|_| ())?;
    Ok(installed_text != cache_text)
}

fn check_dir_files_modified(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<bool, ()> {
    let installed = installed_dir_files(entry, manifest, repo_root).map_err(|_| ())?;
    if installed.is_empty() {
        return Ok(false);
    }
    // If pinned, the installed files are expected to differ from cache
    if has_dir_patch(entry, repo_root) {
        return Ok(false);
    }
    let vdir = vendor_dir_for(entry, repo_root);
    if !vdir.is_dir() {
        return Ok(false);
    }
    for cache_file in walkdir(&vdir) {
        if cache_file.file_name().is_none_or(|n| n == ".meta") {
            continue;
        }
        if is_cache_file_modified(&cache_file, &vdir, &installed)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_dir_modified_local(entry: &Entry, manifest: &Manifest, repo_root: &Path) -> bool {
    check_dir_files_modified(entry, manifest, repo_root).unwrap_or(false)
}

fn check_single_file_modified(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<bool, ()> {
    let dest = installed_path(entry, manifest, repo_root).map_err(|_| ())?;
    if !dest.exists() {
        return Ok(false);
    }
    let vdir = vendor_dir_for(entry, repo_root);
    let cf = content_file(entry);
    if cf.is_empty() {
        return Ok(false);
    }
    let cache_file = vdir.join(&cf);
    if !cache_file.exists() {
        return Ok(false);
    }
    // If pinned, the installed file is expected to differ from cache
    if has_patch(entry, repo_root) {
        return Ok(false);
    }
    let cache_text = std::fs::read_to_string(&cache_file).map_err(|_| ())?;
    let installed_text = std::fs::read_to_string(&dest).map_err(|_| ())?;
    Ok(installed_text != cache_text)
}

/// Check if an installed file differs from cache (local only, no network).
fn is_modified_local(entry: &Entry, manifest: &Manifest, repo_root: &Path) -> bool {
    if matches!(entry.source, SourceFields::Local { .. }) {
        return false;
    }
    if is_dir_entry(entry) {
        return is_dir_modified_local(entry, manifest, repo_root);
    }
    check_single_file_modified(entry, manifest, repo_root).unwrap_or(false)
}

struct StatusContext<'a> {
    manifest: &'a Manifest,
    repo_root: &'a Path,
    locked: &'a std::collections::BTreeMap<String, LockEntry>,
    check_upstream: bool,
    sha_cache: &'a mut HashMap<(String, String), String>,
    col_w: usize,
}

fn resolve_upstream_sha(
    ctx: &mut StatusContext<'_>,
    owner_repo: &str,
    ref_: &str,
) -> Result<String, SkillfileError> {
    let cache_key = (owner_repo.to_string(), ref_.to_string());
    if let Some(cached) = ctx.sha_cache.get(&cache_key) {
        return Ok(cached.clone());
    }
    let client = skillfile_sources::http::UreqClient::new();
    let resolved = skillfile_sources::resolver::resolve_github_sha(&client, owner_repo, ref_)?;
    ctx.sha_cache.insert(cache_key, resolved.clone());
    Ok(resolved)
}

fn upstream_status_for_github(
    ctx: &mut StatusContext<'_>,
    entry: &Entry,
    sha: &str,
) -> Result<String, SkillfileError> {
    let SourceFields::Github {
        owner_repo, ref_, ..
    } = &entry.source
    else {
        return Ok(format!("locked    sha={}", short_sha(sha)));
    };
    let owner_repo = owner_repo.clone();
    let ref_ = ref_.clone();
    let upstream_sha = resolve_upstream_sha(ctx, &owner_repo, &ref_)?;
    let sha_short = short_sha(sha);
    if upstream_sha == sha {
        Ok(format!("up to date  sha={sha_short}"))
    } else {
        let upstream_short = short_sha(&upstream_sha);
        Ok(format!(
            "outdated    locked={sha_short}  upstream={upstream_short}"
        ))
    }
}

fn build_annotation(entry: &Entry, ctx: &StatusContext<'_>) -> String {
    let mut parts = Vec::new();
    if has_patch(entry, ctx.repo_root) || has_dir_patch(entry, ctx.repo_root) {
        parts.push("[pinned]");
    }
    if is_modified_local(entry, ctx.manifest, ctx.repo_root) {
        parts.push("[modified]");
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  {}", parts.join("  "))
    }
}

fn format_entry_status(
    entry: &Entry,
    ctx: &mut StatusContext<'_>,
) -> Result<String, SkillfileError> {
    let key = lock_key(entry);
    let name = &entry.name;
    let col_w = ctx.col_w;

    if let SourceFields::Local { path } = &entry.source {
        let status = if ctx.repo_root.join(path).exists() {
            "local".to_string()
        } else {
            format!("local  \u{2717} path missing: {path}")
        };
        return Ok(format!("{name:<col_w$} {status}"));
    }

    let Some(locked_info) = ctx.locked.get(&key) else {
        return Ok(format!("{name:<col_w$} unlocked"));
    };

    let sha = &locked_info.sha;
    let vdir = vendor_dir_for(entry, ctx.repo_root);
    let meta = meta_sha(&vdir);
    let sha_short = short_sha(sha);

    let base_status = if meta.as_deref() != Some(sha.as_str()) {
        format!("locked    sha={sha_short}  (missing or stale)")
    } else if ctx.check_upstream {
        upstream_status_for_github(ctx, entry, sha)?
    } else {
        format!("locked    sha={sha_short}")
    };

    let annotation = build_annotation(entry, ctx);
    Ok(format!("{name:<col_w$} {base_status}{annotation}"))
}

pub fn cmd_status(repo_root: &Path, check_upstream: bool) -> Result<(), SkillfileError> {
    let manifest_path = repo_root.join(MANIFEST_NAME);
    if !manifest_path.exists() {
        return Err(SkillfileError::Manifest(format!(
            "{MANIFEST_NAME} not found in {}. Create one and run `skillfile init`.",
            repo_root.display()
        )));
    }

    let manifest = crate::config::parse_and_resolve(&manifest_path)?;
    let locked = read_lock(repo_root)?;

    let col_w = manifest
        .entries
        .iter()
        .map(|e| e.name.len())
        .max()
        .unwrap_or(10)
        + 2;

    let mut ctx = StatusContext {
        manifest: &manifest,
        repo_root,
        locked: &locked,
        check_upstream,
        sha_cache: &mut HashMap::new(),
        col_w,
    };

    for entry in &manifest.entries {
        let line = format_entry_status(entry, &mut ctx)?;
        println!("{line}");
    }

    if !manifest.entries.is_empty() {
        let summary = format_summary(&manifest, repo_root);
        println!("\n{summary}");
    }

    Ok(())
}

fn count_entries(manifest: &Manifest, repo_root: &Path) -> (usize, usize, usize, usize) {
    let mut skills: usize = 0;
    let mut agents: usize = 0;
    let mut pinned: usize = 0;
    let mut modified: usize = 0;

    for entry in &manifest.entries {
        match entry.entity_type {
            EntityType::Skill => skills += 1,
            EntityType::Agent => agents += 1,
        }
        if has_patch(entry, repo_root) || has_dir_patch(entry, repo_root) {
            pinned += 1;
        }
        if is_modified_local(entry, manifest, repo_root) {
            modified += 1;
        }
    }

    (skills, agents, pinned, modified)
}

fn format_summary(manifest: &Manifest, repo_root: &Path) -> String {
    use std::fmt::Write;

    let (skills, agents, pinned, modified) = count_entries(manifest, repo_root);

    let mut counts = Vec::new();
    if skills > 0 {
        counts.push(format!(
            "{skills} skill{}",
            if skills == 1 { "" } else { "s" }
        ));
    }
    if agents > 0 {
        counts.push(format!(
            "{agents} agent{}",
            if agents == 1 { "" } else { "s" }
        ));
    }

    let mut summary = counts.join(", ");

    if pinned > 0 {
        let _ = write!(summary, " · {pinned} pinned");
    }
    if modified > 0 {
        let _ = write!(summary, " · {modified} modified");
    }

    let mut lines = format!("  {summary}");

    if !manifest.install_targets.is_empty() {
        let targets: Vec<String> = manifest
            .install_targets
            .iter()
            .map(ToString::to_string)
            .collect();
        let _ = write!(lines, "\n  Install targets: {}", targets.join(", "));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use skillfile_core::models::{EntityType, InstallTarget, Scope, SourceFields};

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

    struct VendorEntry<'a> {
        entity_type: &'a str,
        name: &'a str,
    }

    fn write_meta(dir: &Path, ve: &VendorEntry<'_>, sha: &str) {
        let vdir = dir
            .join(".skillfile/cache")
            .join(format!("{}s", ve.entity_type))
            .join(ve.name);
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(
            vdir.join(".meta"),
            serde_json::json!({"sha": sha}).to_string(),
        )
        .unwrap();
    }

    struct VendorFile<'a> {
        entry: &'a VendorEntry<'a>,
        filename: &'a str,
    }

    fn write_vendor_content(dir: &Path, vf: &VendorFile<'_>, content: &str) {
        let vdir = dir
            .join(".skillfile/cache")
            .join(format!("{}s", vf.entry.entity_type))
            .join(vf.entry.name);
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join(vf.filename), content).unwrap();
    }

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ORIGINAL: &str = "# Agent\n\nUpstream content.\n";
    const MODIFIED: &str = "# Agent\n\nUpstream content.\n\n## Custom Section\n\nAdded by user.\n";
    const VE_AGENT: VendorEntry<'_> = VendorEntry {
        entity_type: "agent",
        name: "my-agent",
    };

    fn local_entry(name: &str, path: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: name.into(),
            source: SourceFields::Local { path: path.into() },
        }
    }

    fn claude_local_target() -> InstallTarget {
        InstallTarget {
            adapter: "claude-code".into(),
            scope: Scope::Local,
        }
    }

    fn agent_manifest() -> Manifest {
        Manifest {
            entries: vec![Entry {
                entity_type: EntityType::Agent,
                name: "my-agent".into(),
                source: SourceFields::Github {
                    owner_repo: "owner/repo".into(),
                    path_in_repo: "agents/agent.md".into(),
                    ref_: "main".into(),
                },
            }],
            install_targets: vec![claude_local_target()],
        }
    }

    fn dir_skill_manifest() -> Manifest {
        Manifest {
            entries: vec![Entry {
                entity_type: EntityType::Skill,
                name: "my-dir".into(),
                source: SourceFields::Github {
                    owner_repo: "owner/repo".into(),
                    path_in_repo: "skills/my-dir".into(),
                    ref_: "main".into(),
                },
            }],
            install_targets: vec![claude_local_target()],
        }
    }

    #[test]
    fn no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_status(dir.path(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn local_entry_path_exists_shows_local() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("skills/foo.md");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "# Foo").unwrap();
        write_manifest(dir.path(), "local  skill  foo  skills/foo.md\n");
        cmd_status(dir.path(), false).unwrap();
    }

    #[test]
    fn local_entry_path_missing_shows_status_without_error() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  foo  skills/foo.md\n");
        // Missing path should not cause an error — status reports it inline
        cmd_status(dir.path(), false).unwrap();
    }

    #[test]
    fn github_entry_unlocked() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "github  agent  my-agent  owner/repo  agents/agent.md  main\n",
        );
        cmd_status(dir.path(), false).unwrap();
    }

    #[test]
    fn github_entry_locked_vendor_matches() {
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
        write_meta(dir.path(), &VE_AGENT, sha);
        cmd_status(dir.path(), false).unwrap();
    }

    #[test]
    fn github_entry_locked_vendor_missing() {
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
        // No .meta written
        cmd_status(dir.path(), false).unwrap();
    }

    #[test]
    fn modified_shows_for_changed_installed_file() {
        let dir = tempfile::tempdir().unwrap();
        write_lock(
            dir.path(),
            &serde_json::json!({"github/agent/my-agent": {"sha": SHA, "raw_url": "https://example.com"}}),
        );
        write_meta(dir.path(), &VE_AGENT, SHA);
        write_vendor_content(
            dir.path(),
            &VendorFile {
                entry: &VE_AGENT,
                filename: "agent.md",
            },
            ORIGINAL,
        );
        let installed = dir.path().join(".claude/agents");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("my-agent.md"), MODIFIED).unwrap();

        // is_modified_local should return true
        let manifest = agent_manifest();
        let entry = &manifest.entries[0];
        assert!(is_modified_local(entry, &manifest, dir.path()));
    }

    #[test]
    fn modified_not_shown_for_clean_entry() {
        let dir = tempfile::tempdir().unwrap();
        write_lock(
            dir.path(),
            &serde_json::json!({"github/agent/my-agent": {"sha": SHA, "raw_url": "https://example.com"}}),
        );
        write_meta(dir.path(), &VE_AGENT, SHA);
        write_vendor_content(
            dir.path(),
            &VendorFile {
                entry: &VE_AGENT,
                filename: "agent.md",
            },
            ORIGINAL,
        );
        let installed = dir.path().join(".claude/agents");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("my-agent.md"), ORIGINAL).unwrap();

        let manifest = agent_manifest();
        let entry = &manifest.entries[0];
        assert!(!is_modified_local(entry, &manifest, dir.path()));
    }

    #[test]
    fn modified_not_shown_when_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        write_lock(
            dir.path(),
            &serde_json::json!({"github/agent/my-agent": {"sha": SHA, "raw_url": "https://example.com"}}),
        );
        write_meta(dir.path(), &VE_AGENT, SHA);
        write_vendor_content(
            dir.path(),
            &VendorFile {
                entry: &VE_AGENT,
                filename: "agent.md",
            },
            ORIGINAL,
        );
        // No installed file

        let manifest = agent_manifest();
        let entry = &manifest.entries[0];
        assert!(!is_modified_local(entry, &manifest, dir.path()));
    }

    #[test]
    fn modified_not_shown_without_vendor_cache() {
        let dir = tempfile::tempdir().unwrap();
        write_lock(
            dir.path(),
            &serde_json::json!({"github/agent/my-agent": {"sha": SHA, "raw_url": "https://example.com"}}),
        );
        write_meta(dir.path(), &VE_AGENT, SHA);
        // No vendor cache content file
        let installed = dir.path().join(".claude/agents");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("my-agent.md"), MODIFIED).unwrap();

        let manifest = agent_manifest();
        let entry = &manifest.entries[0];
        assert!(!is_modified_local(entry, &manifest, dir.path()));
    }

    // Dir-entry tests: claude-code skills use Nested dir mode (.claude/skills/<name>/)

    /// Build a manifest with a github skill dir entry (path_in_repo without .md).
    /// claude-code skills are Nested, so installed files live under .claude/skills/<name>/.
    fn setup_dir_entry(dir: &Path, installed_content: Option<&str>, cache_content: &str) {
        write_lock(
            dir,
            &serde_json::json!({"github/skill/my-dir": {"sha": SHA, "raw_url": "https://example.com"}}),
        );

        // Write the cache vendor dir with a file
        let vdir = dir.join(".skillfile/cache").join("skills").join("my-dir");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("tool.md"), cache_content).unwrap();
        std::fs::write(
            vdir.join(".meta"),
            serde_json::json!({"sha": SHA}).to_string(),
        )
        .unwrap();

        // Write the installed nested dir if content is provided
        if let Some(content) = installed_content {
            let installed_dir = dir.join(".claude/skills/my-dir");
            std::fs::create_dir_all(&installed_dir).unwrap();
            std::fs::write(installed_dir.join("tool.md"), content).unwrap();
        }
    }

    #[test]
    fn dir_entry_modified_shows_modified() {
        let dir = tempfile::tempdir().unwrap();
        setup_dir_entry(dir.path(), Some(MODIFIED), ORIGINAL);

        let manifest = dir_skill_manifest();
        let entry = &manifest.entries[0];
        assert!(
            is_dir_entry(entry),
            "expected entry to be recognised as a dir entry"
        );
        assert!(
            is_modified_local(entry, &manifest, dir.path()),
            "expected modified=true when installed content differs from cache"
        );
    }

    #[test]
    fn dir_entry_clean_shows_not_modified() {
        let dir = tempfile::tempdir().unwrap();
        setup_dir_entry(dir.path(), Some(ORIGINAL), ORIGINAL);

        let manifest = dir_skill_manifest();
        let entry = &manifest.entries[0];
        assert!(
            is_dir_entry(entry),
            "expected entry to be recognised as a dir entry"
        );
        assert!(
            !is_modified_local(entry, &manifest, dir.path()),
            "expected modified=false when installed content matches cache"
        );
    }

    #[test]
    fn dir_entry_missing_vendor_dir_not_modified() {
        let dir = tempfile::tempdir().unwrap();
        // Write lock but no vendor cache dir at all
        write_lock(
            dir.path(),
            &serde_json::json!({"github/skill/my-dir": {"sha": SHA, "raw_url": "https://example.com"}}),
        );
        // No .skillfile/cache/skills/my-dir/ written

        let manifest = dir_skill_manifest();
        let entry = &manifest.entries[0];
        assert!(
            is_dir_entry(entry),
            "expected entry to be recognised as a dir entry"
        );
        assert!(
            !is_modified_local(entry, &manifest, dir.path()),
            "expected modified=false when vendor cache dir is absent"
        );
    }

    #[test]
    fn local_entry_always_not_modified() {
        let dir = tempfile::tempdir().unwrap();

        let manifest = Manifest {
            entries: vec![local_entry("foo", "skills/foo.md")],
            ..Manifest::default()
        };
        let entry = &manifest.entries[0];
        assert!(
            !is_modified_local(entry, &manifest, dir.path()),
            "local entries must always report modified=false"
        );
    }

    #[test]
    fn pinned_entry_not_modified() {
        let dir = tempfile::tempdir().unwrap();
        write_lock(
            dir.path(),
            &serde_json::json!({"github/agent/my-agent": {"sha": SHA, "raw_url": "https://example.com"}}),
        );
        write_meta(dir.path(), &VE_AGENT, SHA);
        write_vendor_content(
            dir.path(),
            &VendorFile {
                entry: &VE_AGENT,
                filename: "agent.md",
            },
            ORIGINAL,
        );
        let installed = dir.path().join(".claude/agents");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("my-agent.md"), MODIFIED).unwrap();

        // Write a patch file — entry is pinned
        let patches_dir = dir.path().join(".skillfile/patches/agents");
        std::fs::create_dir_all(&patches_dir).unwrap();
        std::fs::write(patches_dir.join("my-agent.patch"), "patch content").unwrap();

        let manifest = agent_manifest();
        let entry = &manifest.entries[0];
        assert!(
            !is_modified_local(entry, &manifest, dir.path()),
            "pinned entries must not report as modified"
        );
    }

    #[test]
    fn dir_entry_pinned_not_modified() {
        let dir = tempfile::tempdir().unwrap();
        setup_dir_entry(dir.path(), Some(MODIFIED), ORIGINAL);

        // Write a dir patch — entry is pinned
        let patches_dir = dir.path().join(".skillfile/patches/skills/my-dir");
        std::fs::create_dir_all(&patches_dir).unwrap();
        std::fs::write(patches_dir.join("tool.md.patch"), "patch content").unwrap();

        let manifest = dir_skill_manifest();
        let entry = &manifest.entries[0];
        assert!(
            !is_modified_local(entry, &manifest, dir.path()),
            "pinned dir entries must not report as modified"
        );
    }

    // -- format_entry_status: local path drift --

    #[test]
    fn local_entry_existing_path_formats_as_local() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("skills/foo.md");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "# Foo").unwrap();

        let manifest = Manifest {
            entries: vec![local_entry("foo", "skills/foo.md")],
            ..Manifest::default()
        };
        let locked = std::collections::BTreeMap::new();
        let mut sha_cache = HashMap::new();
        let mut ctx = StatusContext {
            manifest: &manifest,
            repo_root: dir.path(),
            locked: &locked,
            check_upstream: false,
            sha_cache: &mut sha_cache,
            col_w: 12,
        };
        let line = format_entry_status(&manifest.entries[0], &mut ctx).unwrap();
        assert!(
            line.contains("local") && !line.contains("path missing"),
            "existing path should show 'local' without warning, got: {line}"
        );
    }

    #[test]
    fn local_entry_missing_path_formats_with_warning() {
        let dir = tempfile::tempdir().unwrap();

        let manifest = Manifest {
            entries: vec![local_entry("foo", "skills/foo.md")],
            ..Manifest::default()
        };
        let locked = std::collections::BTreeMap::new();
        let mut sha_cache = HashMap::new();
        let mut ctx = StatusContext {
            manifest: &manifest,
            repo_root: dir.path(),
            locked: &locked,
            check_upstream: false,
            sha_cache: &mut sha_cache,
            col_w: 12,
        };
        let line = format_entry_status(&manifest.entries[0], &mut ctx).unwrap();
        assert!(
            line.contains("path missing"),
            "missing path should show warning, got: {line}"
        );
        assert!(
            line.contains("skills/foo.md"),
            "warning should include the path, got: {line}"
        );
    }

    // -- format_summary --

    #[test]
    fn summary_counts_skills_and_agents() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![
                local_entry("a", "a.md"),
                local_entry("b", "b.md"),
                Entry {
                    entity_type: EntityType::Agent,
                    name: "c".into(),
                    source: SourceFields::Github {
                        owner_repo: "o/r".into(),
                        path_in_repo: "c.md".into(),
                        ref_: "main".into(),
                    },
                },
            ],
            install_targets: vec![],
        };
        let out = format_summary(&manifest, dir.path());
        assert!(
            out.contains("2 skills, 1 agent"),
            "expected skill/agent counts, got: {out}"
        );
    }

    #[test]
    fn summary_shows_install_targets() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![local_entry("x", "x.md")],
            install_targets: vec![
                claude_local_target(),
                InstallTarget {
                    adapter: "cursor".into(),
                    scope: Scope::Global,
                },
            ],
        };
        let out = format_summary(&manifest, dir.path());
        assert!(
            out.contains("Install targets: claude-code (local), cursor (global)"),
            "expected install targets line, got: {out}"
        );
    }

    #[test]
    fn summary_shows_pinned_and_modified() {
        let dir = tempfile::tempdir().unwrap();
        write_lock(
            dir.path(),
            &serde_json::json!({"github/agent/my-agent": {"sha": SHA, "raw_url": "https://example.com"}}),
        );
        write_meta(dir.path(), &VE_AGENT, SHA);
        write_vendor_content(
            dir.path(),
            &VendorFile {
                entry: &VE_AGENT,
                filename: "agent.md",
            },
            ORIGINAL,
        );
        let installed = dir.path().join(".claude/agents");
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("my-agent.md"), MODIFIED).unwrap();

        let manifest = agent_manifest();
        let out = format_summary(&manifest, dir.path());
        assert!(out.contains("1 agent"), "expected agent count, got: {out}");
        assert!(
            out.contains("1 modified"),
            "expected modified flag, got: {out}"
        );
    }

    #[test]
    fn summary_singular_skill() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            entries: vec![local_entry("solo", "solo.md")],
            install_targets: vec![],
        };
        let out = format_summary(&manifest, dir.path());
        assert!(
            out.contains("1 skill") && !out.contains("1 skills"),
            "expected singular 'skill', got: {out}"
        );
    }
}
