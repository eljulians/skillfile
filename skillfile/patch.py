from __future__ import annotations

import difflib
import subprocess
from pathlib import Path

from .exceptions import InstallError
from .models import Entry
from .strategies import STRATEGIES
from .sync import vendor_dir_for

PATCHES_DIR = "Skillfile.patches"


class PatchConflictError(Exception):
    pass


def patches_root(repo_root: Path) -> Path:
    return repo_root / PATCHES_DIR


# ---------------------------------------------------------------------------
# Single-file entry patches
# ---------------------------------------------------------------------------


def patch_path(entry: Entry, repo_root: Path) -> Path:
    return patches_root(repo_root) / f"{entry.entity_type}s" / f"{entry.name}.patch"


def has_patch(entry: Entry, repo_root: Path) -> bool:
    return patch_path(entry, repo_root).exists()


def write_patch(entry: Entry, patch_text: str, repo_root: Path) -> None:
    p = patch_path(entry, repo_root)
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(patch_text)


def remove_patch(entry: Entry, repo_root: Path) -> None:
    p = patch_path(entry, repo_root)
    if not p.exists():
        return
    p.unlink()
    parent = p.parent
    if parent.exists() and not any(parent.iterdir()):
        parent.rmdir()


def read_patch(entry: Entry, repo_root: Path) -> str:
    return patch_path(entry, repo_root).read_text()


# ---------------------------------------------------------------------------
# Directory entry patches  (one .patch file per modified file)
# ---------------------------------------------------------------------------


def dir_patch_path(entry: Entry, filename: str, repo_root: Path) -> Path:
    """Path for a per-file patch within a directory entry.
    e.g. Skillfile.patches/skills/architecture-patterns/SKILL.md.patch
    """
    return patches_root(repo_root) / f"{entry.entity_type}s" / entry.name / f"{filename}.patch"


def has_dir_patch(entry: Entry, repo_root: Path) -> bool:
    d = patches_root(repo_root) / f"{entry.entity_type}s" / entry.name
    return d.is_dir() and any(d.rglob("*.patch"))


def write_dir_patch(entry: Entry, filename: str, patch_text: str, repo_root: Path) -> None:
    p = dir_patch_path(entry, filename, repo_root)
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(patch_text)


def remove_dir_patch(entry: Entry, filename: str, repo_root: Path) -> None:
    p = dir_patch_path(entry, filename, repo_root)
    if not p.exists():
        return
    p.unlink()
    parent = p.parent
    if parent.exists() and not any(parent.iterdir()):
        parent.rmdir()


def remove_all_dir_patches(entry: Entry, repo_root: Path) -> None:
    """Remove all per-file patches for a directory entry."""
    import shutil

    d = patches_root(repo_root) / f"{entry.entity_type}s" / entry.name
    if d.is_dir():
        shutil.rmtree(d)


# ---------------------------------------------------------------------------
# Shared utilities
# ---------------------------------------------------------------------------


def generate_patch(original: str, modified: str, label: str) -> str:
    """Return unified diff of original → modified. Empty string if identical.

    All output lines are guaranteed to end with '\\n'.  difflib emits lines without
    trailing newline for files that lack one; we normalise them here so that
    patch_text.splitlines(keepends=True) always splits at the right boundaries.
    '\\  No newline at end of file' markers are discarded — the normalisation makes
    them redundant.
    """
    diff = difflib.unified_diff(
        original.splitlines(keepends=True),
        modified.splitlines(keepends=True),
        fromfile=f"a/{label}",
        tofile=f"b/{label}",
    )
    parts: list[str] = []
    for line in diff:
        if line.startswith("\\ "):
            # "\ No newline at end of file" — normalise the preceding line instead.
            if parts and not parts[-1].endswith("\n"):
                parts[-1] += "\n"
            continue
        if not line.endswith("\n"):
            line += "\n"
        parts.append(line)
    return "".join(parts)


def _try_hunk_at(lines: list[str], start: int, ctx_lines: list[str]) -> bool:
    """Check whether context/removal lines match at the given 0-based start position."""
    if start < 0 or start + len(ctx_lines) > len(lines):
        return False
    for i, expected in enumerate(ctx_lines):
        if lines[start + i].rstrip("\n") != expected:
            return False
    return True


