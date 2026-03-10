use std::collections::BTreeMap;
use std::path::Path;

use crate::error::SkillfileError;
use crate::models::{Entry, LockEntry};

pub const LOCK_NAME: &str = "Skillfile.lock";

/// Generate the lock file key for an entry: `"{source_type}/{entity_type}/{name}"`.
///
/// ```
/// use skillfile_core::models::*;
/// use skillfile_core::lock::lock_key;
///
/// let entry = Entry {
///     entity_type: EntityType::Agent,
///     name: "code-refactorer".into(),
///     source: SourceFields::Github {
///         owner_repo: "owner/repo".into(),
///         path_in_repo: "agents/code-refactorer.md".into(),
///         ref_: "main".into(),
///     },
/// };
/// assert_eq!(lock_key(&entry), "github/agent/code-refactorer");
/// ```
#[must_use]
pub fn lock_key(entry: &Entry) -> String {
    format!(
        "{}/{}/{}",
        entry.source_type(),
        entry.entity_type,
        entry.name
    )
}

/// Read lock entries from `Skillfile.lock`. Returns empty map if file is missing.
pub fn read_lock(repo_root: &Path) -> Result<BTreeMap<String, LockEntry>, SkillfileError> {
    let lock_path = repo_root.join(LOCK_NAME);
    if !lock_path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = std::fs::read_to_string(&lock_path)?;
    let data: BTreeMap<String, LockEntry> = serde_json::from_str(&text)
        .map_err(|e| SkillfileError::Manifest(format!("invalid lock file: {e}")))?;
    Ok(data)
}

/// Write lock entries to `Skillfile.lock` with sorted keys, 2-space indent, trailing newline.
pub fn write_lock(
    repo_root: &Path,
    locked: &BTreeMap<String, LockEntry>,
) -> Result<(), SkillfileError> {
    let lock_path = repo_root.join(LOCK_NAME);
    // BTreeMap iterates in sorted order, matching Python's sort_keys=True
    let json = serde_json::to_string_pretty(locked)
        .map_err(|e| SkillfileError::Manifest(format!("failed to serialize lock: {e}")))?;
    std::fs::write(&lock_path, format!("{json}\n"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_github_entry(name: &str) -> Entry {
        use crate::models::{EntityType, SourceFields};
        Entry {
            entity_type: EntityType::Agent,
            name: name.into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "agent.md".into(),
                ref_: "main".into(),
            },
        }
    }

    #[test]
    fn lock_key_format() {
        let e = make_github_entry("my-agent");
        assert_eq!(lock_key(&e), "github/agent/my-agent");
    }

    #[test]
    fn write_lock_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let mut locked = BTreeMap::new();
        locked.insert(
            "github/agent/test".to_string(),
            LockEntry {
                sha: "abc123".into(),
                raw_url: "https://example.com/file.md".into(),
            },
        );
        write_lock(dir.path(), &locked).unwrap();
        let content = std::fs::read_to_string(dir.path().join(LOCK_NAME)).unwrap();
        let data: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(data["github/agent/test"]["sha"], "abc123");
        assert_eq!(
            data["github/agent/test"]["raw_url"],
            "https://example.com/file.md"
        );
    }

    #[test]
    fn read_lock_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_lock(dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut locked = BTreeMap::new();
        locked.insert(
            "github/agent/foo".to_string(),
            LockEntry {
                sha: "deadbeef".into(),
                raw_url: "https://example.com/foo.md".into(),
            },
        );
        locked.insert(
            "github/skill/bar".to_string(),
            LockEntry {
                sha: "cafebabe".into(),
                raw_url: "https://example.com/bar.md".into(),
            },
        );
        write_lock(dir.path(), &locked).unwrap();
        let result = read_lock(dir.path()).unwrap();
        assert_eq!(result, locked);
    }

    #[test]
    fn write_lock_sorted_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut locked = BTreeMap::new();
        locked.insert(
            "github/skill/zebra".to_string(),
            LockEntry {
                sha: "aaa".into(),
                raw_url: "https://example.com/z.md".into(),
            },
        );
        locked.insert(
            "github/agent/alpha".to_string(),
            LockEntry {
                sha: "bbb".into(),
                raw_url: "https://example.com/a.md".into(),
            },
        );
        write_lock(dir.path(), &locked).unwrap();
        let content = std::fs::read_to_string(dir.path().join(LOCK_NAME)).unwrap();
        let alpha_pos = content.find("alpha").unwrap();
        let zebra_pos = content.find("zebra").unwrap();
        assert!(alpha_pos < zebra_pos);
    }
}
