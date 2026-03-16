use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use skillfile_core::models::{Entry, InstallOptions, Scope};
use skillfile_core::patch::walkdir;
use skillfile_core::progress;
use skillfile_sources::strategy::is_dir_entry;

// ---------------------------------------------------------------------------
// PlatformAdapter trait — the core abstraction for tool-specific deployment
// ---------------------------------------------------------------------------

/// How a directory entry is deployed to a platform's target directory.
///
/// - `Flat`: each `.md` placed individually in `target_dir/` (e.g. claude-code agents)
/// - `Nested`: directory placed as `target_dir/<name>/` (e.g. all skill adapters)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirInstallMode {
    Flat,
    Nested,
}

/// The deployment result: a map of `{patch_key: installed_path}`.
///
/// Keys match the relative paths used in `.skillfile/patches/` so patch lookups
/// work correctly:
/// - Single-file entries: key is `"{name}.md"`
/// - Directory entries: keys are paths relative to the source directory
pub type DeployResult = HashMap<String, PathBuf>;

/// Contract for deploying skill/agent files to a specific AI tool's directory.
///
/// Each AI tool (Claude Code, Gemini CLI, Codex, etc.) has its own convention
/// for where skills and agents live on disk. A `PlatformAdapter` encapsulates
/// that knowledge.
///
/// The trait is object-safe so adapters can be stored in a heterogeneous registry.
pub trait PlatformAdapter: Send + Sync + fmt::Debug {
    /// The adapter identifier (e.g. `"claude-code"`, `"gemini-cli"`).
    fn name(&self) -> &str;

    /// Whether this platform supports the given entity type (e.g. `"skill"`, `"agent"`).
    fn supports(&self, entity_type: &str) -> bool;

    /// Resolve the absolute target directory for an entity type + scope.
    #[allow(clippy::too_many_arguments)]
    fn target_dir(&self, entity_type: &str, scope: Scope, repo_root: &Path) -> PathBuf;

    /// The install mode for directory entries of this entity type.
    fn dir_mode(&self, entity_type: &str) -> Option<DirInstallMode>;

    /// Deploy a single entry from `source` to its platform-specific location.
    ///
    /// Returns `{patch_key: installed_path}` for every file that was placed.
    /// Returns an empty map for dry-run or when deployment is skipped.
    #[allow(clippy::too_many_arguments)]
    fn deploy_entry(
        &self,
        entry: &Entry,
        source: &Path,
        scope: Scope,
        repo_root: &Path,
        opts: &InstallOptions,
    ) -> DeployResult;

    /// The installed path for a single-file entry.
    #[allow(clippy::too_many_arguments)]
    fn installed_path(&self, entry: &Entry, scope: Scope, repo_root: &Path) -> PathBuf;

    /// Map of `{relative_path: absolute_path}` for all installed files of a directory entry.
    #[allow(clippy::too_many_arguments)]
    fn installed_dir_files(
        &self,
        entry: &Entry,
        scope: Scope,
        repo_root: &Path,
    ) -> HashMap<String, PathBuf>;
}

// ---------------------------------------------------------------------------
// EntityConfig — per-entity-type path configuration
// ---------------------------------------------------------------------------

/// Paths and install mode for one entity type within a platform.
#[derive(Debug, Clone)]
pub struct EntityConfig {
    pub global_path: String,
    pub local_path: String,
    pub dir_mode: DirInstallMode,
}

// ---------------------------------------------------------------------------
// FileSystemAdapter — the concrete implementation of PlatformAdapter
// ---------------------------------------------------------------------------

/// Filesystem-based platform adapter.
///
/// Each instance is configured with a name and a map of `EntityConfig`s.
/// All three built-in adapters (claude-code, gemini-cli, codex) are instances
/// of this struct with different configurations — the `PlatformAdapter` trait
/// allows alternative implementations if needed.
#[derive(Debug, Clone)]
pub struct FileSystemAdapter {
    name: String,
    entities: HashMap<String, EntityConfig>,
}

impl FileSystemAdapter {
    pub fn new(name: &str, entities: HashMap<String, EntityConfig>) -> Self {
        Self {
            name: name.to_string(),
            entities,
        }
    }
}

