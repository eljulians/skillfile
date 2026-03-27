use std::fmt;

use serde::{Deserialize, Serialize};

pub const DEFAULT_REF: &str = "main";

// ---------------------------------------------------------------------------
// Scope — typed replacement for bare "global"/"local" strings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Global,
    Local,
}

impl Scope {
    pub const ALL: &[Scope] = &[Scope::Global, Scope::Local];

    /// Parse a scope string. Returns `None` for unrecognised values.
    ///
    /// ```
    /// use skillfile_core::models::Scope;
    /// assert_eq!(Scope::parse("global"), Some(Scope::Global));
    /// assert_eq!(Scope::parse("local"), Some(Scope::Local));
    /// assert_eq!(Scope::parse("invalid"), None);
    /// ```
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "global" => Some(Scope::Global),
            "local" => Some(Scope::Local),
            _ => None,
        }
    }

    /// The canonical string representation (used in Skillfile format).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Local => "local",
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// EntityType — typed replacement for bare "skill"/"agent" strings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityType {
    Skill,
    Agent,
}

impl EntityType {
    pub const ALL: &[EntityType] = &[EntityType::Agent, EntityType::Skill];

    /// Parse an entity type string. Returns `None` for unrecognised values.
    ///
    /// ```
    /// use skillfile_core::models::EntityType;
    /// assert_eq!(EntityType::parse("skill"), Some(EntityType::Skill));
    /// assert_eq!(EntityType::parse("agent"), Some(EntityType::Agent));
    /// assert_eq!(EntityType::parse("rule"), None);
    /// ```
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "skill" => Some(EntityType::Skill),
            "agent" => Some(EntityType::Agent),
            _ => None,
        }
    }

    /// The canonical string representation (used in Skillfile format).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            EntityType::Skill => "skill",
            EntityType::Agent => "agent",
        }
    }

    /// Pluralized directory name (e.g. "skills", "agents").
    #[must_use]
    pub fn dir_name(&self) -> &'static str {
        match self {
            EntityType::Skill => "skills",
            EntityType::Agent => "agents",
        }
    }
}

impl fmt::Display for EntityType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// short_sha — truncate SHA for display
// ---------------------------------------------------------------------------

/// Return the first 12 characters of a SHA (or the full string if shorter).
///
/// ```
/// assert_eq!(skillfile_core::models::short_sha("abcdef1234567890"), "abcdef123456");
/// assert_eq!(skillfile_core::models::short_sha("short"), "short");
/// ```
#[must_use]
pub fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

// ---------------------------------------------------------------------------
// SourceFields — making illegal states unrepresentable
// ---------------------------------------------------------------------------

/// Source-specific fields, making illegal states unrepresentable.
///
/// Instead of optional fields on Entry (Python's approach), we use an enum
/// so each variant carries exactly the fields it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceFields {
    Github {
        owner_repo: String,
        path_in_repo: String,
        ref_: String,
    },
    Local {
        path: String,
    },
    Url {
        url: String,
    },
}

impl SourceFields {
    /// The source type identifier used in the Skillfile format and lock keys.
    #[must_use]
    pub fn source_type(&self) -> &str {
        match self {
            SourceFields::Github { .. } => "github",
            SourceFields::Local { .. } => "local",
            SourceFields::Url { .. } => "url",
        }
    }

