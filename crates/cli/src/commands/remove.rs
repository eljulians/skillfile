use std::path::Path;

use skillfile_core::error::SkillfileError;
use skillfile_core::lock::{lock_key, read_lock, write_lock};
use skillfile_core::parser::{find_entry_in, parse_manifest, MANIFEST_NAME};
use skillfile_sources::sync::vendor_dir_for;

fn name_from_line(line: &str) -> Option<String> {
    let result = skillfile_core::parser::parse_manifest_line(line);
    result.map(|entry| entry.name)
}

pub fn cmd_remove(name: &str, repo_root: &Path) -> Result<(), SkillfileError> {
    let manifest_path = repo_root.join(MANIFEST_NAME);
    if !manifest_path.exists() {
        return Err(SkillfileError::Manifest(format!(
            "{MANIFEST_NAME} not found in {}. Create one and run `skillfile init`.",
            repo_root.display()
        )));
    }

    let result = parse_manifest(&manifest_path)?;
    let manifest = result.manifest;
    let entry = find_entry_in(name, &manifest)?;

    // Remove the matching line from Skillfile.
    let raw = std::fs::read_to_string(&manifest_path)?;
    let mut new_lines: Vec<&str> = Vec::new();
    let mut removed = false;
    for line in raw.lines() {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            new_lines.push(line);
            continue;
        }
        if !removed && name_from_line(stripped).as_deref() == Some(name) {
            removed = true;
            continue;
        }
        new_lines.push(line);
    }
    // Preserve trailing newline
    let mut output = new_lines.join("\n");
    if raw.ends_with('\n') {
        output.push('\n');
    }
    std::fs::write(&manifest_path, output)?;

    // Remove from lock.
    let mut locked = read_lock(repo_root)?;
    let key = lock_key(entry);
    if locked.remove(&key).is_some() {
        write_lock(repo_root, &locked)?;
    }

    // Remove cache directory.
    let vdir = vendor_dir_for(entry, repo_root);
    if vdir.exists() {
        std::fs::remove_dir_all(&vdir)?;
        println!("Removed cache: {}", vdir.display());
    }

    println!("Removed: {name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::write(dir.join(MANIFEST_NAME), content).unwrap();
    }

    fn write_lock_file(dir: &Path, data: &serde_json::Value) {
        std::fs::write(
            dir.join("Skillfile.lock"),
            serde_json::to_string_pretty(data).unwrap(),
        )
        .unwrap();
    }

    fn write_cache(dir: &Path, entity_type: &str, name: &str) -> std::path::PathBuf {
        let vdir = dir
            .join(".skillfile/cache")
            .join(format!("{entity_type}s"))
            .join(name);
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("agent.md"), "# content").unwrap();
        vdir
    }

    #[test]
    fn no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_remove("foo", dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn unknown_name_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");
        let result = cmd_remove("nonexistent", dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no entry named"));
    }

    #[test]
    fn remove_github_entry_removes_line() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "github  agent  owner/repo  agents/my-agent.md\nlocal  skill  skills/foo.md\n",
        );
        cmd_remove("my-agent", dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(!text.contains("my-agent"));
        assert!(text.contains("skills/foo.md"));
    }

    #[test]
    fn remove_local_entry_removes_line() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");
        cmd_remove("foo", dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(!text.contains("foo"));
    }

    #[test]
    fn remove_preserves_comments_and_blanks() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "# My agents\n\nlocal  skill  skills/foo.md\n");
        cmd_remove("foo", dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("# My agents"));
    }

    #[test]
    fn remove_clears_cache() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "github  agent  owner/repo  agents/my-agent.md\n",
        );
        let vdir = write_cache(dir.path(), "agent", "my-agent");
        cmd_remove("my-agent", dir.path()).unwrap();
        assert!(!vdir.exists());
    }

    #[test]
    fn remove_local_entry_no_cache_no_error() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");
        cmd_remove("foo", dir.path()).unwrap();
        assert!(!dir.path().join(".skillfile/cache").exists());
    }

    #[test]
    fn remove_updates_lock() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "github  agent  owner/repo  agents/my-agent.md\n",
        );
        write_lock_file(
            dir.path(),
            &serde_json::json!({
                "github/agent/my-agent": {"sha": "abc123", "raw_url": "https://example.com"},
                "github/agent/other": {"sha": "def456", "raw_url": "https://example.com/other"}
            }),
        );
        cmd_remove("my-agent", dir.path()).unwrap();
        let lock_text = std::fs::read_to_string(dir.path().join("Skillfile.lock")).unwrap();
        let lock: serde_json::Value = serde_json::from_str(&lock_text).unwrap();
        assert!(lock.get("github/agent/my-agent").is_none());
        assert!(lock.get("github/agent/other").is_some());
    }

    #[test]
    fn remove_no_lock_entry_no_error() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "github  agent  owner/repo  agents/my-agent.md\n",
        );
        cmd_remove("my-agent", dir.path()).unwrap();
    }
}
