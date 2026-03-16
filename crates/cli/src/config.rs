//! User-global config for personal platform preferences.
//!
//! Stores install targets (platform + scope) in a TOML config file
//! so collaborative repos don't need `install` lines in the committed Skillfile.
//!
//! Config file location (via `dirs::config_dir()`):
//! - Linux: `~/.config/skillfile/config.toml`
//! - macOS: `~/Library/Application Support/skillfile/config.toml`
//!
//! Precedence: Skillfile install targets (if present) > user config > error.
//!
//! # Format
//!
//! ```toml
//! [[install]]
//! platform = "claude-code"
//! scope = "global"
//!
//! [[install]]
//! platform = "cursor"
//! scope = "local"
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use skillfile_core::error::SkillfileError;
use skillfile_core::models::{InstallTarget, Manifest, Scope};
use skillfile_core::parser::parse_manifest;

// ---------------------------------------------------------------------------
// TOML schema
// ---------------------------------------------------------------------------

/// Root structure of `config.toml`.
/// Unknown fields are ignored for forward compatibility.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    /// Platform install targets.
    #[serde(default)]
    install: Vec<InstallEntry>,
}

/// A single `[[install]]` entry in the config file.
#[derive(Debug, Serialize, Deserialize)]
struct InstallEntry {
    platform: String,
    scope: String,
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

impl From<&InstallTarget> for InstallEntry {
    fn from(target: &InstallTarget) -> Self {
        Self {
            platform: target.adapter.clone(),
            scope: target.scope.as_str().to_string(),
        }
    }
}

impl InstallEntry {
    fn to_install_target(&self) -> Option<InstallTarget> {
        let scope = Scope::parse(&self.scope)?;
        Some(InstallTarget {
            adapter: self.platform.clone(),
            scope,
        })
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns the path to the user config file.
///
/// Uses `dirs::config_dir()` for a platform-appropriate location:
/// - Linux: `~/.config/skillfile/config.toml`
/// - macOS: `~/Library/Application Support/skillfile/config.toml`
/// - Windows: `{FOLDERID_RoamingAppData}/skillfile/config.toml`
///
/// Returns `None` if the platform has no config directory.
pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("skillfile").join("config.toml"))
}

/// Read install targets from a TOML config file at the given path.
///
/// Returns an empty `Vec` if the file doesn't exist, can't be parsed,
/// or contains no valid `[[install]]` entries.
pub fn read_user_targets_from(path: &Path) -> Vec<InstallTarget> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(config): Result<Config, _> = toml::from_str(&content) else {
        return Vec::new();
    };
    config
        .install
        .iter()
        .filter_map(InstallEntry::to_install_target)
        .collect()
}

/// Read install targets from the user-global config file.
///
/// Returns an empty `Vec` if no config directory exists, the file is missing,
/// or it contains no valid entries.
pub fn read_user_targets() -> Vec<InstallTarget> {
    match config_path() {
        Some(path) => read_user_targets_from(&path),
        None => Vec::new(),
    }
}

/// Write install targets to the given TOML config file path.
///
/// Creates parent directories if needed. Overwrites any existing content.
pub fn write_user_targets_to(targets: &[InstallTarget], path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let config = Config {
        install: targets.iter().map(InstallEntry::from).collect(),
    };
    let content = toml::to_string_pretty(&config).map_err(std::io::Error::other)?;
    std::fs::write(path, content)
}

/// Write install targets to the user-global config file.
///
/// Returns `Ok(())` if successful, or an error if the config directory
/// cannot be determined or the file cannot be written.
pub fn write_user_targets(targets: &[InstallTarget]) -> Result<(), std::io::Error> {
    match config_path() {
        Some(path) => write_user_targets_to(targets, &path),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "could not determine config directory",
        )),
    }
}

/// If the manifest has no install targets, fill them from the user config.
///
/// This is the central target resolution point for CLI commands. Call after
/// `parse_manifest()` in any command that needs install targets.
pub fn resolve_targets_into(manifest: &mut Manifest) {
    if manifest.install_targets.is_empty() {
        manifest.install_targets = read_user_targets();
    }
}