    #[must_use]
    pub fn as_github(&self) -> Option<(&str, &str, &str)> {
        match self {
            SourceFields::Github {
                owner_repo,
                path_in_repo,
                ref_,
            } => Some((owner_repo, path_in_repo, ref_)),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_local(&self) -> Option<&str> {
        match self {
            SourceFields::Local { path } => Some(path),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_url(&self) -> Option<&str> {
        match self {
            SourceFields::Url { url } => Some(url),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Entry — a single manifest entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub entity_type: EntityType,
    pub name: String,
    pub source: SourceFields,
}

impl Entry {
    #[must_use]
    pub fn source_type(&self) -> &str {
        self.source.source_type()
    }

    // Legacy convenience accessors — only used in tests.
    // Prefer matching on `entry.source` directly.

    #[cfg(test)]
    pub fn owner_repo(&self) -> &str {
        self.source.as_github().map_or("", |(or, _, _)| or)
    }

    #[cfg(test)]
    pub fn path_in_repo(&self) -> &str {
        self.source.as_github().map_or("", |(_, pir, _)| pir)
    }

    #[cfg(test)]
    pub fn ref_(&self) -> &str {
        self.source.as_github().map_or("", |(_, _, r)| r)
    }

    #[cfg(test)]
    pub fn local_path(&self) -> &str {
        self.source.as_local().unwrap_or("")
    }

    #[cfg(test)]
    pub fn url(&self) -> &str {
        self.source.as_url().unwrap_or("")
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}/{}",
            self.source_type(),
            self.entity_type,
            self.name
        )
    }
}

// ---------------------------------------------------------------------------
// InstallTarget
// ---------------------------------------------------------------------------

/// An install target line from the Skillfile: `install <adapter> <scope>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallTarget {
    pub adapter: String,
    pub scope: Scope,
}

impl fmt::Display for InstallTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.adapter, self.scope)
    }
}

// ---------------------------------------------------------------------------
// LockEntry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    pub sha: String,
    pub raw_url: String,
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub entries: Vec<Entry>,
    pub install_targets: Vec<InstallTarget>,
}

// ---------------------------------------------------------------------------
// InstallOptions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub dry_run: bool,
    pub overwrite: bool,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            overwrite: true,
        }
    }
}

// ---------------------------------------------------------------------------
// ConflictState
// ---------------------------------------------------------------------------

