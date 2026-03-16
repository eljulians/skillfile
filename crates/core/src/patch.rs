use std::path::{Path, PathBuf};

use crate::error::SkillfileError;
use crate::models::Entry;

pub const PATCHES_DIR: &str = ".skillfile/patches";

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Root directory for all patches: `.skillfile/patches/`.
#[must_use]
pub fn patches_root(repo_root: &Path) -> PathBuf {
    repo_root.join(PATCHES_DIR)
}

/// Path to the patch file for a single-file entry.
/// e.g. `.skillfile/patches/agents/my-agent.patch`
pub fn patch_path(entry: &Entry, repo_root: &Path) -> PathBuf {
    patches_root(repo_root)
        .join(entry.entity_type.dir_name())
        .join(format!("{}.patch", entry.name))
}

/// Check whether a single-file patch exists for this entry.
///
/// Returns `true` if `.skillfile/patches/<type>s/<name>.patch` exists.
#[must_use]
pub fn has_patch(entry: &Entry, repo_root: &Path) -> bool {
    patch_path(entry, repo_root).exists()
}

/// Write a single-file patch for the given entry.
pub fn write_patch(
    entry: &Entry,
    patch_text: &str,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    let p = patch_path(entry, repo_root);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, patch_text)?;
    Ok(())
}

/// Read the patch text for a single-file entry.
pub fn read_patch(entry: &Entry, repo_root: &Path) -> Result<String, SkillfileError> {
    let p = patch_path(entry, repo_root);
    Ok(std::fs::read_to_string(&p)?)
}

/// Remove the patch file for a single-file entry. No-op if it doesn't exist.
pub fn remove_patch(entry: &Entry, repo_root: &Path) -> Result<(), SkillfileError> {
    let p = patch_path(entry, repo_root);
    if !p.exists() {
        return Ok(());
    }
    std::fs::remove_file(&p)?;
    remove_empty_parent(&p);
    Ok(())
}

// ---------------------------------------------------------------------------
// Directory entry patches (one .patch file per modified file)
// ---------------------------------------------------------------------------

/// Path to a per-file patch within a directory entry.
/// e.g. `.skillfile/patches/skills/architecture-patterns/SKILL.md.patch`
pub fn dir_patch_path(entry: &Entry, filename: &str, repo_root: &Path) -> PathBuf {
    patches_root(repo_root)
        .join(entry.entity_type.dir_name())
        .join(&entry.name)
        .join(format!("{filename}.patch"))
}

/// Check whether any directory patches exist for this entry.
#[must_use]
pub fn has_dir_patch(entry: &Entry, repo_root: &Path) -> bool {
    let d = patches_root(repo_root)
        .join(entry.entity_type.dir_name())
        .join(&entry.name);
    if !d.is_dir() {
        return false;
    }
    walkdir(&d)
        .into_iter()
        .any(|p| p.extension().is_some_and(|e| e == "patch"))
}

/// Write a per-file patch for a directory entry.
///
/// # Arguments
///
/// * `entry` - The directory entry this patch belongs to.
/// * `filename` - Relative filename within the entry (e.g. `"python.md"`).
/// * `patch_text` - Unified diff text to persist.
/// * `repo_root` - Repository root used to locate `.skillfile/patches/`.
#[allow(clippy::too_many_arguments)] // 4 args, each semantically distinct; no better grouping
pub fn write_dir_patch(
    entry: &Entry,
    filename: &str,
    patch_text: &str,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    let p = dir_patch_path(entry, filename, repo_root);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, patch_text)?;
    Ok(())
}

pub fn remove_dir_patch(
    entry: &Entry,
    filename: &str,
    repo_root: &Path,
) -> Result<(), SkillfileError> {
    let p = dir_patch_path(entry, filename, repo_root);
    if !p.exists() {
        return Ok(());
    }
    std::fs::remove_file(&p)?;
    remove_empty_parent(&p);
    Ok(())
}