impl PlatformAdapter for FileSystemAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports(&self, entity_type: &str) -> bool {
        self.entities.contains_key(entity_type)
    }

    fn target_dir(&self, entity_type: &str, scope: Scope, repo_root: &Path) -> PathBuf {
        let config = self.entities.get(entity_type).unwrap_or_else(|| {
            panic!(
                "BUG: target_dir called for unsupported entity type '{entity_type}' on adapter '{}'. \
                 Call supports() first.",
                self.name
            )
        });
        let raw = match scope {
            Scope::Global => &config.global_path,
            Scope::Local => &config.local_path,
        };
        if raw.starts_with('~') {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
            home.join(raw.strip_prefix("~/").unwrap_or(raw))
        } else {
            repo_root.join(raw)
        }
    }

    fn dir_mode(&self, entity_type: &str) -> Option<DirInstallMode> {
        self.entities.get(entity_type).map(|c| c.dir_mode)
    }

    fn deploy_entry(
        &self,
        entry: &Entry,
        source: &Path,
        scope: Scope,
        repo_root: &Path,
        opts: &InstallOptions,
    ) -> DeployResult {
        let target_dir = self.target_dir(entry.entity_type.as_str(), scope, repo_root);
        // Use filesystem truth: source.is_dir() catches local directory entries
        // that is_dir_entry() misses (it only inspects GitHub path_in_repo).
        let is_dir = is_dir_entry(entry) || source.is_dir();

        if is_dir
            && self
                .entities
                .get(entry.entity_type.as_str())
                .is_some_and(|c| c.dir_mode == DirInstallMode::Flat)
        {
            return deploy_flat(source, &target_dir, opts);
        }

        let dest = if is_dir {
            target_dir.join(&entry.name)
        } else {
            target_dir.join(format!("{}.md", entry.name))
        };

        if !place_file(source, &dest, is_dir, opts) || opts.dry_run {
            return HashMap::new();
        }

        if is_dir {
            collect_dir_deploy_result(source, &dest)
        } else {
            HashMap::from([(format!("{}.md", entry.name), dest)])
        }
    }

    fn installed_path(&self, entry: &Entry, scope: Scope, repo_root: &Path) -> PathBuf {
        self.target_dir(entry.entity_type.as_str(), scope, repo_root)
            .join(format!("{}.md", entry.name))
    }

    fn installed_dir_files(
        &self,
        entry: &Entry,
        scope: Scope,
        repo_root: &Path,
    ) -> HashMap<String, PathBuf> {
        let target_dir = self.target_dir(entry.entity_type.as_str(), scope, repo_root);
        let mode = self
            .entities
            .get(entry.entity_type.as_str())
            .map_or(DirInstallMode::Nested, |c| c.dir_mode);

        if mode == DirInstallMode::Nested {
            collect_nested_installed(entry, &target_dir)
        } else {
            // Flat: keys are relative-from-vdir so they match patch lookup keys
            let vdir = skillfile_sources::sync::vendor_dir_for(entry, repo_root);
            collect_flat_installed_checked(&vdir, &target_dir)
        }
    }
}

// ---------------------------------------------------------------------------
// Deployment helpers (used by FileSystemAdapter)
// ---------------------------------------------------------------------------

/// Build `{relative_path: absolute_path}` for all non-.meta files in a deployed directory.
fn collect_dir_deploy_result(source: &Path, dest: &Path) -> DeployResult {
    let mut result = HashMap::new();
    for file in walkdir(source) {
        if file.file_name().is_none_or(|n| n == ".meta") {
            continue;
        }
        let Ok(rel) = file.strip_prefix(source) else {
            continue;
        };
        result.insert(rel.to_string_lossy().to_string(), dest.join(rel));
    }
    result
}

/// Build `{relative_path: absolute_path}` for nested-mode installed dir.
/// Returns empty map when the installed directory does not exist.
fn collect_nested_installed(entry: &Entry, target_dir: &Path) -> HashMap<String, PathBuf> {
    let installed_dir = target_dir.join(&entry.name);
    if !installed_dir.is_dir() {
        return HashMap::new();
    }
    collect_walkdir_relative(&installed_dir)
}

/// Build `{relative_path: target_path}` for flat-mode installed files.
/// Returns empty map when the vendor cache directory does not exist.
fn collect_flat_installed_checked(vdir: &Path, target_dir: &Path) -> HashMap<String, PathBuf> {
    if !vdir.is_dir() {
        return HashMap::new();
    }
    collect_flat_installed(vdir, target_dir)
}

/// Build `{relative_path: absolute_path}` from a walkdir rooted at `base`.
fn collect_walkdir_relative(base: &Path) -> HashMap<String, PathBuf> {
    let mut result = HashMap::new();
    for file in walkdir(base) {
        let Ok(rel) = file.strip_prefix(base) else {
            continue;
        };
        result.insert(rel.to_string_lossy().to_string(), file);
    }
    result
}

/// Build `{relative_path: target_path}` for `.md` files in a flat-mode vendor dir
/// that have corresponding deployed files in `target_dir`.
fn collect_flat_installed(vdir: &Path, target_dir: &Path) -> HashMap<String, PathBuf> {
    let mut result = HashMap::new();
    for file in walkdir(vdir) {
        if file
            .extension()
            .is_none_or(|ext| ext.to_string_lossy() != "md")
        {
            continue;
        }
        let Ok(rel) = file.strip_prefix(vdir) else {
            continue;
        };
        let dest = target_dir.join(file.file_name().unwrap_or_default());
        if dest.exists() {
            result.insert(rel.to_string_lossy().to_string(), dest);
        }
    }
    result
}

