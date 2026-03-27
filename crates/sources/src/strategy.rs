use std::path::Path;

use skillfile_core::models::{Entry, SourceFields, DEFAULT_REF};
use skillfile_core::parser::infer_name;

/// Known source types.
pub const KNOWN_SOURCES: &[&str] = &["github", "local", "url"];

/// Return the expected filename in the vendor cache directory.
/// Empty string for directory entries and local entries.
#[must_use]
pub fn content_file(entry: &Entry) -> String {
    match &entry.source {
        SourceFields::Github { path_in_repo, .. } => github_content_file(entry, path_in_repo),
        SourceFields::Local { .. } => String::new(),
        SourceFields::Url { url } => url_content_file(url),
    }
}

fn github_content_file(entry: &Entry, path_in_repo: &str) -> String {
    if is_dir_entry(entry) {
        return String::new();
    }
    let effective = if path_in_repo == "." {
        "SKILL.md"
    } else {
        path_in_repo
    };
    Path::new(effective)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("")
        .to_string()
}

fn url_content_file(url: &str) -> String {
    let name = Path::new(url)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");
    if name.is_empty() {
        "content.md".to_string()
    } else {
        name.to_string()
    }
}

#[must_use]
pub fn is_dir_entry(entry: &Entry) -> bool {
    match &entry.source {
        SourceFields::Github { path_in_repo, .. } => {
            path_in_repo != "."
                && !Path::new(path_in_repo)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        }
        _ => false,
    }
}

/// Return source-type-specific Skillfile fields (after source_type and entity_type).
/// Used by `add` and `sort` commands.
#[must_use]
pub fn format_parts(entry: &Entry) -> Vec<String> {
    match &entry.source {
        SourceFields::Github {
            owner_repo,
            path_in_repo,
            ref_,
        } => {
            let mut parts = Vec::new();
            if entry.name != infer_name(path_in_repo) {
                parts.push(entry.name.clone());
            }
            parts.push(owner_repo.clone());
            parts.push(path_in_repo.clone());
            if ref_ != DEFAULT_REF {
                parts.push(ref_.clone());
            }
            parts
        }
        SourceFields::Local { path } => {
            let mut parts = Vec::new();
            if entry.name != infer_name(path) {
                parts.push(entry.name.clone());
            }
            parts.push(path.clone());
            parts
        }
        SourceFields::Url { url } => {
            let mut parts = Vec::new();
            if entry.name != infer_name(url) {
                parts.push(entry.name.clone());
            }
            parts.push(url.clone());
            parts
        }
    }
}

#[must_use]
pub fn meta_sha(vdir: &Path) -> Option<String> {
    let meta_path = vdir.join(".meta");
    let text = std::fs::read_to_string(&meta_path).ok()?;
    let data: serde_json::Value = serde_json::from_str(&text).ok()?;
    data["sha"].as_str().map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use skillfile_core::models::{EntityType, SourceFields};

    fn github_entry(path_in_repo: &str) -> Entry {
        Entry {
            entity_type: EntityType::Skill,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: path_in_repo.into(),
                ref_: "main".into(),
            },
        }
    }

    #[test]
    fn content_file_single_file() {
        let e = github_entry("skills/my-skill.md");
        assert_eq!(content_file(&e), "my-skill.md");
    }

    #[test]
    fn content_file_dot_path() {
        let e = github_entry(".");
        assert_eq!(content_file(&e), "SKILL.md");
    }

    #[test]
    fn content_file_dir_entry() {
        let e = github_entry("skills/python-pro");
        assert_eq!(content_file(&e), "");
    }

    #[test]
    fn content_file_local() {
        let e = Entry {
            entity_type: EntityType::Skill,
            name: "test".into(),
            source: SourceFields::Local {
                path: "skills/test.md".into(),
            },
        };
        assert_eq!(content_file(&e), "");
    }

    #[test]
    fn content_file_url() {
        let e = Entry {
            entity_type: EntityType::Skill,
            name: "test".into(),
            source: SourceFields::Url {
                url: "https://example.com/skill.md".into(),
            },
        };
        assert_eq!(content_file(&e), "skill.md");
    }

    #[test]
    fn is_dir_entry_md_file() {
        assert!(!is_dir_entry(&github_entry("skills/foo.md")));
    }

    #[test]
    fn is_dir_entry_dot_path() {
        assert!(!is_dir_entry(&github_entry(".")));
    }

    #[test]
    fn is_dir_entry_directory() {
        assert!(is_dir_entry(&github_entry("skills/python-pro")));
    }

    #[test]
    fn is_dir_entry_local() {
        let e = Entry {
            entity_type: EntityType::Skill,
            name: "test".into(),
            source: SourceFields::Local {
                path: "skills/test".into(),
            },
        };
        assert!(!is_dir_entry(&e));
    }

    #[test]
    fn format_parts_github_inferred_name() {
        let e = Entry {
            entity_type: EntityType::Agent,
            name: "agent".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "path/to/agent.md".into(),
                ref_: "main".into(),
            },
        };
        // name matches infer_name("path/to/agent.md") = "agent", ref is default
        assert_eq!(format_parts(&e), vec!["owner/repo", "path/to/agent.md"]);
    }

    #[test]
    fn format_parts_github_explicit_name_and_ref() {
        let e = Entry {
            entity_type: EntityType::Agent,
            name: "my-agent".into(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "path/to/agent.md".into(),
                ref_: "v1.0".into(),
            },
        };
        assert_eq!(
            format_parts(&e),
            vec!["my-agent", "owner/repo", "path/to/agent.md", "v1.0"]
        );
    }

    #[test]
    fn format_parts_local_inferred_name() {
        let e = Entry {
            entity_type: EntityType::Skill,
            name: "commit".into(),
            source: SourceFields::Local {
                path: "skills/git/commit.md".into(),
            },
        };
        assert_eq!(format_parts(&e), vec!["skills/git/commit.md"]);
    }

    #[test]
    fn format_parts_local_explicit_name() {
        let e = Entry {
            entity_type: EntityType::Skill,
            name: "git-commit".into(),
            source: SourceFields::Local {
                path: "skills/git/commit.md".into(),
            },
        };
        assert_eq!(format_parts(&e), vec!["git-commit", "skills/git/commit.md"]);
    }

    #[test]
    fn meta_sha_reads_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let meta = serde_json::json!({"sha": "abc123", "source_type": "github"});
        std::fs::write(
            dir.path().join(".meta"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
        assert_eq!(meta_sha(dir.path()), Some("abc123".to_string()));
    }

    #[test]
    fn meta_sha_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(meta_sha(dir.path()), None);
    }
}