pub fn remove_all_dir_patches(entry: &Entry, repo_root: &Path) -> Result<(), SkillfileError> {
    let d = patches_root(repo_root)
        .join(entry.entity_type.dir_name())
        .join(&entry.name);
    if d.is_dir() {
        std::fs::remove_dir_all(&d)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Remove `path`'s parent directory if it exists and is now empty. No-op otherwise.
fn remove_empty_parent(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if !parent.exists() {
        return;
    }
    let is_empty = std::fs::read_dir(parent)
        .map(|mut rd| rd.next().is_none())
        .unwrap_or(true);
    if is_empty {
        let _ = std::fs::remove_dir(parent);
    }
}

// ---------------------------------------------------------------------------
// Diff generation
// ---------------------------------------------------------------------------

/// Generate a unified diff of original → modified. Empty string if identical.
/// All output lines are guaranteed to end with '\n'.
/// Format: `--- a/{label}` / `+++ b/{label}`, 3 lines of context.
///
/// ```
/// use skillfile_core::patch::generate_patch;
///
/// // Identical content produces no patch
/// assert_eq!(generate_patch("hello\n", "hello\n", "test.md"), "");
///
/// // Different content produces a unified diff
/// let patch = generate_patch("old\n", "new\n", "test.md");
/// assert!(patch.contains("--- a/test.md"));
/// assert!(patch.contains("+++ b/test.md"));
/// ```
pub fn generate_patch(original: &str, modified: &str, label: &str) -> String {
    if original == modified {
        return String::new();
    }

    let diff = similar::TextDiff::from_lines(original, modified);
    let raw = format!(
        "{}",
        diff.unified_diff()
            .context_radius(3)
            .header(&format!("a/{label}"), &format!("b/{label}"))
    );

    if raw.is_empty() {
        return String::new();
    }

    // Post-process: remove "\ No newline at end of file" markers,
    // normalize any lines not ending with \n.
    let mut result = String::new();
    for line in raw.split_inclusive('\n') {
        normalize_diff_line(line, &mut result);
    }

    result
}

/// Process one line from a raw unified-diff output into `result`.
///
/// "\ No newline at end of file" markers are dropped (but a trailing newline is
/// ensured on the preceding content line). Every other line is guaranteed to end
/// with `'\n'`.
fn normalize_diff_line(line: &str, result: &mut String) {
    if line.starts_with("\\ ") {
        // "\ No newline at end of file" — ensure the preceding line ends with \n
        if !result.ends_with('\n') {
            result.push('\n');
        }
        return;
    }
    result.push_str(line);
    if !line.ends_with('\n') {
        result.push('\n');
    }
}

// ---------------------------------------------------------------------------
// Patch application (pure Rust, no subprocess)
// ---------------------------------------------------------------------------

struct Hunk {
    orig_start: usize, // 1-based line number from @@ header
    body: Vec<String>,
}

fn parse_hunks(patch_text: &str) -> Result<Vec<Hunk>, SkillfileError> {
    let lines: Vec<&str> = patch_text.split_inclusive('\n').collect();
    let mut pi = 0;

    // Skip file headers (--- / +++ lines)
    while pi < lines.len() && (lines[pi].starts_with("--- ") || lines[pi].starts_with("+++ ")) {
        pi += 1;
    }

    let mut hunks: Vec<Hunk> = Vec::new();

    while pi < lines.len() {
        let pl = lines[pi];
        if !pl.starts_with("@@ ") {
            pi += 1;
            continue;
        }

        // Parse hunk header: @@ -l[,s] +l[,s] @@
        // We only need orig_start (the -l part)
        let orig_start = pl
            .split_whitespace()
            .nth(1) // "-l[,s]"
            .and_then(|s| s.trim_start_matches('-').split(',').next())
            .and_then(|n| n.parse::<usize>().ok())
            .ok_or_else(|| SkillfileError::Manifest(format!("malformed hunk header: {pl:?}")))?;

        pi += 1;
        let body = collect_hunk_body(&lines, &mut pi);

        hunks.push(Hunk { orig_start, body });
    }

    Ok(hunks)
}

/// Collect the body lines of a single hunk, advancing `pi` past them.
///
/// Stops at the next `@@ `, `--- `, or `+++ ` line. Skips "\ No newline" markers.
fn collect_hunk_body(lines: &[&str], pi: &mut usize) -> Vec<String> {
    let mut body: Vec<String> = Vec::new();
    while *pi < lines.len() {
        let hl = lines[*pi];
        if hl.starts_with("@@ ") || hl.starts_with("--- ") || hl.starts_with("+++ ") {
            break;
        }
        if hl.starts_with("\\ ") {
            // "\ No newline at end of file" — skip
            *pi += 1;
            continue;
        }
        body.push(hl.to_string());
        *pi += 1;
    }
    body
}

fn try_hunk_at(lines: &[String], start: usize, ctx_lines: &[&str]) -> bool {
    if start + ctx_lines.len() > lines.len() {
        return false;
    }
    for (i, expected) in ctx_lines.iter().enumerate() {
        if lines[start + i].trim_end_matches('\n') != *expected {
            return false;
        }
    }
    true
}

/// Groups the search inputs for hunk-position lookup to stay within the 3-argument limit.
struct HunkSearch<'a> {
    lines: &'a [String],
    min_pos: usize,
}

impl HunkSearch<'_> {
    /// Scan outward from `center` within ±100 lines for a position where the hunk context matches.
    fn search_nearby(&self, center: usize, ctx_lines: &[&str]) -> Option<usize> {
        (1..100usize)
            .flat_map(|delta| [Some(center + delta), center.checked_sub(delta)])
            .flatten()
            .filter(|&c| c >= self.min_pos && c <= self.lines.len())
            .find(|&c| try_hunk_at(self.lines, c, ctx_lines))
    }
}

