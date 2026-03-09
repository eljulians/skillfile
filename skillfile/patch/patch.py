from __future__ import annotations

import difflib
from pathlib import Path

from skillfile.core.models import Entry

PATCHES_DIR = ".skillfile/patches"


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


class _Hunk:
    __slots__ = ("orig_start", "body")

    def __init__(self, orig_start: int, body: list[str]):
        self.orig_start = orig_start
        self.body = body


def _parse_hunks(patch_text: str) -> list[_Hunk]:
    """Parse a unified diff into a list of hunks (skipping file headers)."""
    import re

    patch_lines = patch_text.splitlines(keepends=True)
    pi = 0

    # Skip file headers (--- / +++ lines)
    while pi < len(patch_lines) and (patch_lines[pi].startswith("--- ") or patch_lines[pi].startswith("+++ ")):
        pi += 1

    hunks: list[_Hunk] = []
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

        body: list[str] = []
        while pi < len(patch_lines):
            hl = patch_lines[pi]
            if hl.startswith("@@ ") or hl.startswith("--- ") or hl.startswith("+++ "):
                break
            if hl.startswith("\\ "):  # "No newline at end of file"
                pi += 1
                continue
            body.append(hl)
            pi += 1

        hunks.append(_Hunk(orig_start, body))

    return hunks


def _find_hunk_position(lines: list[str], hunk_start: int, ctx_lines: list[str], min_pos: int) -> int:
    """Find where a hunk's context matches in lines. Returns 0-based position.

    Tries exact position first, then scans nearby offsets (like GNU patch fuzz).
    Raises PatchConflictError if no match found.
    """
    if _try_hunk_at(lines, hunk_start, ctx_lines):
        return hunk_start

    for delta in range(1, 100):
        for candidate in (hunk_start + delta, hunk_start - delta):
            if candidate < min_pos or candidate > len(lines):
                continue
            if _try_hunk_at(lines, candidate, ctx_lines):
                return candidate

    if ctx_lines:
        raise PatchConflictError(
            f"context mismatch: cannot find context starting with {ctx_lines[0]!r} near line {hunk_start + 1}"
        )
    raise PatchConflictError("patch extends beyond end of file")


def apply_patch_pure(original: str, patch_text: str) -> str:
    """Apply a unified diff to original text, returning modified content.

    Pure-Python implementation — no subprocess, no `patch` binary required.
    Only handles patches produced by generate_patch() (difflib.unified_diff format).
    Raises PatchConflictError if the patch does not apply cleanly.
    """
    if not patch_text:
        return original

    lines = original.splitlines(keepends=True)
    output: list[str] = []
    li = 0  # current position in `lines` (0-based)

    for hunk in _parse_hunks(patch_text):
        ctx_lines = [hl[1:].rstrip("\n") for hl in hunk.body if hl and hl[0] in (" ", "-")]
        hunk_start = _find_hunk_position(lines, hunk.orig_start - 1, ctx_lines, li)

        # Copy unchanged lines before this hunk
        output.extend(lines[li:hunk_start])
        li = hunk_start

        # Apply hunk: emit context and additions, skip removals.
        for hl in hunk.body:
            if not hl:
                continue
            if hl[0] == " ":
                output.append(lines[li])
                li += 1
            elif hl[0] == "-":
                li += 1
            elif hl[0] == "+":
                output.append(hl[1:])

    output.extend(lines[li:])
    return "".join(output)