/// Conflict state persisted in `.skillfile/conflict`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictState {
    pub entry: String,
    pub entity_type: EntityType,
    pub old_sha: String,
    pub new_sha: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_parse_and_display() {
        assert_eq!(Scope::parse("global"), Some(Scope::Global));
        assert_eq!(Scope::parse("local"), Some(Scope::Local));
        assert_eq!(Scope::parse("worldwide"), None);
        assert_eq!(Scope::Global.to_string(), "global");
        assert_eq!(Scope::Local.to_string(), "local");
        assert_eq!(Scope::Global.as_str(), "global");
    }

    #[test]
    fn scope_all_variants() {
        assert_eq!(Scope::ALL.len(), 2);
        assert!(Scope::ALL.contains(&Scope::Global));
        assert!(Scope::ALL.contains(&Scope::Local));
    }

    #[test]
    fn entity_type_parse_and_display() {
        assert_eq!(EntityType::parse("skill"), Some(EntityType::Skill));
        assert_eq!(EntityType::parse("agent"), Some(EntityType::Agent));
        assert_eq!(EntityType::parse("hook"), None);
        assert_eq!(EntityType::Skill.to_string(), "skill");
        assert_eq!(EntityType::Agent.to_string(), "agent");
        assert_eq!(EntityType::Skill.as_str(), "skill");
        assert_eq!(EntityType::Agent.as_str(), "agent");
    }

    #[test]
    fn entity_type_dir_name() {
        assert_eq!(EntityType::Skill.dir_name(), "skills");
        assert_eq!(EntityType::Agent.dir_name(), "agents");
    }

    #[test]
    fn entity_type_all_variants() {
        assert_eq!(EntityType::ALL.len(), 2);
        assert!(EntityType::ALL.contains(&EntityType::Skill));
        assert!(EntityType::ALL.contains(&EntityType::Agent));
    }

    #[test]
    fn short_sha_truncates() {
        let sha = "abcdef123456789012345678";
        assert_eq!(short_sha(sha), "abcdef123456");
    }

    #[test]
    fn short_sha_short_input() {
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn source_fields_typed_accessors() {
        let gh = SourceFields::Github {
            owner_repo: "o/r".into(),
            path_in_repo: "a.md".into(),
            ref_: "main".into(),
        };
        assert_eq!(gh.as_github(), Some(("o/r", "a.md", "main")));
        assert_eq!(gh.as_local(), None);
        assert_eq!(gh.as_url(), None);

        let local = SourceFields::Local {
            path: "test.md".into(),
        };
        assert_eq!(local.as_local(), Some("test.md"));
        assert_eq!(local.as_github(), None);

        let url = SourceFields::Url {
            url: "https://x.com/s.md".into(),
        };
        assert_eq!(url.as_url(), Some("https://x.com/s.md"));
        assert_eq!(url.as_github(), None);
    }

    #[test]
    fn github_entry_source_type() {
        let e = Entry {
            entity_type: EntityType::Agent,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "a.md".into(),
                ref_: "main".into(),
            },
        };
        assert_eq!(e.source_type(), "github");
        assert_eq!(e.entity_type, EntityType::Agent);
        assert_eq!(e.name, "test");
        assert_eq!(e.owner_repo(), "o/r");
        assert_eq!(e.path_in_repo(), "a.md");
        assert_eq!(e.ref_(), "main");
        assert_eq!(e.local_path(), "");
        assert_eq!(e.url(), "");
    }

    #[test]
    fn github_entry_fields() {
        let e = Entry {
            entity_type: EntityType::Skill,
            name: "my-skill".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "skills/s.md".into(),
                ref_: "v1".into(),
            },
        };
        assert_eq!(e.owner_repo(), "o/r");
        assert_eq!(e.path_in_repo(), "skills/s.md");
        assert_eq!(e.ref_(), "v1");
    }

    #[test]
    fn local_entry_fields() {
        let e = Entry {
            entity_type: EntityType::Skill,
            name: "test".into(),
            source: SourceFields::Local {
                path: "test.md".into(),
            },
        };
        assert_eq!(e.source_type(), "local");
        assert_eq!(e.local_path(), "test.md");
        assert_eq!(e.owner_repo(), "");
        assert_eq!(e.url(), "");
    }

    #[test]
    fn url_entry_fields() {
        let e = Entry {
            entity_type: EntityType::Skill,
            name: "my-skill".into(),
            source: SourceFields::Url {
                url: "https://example.com/skill.md".into(),
            },
        };
        assert_eq!(e.source_type(), "url");
        assert_eq!(e.url(), "https://example.com/skill.md");
        assert_eq!(e.owner_repo(), "");
    }

    #[test]
    fn entry_display() {
        let e = Entry {
            entity_type: EntityType::Agent,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "a.md".into(),
                ref_: "main".into(),
            },
        };
        assert_eq!(e.to_string(), "github/agent/test");
    }

    #[test]
    fn lock_entry() {
        let le = LockEntry {
            sha: "abc123".into(),
            raw_url: "https://example.com".into(),
        };
        assert_eq!(le.sha, "abc123");
        assert_eq!(le.raw_url, "https://example.com");
    }

    #[test]
    fn install_target_with_scope_enum() {
        let t = InstallTarget {
            adapter: "claude-code".into(),
            scope: Scope::Global,
        };
        assert_eq!(t.adapter, "claude-code");
        assert_eq!(t.scope, Scope::Global);
        assert_eq!(t.to_string(), "claude-code (global)");
    }

    #[test]
    fn manifest_defaults() {
        let m = Manifest::default();
        assert!(m.entries.is_empty());
        assert!(m.install_targets.is_empty());
    }

    #[test]
    fn manifest_with_entries() {
        let e = Entry {
            entity_type: EntityType::Skill,
            name: "test".into(),
            source: SourceFields::Local {
                path: "test.md".into(),
            },
        };
        let t = InstallTarget {
            adapter: "claude-code".into(),
            scope: Scope::Local,
        };
        let m = Manifest {
            entries: vec![e],
            install_targets: vec![t],
        };
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.install_targets.len(), 1);
    }

    #[test]
    fn conflict_state_equality() {
        let a = ConflictState {
            entry: "foo".into(),
            entity_type: EntityType::Agent,
            old_sha: "aaa".into(),
            new_sha: "bbb".into(),
        };
        let b = a.clone();
        assert_eq!(a, b);
    }
}