fn find_hunk_position(
    ctx: &HunkSearch<'_>,
    hunk_start: usize,
    ctx_lines: &[&str],
) -> Result<usize, SkillfileError> {
    if try_hunk_at(ctx.lines, hunk_start, ctx_lines) {
        return Ok(hunk_start);
    }

    if let Some(pos) = ctx.search_nearby(hunk_start, ctx_lines) {
        return Ok(pos);
    }

    if !ctx_lines.is_empty() {
        return Err(SkillfileError::Manifest(format!(
            "context mismatch: cannot find context starting with {:?} near line {}",
            ctx_lines[0],
            hunk_start + 1
        )));
    }
    Err(SkillfileError::Manifest(
        "patch extends beyond end of file".into(),
    ))
}

/// State threaded through hunk application in [`apply_patch_pure`].
struct PatchState<'a> {
    lines: &'a [String],
    output: Vec<String>,
    pos: usize,
}

impl<'a> PatchState<'a> {
    fn new(lines: &'a [String]) -> Self {
        Self {
            lines,
            output: Vec::new(),
            pos: 0,
        }
    }

    /// Apply a single hunk body line (`hl`) from a unified diff, updating `output` and `pos`.
    fn apply_line(&mut self, hl: &str) {
        let Some(prefix) = hl.as_bytes().first() else {
            return;
        };
        match prefix {
            b' ' if self.pos < self.lines.len() => {
                self.output.push(self.lines[self.pos].clone());
                self.pos += 1;
            }
            b'-' => self.pos += 1,
            b'+' => self.output.push(hl[1..].to_string()),
            _ => {} // context beyond EOF or unrecognized — skip
        }
    }

    /// Apply all body lines of a hunk, updating `output` and `pos`.
    fn apply_hunk(&mut self, hunk: &Hunk) {
        for hl in &hunk.body {
            self.apply_line(hl);
        }
    }
}

/// Apply a unified diff to original text, returning modified content.
/// Pure implementation — no subprocess, no `patch` binary required.
/// Only handles patches produced by [`generate_patch()`] (unified diff format).
/// Returns an error if the patch does not apply cleanly.
///
/// ```
/// use skillfile_core::patch::{generate_patch, apply_patch_pure};
///
/// let original = "line1\nline2\nline3\n";
/// let modified = "line1\nchanged\nline3\n";
/// let patch = generate_patch(original, modified, "test.md");
/// let result = apply_patch_pure(original, &patch).unwrap();
/// assert_eq!(result, modified);
/// ```
pub fn apply_patch_pure(original: &str, patch_text: &str) -> Result<String, SkillfileError> {
    if patch_text.is_empty() {
        return Ok(original.to_string());
    }

    // Split into lines preserving newlines (like Python's splitlines(keepends=True))
    let lines: Vec<String> = original
        .split_inclusive('\n')
        .map(std::string::ToString::to_string)
        .collect();

    let mut state = PatchState::new(&lines);

    for hunk in parse_hunks(patch_text)? {
        // Build context: lines with ' ' or '-' prefix, stripped of prefix and trailing \n
        let ctx_lines: Vec<&str> = hunk
            .body
            .iter()
            .filter(|hl| !hl.is_empty() && (hl.starts_with(' ') || hl.starts_with('-')))
            .map(|hl| hl[1..].trim_end_matches('\n'))
            .collect();

        let search = HunkSearch {
            lines: &lines,
            min_pos: state.pos,
        };
        let hunk_start =
            find_hunk_position(&search, hunk.orig_start.saturating_sub(1), &ctx_lines)?;

        // Copy unchanged lines before this hunk
        state
            .output
            .extend_from_slice(&lines[state.pos..hunk_start]);
        state.pos = hunk_start;

        state.apply_hunk(&hunk);
    }

    // Copy remaining lines
    state.output.extend_from_slice(&lines[state.pos..]);
    Ok(state.output.concat())
}

