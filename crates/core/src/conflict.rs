use std::path::Path;

use crate::error::SkillfileError;
use crate::models::ConflictState;

pub const CONFLICT_FILE: &str = ".skillfile/conflict";

/// Read conflict state from `.skillfile/conflict`. Returns `None` if no conflict file exists.
pub fn read_conflict(repo_root: &Path) -> Result<Option<ConflictState>, SkillfileError> {
    let p = repo_root.join(CONFLICT_FILE);
    if !p.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&p)?;
    let state: ConflictState = serde_json::from_str(&text)
        .map_err(|e| SkillfileError::Manifest(format!("invalid conflict file: {e}")))?;
    Ok(Some(state))
}

/// Write conflict state to `.skillfile/conflict`.
pub fn write_conflict(repo_root: &Path, state: &ConflictState) -> Result<(), SkillfileError> {
    let p = repo_root.join(CONFLICT_FILE);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| SkillfileError::Manifest(format!("failed to serialize conflict: {e}")))?;
    std::fs::write(&p, format!("{json}\n"))?;
    Ok(())
}

/// Remove the conflict file. No-op if it doesn't exist.
pub fn clear_conflict(repo_root: &Path) -> Result<(), SkillfileError> {
    let p = repo_root.join(CONFLICT_FILE);
    if p.exists() {
        std::fs::remove_file(&p)?;
    }
    Ok(())
}

/// Check whether a conflict file exists.
#[must_use]
pub fn has_conflict(repo_root: &Path) -> bool {
    repo_root.join(CONFLICT_FILE).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::EntityType;

    fn make_state() -> ConflictState {
        ConflictState {
            entry: "foo".into(),
            entity_type: EntityType::Agent,
            old_sha: "a".repeat(40),
            new_sha: "b".repeat(40),
        }
    }

    // -------------------------------------------------------------------
    // read_conflict
    // -------------------------------------------------------------------

    #[test]
    fn read_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_conflict(dir.path()).unwrap().is_none());
    }

    #[test]
    fn write_then_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state();
        write_conflict(dir.path(), &state).unwrap();
        assert_eq!(read_conflict(dir.path()).unwrap(), Some(state));
    }

    // -------------------------------------------------------------------
    // write_conflict
    // -------------------------------------------------------------------

    #[test]
    fn write_produces_valid_json_structure() {
        let dir = tempfile::tempdir().unwrap();
        let state = ConflictState {
            entry: "bar".into(),
            entity_type: EntityType::Skill,
            ..make_state()
        };
        write_conflict(dir.path(), &state).unwrap();
        let data: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(CONFLICT_FILE)).unwrap())
                .unwrap();
        assert_eq!(data["entry"], "bar");
        assert_eq!(data["entity_type"], "skill");
        assert_eq!(data["old_sha"], "a".repeat(40));
        assert_eq!(data["new_sha"], "b".repeat(40));
    }

    #[test]
    fn write_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        write_conflict(dir.path(), &make_state()).unwrap();
        assert!(dir.path().join(CONFLICT_FILE).exists());
    }

    // -------------------------------------------------------------------
    // has_conflict
    // -------------------------------------------------------------------

    #[test]
    fn has_conflict_false_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_conflict(dir.path()));
    }

    #[test]
    fn has_conflict_true_after_write() {
        let dir = tempfile::tempdir().unwrap();
        write_conflict(dir.path(), &make_state()).unwrap();
        assert!(has_conflict(dir.path()));
    }

    // -------------------------------------------------------------------
    // clear_conflict
    // -------------------------------------------------------------------

    #[test]
    fn clear_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        write_conflict(dir.path(), &make_state()).unwrap();
        clear_conflict(dir.path()).unwrap();
        assert!(!has_conflict(dir.path()));
        assert!(!dir.path().join(CONFLICT_FILE).exists());
    }

    #[test]
    fn clear_noop_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        clear_conflict(dir.path()).unwrap(); // must not panic
    }
}
