use std::collections::{HashMap, HashSet};
use std::path::Path;

use console::{style, StyledObject};
use skillfile_core::error::SkillfileError;
use skillfile_core::lock::{lock_key, read_lock};
use skillfile_core::models::{Manifest, Scope, SourceFields};
use skillfile_core::parser::{parse_manifest, MANIFEST_NAME};
use skillfile_deploy::adapter::adapters;

fn check_duplicate_names(manifest: &Manifest, errors: &mut Vec<String>) {
    let mut seen: HashMap<String, String> = HashMap::new();
    for entry in &manifest.entries {
        if let Some(existing_type) = seen.get(&entry.name) {
            errors.push(format!(
                "duplicate name '{}' ({} and {})",
                entry.name,
                existing_type,
                entry.source_type()
            ));
        } else {
            seen.insert(entry.name.clone(), entry.source_type().to_string());
        }
    }
}

fn check_local_paths(manifest: &Manifest, repo_root: &Path, errors: &mut Vec<String>) {
    for entry in &manifest.entries {
        let SourceFields::Local { path } = &entry.source else {
            continue;
        };
        if !repo_root.join(path).exists() {
            errors.push(format!(
                "local path not found: '{}' (entry: {})",
                path, entry.name
            ));
        }
    }
}

fn check_platforms(manifest: &Manifest, errors: &mut Vec<String>) {
    let all_adapters = adapters();
    for target in &manifest.install_targets {
        if !all_adapters.contains(&target.adapter) {
            errors.push(format!("unknown platform: '{}'", target.adapter));
        }
    }
}

fn check_duplicate_targets(manifest: &Manifest, errors: &mut Vec<String>) {
    let mut seen_targets: HashSet<(String, Scope)> = HashSet::new();
    for target in &manifest.install_targets {
        let key = (target.adapter.clone(), target.scope);
        if seen_targets.contains(&key) {
            errors.push(format!(
                "duplicate install target: '{} {}'",
                target.adapter, target.scope
            ));
        } else {
            seen_targets.insert(key);
        }
    }
}

fn check_orphaned_locks(
    manifest: &Manifest,
    repo_root: &Path,
    errors: &mut Vec<String>,
) -> Result<(), SkillfileError> {
    let locked = read_lock(repo_root)?;
    let manifest_keys: HashSet<String> = manifest.entries.iter().map(lock_key).collect();
    let mut orphaned: Vec<&String> = locked
        .keys()
        .filter(|k| !manifest_keys.contains(*k))
        .collect();
    orphaned.sort();
    for key in orphaned {
        errors.push(format!("orphaned lock entry: '{key}' (not in Skillfile)"));
    }
    Ok(())
}

fn error_label() -> StyledObject<&'static str> {
    style("error").for_stderr().red().bold()
}

fn ok_label() -> StyledObject<&'static str> {
    style("Skillfile OK").green().bold()
}

fn parse_warning_as_error(warning: &str) -> String {
    warning
        .strip_prefix("warning: ")
        .unwrap_or(warning)
        .to_string()
}

fn should_promote_parse_warning(warning: &str) -> bool {
    !warning.contains("duplicate entry name")
}