/// Deploy each `.md` in `source_dir` as an individual file in `target_dir` (flat mode).
#[allow(clippy::too_many_arguments)]
fn deploy_flat(source_dir: &Path, target_dir: &Path, opts: &InstallOptions) -> DeployResult {
    let mut md_files: Vec<PathBuf> = walkdir(source_dir)
        .into_iter()
        .filter(|f| f.extension().is_some_and(|ext| ext == "md"))
        .collect();
    md_files.sort();

    if opts.dry_run {
        for src in md_files.iter().filter(|s| s.file_name().is_some()) {
            let name = src.file_name().unwrap_or_default();
            progress!(
                "  {} -> {} [copy, dry-run]",
                name.to_string_lossy(),
                target_dir.join(name).display()
            );
        }
        return HashMap::new();
    }

    std::fs::create_dir_all(target_dir).ok();
    let mut result = HashMap::new();
    for src in &md_files {
        let Some(name) = src.file_name() else {
            continue;
        };
        let dest = target_dir.join(name);
        if !opts.overwrite && dest.is_file() {
            continue;
        }
        if dest.exists() {
            std::fs::remove_file(&dest).ok();
        }
        if std::fs::copy(src, &dest).is_ok() {
            progress!("  {} -> {}", name.to_string_lossy(), dest.display());
            deploy_flat_insert_result(&mut result, src, source_dir, dest);
        }
    }
    result
}

/// Insert `{relative_path: dest}` into `result` if `src` is within `source_dir`.
#[allow(clippy::too_many_arguments)]
fn deploy_flat_insert_result(
    result: &mut DeployResult,
    src: &Path,
    source_dir: &Path,
    dest: PathBuf,
) {
    if let Ok(rel) = src.strip_prefix(source_dir) {
        result.insert(rel.to_string_lossy().to_string(), dest);
    }
}

/// Copy `source` to `dest`. Returns `true` if placed, `false` if skipped.
#[allow(clippy::too_many_arguments)]
fn place_file(source: &Path, dest: &Path, is_dir: bool, opts: &InstallOptions) -> bool {
    if !opts.overwrite && !opts.dry_run {
        if is_dir && dest.is_dir() {
            return false;
        }
        if !is_dir && dest.is_file() {
            return false;
        }
    }

    let label = format!(
        "  {} -> {}",
        source.file_name().unwrap_or_default().to_string_lossy(),
        dest.display()
    );

    if opts.dry_run {
        progress!("{label} [copy, dry-run]");
        return true;
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    // Remove existing
    if dest.exists() || dest.is_symlink() {
        if dest.is_dir() {
            std::fs::remove_dir_all(dest).ok();
        } else {
            std::fs::remove_file(dest).ok();
        }
    }

    if is_dir {
        copy_dir_recursive(source, dest).ok();
    } else {
        std::fs::copy(source, dest).ok();
    }

    progress!("{label}");
    true
}

/// Recursively copy a directory tree.
// The recursive structure naturally produces multiple `?` operators and
// branching that triggers cognitive-complexity, but the logic is straightforward.
#[allow(clippy::cognitive_complexity)]
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AdapterRegistry — injectable, testable collection of platform adapters
// ---------------------------------------------------------------------------

/// A collection of platform adapters, indexed by name.
///
/// The registry owns the adapters and provides lookup by name. It can be
/// constructed with the built-in adapters via [`AdapterRegistry::builtin()`],
/// or built manually for testing.
pub struct AdapterRegistry {
    adapters: HashMap<String, Box<dyn PlatformAdapter>>,
}

impl AdapterRegistry {
    /// Create a registry from a vec of boxed adapters.
    pub fn new(adapters: Vec<Box<dyn PlatformAdapter>>) -> Self {
        let map = adapters
            .into_iter()
            .map(|a| (a.name().to_string(), a))
            .collect();
        Self { adapters: map }
    }

    /// Create the built-in registry with all known platform adapters.
    pub fn builtin() -> Self {
        Self::new(vec![
            Box::new(claude_code_adapter()),
            Box::new(gemini_cli_adapter()),
            Box::new(codex_adapter()),
            Box::new(cursor_adapter()),
            Box::new(windsurf_adapter()),
            Box::new(opencode_adapter()),
            Box::new(copilot_adapter()),
        ])
    }

    /// Look up an adapter by name.
    pub fn get(&self, name: &str) -> Option<&dyn PlatformAdapter> {
        self.adapters.get(name).map(|b| &**b)
    }

    /// Check if an adapter with this name exists.
    pub fn contains(&self, name: &str) -> bool {
        self.adapters.contains_key(name)
    }

    /// Sorted list of all adapter names.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .adapters
            .keys()
            .map(std::string::String::as_str)
            .collect();
        names.sort_unstable();
        names
    }
}

impl fmt::Debug for AdapterRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdapterRegistry")
            .field("adapters", &self.names())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Built-in adapters
// ---------------------------------------------------------------------------

fn claude_code_adapter() -> FileSystemAdapter {
    FileSystemAdapter::new(
        "claude-code",
        HashMap::from([
            (
                "agent".to_string(),
                EntityConfig {
                    global_path: "~/.claude/agents".into(),
                    local_path: ".claude/agents".into(),
                    dir_mode: DirInstallMode::Flat,
                },
            ),
            (
                "skill".to_string(),
                EntityConfig {
                    global_path: "~/.claude/skills".into(),
                    local_path: ".claude/skills".into(),
                    dir_mode: DirInstallMode::Nested,
                },
            ),
        ]),
    )
}

