use std::io::Write as IoWrite;
use std::path::Path;

use skillfile_core::conflict::read_conflict;
use skillfile_core::error::SkillfileError;
use skillfile_core::lock::{lock_key, read_lock};
use skillfile_core::models::{short_sha, Entry};
use skillfile_core::parser::{find_entry_in, parse_manifest, MANIFEST_NAME};
use skillfile_core::progress;
use skillfile_deploy::paths::{installed_dir_files, installed_path};
use skillfile_sources::strategy::{content_file, is_dir_entry};
use skillfile_sources::sync::vendor_dir_for;

use crate::patch::walkdir;

fn diff_local_single(entry: &Entry, sha: &str, repo_root: &Path) -> Result<(), SkillfileError> {
    let manifest = crate::config::parse_and_resolve(&repo_root.join(MANIFEST_NAME))?;
    let vdir = vendor_dir_for(entry, repo_root);
    let cf = content_file(entry);
    if cf.is_empty() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not cached — run `skillfile install` first",
            entry.name
        )));
    }
    let cache_file = vdir.join(&cf);
    if !cache_file.exists() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not cached — run `skillfile install` first",
            entry.name
        )));
    }

    let dest = installed_path(entry, &manifest, repo_root)?;
    if !dest.exists() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not installed — run `skillfile install` first",
            entry.name
        )));
    }

    let upstream = std::fs::read_to_string(&cache_file)?;
    let installed_text = std::fs::read_to_string(&dest)?;

    let diff_text = similar::TextDiff::from_lines(upstream.as_str(), installed_text.as_str());
    let formatted = format!(
        "{}",
        diff_text.unified_diff().context_radius(3).header(
            &format!("a/{}.md (upstream sha={})", entry.name, short_sha(sha)),
            &format!("b/{}.md (installed)", entry.name),
        )
    );

    if formatted.is_empty() {
        println!("'{}' is clean — no local modifications", entry.name);
    } else {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(formatted.as_bytes())?;
    }

    Ok(())
}

fn diff_local_dir(entry: &Entry, sha: &str, repo_root: &Path) -> Result<(), SkillfileError> {
    let manifest = crate::config::parse_and_resolve(&repo_root.join(MANIFEST_NAME))?;
    let vdir = vendor_dir_for(entry, repo_root);
    if !vdir.is_dir() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not cached — run `skillfile install` first",
            entry.name
        )));
    }

    let installed = installed_dir_files(entry, &manifest, repo_root)?;
    if installed.is_empty() {
        return Err(SkillfileError::Manifest(format!(
            "'{}' is not installed — run `skillfile install` first",
            entry.name
        )));
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut any_diff = false;

    for cache_file in walkdir(&vdir) {
        if cache_file.file_name().is_some_and(|n| n == ".meta") {
            continue;
        }
        let filename = match cache_file.strip_prefix(&vdir).ok().and_then(|p| p.to_str()) {
            Some(f) => f.to_string(),
            None => continue,
        };
        let Some(inst_path) = installed.get(&filename) else {
            continue;
        };
        if !inst_path.exists() {
            continue;
        }

        let original_text = std::fs::read_to_string(&cache_file)?;
        let installed_text = std::fs::read_to_string(inst_path)?;
        let diff_text =
            similar::TextDiff::from_lines(original_text.as_str(), installed_text.as_str());
        let formatted = format!(
            "{}",
            diff_text.unified_diff().context_radius(3).header(
                &format!(
                    "a/{}/{filename} (upstream sha={})",
                    entry.name,
                    short_sha(sha)
                ),
                &format!("b/{}/{filename} (installed)", entry.name),
            )
        );

        if !formatted.is_empty() {
            any_diff = true;
            out.write_all(formatted.as_bytes())?;
        }
    }

    if !any_diff {
        println!("'{}' is clean — no local modifications", entry.name);
    }

    Ok(())
}