pub fn cmd_validate(repo_root: &Path) -> Result<(), SkillfileError> {
    let manifest_path = repo_root.join(MANIFEST_NAME);
    if !manifest_path.exists() {
        return Err(SkillfileError::Manifest(format!(
            "{MANIFEST_NAME} not found in {}. Create one and run `skillfile init`.",
            repo_root.display()
        )));
    }

    let result = parse_manifest(&manifest_path)?;
    let mut errors: Vec<String> = result
        .warnings
        .iter()
        .filter(|warning| should_promote_parse_warning(warning))
        .map(|warning| parse_warning_as_error(warning))
        .collect();
    let manifest = result.manifest;

    check_duplicate_names(&manifest, &mut errors);
    check_local_paths(&manifest, repo_root, &mut errors);
    check_platforms(&manifest, &mut errors);
    check_duplicate_targets(&manifest, &mut errors);
    check_orphaned_locks(&manifest, repo_root, &mut errors)?;

    if !errors.is_empty() {
        for msg in &errors {
            eprintln!("{}: {msg}", error_label());
        }
        return Err(SkillfileError::Manifest(String::new()));
    }

    let n = manifest.entries.len();
    let t = manifest.install_targets.len();
    let entry_word = if n == 1 { "entry" } else { "entries" };
    let target_word = if t == 1 { "target" } else { "targets" };
    println!(
        "{} — {n} {entry_word}, {t} install {target_word}",
        ok_label(),
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use console::{
        colors_enabled, colors_enabled_stderr, set_colors_enabled, set_colors_enabled_stderr,
    };
    use serial_test::serial;

    struct ConsoleColorGuard {
        stdout_enabled: bool,
        stderr_enabled: bool,
    }

    impl Drop for ConsoleColorGuard {
        fn drop(&mut self) {
            set_colors_enabled(self.stdout_enabled);
            set_colors_enabled_stderr(self.stderr_enabled);
        }
    }

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::write(dir.join(MANIFEST_NAME), content).unwrap();
    }

    fn set_console_colors(stdout_enabled: bool, stderr_enabled: bool) -> ConsoleColorGuard {
        let guard = ConsoleColorGuard {
            stdout_enabled: colors_enabled(),
            stderr_enabled: colors_enabled_stderr(),
        };
        set_colors_enabled(stdout_enabled);
        set_colors_enabled_stderr(stderr_enabled);
        guard
    }

    #[test]
    fn no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_validate(dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn valid_empty_manifest() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        cmd_validate(dir.path()).unwrap();
    }

    #[test]
    fn valid_github_entry() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "github  agent  owner/repo  agents/agent.md\n");
        cmd_validate(dir.path()).unwrap();
    }

    #[test]
    fn valid_local_entry_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("skills/foo.md");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "# Foo").unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");
        cmd_validate(dir.path()).unwrap();
    }

    #[test]
    fn valid_with_known_install_target() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "install  claude-code  global\n");
        cmd_validate(dir.path()).unwrap();
    }

    #[test]
    fn duplicate_name_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "local  skill  skills/foo.md\ngithub  agent  owner/repo  skills/foo.md\n",
        );
        let result = cmd_validate(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn duplicate_name_parser_warning_not_promoted_twice() {
        assert!(!should_promote_parse_warning(
            "warning: line 2: duplicate entry name 'dup'"
        ));
    }

    #[test]
    fn missing_local_path_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/nonexistent.md\n");
        let result = cmd_validate(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn unknown_platform_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "install  unknown-platform  global\n");
        let result = cmd_validate(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn multiple_errors_all_reported() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  unknown-platform  global\nlocal  skill  skills/missing.md\n",
        );
        let result = cmd_validate(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn duplicate_install_target_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  global\ninstall  claude-code  global\n",
        );
        let result = cmd_validate(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn different_scopes_not_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  global\ninstall  claude-code  local\n",
        );
        cmd_validate(dir.path()).unwrap();
    }

    #[test]
    fn orphaned_lock_entry_errors() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("skills/foo.md");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "# Foo").unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");
        let lock_data = serde_json::json!({
            "github/agent/removed-entry": {"sha": "abc123", "raw_url": "https://example.com"}
        });
        std::fs::write(
            dir.path().join("Skillfile.lock"),
            serde_json::to_string_pretty(&lock_data).unwrap(),
        )
        .unwrap();
        let result = cmd_validate(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn no_orphans_when_lock_matches() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "github  agent  owner/repo  agents/agent.md\n");
        let lock_data = serde_json::json!({
            "github/agent/agent": {"sha": "abc123", "raw_url": "https://example.com"}
        });
        std::fs::write(
            dir.path().join("Skillfile.lock"),
            serde_json::to_string_pretty(&lock_data).unwrap(),
        )
        .unwrap();
        cmd_validate(dir.path()).unwrap();
    }

    #[test]
    #[serial]
    fn error_label_uses_stderr_color_detection() {
        let _guard = set_console_colors(false, true);
        let rendered = format!("{}", error_label());
        assert!(
            rendered.contains("\u{1b}["),
            "stderr styling should render ANSI when stderr colors are enabled: {rendered:?}"
        );
    }

    #[test]
    #[serial]
    fn ok_label_uses_stdout_color_detection() {
        let _guard = set_console_colors(false, true);
        assert_eq!(format!("{}", ok_label()), "Skillfile OK");
    }
}