// ---------------------------------------------------------------------------
// Directory walking helper
// ---------------------------------------------------------------------------

/// Recursively list all files under a directory, sorted.
#[must_use]
pub fn walkdir(dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    walkdir_inner(dir, &mut result);
    result.sort();
    result
}

fn walkdir_inner(dir: &Path, result: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walkdir_inner(&path, result);
        } else {
            result.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{EntityType, SourceFields};

    fn github_entry(name: &str, entity_type: EntityType) -> Entry {
        Entry {
            entity_type,
            name: name.to_string(),
            source: SourceFields::Github {
                owner_repo: "owner/repo".into(),
                path_in_repo: "agents/test.md".into(),
                ref_: "main".into(),
            },
        }
    }

    // --- generate_patch ---

    #[test]
    fn generate_patch_identical_returns_empty() {
        assert_eq!(generate_patch("hello\n", "hello\n", "test.md"), "");
    }

    #[test]
    fn generate_patch_has_headers() {
        let p = generate_patch("old\n", "new\n", "test.md");
        assert!(p.contains("--- a/test.md"), "missing fromfile header");
        assert!(p.contains("+++ b/test.md"), "missing tofile header");
    }

    #[test]
    fn generate_patch_add_line() {
        let p = generate_patch("line1\n", "line1\nline2\n", "test.md");
        assert!(p.contains("+line2"));
    }

    #[test]
    fn generate_patch_remove_line() {
        let p = generate_patch("line1\nline2\n", "line1\n", "test.md");
        assert!(p.contains("-line2"));
    }

    #[test]
    fn generate_patch_all_lines_end_with_newline() {
        let p = generate_patch("a\nb\n", "a\nc\n", "test.md");
        for seg in p.split_inclusive('\n') {
            assert!(seg.ends_with('\n'), "line does not end with \\n: {seg:?}");
        }
    }

    // --- apply_patch_pure ---

    #[test]
    fn apply_patch_empty_patch_returns_original() {
        let result = apply_patch_pure("hello\n", "").unwrap();
        assert_eq!(result, "hello\n");
    }

    #[test]
    fn apply_patch_round_trip_add_line() {
        let orig = "line1\nline2\n";
        let modified = "line1\nline2\nline3\n";
        let patch = generate_patch(orig, modified, "test.md");
        let result = apply_patch_pure(orig, &patch).unwrap();
        assert_eq!(result, modified);
    }

    #[test]
    fn apply_patch_round_trip_remove_line() {
        let orig = "line1\nline2\nline3\n";
        let modified = "line1\nline3\n";
        let patch = generate_patch(orig, modified, "test.md");
        let result = apply_patch_pure(orig, &patch).unwrap();
        assert_eq!(result, modified);
    }

    #[test]
    fn apply_patch_round_trip_modify_line() {
        let orig = "# Title\n\nSome text here.\n";
        let modified = "# Title\n\nSome modified text here.\n";
        let patch = generate_patch(orig, modified, "test.md");
        let result = apply_patch_pure(orig, &patch).unwrap();
        assert_eq!(result, modified);
    }

    #[test]
    fn apply_patch_multi_hunk() {
        use std::fmt::Write;
        let mut orig = String::new();
        for i in 0..20 {
            let _ = writeln!(orig, "line{i}");
        }
        let mut modified = orig.clone();
        modified = modified.replace("line2\n", "MODIFIED2\n");
        modified = modified.replace("line15\n", "MODIFIED15\n");
        let patch = generate_patch(&orig, &modified, "test.md");
        assert!(patch.contains("@@"), "should have hunk headers");
        let result = apply_patch_pure(&orig, &patch).unwrap();
        assert_eq!(result, modified);
    }

    #[test]
    fn apply_patch_context_mismatch_errors() {
        let orig = "line1\nline2\n";
        let patch = "--- a/test.md\n+++ b/test.md\n@@ -1,2 +1,2 @@\n-totally_wrong\n+new\n";
        let result = apply_patch_pure(orig, patch);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("context mismatch"));
    }

    // --- Patch path helpers ---

    #[test]
    fn patch_path_single_file_agent() {
        let entry = github_entry("my-agent", EntityType::Agent);
        let root = Path::new("/repo");
        let p = patch_path(&entry, root);
        assert_eq!(
            p,
            Path::new("/repo/.skillfile/patches/agents/my-agent.patch")
        );
    }

    #[test]
    fn patch_path_single_file_skill() {
        let entry = github_entry("my-skill", EntityType::Skill);
        let root = Path::new("/repo");
        let p = patch_path(&entry, root);
        assert_eq!(
            p,
            Path::new("/repo/.skillfile/patches/skills/my-skill.patch")
        );
    }

    #[test]
    fn dir_patch_path_returns_correct() {
        let entry = github_entry("lang-pro", EntityType::Skill);
        let root = Path::new("/repo");
        let p = dir_patch_path(&entry, "python.md", root);
        assert_eq!(
            p,
            Path::new("/repo/.skillfile/patches/skills/lang-pro/python.md.patch")
        );
    }

    #[test]
    fn write_read_remove_patch_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let entry = github_entry("test-agent", EntityType::Agent);
        let patch_text = "--- a/test-agent.md\n+++ b/test-agent.md\n@@ -1 +1 @@\n-old\n+new\n";
        write_patch(&entry, patch_text, dir.path()).unwrap();
        assert!(has_patch(&entry, dir.path()));
        let read = read_patch(&entry, dir.path()).unwrap();
        assert_eq!(read, patch_text);
        remove_patch(&entry, dir.path()).unwrap();
        assert!(!has_patch(&entry, dir.path()));
    }

    #[test]
    fn has_dir_patch_detects_patches() {
        let dir = tempfile::tempdir().unwrap();
        let entry = github_entry("lang-pro", EntityType::Skill);
        assert!(!has_dir_patch(&entry, dir.path()));
        write_dir_patch(&entry, "python.md", "patch content", dir.path()).unwrap();
        assert!(has_dir_patch(&entry, dir.path()));
    }

    #[test]
    fn remove_all_dir_patches_clears_dir() {
        let dir = tempfile::tempdir().unwrap();
        let entry = github_entry("lang-pro", EntityType::Skill);
        write_dir_patch(&entry, "python.md", "p1", dir.path()).unwrap();
        write_dir_patch(&entry, "typescript.md", "p2", dir.path()).unwrap();
        assert!(has_dir_patch(&entry, dir.path()));
        remove_all_dir_patches(&entry, dir.path()).unwrap();
        assert!(!has_dir_patch(&entry, dir.path()));
    }

    // --- remove_patch: no-op when patch does not exist ---

    #[test]
    fn remove_patch_nonexistent_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let entry = github_entry("ghost-agent", EntityType::Agent);
        // No patch was written — remove_patch must return Ok without panicking.
        assert!(!has_patch(&entry, dir.path()));
        remove_patch(&entry, dir.path()).unwrap();
        assert!(!has_patch(&entry, dir.path()));
    }

    // --- remove_patch: parent directory cleaned up when empty ---

    #[test]
    fn remove_patch_cleans_up_empty_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let entry = github_entry("solo-skill", EntityType::Skill);
        write_patch(&entry, "some patch text\n", dir.path()).unwrap();

        // Confirm that the parent directory (.skillfile/patches/skills/) was created.
        let parent = patches_root(dir.path()).join("skills");
        assert!(parent.is_dir(), "parent dir should exist after write_patch");

        remove_patch(&entry, dir.path()).unwrap();

        // The patch file and the now-empty parent dir should both be gone.
        assert!(
            !has_patch(&entry, dir.path()),
            "patch file should be removed"
        );
        assert!(
            !parent.exists(),
            "empty parent dir should be removed after last patch is deleted"
        );
    }

    // --- remove_patch: parent directory NOT cleaned up when non-empty ---

    #[test]
    fn remove_patch_keeps_parent_dir_when_nonempty() {
        let dir = tempfile::tempdir().unwrap();
        let entry_a = github_entry("skill-a", EntityType::Skill);
        let entry_b = github_entry("skill-b", EntityType::Skill);
        write_patch(&entry_a, "patch a\n", dir.path()).unwrap();
        write_patch(&entry_b, "patch b\n", dir.path()).unwrap();

        let parent = patches_root(dir.path()).join("skills");
        remove_patch(&entry_a, dir.path()).unwrap();

        // skill-b.patch still lives there — parent dir must NOT be removed.
        assert!(
            parent.is_dir(),
            "parent dir must survive when another patch still exists"
        );
        assert!(has_patch(&entry_b, dir.path()));
    }

    // --- remove_dir_patch: no-op when patch does not exist ---

    #[test]
    fn remove_dir_patch_nonexistent_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let entry = github_entry("ghost-skill", EntityType::Skill);
        // No patch was written — must return Ok without panicking.
        remove_dir_patch(&entry, "missing.md", dir.path()).unwrap();
    }

    // --- remove_dir_patch: entry-specific directory cleaned up when empty ---

    #[test]
    fn remove_dir_patch_cleans_up_empty_entry_dir() {
        let dir = tempfile::tempdir().unwrap();
        let entry = github_entry("lang-pro", EntityType::Skill);
        write_dir_patch(&entry, "python.md", "patch text\n", dir.path()).unwrap();

        // The entry-specific directory (.skillfile/patches/skills/lang-pro/) should exist.
        let entry_dir = patches_root(dir.path()).join("skills").join("lang-pro");
        assert!(
            entry_dir.is_dir(),
            "entry dir should exist after write_dir_patch"
        );

        remove_dir_patch(&entry, "python.md", dir.path()).unwrap();

        // The single patch is gone — the entry dir should be removed too.
        assert!(
            !entry_dir.exists(),
            "entry dir should be removed when it becomes empty"
        );
    }

    // --- remove_dir_patch: entry-specific directory kept when non-empty ---

    #[test]
    fn remove_dir_patch_keeps_entry_dir_when_nonempty() {
        let dir = tempfile::tempdir().unwrap();
        let entry = github_entry("lang-pro", EntityType::Skill);
        write_dir_patch(&entry, "python.md", "p1\n", dir.path()).unwrap();
        write_dir_patch(&entry, "typescript.md", "p2\n", dir.path()).unwrap();

        let entry_dir = patches_root(dir.path()).join("skills").join("lang-pro");
        remove_dir_patch(&entry, "python.md", dir.path()).unwrap();

        // typescript.md.patch still exists — entry dir must be kept.
        assert!(
            entry_dir.is_dir(),
            "entry dir must survive when another patch still exists"
        );
    }

    // --- generate_patch: inputs without trailing newline ---

    #[test]
    fn generate_patch_no_trailing_newline_original() {
        // original has no trailing \n; all output lines must still end with \n.
        let p = generate_patch("old text", "new text\n", "test.md");
        assert!(!p.is_empty(), "patch should not be empty");
        for seg in p.split_inclusive('\n') {
            assert!(
                seg.ends_with('\n'),
                "every output line must end with \\n, got: {seg:?}"
            );
        }
    }

    #[test]
    fn generate_patch_no_trailing_newline_modified() {
        // modified has no trailing \n; all output lines must still end with \n.
        let p = generate_patch("old text\n", "new text", "test.md");
        assert!(!p.is_empty(), "patch should not be empty");
        for seg in p.split_inclusive('\n') {
            assert!(
                seg.ends_with('\n'),
                "every output line must end with \\n, got: {seg:?}"
            );
        }
    }

    #[test]
    fn generate_patch_both_inputs_no_trailing_newline() {
        // Neither original nor modified ends with \n.
        let p = generate_patch("old line", "new line", "test.md");
        assert!(!p.is_empty(), "patch should not be empty");
        for seg in p.split_inclusive('\n') {
            assert!(
                seg.ends_with('\n'),
                "every output line must end with \\n, got: {seg:?}"
            );
        }
    }

    #[test]
    fn generate_patch_no_trailing_newline_roundtrip() {
        // apply_patch_pure must reconstruct the modified text even when neither
        // side ends with a newline.
        let orig = "line one\nline two";
        let modified = "line one\nline changed";
        let patch = generate_patch(orig, modified, "test.md");
        assert!(!patch.is_empty());
        // The patch must normalise to a clean result — at minimum not error.
        let result = apply_patch_pure(orig, &patch).unwrap();
        // The applied result should match modified (possibly with a trailing newline
        // added by the normalization, so we compare trimmed content).
        assert_eq!(
            result.trim_end_matches('\n'),
            modified.trim_end_matches('\n')
        );
    }

    // --- apply_patch_pure: "\ No newline at end of file" marker in patch ---

    #[test]
    fn apply_patch_pure_with_no_newline_marker() {
        // A patch that was generated externally may contain the "\ No newline at
        // end of file" marker.  parse_hunks() must skip it cleanly.
        let orig = "line1\nline2\n";
        let patch = concat!(
            "--- a/test.md\n",
            "+++ b/test.md\n",
            "@@ -1,2 +1,2 @@\n",
            " line1\n",
            "-line2\n",
            "+changed\n",
            "\\ No newline at end of file\n",
        );
        let result = apply_patch_pure(orig, patch).unwrap();
        assert_eq!(result, "line1\nchanged\n");
    }

    // --- walkdir: edge cases ---

    #[test]
    fn walkdir_empty_directory_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let files = walkdir(dir.path());
        assert!(
            files.is_empty(),
            "walkdir of empty dir should return empty vec"
        );
    }

    #[test]
    fn walkdir_nonexistent_directory_returns_empty() {
        let path = Path::new("/tmp/skillfile_test_does_not_exist_xyz_9999");
        let files = walkdir(path);
        assert!(
            files.is_empty(),
            "walkdir of non-existent dir should return empty vec"
        );
    }

    #[test]
    fn walkdir_nested_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.path().join("top.txt"), "top").unwrap();
        std::fs::write(sub.join("nested.txt"), "nested").unwrap();

        let files = walkdir(dir.path());
        assert_eq!(files.len(), 2, "should find both files");

        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"top.txt".to_string()));
        assert!(names.contains(&"nested.txt".to_string()));
    }

    #[test]
    fn walkdir_results_are_sorted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("z.txt"), "z").unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(dir.path().join("m.txt"), "m").unwrap();

        let files = walkdir(dir.path());
        let sorted = {
            let mut v = files.clone();
            v.sort();
            v
        };
        assert_eq!(files, sorted, "walkdir results must be sorted");
    }

    // --- apply_patch_pure: fuzzy hunk matching ---

    #[test]
    fn apply_patch_pure_fuzzy_hunk_matching() {
        use std::fmt::Write;
        // Build an original with 20 lines.
        let mut orig = String::new();
        for i in 1..=20 {
            let _ = writeln!(orig, "line{i}");
        }

        // Construct a patch whose hunk header claims the context starts at line 5
        // (1-based), but the actual content we want to change is at line 7.
        // find_hunk_position will search ±100 lines and should find the match.
        let patch = concat!(
            "--- a/test.md\n",
            "+++ b/test.md\n",
            "@@ -5,3 +5,3 @@\n", // header says line 5, but context matches line 7
            " line7\n",
            "-line8\n",
            "+CHANGED8\n",
            " line9\n",
        );

        let result = apply_patch_pure(&orig, patch).unwrap();
        assert!(
            result.contains("CHANGED8\n"),
            "fuzzy match should have applied the change"
        );
        assert!(
            !result.contains("line8\n"),
            "original line8 should have been replaced"
        );
    }

    // --- apply_patch_pure: patch extends beyond end of file ---

    #[test]
    fn apply_patch_pure_extends_beyond_eof_errors() {
        // A patch with an empty context list and hunk start beyond the file length
        // triggers the "patch extends beyond end of file" error path in
        // find_hunk_position when ctx_lines is empty.
        //
        // We craft a hunk header that places the hunk at line 999 of a 2-line file
        // and supply a context line that won't match anywhere — this exercises the
        // "context mismatch" branch (which is what fires when ctx_lines is non-empty
        // and nothing is found within ±100 of the declared position).
        let orig = "line1\nline2\n";
        let patch = concat!(
            "--- a/test.md\n",
            "+++ b/test.md\n",
            "@@ -999,1 +999,1 @@\n",
            "-nonexistent_line\n",
            "+replacement\n",
        );
        let result = apply_patch_pure(orig, patch);
        assert!(
            result.is_err(),
            "applying a patch beyond EOF should return an error"
        );
    }
}
