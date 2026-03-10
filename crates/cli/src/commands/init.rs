use std::io::{self, BufRead, Write};
use std::path::Path;

use skillfile_core::error::SkillfileError;
use skillfile_core::parser::{parse_manifest, MANIFEST_NAME};
use skillfile_deploy::adapter::known_adapters;

const GITIGNORE_ENTRIES: &[&str] = &[".skillfile/cache/", ".skillfile/conflict"];

fn prompt(
    reader: &mut dyn BufRead,
    writer: &mut dyn Write,
    msg: &str,
) -> Result<String, SkillfileError> {
    write!(writer, "{msg}")?;
    writer.flush()?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn prompt_yn(
    reader: &mut dyn BufRead,
    writer: &mut dyn Write,
    msg: &str,
) -> Result<bool, SkillfileError> {
    let answer = prompt(reader, writer, &format!("{msg} [y/N] "))?;
    Ok(answer.eq_ignore_ascii_case("y"))
}

fn collect_targets(
    reader: &mut dyn BufRead,
    writer: &mut dyn Write,
) -> Result<Vec<(String, String)>, SkillfileError> {
    let adapter_list = known_adapters().join(", ");
    let adapters_set: std::collections::HashSet<String> =
        known_adapters().iter().map(|s| s.to_string()).collect();
    let mut targets = Vec::new();

    loop {
        writeln!(writer, "\nKnown platforms: {adapter_list}")?;
        let adapter = loop {
            let a = prompt(reader, writer, "Platform: ")?;
            if adapters_set.contains(&a) {
                break a;
            }
            writeln!(writer, "  Please enter one of: {adapter_list}")?;
        };

        let scope = loop {
            let s = prompt(reader, writer, "Scope [global/local/both]: ")?;
            if ["global", "local", "both"].contains(&s.as_str()) {
                break s;
            }
            writeln!(writer, "  Please enter one of: global, local, both")?;
        };

        if scope == "both" {
            targets.push((adapter.clone(), "global".to_string()));
            targets.push((adapter, "local".to_string()));
        } else {
            targets.push((adapter, scope));
        }

        if !prompt_yn(reader, writer, "Add another platform?")? {
            break;
        }
    }

    Ok(targets)
}

fn rewrite_install_lines(
    manifest_path: &Path,
    new_targets: &[(String, String)],
) -> Result<(), SkillfileError> {
    let text = std::fs::read_to_string(manifest_path)?;
    let mut non_install: Vec<&str> = text
        .lines()
        .filter(|line| {
            let stripped = line.trim();
            !stripped.starts_with("install ") && stripped != "install"
        })
        .collect();

    // Strip leading blank lines from remaining content
    while non_install.first().is_some_and(|l| l.trim().is_empty()) {
        non_install.remove(0);
    }

    let mut output = String::new();
    for (adapter, scope) in new_targets {
        output.push_str(&format!("install  {adapter}  {scope}\n"));
    }
    output.push('\n');
    for line in &non_install {
        output.push_str(line);
        output.push('\n');
    }

    std::fs::write(manifest_path, &output)?;
    Ok(())
}

fn update_gitignore(repo_root: &Path) -> Result<(), SkillfileError> {
    let gitignore = repo_root.join(".gitignore");
    let existing: Vec<String> = if gitignore.exists() {
        std::fs::read_to_string(&gitignore)?
            .lines()
            .map(|l| l.to_string())
            .collect()
    } else {
        Vec::new()
    };

    let missing: Vec<&&str> = GITIGNORE_ENTRIES
        .iter()
        .filter(|e| !existing.iter().any(|l| l == **e))
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&gitignore)?;

    if !existing.is_empty() && existing.last().is_some_and(|l| !l.is_empty()) {
        writeln!(file)?;
    }
    writeln!(file, "# skillfile")?;
    for entry in &missing {
        writeln!(file, "{entry}")?;
    }

    let names: Vec<&str> = missing.iter().map(|e| **e).collect();
    println!("\n.gitignore updated: {}", names.join(", "));

    Ok(())
}

pub fn cmd_init(repo_root: &Path) -> Result<(), SkillfileError> {
    cmd_init_with_io(repo_root, &mut io::stdin().lock(), &mut io::stdout())
}