/// Parse the manifest and resolve install targets from user config.
///
/// Convenience wrapper that combines [`parse_manifest()`] with user-config
/// target fallback. Use in any CLI command that needs a manifest with
/// resolved install targets.
pub fn parse_and_resolve(manifest_path: &Path) -> Result<Manifest, SkillfileError> {
    let result = parse_manifest(manifest_path)?;
    let mut manifest = result.manifest;
    resolve_targets_into(&mut manifest);
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_path_ends_with_expected_components() {
        if let Some(path) = config_path() {
            assert!(path.ends_with("skillfile/config.toml"));
        }
        // On platforms without config_dir, returns None — that's fine.
    }

    #[test]
    fn read_user_targets_from_missing_file_returns_empty() {
        let targets = read_user_targets_from(Path::new("/nonexistent/config.toml"));
        assert!(targets.is_empty());
    }

    #[test]
    fn read_user_targets_from_parses_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[[install]]\nplatform = \"claude-code\"\nscope = \"global\"\n\n\
             [[install]]\nplatform = \"cursor\"\nscope = \"local\"\n",
        )
        .unwrap();

        let targets = read_user_targets_from(&path);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].adapter, "claude-code");
        assert_eq!(targets[0].scope, Scope::Global);
        assert_eq!(targets[1].adapter, "cursor");
        assert_eq!(targets[1].scope, Scope::Local);
    }

    #[test]
    fn read_user_targets_from_ignores_invalid_scope() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[[install]]\nplatform = \"claude-code\"\nscope = \"global\"\n\n\
             [[install]]\nplatform = \"cursor\"\nscope = \"invalid\"\n",
        )
        .unwrap();

        let targets = read_user_targets_from(&path);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].adapter, "claude-code");
    }

    #[test]
    fn read_user_targets_from_empty_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# empty config\n").unwrap();

        let targets = read_user_targets_from(&path);
        assert!(targets.is_empty());
    }

    #[test]
    fn read_user_targets_from_invalid_toml_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is not valid toml {{{").unwrap();

        let targets = read_user_targets_from(&path);
        assert!(targets.is_empty());
    }

    #[test]
    fn write_user_targets_to_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("dir").join("config.toml");

        let targets = vec![InstallTarget {
            adapter: "claude-code".to_string(),
            scope: Scope::Global,
        }];
        write_user_targets_to(&targets, &path).unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[[install]]"));
        assert!(content.contains("platform = \"claude-code\""));
        assert!(content.contains("scope = \"global\""));
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let targets = vec![
            InstallTarget {
                adapter: "claude-code".to_string(),
                scope: Scope::Global,
            },
            InstallTarget {
                adapter: "gemini-cli".to_string(),
                scope: Scope::Local,
            },
        ];
        write_user_targets_to(&targets, &path).unwrap();

        let read_back = read_user_targets_from(&path);
        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back[0].adapter, "claude-code");
        assert_eq!(read_back[0].scope, Scope::Global);
        assert_eq!(read_back[1].adapter, "gemini-cli");
        assert_eq!(read_back[1].scope, Scope::Local);
    }

    #[test]
    fn write_produces_valid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let targets = vec![InstallTarget {
            adapter: "claude-code".to_string(),
            scope: Scope::Global,
        }];
        write_user_targets_to(&targets, &path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        // Verify it's valid TOML by parsing it back
        let parsed: Config = toml::from_str(&content).unwrap();
        assert_eq!(parsed.install.len(), 1);
        assert_eq!(parsed.install[0].platform, "claude-code");
        assert_eq!(parsed.install[0].scope, "global");
    }

    #[test]
    fn resolve_targets_into_prefers_manifest_targets() {
        let mut manifest = Manifest {
            entries: vec![],
            install_targets: vec![InstallTarget {
                adapter: "from-skillfile".to_string(),
                scope: Scope::Global,
            }],
        };

        // Even if user config has targets, manifest targets should win.
        // resolve_targets_into only fills when manifest is empty.
        resolve_targets_into(&mut manifest);
        assert_eq!(manifest.install_targets.len(), 1);
        assert_eq!(manifest.install_targets[0].adapter, "from-skillfile");
    }

    #[test]
    fn read_user_targets_from_with_extra_fields_is_forward_compatible() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Future config might have extra sections — current parser should ignore them.
        std::fs::write(
            &path,
            "[[install]]\nplatform = \"claude-code\"\nscope = \"global\"\n\n\
             [resolve]\ntool = \"vimdiff\"\n",
        )
        .unwrap();

        let targets = read_user_targets_from(&path);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].adapter, "claude-code");
    }
}