pub fn cmd_diff(name: &str, repo_root: &Path) -> Result<(), SkillfileError> {
    let manifest_path = repo_root.join(MANIFEST_NAME);
    let result = parse_manifest(&manifest_path)?;
    let entry = find_entry_in(name, &result.manifest)?;

    // Check if there's a pending conflict for this entry
    let conflict = read_conflict(repo_root)?;
    if let Some(ref c) = conflict {
        if c.entry == name {
            return diff_conflict(entry, c, repo_root);
        }
    }

    if entry.source_type() == "local" {
        println!("'{name}' is a local entry — nothing to diff");
        return Ok(());
    }

    let locked = read_lock(repo_root)?;
    let key = lock_key(entry);
    if !locked.contains_key(&key) {
        return Err(SkillfileError::Manifest(format!(
            "'{name}' is not locked — run `skillfile install` first"
        )));
    }
    let sha = locked[&key].sha.clone();

    if is_dir_entry(entry) {
        diff_local_dir(entry, &sha, repo_root)
    } else {
        diff_local_single(entry, &sha, repo_root)
    }
}

fn diff_conflict(
    entry: &Entry,
    conflict: &skillfile_core::models::ConflictState,
    _repo_root: &Path,
) -> Result<(), SkillfileError> {
    // Conflict mode: fetch old and new upstream, show upstream delta
    // This requires network access
    progress!(
        "  fetching upstream at old sha={} ...",
        short_sha(&conflict.old_sha)
    );
    let client = skillfile_sources::http::UreqClient::new();

    if is_dir_entry(entry) {
        diff_conflict_dir(entry, conflict, &client)?;
    } else {
        diff_conflict_single(entry, conflict, &client)?;
    }
    Ok(())
}

fn diff_conflict_single(
    entry: &Entry,
    conflict: &skillfile_core::models::ConflictState,
    client: &dyn skillfile_sources::http::HttpClient,
) -> Result<(), SkillfileError> {
    let old_content = skillfile_sources::sync::fetch_file_at_sha(client, entry, &conflict.old_sha)?;
    progress!("done");
    progress!(
        "  fetching upstream at new sha={} ...",
        short_sha(&conflict.new_sha)
    );
    let new_content = skillfile_sources::sync::fetch_file_at_sha(client, entry, &conflict.new_sha)?;
    progress!("done\n");

    let diff_text = similar::TextDiff::from_lines(old_content.as_str(), new_content.as_str());
    let formatted = format!(
        "{}",
        diff_text.unified_diff().context_radius(3).header(
            &format!(
                "{}.md (old upstream sha={})",
                entry.name,
                short_sha(&conflict.old_sha)
            ),
            &format!(
                "{}.md (new upstream sha={})",
                entry.name,
                short_sha(&conflict.new_sha)
            ),
        )
    );

    if formatted.is_empty() {
        println!("No upstream changes detected (patch conflict may be due to local file drift).");
    } else {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(formatted.as_bytes())?;
    }
    Ok(())
}