pub fn cmd_init_with_io(
    repo_root: &Path,
    reader: &mut dyn BufRead,
    writer: &mut dyn Write,
) -> Result<(), SkillfileError> {
    let manifest_path = repo_root.join(MANIFEST_NAME);
    if !manifest_path.exists() {
        std::fs::write(&manifest_path, "")?;
        writeln!(writer, "Created {MANIFEST_NAME}.")?;
    }

    let result = parse_manifest(&manifest_path)?;
    let existing = &result.manifest.install_targets;

    if !existing.is_empty() {
        writeln!(writer, "Existing install config found:")?;
        for t in existing {
            writeln!(writer, "  install  {}  {}", t.adapter, t.scope)?;
        }
        writeln!(writer, "This will be replaced.")?;
        if !prompt_yn(reader, writer, "Continue?")? {
            writeln!(writer, "Aborted.")?;
            return Ok(());
        }
    }

    writeln!(writer, "\nConfigure install targets.")?;
    let new_targets = collect_targets(reader, writer)?;

    rewrite_install_lines(&manifest_path, &new_targets)?;
    update_gitignore(repo_root)?;

    writeln!(writer, "\nInstall config written to Skillfile:")?;
    for (adapter, scope) in &new_targets {
        writeln!(writer, "  install  {adapter}  {scope}")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::write(dir.join(MANIFEST_NAME), content).unwrap();
    }

    fn run_init(dir: &Path, input: &str) -> Vec<u8> {
        let mut reader = io::Cursor::new(input.as_bytes().to_vec());
        let mut output = Vec::new();
        cmd_init_with_io(dir, &mut reader, &mut output).unwrap();
        output
    }

    #[test]
    fn creates_manifest_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!dir.path().join(MANIFEST_NAME).exists());

        run_init(dir.path(), "claude-code\nglobal\nn\n");

        assert!(dir.path().join(MANIFEST_NAME).exists());
        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("install  claude-code  global"));
    }

    #[test]
    fn writes_install_lines() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        run_init(dir.path(), "claude-code\nglobal\nn\n");

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("install  claude-code  global"));
    }

    #[test]
    fn install_lines_at_top() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");
        run_init(dir.path(), "claude-code\nglobal\nn\n");

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "install  claude-code  global");
    }

    #[test]
    fn preserves_existing_entries() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "local  skill  skills/foo.md\n");
        run_init(dir.path(), "claude-code\nlocal\nn\n");

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("local  skill  skills/foo.md"));
        assert!(text.contains("install  claude-code  local"));
    }

    #[test]
    fn both_scope_adds_global_and_local() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        run_init(dir.path(), "claude-code\nboth\nn\n");

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("install  claude-code  global"));
        assert!(text.contains("install  claude-code  local"));
    }

    #[test]
    fn multiple_adapters() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        run_init(dir.path(), "claude-code\nglobal\ny\ngemini-cli\nlocal\nn\n");

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(text.contains("install  claude-code  global"));
        assert!(text.contains("install  gemini-cli  local"));
    }

    #[test]
    fn replaces_existing_install_targets() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "install  claude-code  global\nlocal  skill  skills/foo.md\n",
        );
        // "y" to confirm replacement, then new adapter/scope, then "n" for no more
        run_init(dir.path(), "y\ngemini-cli\nlocal\nn\n");

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert!(!text.contains("claude-code"));
        assert!(text.contains("install  gemini-cli  local"));
        assert!(text.contains("local  skill  skills/foo.md"));
    }

    #[test]
    fn abort_when_existing_targets() {
        let dir = tempfile::tempdir().unwrap();
        let original = "install  claude-code  global\nlocal  skill  skills/foo.md\n";
        write_manifest(dir.path(), original);
        // "n" to abort
        run_init(dir.path(), "n\n");

        let text = std::fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
        assert_eq!(text, original);
    }

    #[test]
    fn creates_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        run_init(dir.path(), "claude-code\nglobal\nn\n");

        let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gitignore.contains(".skillfile/cache/"));
        assert!(gitignore.contains(".skillfile/conflict"));
    }

    #[test]
    fn gitignore_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        std::fs::write(
            dir.path().join(".gitignore"),
            "# skillfile\n.skillfile/cache/\n.skillfile/conflict\n",
        )
        .unwrap();

        run_init(dir.path(), "claude-code\nglobal\nn\n");

        let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        // Should not duplicate entries
        assert_eq!(
            gitignore.matches(".skillfile/cache/").count(),
            1,
            "gitignore should not have duplicates"
        );
    }

    #[test]
    fn gitignore_does_not_include_patches() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "");
        run_init(dir.path(), "claude-code\nglobal\nn\n");

        let gitignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(!gitignore.contains("patches"));
    }
}