def apply_patch_pure(original: str, patch_text: str) -> str:
    """Apply a unified diff to original text, returning modified content.

    Pure-Python implementation — no subprocess, no `patch` binary required.
    Only handles patches produced by generate_patch() (difflib.unified_diff format).
    Raises PatchConflictError if the patch does not apply cleanly.
    """
    import re

    if not patch_text:
        return original

    lines = original.splitlines(keepends=True)
    output: list[str] = []
    li = 0  # current position in `lines` (0-based)

    patch_lines = patch_text.splitlines(keepends=True)
    pi = 0

    # Skip file headers (--- / +++ lines)
    while pi < len(patch_lines) and (patch_lines[pi].startswith("--- ") or patch_lines[pi].startswith("+++ ")):
        pi += 1

    while pi < len(patch_lines):
        pl = patch_lines[pi]
        if not pl.startswith("@@ "):
            pi += 1
            continue

        m = re.match(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@", pl)
        if not m:
            raise PatchConflictError(f"malformed hunk header: {pl!r}")

        orig_start = int(m.group(1))  # 1-based
        pi += 1

        # Collect hunk body
        hunk: list[str] = []
        while pi < len(patch_lines):
            hl = patch_lines[pi]
            if hl.startswith("@@ ") or hl.startswith("--- ") or hl.startswith("+++ "):
                break
            if hl.startswith("\\ "):  # "No newline at end of file"
                pi += 1
                continue
            hunk.append(hl)
            pi += 1

        # Build list of expected context/removal lines for matching.
        ctx_lines = [hl[1:].rstrip("\n") for hl in hunk if hl and hl[0] in (" ", "-")]

        # Try exact position first, then scan nearby offsets (like GNU patch fuzz).
        hunk_start = orig_start - 1  # 0-based
        found = _try_hunk_at(lines, hunk_start, ctx_lines)
        if not found:
            # Scan forward and backward up to 100 lines from the expected position.
            # Only consider positions at or after `li` (where we've consumed to).
            for delta in range(1, 100):
                for candidate in (hunk_start + delta, hunk_start - delta):
                    if candidate < li or candidate > len(lines):
                        continue
                    if _try_hunk_at(lines, candidate, ctx_lines):
                        hunk_start = candidate
                        found = True
                        break
                if found:
                    break

        if not found:
            if ctx_lines:
                raise PatchConflictError(
                    f"context mismatch: cannot find context starting with {ctx_lines[0]!r} near line {orig_start}"
                )
            raise PatchConflictError("patch extends beyond end of file")

        # Copy unchanged lines before this hunk
        output.extend(lines[li:hunk_start])
        li = hunk_start

        # Apply hunk: emit context and additions, skip removals.
        # For context lines, emit the ORIGINAL line (preserves whether it had \n).
        for hl in hunk:
            if not hl:
                continue
            if hl[0] == " ":
                output.append(lines[li])  # original, not hl[1:], to keep its \n state
                li += 1
            elif hl[0] == "-":
                li += 1
            elif hl[0] == "+":
                output.append(hl[1:])

    # Append any remaining unchanged lines
    output.extend(lines[li:])
    return "".join(output)


def apply_patch_in_memory(content: str, patch_text: str) -> str:
    """Apply patch_text to content string, return result. Raises PatchConflictError on failure."""
    return apply_patch_pure(content, patch_text)


def apply_patch(target_file: Path, patch_text: str) -> None:
    """Apply a unified diff to target_file in-place. Raises PatchConflictError on failure."""
    try:
        result = subprocess.run(
            ["patch", "--quiet", str(target_file)],
            input=patch_text.encode(),
            capture_output=True,
        )
    except FileNotFoundError:
        raise InstallError("`patch` command not found — install it with your package manager")

    # Clean up artifacts left by patch on failure
    for suffix in [".orig", ".rej"]:
        Path(str(target_file) + suffix).unlink(missing_ok=True)

    if result.returncode != 0:
        raise PatchConflictError(result.stderr.decode().strip())


# ---------------------------------------------------------------------------
# apply_entry_patch — called by install after fetching
# ---------------------------------------------------------------------------


def apply_entry_patch(entry: Entry, repo_root: Path) -> None:
    """Apply stored patches to the cached files for an entry, if any exist."""
    if entry.source_type == "local":
        return

    strategy = STRATEGIES[entry.source_type]

    if strategy.is_dir_entry(entry):
        _apply_dir_patches(entry, repo_root)
        return

    if not has_patch(entry, repo_root):
        return

    content_file = strategy.content_file(entry)
    if not content_file:
        return

    vdir = vendor_dir_for(entry, repo_root)
    target = vdir / content_file
    if not target.exists():
        return

    apply_patch(target, read_patch(entry, repo_root))


def _apply_dir_patches(entry: Entry, repo_root: Path) -> None:
    patches_dir = patches_root(repo_root) / f"{entry.entity_type}s" / entry.name
    if not patches_dir.is_dir():
        return
    vdir = vendor_dir_for(entry, repo_root)
    for patch_file in sorted(patches_dir.rglob("*.patch")):
        # e.g. patches_dir/resources/playbook.md.patch → relative_path = resources/playbook.md
        rel_patch = patch_file.relative_to(patches_dir)
        rel_str = str(rel_patch)
        if rel_str.endswith(".patch"):
            rel_str = rel_str[: -len(".patch")]
        target = vdir / rel_str
        if target.exists():
            apply_patch(target, patch_file.read_text())