fn gemini_cli_adapter() -> FileSystemAdapter {
    FileSystemAdapter::new(
        "gemini-cli",
        HashMap::from([
            (
                "agent".to_string(),
                EntityConfig {
                    global_path: "~/.gemini/agents".into(),
                    local_path: ".gemini/agents".into(),
                    dir_mode: DirInstallMode::Flat,
                },
            ),
            (
                "skill".to_string(),
                EntityConfig {
                    global_path: "~/.gemini/skills".into(),
                    local_path: ".gemini/skills".into(),
                    dir_mode: DirInstallMode::Nested,
                },
            ),
        ]),
    )
}

fn codex_adapter() -> FileSystemAdapter {
    FileSystemAdapter::new(
        "codex",
        HashMap::from([(
            "skill".to_string(),
            EntityConfig {
                global_path: "~/.codex/skills".into(),
                local_path: ".codex/skills".into(),
                dir_mode: DirInstallMode::Nested,
            },
        )]),
    )
}

/// Cursor adapter.
///
/// Cursor reads skills from `.cursor/skills/<name>/SKILL.md` (nested) and
/// agents from `.cursor/agents/<name>.md` (flat). Same pattern as Claude Code.
fn cursor_adapter() -> FileSystemAdapter {
    FileSystemAdapter::new(
        "cursor",
        HashMap::from([
            (
                "agent".to_string(),
                EntityConfig {
                    global_path: "~/.cursor/agents".into(),
                    local_path: ".cursor/agents".into(),
                    dir_mode: DirInstallMode::Flat,
                },
            ),
            (
                "skill".to_string(),
                EntityConfig {
                    global_path: "~/.cursor/skills".into(),
                    local_path: ".cursor/skills".into(),
                    dir_mode: DirInstallMode::Nested,
                },
            ),
        ]),
    )
}

/// Windsurf adapter.
///
/// Windsurf reads skills from `.windsurf/skills/<name>/SKILL.md` (nested).
/// It does not support agent markdown files in a dedicated directory — agents
/// are defined via `AGENTS.md` files scattered in the project tree instead.
fn windsurf_adapter() -> FileSystemAdapter {
    FileSystemAdapter::new(
        "windsurf",
        HashMap::from([(
            "skill".to_string(),
            EntityConfig {
                global_path: "~/.codeium/windsurf/skills".into(),
                local_path: ".windsurf/skills".into(),
                dir_mode: DirInstallMode::Nested,
            },
        )]),
    )
}

/// OpenCode adapter.
///
/// OpenCode reads skills from `.opencode/skills/<name>/SKILL.md` (nested) and
/// agents from `.opencode/agents/<name>.md` (flat). Global paths follow XDG:
/// `~/.config/opencode/`.
fn opencode_adapter() -> FileSystemAdapter {
    FileSystemAdapter::new(
        "opencode",
        HashMap::from([
            (
                "agent".to_string(),
                EntityConfig {
                    global_path: "~/.config/opencode/agents".into(),
                    local_path: ".opencode/agents".into(),
                    dir_mode: DirInstallMode::Flat,
                },
            ),
            (
                "skill".to_string(),
                EntityConfig {
                    global_path: "~/.config/opencode/skills".into(),
                    local_path: ".opencode/skills".into(),
                    dir_mode: DirInstallMode::Nested,
                },
            ),
        ]),
    )
}

/// GitHub Copilot adapter.
///
/// Copilot reads skills from `.github/skills/<name>/SKILL.md` (nested) and
/// agents from `.github/agents/<name>.md` (flat). Note: Copilot natively
/// expects `.agent.md` extension for agent files, but skillfile deploys
/// standard `.md` files which Copilot also reads.
fn copilot_adapter() -> FileSystemAdapter {
    FileSystemAdapter::new(
        "copilot",
        HashMap::from([
            (
                "agent".to_string(),
                EntityConfig {
                    global_path: "~/.copilot/agents".into(),
                    local_path: ".github/agents".into(),
                    dir_mode: DirInstallMode::Flat,
                },
            ),
            (
                "skill".to_string(),
                EntityConfig {
                    global_path: "~/.copilot/skills".into(),
                    local_path: ".github/skills".into(),
                    dir_mode: DirInstallMode::Nested,
                },
            ),
        ]),
    )
}

// ---------------------------------------------------------------------------
// Global registry accessor (backward-compatible convenience)
// ---------------------------------------------------------------------------

/// Get the global adapter registry (lazily initialized).
#[must_use]
pub fn adapters() -> &'static AdapterRegistry {
    static REGISTRY: OnceLock<AdapterRegistry> = OnceLock::new();
    REGISTRY.get_or_init(AdapterRegistry::builtin)
}