fn diff_conflict_dir(
    entry: &Entry,
    conflict: &skillfile_core::models::ConflictState,
    client: &dyn skillfile_sources::http::HttpClient,
) -> Result<(), SkillfileError> {
    let old_files = skillfile_sources::sync::fetch_dir_at_sha(client, entry, &conflict.old_sha)?;
    progress!("done");
    progress!(
        "  fetching upstream at new sha={} ...",
        short_sha(&conflict.new_sha)
    );
    let new_files = skillfile_sources::sync::fetch_dir_at_sha(client, entry, &conflict.new_sha)?;
    progress!("done\n");

    let mut all_filenames: Vec<String> = old_files
        .keys()
        .chain(new_files.keys())
        .cloned()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    all_filenames.sort();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut any_diff = false;

    for filename in &all_filenames {
        let old_content = old_files
            .get(filename)
            .map_or("", std::string::String::as_str);
        let new_content = new_files
            .get(filename)
            .map_or("", std::string::String::as_str);
        let diff_text = similar::TextDiff::from_lines(old_content, new_content);
        let formatted = format!(
            "{}",
            diff_text.unified_diff().context_radius(3).header(
                &format!(
                    "{}/{filename} (old upstream sha={})",
                    entry.name,
                    short_sha(&conflict.old_sha)
                ),
                &format!(
                    "{}/{filename} (new upstream sha={})",
                    entry.name,
                    short_sha(&conflict.new_sha)
                ),
            )
        );
        if !formatted.is_empty() {
            any_diff = true;
            out.write_all(formatted.as_bytes())?;
        }
    }

    if !any_diff {
        println!("No upstream changes detected (patch conflict may be due to local file drift).");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::write(dir.join(MANIFEST_NAME), content).unwrap();
    }

    fn write_lock_file(dir: &Path, content: &str) {
        std::fs::write(dir.join("Skillfile.lock"), content).unwrap();
    }

    fn make_lock_json(name: &str, entity_type: &str) -> String {
        format!(
            r#"{{
  "github/{entity_type}/{name}": {{
    "sha": "abc123def456abcdef",
    "raw_url": "https://raw.githubusercontent.com/owner/repo/abc123/test.md"
  }}
}}"#
        )
    }

    #[test]
    fn diff_no_manifest_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_diff("foo", dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn diff_local_entry_prints_message() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");
        // cmd_diff will print "local entry — nothing to diff"
        // Since it goes to stdout which we can't capture in unit tests easily,
        // just verify it doesn't error
        // The cmd_diff function looks up the entry in the manifest
        // We need the manifest to have the "foo" entry with local source
        let result = cmd_diff("foo", dir.path());
        // "foo" is not in the manifest (it's inferred as "foo" from "skills/foo.md")
        assert!(result.is_ok());
    }

    #[test]
    fn diff_not_locked_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "github  skill  owner/repo  skills/test.md\n");
        write_lock_file(dir.path(), "{}");
        let result = cmd_diff("test", dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not locked"));
    }

    #[test]
    fn diff_not_cached_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  owner/repo  skills/test.md\n",
        );
        write_lock_file(dir.path(), &make_lock_json("test", "skill"));
        // no cache files
        let result = cmd_diff("test", dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not cached"));
    }

    #[test]
    fn diff_not_installed_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  owner/repo  skills/test.md\n",
        );
        write_lock_file(dir.path(), &make_lock_json("test", "skill"));

        // Create cache but not installed
        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("test.md"), "content\n").unwrap();

        let result = cmd_diff("test", dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not installed"));
    }

    #[test]
    fn diff_clean_shows_clean() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  owner/repo  skills/test.md\n",
        );
        write_lock_file(dir.path(), &make_lock_json("test", "skill"));

        let content = "# Test\n\nContent.\n";
        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("test.md"), content).unwrap();

        let installed_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(installed_dir.join("test.md"), content).unwrap();

        // Should succeed (prints "is clean")
        let result = cmd_diff("test", dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn diff_modified_produces_output() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  owner/repo  skills/test.md\n",
        );
        write_lock_file(dir.path(), &make_lock_json("test", "skill"));

        let vdir = dir.path().join(".skillfile/cache/skills/test");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("test.md"), "original\n").unwrap();

        let installed_dir = dir.path().join(".claude/skills");
        std::fs::create_dir_all(&installed_dir).unwrap();
        std::fs::write(installed_dir.join("test.md"), "modified\n").unwrap();

        // Should succeed (diff goes to stdout)
        let result = cmd_diff("test", dir.path());
        assert!(result.is_ok());
    }

    // -----------------------------------------------------------------------
    // Dir entry helpers
    // -----------------------------------------------------------------------

    /// Build a lock JSON string for a dir entry (path_in_repo has no .md suffix).
    fn make_dir_lock_json(name: &str, entity_type: &str) -> String {
        format!(
            r#"{{
  "github/{entity_type}/{name}": {{
    "sha": "abc123def456abcdef",
    "raw_url": "https://api.github.com/repos/owner/repo/contents/skills/{name}?ref=abc123def456abcdef"
  }}
}}"#
        )
    }

    /// Create the vendor cache directory for a dir entry with two files.
    #[allow(clippy::too_many_arguments)]
    fn setup_dir_cache(dir: &Path, name: &str, content1: &str, content2: &str) {
        let vdir = dir.join(format!(".skillfile/cache/skills/{name}"));
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("file1.md"), content1).unwrap();
        std::fs::write(vdir.join("file2.md"), content2).unwrap();
    }

    /// Create the installed dir for a dir entry under .claude/skills/<name>/
    /// (claude-code + local scope + skill entity type → Nested mode).
    #[allow(clippy::too_many_arguments)]
    fn setup_installed_dir(dir: &Path, name: &str, content1: &str, content2: &str) {
        let installed = dir.join(format!(".claude/skills/{name}"));
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("file1.md"), content1).unwrap();
        std::fs::write(installed.join("file2.md"), content2).unwrap();
    }

    // -----------------------------------------------------------------------
    // diff_local_dir — entry name not found in manifest
    // -----------------------------------------------------------------------

    #[test]
    fn diff_entry_name_not_found() {
        let dir = tempfile::tempdir().unwrap();
        // Manifest has "my-dir" but we ask for "nonexistent"
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  my-dir  owner/repo  skills/my-dir  main\n",
        );
        write_lock_file(dir.path(), &make_dir_lock_json("my-dir", "skill"));

        let result = cmd_diff("nonexistent", dir.path());
        assert!(result.is_err());
        // find_entry_in produces an error message containing the name
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("nonexistent"),
            "error should mention the missing entry name: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // diff_local_dir — vendor cache missing → "not cached" error
    // -----------------------------------------------------------------------

    #[test]
    fn diff_dir_entry_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  my-dir  owner/repo  skills/my-dir  main\n",
        );
        write_lock_file(dir.path(), &make_dir_lock_json("my-dir", "skill"));
        // No vendor cache directory created → is_dir_entry is true, vdir.is_dir() is false

        let result = cmd_diff("my-dir", dir.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not cached"),
            "expected 'not cached' in error, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // diff_local_dir — cache exists but no installed files → "not installed" error
    // -----------------------------------------------------------------------

    #[test]
    fn diff_dir_entry_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  my-dir  owner/repo  skills/my-dir  main\n",
        );
        write_lock_file(dir.path(), &make_dir_lock_json("my-dir", "skill"));

        // Vendor cache exists with content
        setup_dir_cache(dir.path(), "my-dir", "content1\n", "content2\n");
        // But no installed dir → installed_dir_files returns empty map

        let result = cmd_diff("my-dir", dir.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not installed"),
            "expected 'not installed' in error, got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // diff_local_dir — cache and installed files have identical content → clean
    // -----------------------------------------------------------------------

    #[test]
    fn diff_dir_entry_clean() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  my-dir  owner/repo  skills/my-dir  main\n",
        );
        write_lock_file(dir.path(), &make_dir_lock_json("my-dir", "skill"));

        let content = "# Skill content\n\nSame in both places.\n";
        setup_dir_cache(dir.path(), "my-dir", content, content);
        setup_installed_dir(dir.path(), "my-dir", content, content);

        // Should succeed: both cache and installed are identical → prints "is clean"
        let result = cmd_diff("my-dir", dir.path());
        assert!(result.is_ok(), "expected Ok but got: {result:?}");
    }

    // -----------------------------------------------------------------------
    // diff_local_dir — installed files differ from cache → produces diff output
    // -----------------------------------------------------------------------

    #[test]
    fn diff_dir_entry_modified() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  my-dir  owner/repo  skills/my-dir  main\n",
        );
        write_lock_file(dir.path(), &make_dir_lock_json("my-dir", "skill"));

        // Cache has original content; installed has modified content for file1
        setup_dir_cache(dir.path(), "my-dir", "original line\n", "unchanged\n");
        setup_installed_dir(dir.path(), "my-dir", "modified line\n", "unchanged\n");

        // Should succeed: diff output is written to stdout (we just verify no error)
        let result = cmd_diff("my-dir", dir.path());
        assert!(result.is_ok(), "expected Ok but got: {result:?}");
    }

    // -----------------------------------------------------------------------
    // cmd_diff dispatching — github dir entry detected via is_dir_entry
    // -----------------------------------------------------------------------

    #[test]
    fn cmd_diff_dispatches_to_dir_path_for_dir_entry() {
        let dir = tempfile::tempdir().unwrap();
        // path_in_repo = "skills/my-dir" (no .md) → is_dir_entry returns true
        write_manifest(
            dir.path(),
            "install  claude-code  local\ngithub  skill  my-dir  owner/repo  skills/my-dir  main\n",
        );
        write_lock_file(dir.path(), &make_dir_lock_json("my-dir", "skill"));

        // Set up cache and installed with matching content so the dir path succeeds
        let content = "# Dir skill\n";
        setup_dir_cache(dir.path(), "my-dir", content, content);
        setup_installed_dir(dir.path(), "my-dir", content, content);

        // cmd_diff must route to diff_local_dir (not diff_local_single)
        // and succeed without error
        let result = cmd_diff("my-dir", dir.path());
        assert!(
            result.is_ok(),
            "expected Ok for dir entry dispatch: {result:?}"
        );
    }
}