/// Sorted list of known adapter names.
#[must_use]
pub fn known_adapters() -> Vec<&'static str> {
    adapters().names()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Trait compliance: every registered adapter satisfies PlatformAdapter --

    #[test]
    fn all_builtin_adapters_in_registry() {
        let reg = adapters();
        assert!(reg.contains("claude-code"));
        assert!(reg.contains("gemini-cli"));
        assert!(reg.contains("codex"));
        assert!(reg.contains("cursor"));
        assert!(reg.contains("windsurf"));
        assert!(reg.contains("opencode"));
        assert!(reg.contains("copilot"));
    }

    #[test]
    fn known_adapters_contains_all() {
        let names = known_adapters();
        assert!(names.contains(&"claude-code"));
        assert!(names.contains(&"gemini-cli"));
        assert!(names.contains(&"codex"));
        assert!(names.contains(&"cursor"));
        assert!(names.contains(&"windsurf"));
        assert!(names.contains(&"opencode"));
        assert!(names.contains(&"copilot"));
        assert_eq!(names.len(), 7);
    }

    #[test]
    fn adapter_name_matches_registry_key() {
        let reg = adapters();
        for name in reg.names() {
            let adapter = reg.get(name).unwrap();
            assert_eq!(adapter.name(), name);
        }
    }

    #[test]
    fn registry_get_unknown_returns_none() {
        assert!(adapters().get("unknown-tool").is_none());
    }

    // -- supports() --

    #[test]
    fn claude_code_supports_agent_and_skill() {
        let a = adapters().get("claude-code").unwrap();
        assert!(a.supports("agent"));
        assert!(a.supports("skill"));
        assert!(!a.supports("hook"));
    }

    #[test]
    fn gemini_cli_supports_agent_and_skill() {
        let a = adapters().get("gemini-cli").unwrap();
        assert!(a.supports("agent"));
        assert!(a.supports("skill"));
    }

    #[test]
    fn codex_supports_skill_not_agent() {
        let a = adapters().get("codex").unwrap();
        assert!(a.supports("skill"));
        assert!(!a.supports("agent"));
    }

    // -- target_dir() --

    #[test]
    fn local_target_dir_claude_code() {
        let tmp = PathBuf::from("/tmp/test");
        let a = adapters().get("claude-code").unwrap();
        assert_eq!(
            a.target_dir("agent", Scope::Local, &tmp),
            tmp.join(".claude/agents")
        );
        assert_eq!(
            a.target_dir("skill", Scope::Local, &tmp),
            tmp.join(".claude/skills")
        );
    }

    #[test]
    fn local_target_dir_gemini_cli() {
        let tmp = PathBuf::from("/tmp/test");
        let a = adapters().get("gemini-cli").unwrap();
        assert_eq!(
            a.target_dir("agent", Scope::Local, &tmp),
            tmp.join(".gemini/agents")
        );
        assert_eq!(
            a.target_dir("skill", Scope::Local, &tmp),
            tmp.join(".gemini/skills")
        );
    }

    #[test]
    fn local_target_dir_codex() {
        let tmp = PathBuf::from("/tmp/test");
        let a = adapters().get("codex").unwrap();
        assert_eq!(
            a.target_dir("skill", Scope::Local, &tmp),
            tmp.join(".codex/skills")
        );
    }

    #[test]
    fn global_target_dir_is_absolute() {
        let a = adapters().get("claude-code").unwrap();
        let result = a.target_dir("agent", Scope::Global, Path::new("/tmp"));
        assert!(result.is_absolute());
        assert!(result.to_string_lossy().ends_with(".claude/agents"));
    }

    #[test]
    fn global_target_dir_gemini_cli_skill() {
        let a = adapters().get("gemini-cli").unwrap();
        let result = a.target_dir("skill", Scope::Global, Path::new("/tmp"));
        assert!(result.is_absolute());
        assert!(result.to_string_lossy().ends_with(".gemini/skills"));
    }

    #[test]
    fn global_target_dir_codex_skill() {
        let a = adapters().get("codex").unwrap();
        let result = a.target_dir("skill", Scope::Global, Path::new("/tmp"));
        assert!(result.is_absolute());
        assert!(result.to_string_lossy().ends_with(".codex/skills"));
    }

    // -- supports() for new adapters --

    #[test]
    fn cursor_supports_agent_and_skill() {
        let a = adapters().get("cursor").unwrap();
        assert!(a.supports("agent"));
        assert!(a.supports("skill"));
        assert!(!a.supports("hook"));
    }

    #[test]
    fn windsurf_supports_skill_not_agent() {
        let a = adapters().get("windsurf").unwrap();
        assert!(a.supports("skill"));
        assert!(!a.supports("agent"));
    }

    #[test]
    fn opencode_supports_agent_and_skill() {
        let a = adapters().get("opencode").unwrap();
        assert!(a.supports("agent"));
        assert!(a.supports("skill"));
        assert!(!a.supports("hook"));
    }

    #[test]
    fn copilot_supports_agent_and_skill() {
        let a = adapters().get("copilot").unwrap();
        assert!(a.supports("agent"));
        assert!(a.supports("skill"));
        assert!(!a.supports("rule"));
    }

    // -- target_dir() for new adapters --

    #[test]
    fn local_target_dir_cursor() {
        let tmp = PathBuf::from("/tmp/test");
        let a = adapters().get("cursor").unwrap();
        assert_eq!(
            a.target_dir("agent", Scope::Local, &tmp),
            tmp.join(".cursor/agents")
        );
        assert_eq!(
            a.target_dir("skill", Scope::Local, &tmp),
            tmp.join(".cursor/skills")
        );
    }

    #[test]
    fn local_target_dir_windsurf() {
        let tmp = PathBuf::from("/tmp/test");
        let a = adapters().get("windsurf").unwrap();
        assert_eq!(
            a.target_dir("skill", Scope::Local, &tmp),
            tmp.join(".windsurf/skills")
        );
    }

    #[test]
    fn local_target_dir_opencode() {
        let tmp = PathBuf::from("/tmp/test");
        let a = adapters().get("opencode").unwrap();
        assert_eq!(
            a.target_dir("agent", Scope::Local, &tmp),
            tmp.join(".opencode/agents")
        );
        assert_eq!(
            a.target_dir("skill", Scope::Local, &tmp),
            tmp.join(".opencode/skills")
        );
    }

    #[test]
    fn local_target_dir_copilot() {
        let tmp = PathBuf::from("/tmp/test");
        let a = adapters().get("copilot").unwrap();
        assert_eq!(
            a.target_dir("agent", Scope::Local, &tmp),
            tmp.join(".github/agents")
        );
        assert_eq!(
            a.target_dir("skill", Scope::Local, &tmp),
            tmp.join(".github/skills")
        );
    }

    #[test]
    fn global_target_dir_cursor() {
        let a = adapters().get("cursor").unwrap();
        let skill = a.target_dir("skill", Scope::Global, Path::new("/tmp"));
        assert!(skill.is_absolute());
        assert!(skill.to_string_lossy().ends_with(".cursor/skills"));
        let agent = a.target_dir("agent", Scope::Global, Path::new("/tmp"));
        assert!(agent.is_absolute());
        assert!(agent.to_string_lossy().ends_with(".cursor/agents"));
    }

    #[test]
    fn global_target_dir_windsurf() {
        let a = adapters().get("windsurf").unwrap();
        let result = a.target_dir("skill", Scope::Global, Path::new("/tmp"));
        assert!(result.is_absolute());
        assert!(
            result.to_string_lossy().ends_with("windsurf/skills"),
            "unexpected: {result:?}"
        );
    }

    #[test]
    fn global_target_dir_opencode() {
        let a = adapters().get("opencode").unwrap();
        let skill = a.target_dir("skill", Scope::Global, Path::new("/tmp"));
        assert!(skill.is_absolute());
        assert!(
            skill.to_string_lossy().ends_with("opencode/skills"),
            "unexpected: {skill:?}"
        );
        let agent = a.target_dir("agent", Scope::Global, Path::new("/tmp"));
        assert!(agent.is_absolute());
        assert!(
            agent.to_string_lossy().ends_with("opencode/agents"),
            "unexpected: {agent:?}"
        );
    }

    #[test]
    fn global_target_dir_copilot() {
        let a = adapters().get("copilot").unwrap();
        let skill = a.target_dir("skill", Scope::Global, Path::new("/tmp"));
        assert!(skill.is_absolute());
        assert!(skill.to_string_lossy().ends_with(".copilot/skills"));
        let agent = a.target_dir("agent", Scope::Global, Path::new("/tmp"));
        assert!(agent.is_absolute());
        assert!(agent.to_string_lossy().ends_with(".copilot/agents"));
    }

    // -- dir_mode for new adapters --

    #[test]
    fn cursor_dir_modes() {
        let a = adapters().get("cursor").unwrap();
        assert_eq!(a.dir_mode("agent"), Some(DirInstallMode::Flat));
        assert_eq!(a.dir_mode("skill"), Some(DirInstallMode::Nested));
    }

    #[test]
    fn windsurf_dir_mode() {
        let a = adapters().get("windsurf").unwrap();
        assert_eq!(a.dir_mode("skill"), Some(DirInstallMode::Nested));
        assert_eq!(a.dir_mode("agent"), None);
    }

    #[test]
    fn opencode_dir_modes() {
        let a = adapters().get("opencode").unwrap();
        assert_eq!(a.dir_mode("agent"), Some(DirInstallMode::Flat));
        assert_eq!(a.dir_mode("skill"), Some(DirInstallMode::Nested));
    }

    #[test]
    fn copilot_dir_modes() {
        let a = adapters().get("copilot").unwrap();
        assert_eq!(a.dir_mode("agent"), Some(DirInstallMode::Flat));
        assert_eq!(a.dir_mode("skill"), Some(DirInstallMode::Nested));
    }

    // -- dir_mode --

    #[test]
    fn claude_code_dir_modes() {
        let a = adapters().get("claude-code").unwrap();
        assert_eq!(a.dir_mode("agent"), Some(DirInstallMode::Flat));
        assert_eq!(a.dir_mode("skill"), Some(DirInstallMode::Nested));
    }

    #[test]
    fn gemini_cli_dir_modes() {
        let a = adapters().get("gemini-cli").unwrap();
        assert_eq!(a.dir_mode("agent"), Some(DirInstallMode::Flat));
        assert_eq!(a.dir_mode("skill"), Some(DirInstallMode::Nested));
    }

    #[test]
    fn codex_dir_mode() {
        let a = adapters().get("codex").unwrap();
        assert_eq!(a.dir_mode("skill"), Some(DirInstallMode::Nested));
    }

    // -- Custom adapter extensibility --

    #[test]
    fn custom_adapter_via_registry() {
        let custom = FileSystemAdapter::new(
            "my-tool",
            HashMap::from([(
                "skill".to_string(),
                EntityConfig {
                    global_path: "~/.my-tool/skills".into(),
                    local_path: ".my-tool/skills".into(),
                    dir_mode: DirInstallMode::Nested,
                },
            )]),
        );
        let registry = AdapterRegistry::new(vec![Box::new(custom)]);
        let a = registry.get("my-tool").unwrap();
        assert!(a.supports("skill"));
        assert!(!a.supports("agent"));
        assert_eq!(registry.names(), vec!["my-tool"]);
    }

    // -- deploy_entry key contract --

    #[test]
    fn deploy_entry_single_file_key_matches_patch_convention() {
        use skillfile_core::models::{EntityType, SourceFields};

        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join(".skillfile/cache/agents/test");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("agent.md"), "# Agent\n").unwrap();
        let source = source_dir.join("agent.md");

        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "test".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "agents/agent.md".into(),
                ref_: "main".into(),
            },
        };
        let a = adapters().get("claude-code").unwrap();
        let result = a.deploy_entry(
            &entry,
            &source,
            Scope::Local,
            dir.path(),
            &InstallOptions::default(),
        );
        assert!(
            result.contains_key("test.md"),
            "Single-file key must be 'test.md', got {:?}",
            result.keys().collect::<Vec<_>>()
        );
    }

    // -- deploy_flat --

    #[test]
    fn deploy_flat_copies_md_files_to_target_dir() {
        use skillfile_core::models::{EntityType, SourceFields};

        let dir = tempfile::tempdir().unwrap();
        // Set up vendor cache dir with .md files and a .meta
        let source_dir = dir.path().join(".skillfile/cache/agents/core-dev");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("backend.md"), "# Backend").unwrap();
        std::fs::write(source_dir.join("frontend.md"), "# Frontend").unwrap();
        std::fs::write(source_dir.join(".meta"), "{}").unwrap();

        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "core-dev".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "agents/core-dev".into(),
                ref_: "main".into(),
            },
        };
        let a = adapters().get("claude-code").unwrap();
        let result = a.deploy_entry(
            &entry,
            &source_dir,
            Scope::Local,
            dir.path(),
            &InstallOptions {
                dry_run: false,
                overwrite: true,
            },
        );
        // Flat mode: keys are relative paths from source dir
        assert!(result.contains_key("backend.md"));
        assert!(result.contains_key("frontend.md"));
        assert!(!result.contains_key(".meta"));
        // Files actually exist
        let target = dir.path().join(".claude/agents");
        assert!(target.join("backend.md").exists());
        assert!(target.join("frontend.md").exists());
    }

    #[test]
    fn deploy_flat_dry_run_returns_empty() {
        use skillfile_core::models::{EntityType, SourceFields};

        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join(".skillfile/cache/agents/core-dev");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("backend.md"), "# Backend").unwrap();

        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "core-dev".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "agents/core-dev".into(),
                ref_: "main".into(),
            },
        };
        let a = adapters().get("claude-code").unwrap();
        let result = a.deploy_entry(
            &entry,
            &source_dir,
            Scope::Local,
            dir.path(),
            &InstallOptions {
                dry_run: true,
                overwrite: false,
            },
        );
        assert!(result.is_empty());
        assert!(!dir.path().join(".claude/agents/backend.md").exists());
    }

    #[test]
    fn deploy_flat_skips_existing_when_no_overwrite() {
        use skillfile_core::models::{EntityType, SourceFields};

        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join(".skillfile/cache/agents/core-dev");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("backend.md"), "# New").unwrap();

        // Pre-create the target file
        let target = dir.path().join(".claude/agents");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("backend.md"), "# Old").unwrap();

        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "core-dev".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "agents/core-dev".into(),
                ref_: "main".into(),
            },
        };
        let a = adapters().get("claude-code").unwrap();
        let result = a.deploy_entry(
            &entry,
            &source_dir,
            Scope::Local,
            dir.path(),
            &InstallOptions {
                dry_run: false,
                overwrite: false,
            },
        );
        // Should skip the existing file
        assert!(result.is_empty());
        // Original content preserved
        assert_eq!(
            std::fs::read_to_string(target.join("backend.md")).unwrap(),
            "# Old"
        );
    }

    #[test]
    fn deploy_flat_overwrites_existing_when_overwrite_true() {
        use skillfile_core::models::{EntityType, SourceFields};

        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join(".skillfile/cache/agents/core-dev");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("backend.md"), "# New").unwrap();

        let target = dir.path().join(".claude/agents");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("backend.md"), "# Old").unwrap();

        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "core-dev".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "agents/core-dev".into(),
                ref_: "main".into(),
            },
        };
        let a = adapters().get("claude-code").unwrap();
        let result = a.deploy_entry(
            &entry,
            &source_dir,
            Scope::Local,
            dir.path(),
            &InstallOptions {
                dry_run: false,
                overwrite: true,
            },
        );
        assert!(result.contains_key("backend.md"));
        assert_eq!(
            std::fs::read_to_string(target.join("backend.md")).unwrap(),
            "# New"
        );
    }

    // -- place_file skip logic --

    #[test]
    fn place_file_skips_existing_dir_when_no_overwrite() {
        use skillfile_core::models::{EntityType, SourceFields};

        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join(".skillfile/cache/skills/my-skill");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("SKILL.md"), "# Skill").unwrap();

        // Pre-create the destination dir
        let dest = dir.path().join(".claude/skills/my-skill");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("OLD.md"), "# Old").unwrap();

        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "my-skill".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "skills/my-skill".into(),
                ref_: "main".into(),
            },
        };
        let a = adapters().get("claude-code").unwrap();
        let result = a.deploy_entry(
            &entry,
            &source_dir,
            Scope::Local,
            dir.path(),
            &InstallOptions {
                dry_run: false,
                overwrite: false,
            },
        );
        // Should skip — dir already exists
        assert!(result.is_empty());
        // Old file still there
        assert!(dest.join("OLD.md").exists());
    }

    #[test]
    fn place_file_skips_existing_single_file_when_no_overwrite() {
        use skillfile_core::models::{EntityType, SourceFields};

        let dir = tempfile::tempdir().unwrap();
        let source_file = dir.path().join("skills/my-skill.md");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, "# New").unwrap();

        let dest = dir.path().join(".claude/skills/my-skill.md");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, "# Old").unwrap();

        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "my-skill".into(),
            source: SourceFields::Local {
                path: "skills/my-skill.md".into(),
            },
        };
        let a = adapters().get("claude-code").unwrap();
        let result = a.deploy_entry(
            &entry,
            &source_file,
            Scope::Local,
            dir.path(),
            &InstallOptions {
                dry_run: false,
                overwrite: false,
            },
        );
        assert!(result.is_empty());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# Old");
    }

    // -- installed_dir_files flat mode --

    #[test]
    fn installed_dir_files_flat_mode_returns_deployed_files() {
        use skillfile_core::models::{EntityType, SourceFields};

        let dir = tempfile::tempdir().unwrap();
        // Set up vendor cache dir
        let vdir = dir.path().join(".skillfile/cache/agents/core-dev");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("backend.md"), "# Backend").unwrap();
        std::fs::write(vdir.join("frontend.md"), "# Frontend").unwrap();
        std::fs::write(vdir.join(".meta"), "{}").unwrap();

        // Set up installed flat files
        let target = dir.path().join(".claude/agents");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("backend.md"), "# Backend").unwrap();
        std::fs::write(target.join("frontend.md"), "# Frontend").unwrap();

        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "core-dev".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "agents/core-dev".into(),
                ref_: "main".into(),
            },
        };
        let a = adapters().get("claude-code").unwrap();
        let files = a.installed_dir_files(&entry, Scope::Local, dir.path());
        assert!(files.contains_key("backend.md"));
        assert!(files.contains_key("frontend.md"));
        assert!(!files.contains_key(".meta"));
    }

    #[test]
    fn installed_dir_files_flat_mode_no_vdir_returns_empty() {
        use skillfile_core::models::{EntityType, SourceFields};

        let dir = tempfile::tempdir().unwrap();
        // No vendor cache dir
        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "core-dev".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "agents/core-dev".into(),
                ref_: "main".into(),
            },
        };
        let a = adapters().get("claude-code").unwrap();
        let files = a.installed_dir_files(&entry, Scope::Local, dir.path());
        assert!(files.is_empty());
    }

    #[test]
    fn installed_dir_files_flat_mode_skips_non_deployed_files() {
        use skillfile_core::models::{EntityType, SourceFields};

        let dir = tempfile::tempdir().unwrap();
        let vdir = dir.path().join(".skillfile/cache/agents/core-dev");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("backend.md"), "# Backend").unwrap();
        std::fs::write(vdir.join("frontend.md"), "# Frontend").unwrap();

        // Only deploy one file
        let target = dir.path().join(".claude/agents");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("backend.md"), "# Backend").unwrap();
        // frontend.md NOT deployed

        let entry = Entry {
            entity_type: EntityType::Agent,
            name: "core-dev".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "agents/core-dev".into(),
                ref_: "main".into(),
            },
        };
        let a = adapters().get("claude-code").unwrap();
        let files = a.installed_dir_files(&entry, Scope::Local, dir.path());
        assert!(files.contains_key("backend.md"));
        assert!(!files.contains_key("frontend.md"));
    }

    #[test]
    fn deploy_entry_dir_keys_match_source_relative_paths() {
        use skillfile_core::models::{EntityType, SourceFields};

        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join(".skillfile/cache/skills/my-skill");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("SKILL.md"), "# Skill\n").unwrap();
        std::fs::write(source_dir.join("examples.md"), "# Examples\n").unwrap();

        let entry = Entry {
            entity_type: EntityType::Skill,
            name: "my-skill".into(),
            source: SourceFields::Github {
                owner_repo: "o/r".into(),
                path_in_repo: "skills/my-skill".into(),
                ref_: "main".into(),
            },
        };
        let a = adapters().get("claude-code").unwrap();
        let result = a.deploy_entry(
            &entry,
            &source_dir,
            Scope::Local,
            dir.path(),
            &InstallOptions::default(),
        );
        assert!(result.contains_key("SKILL.md"));
        assert!(result.contains_key("examples.md"));
    }
}
