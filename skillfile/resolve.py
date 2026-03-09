from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
from pathlib import Path

from .conflict import clear_conflict, read_conflict
from .exceptions import InstallError, ManifestError
from .install import installed_dir_files, installed_path
from .parser import MANIFEST_NAME, find_entry, parse_manifest
from .patch import (
    apply_patch_pure,
    dir_patch_path,
    generate_patch,
    has_patch,
    read_patch,
    remove_all_dir_patches,
    remove_patch,
    write_dir_patch,
    write_patch,
)
from .strategies import STRATEGIES


def _three_way_merge(base: str, theirs: str, yours: str, filename: str) -> tuple[str, bool]:
    """
    Merge theirs into yours using base as common ancestor.
    Returns (merged_content, has_conflicts).
    Uses `git merge-file -p` which writes to stdout and exits non-zero if conflicts remain.
    """
    with tempfile.TemporaryDirectory() as tmpdir:
        base_f = Path(tmpdir) / f"base_{filename}"
        theirs_f = Path(tmpdir) / f"theirs_{filename}"
        yours_f = Path(tmpdir) / f"yours_{filename}"
        base_f.write_text(base, encoding="utf-8")
        theirs_f.write_text(theirs, encoding="utf-8")
        yours_f.write_text(yours, encoding="utf-8")

        try:
            result = subprocess.run(
                ["git", "merge-file", "-p", "--diff3", str(yours_f), str(base_f), str(theirs_f)],
                capture_output=True,
                text=True,
            )
        except FileNotFoundError:
            raise InstallError("`git` not found — install git to use `skillfile resolve`")

        # exit 0 = clean merge, >0 = conflicts, <0 = error
        if result.returncode < 0:
            raise InstallError(f"git merge-file failed: {result.stderr.strip()}")

        return result.stdout, result.returncode > 0


def _open_in_editor(content: str, filename: str) -> str:
    """Write content to a temp file, open in $MERGETOOL or $EDITOR, return result."""
    editor = os.environ.get("MERGETOOL") or os.environ.get("EDITOR") or "vi"
    with tempfile.NamedTemporaryFile(suffix=f"_{filename}", mode="w", delete=False, encoding="utf-8") as f:
        f.write(content)
        tmp = Path(f.name)
    try:
        subprocess.run([editor, str(tmp)], check=True)
        return tmp.read_text(encoding="utf-8")
    finally:
        tmp.unlink(missing_ok=True)


def _resolve_single_file(entry, manifest, conflict, repo_root: Path) -> None:
    """Three-way merge for a single-file entry."""
    filename = f"{entry.name}.md"
    strategy = STRATEGIES[entry.source_type]

    print(f"  fetching upstream at old sha={conflict.old_sha[:12]} (common ancestor) ...", end=" ", flush=True)
    base = strategy.fetch_original(entry, conflict.old_sha)
    print("done")

    print(f"  fetching upstream at new sha={conflict.new_sha[:12]} ...", end=" ", flush=True)
    theirs = strategy.fetch_original(entry, conflict.new_sha)
    print("done")

    installed = installed_path(entry, manifest, repo_root)

    # Reconstruct "yours" from the stored patch applied to the base upstream.
    # We cannot use the installed file: install --update overwrites it with pristine
    # upstream content before the patch application fails and the conflict is raised.
    # The stored patch is generate_patch(base_content, user_edits), so:
    #   user_edits = apply_patch_pure(base, stored_patch)
    if has_patch(entry, repo_root):
        yours = apply_patch_pure(base, read_patch(entry, repo_root))
    else:
        if not installed.exists():
            raise ManifestError(f"'{entry.name}' is not installed at {installed}")
        yours = installed.read_text(encoding="utf-8")

    print("  merging ...", end=" ", flush=True)
    merged, has_conflicts = _three_way_merge(base, theirs, yours, filename)
    print("done")

    if has_conflicts:
        print(
            f"\nConflicts detected in '{entry.name}'. Opening in editor to resolve...\n"
            "  Save and close when done. Conflict markers must be removed before re-pinning.\n"
        )
        merged = _open_in_editor(merged, filename)
        if "<<<<<<" in merged:
            print(
                "error: conflict markers still present — resolve all conflicts and try again",
                file=sys.stderr,
            )
            return
    else:
        print(f"  clean merge — no conflicts in '{entry.name}'")

    # Write merged result to installed path
    installed.write_text(merged, encoding="utf-8")

    # Regenerate patch: diff between new upstream and merged result
    patch_text = generate_patch(theirs, merged, filename)
    if patch_text:
        write_patch(entry, patch_text, repo_root)
        print(f"  updated Skillfile.patches/ for '{entry.name}'")
    else:
        remove_patch(entry, repo_root)
        print(f"  merged result matches upstream — removed pin for '{entry.name}'")

    clear_conflict(repo_root)
    print(f"\nResolved. Run `skillfile install` to deploy '{entry.name}'.")


def _resolve_dir_entry(entry, manifest, conflict, repo_root: Path) -> None:
    """Three-way merge for each file in a directory entry."""
    strategy = STRATEGIES[entry.source_type]

    print(f"  fetching upstream at old sha={conflict.old_sha[:12]} (common ancestor) ...", end=" ", flush=True)
    base_files = strategy.fetch_dir_files(entry, conflict.old_sha)
    print("done")

    print(f"  fetching upstream at new sha={conflict.new_sha[:12]} ...", end=" ", flush=True)
    theirs_files = strategy.fetch_dir_files(entry, conflict.new_sha)
    print("done")

    installed = installed_dir_files(entry, manifest, repo_root)
    if not installed:
        raise ManifestError(f"'{entry.name}' is not installed")

    # Merge each file; collect results
    all_filenames = sorted(set(theirs_files) | set(base_files))
    merged_results: dict[str, str] = {}
    any_conflict = False

    for filename in all_filenames:
        base = base_files.get(filename, "")
        theirs = theirs_files.get(filename, "")
        # Reconstruct "yours" from stored patch + base, for the same reason as the
        # single-file case: the installed file was overwritten before the conflict raised.
        p = dir_patch_path(entry, filename, repo_root)
        if p.exists():
            yours = apply_patch_pure(base, p.read_text())
        else:
            inst_path = installed.get(filename)
            yours = inst_path.read_text(encoding="utf-8") if inst_path and inst_path.exists() else base

        merged, has_conflicts = _three_way_merge(base, theirs, yours, filename)

        if has_conflicts:
            any_conflict = True
            print(f"\n  Conflicts in '{filename}'. Opening in editor...")
            merged = _open_in_editor(merged, filename)
            if "<<<<<<" in merged:
                print(
                    f"error: conflict markers still present in '{filename}' — resolve and try again",
                    file=sys.stderr,
                )
                return
        else:
            print(f"  {filename}: clean merge")

        merged_results[filename] = merged

    if not any_conflict:
        print(f"  all files merged cleanly in '{entry.name}'")

    # Write merged results and update patches
    remove_all_dir_patches(entry, repo_root)
    for filename, merged_text in merged_results.items():
        theirs = theirs_files.get(filename, "")
        inst_path = installed.get(filename)
        if inst_path:
            inst_path.write_text(merged_text, encoding="utf-8")

        patch_text = generate_patch(theirs, merged_text, filename)
        if patch_text:
            write_dir_patch(entry, filename, patch_text, repo_root)

    pinned = [f for f in merged_results if generate_patch(theirs_files.get(f, ""), merged_results[f], f)]
    if pinned:
        print(f"  updated Skillfile.patches/ for '{entry.name}' ({', '.join(pinned)})")
    else:
        print(f"  merged result matches upstream — no pin needed for '{entry.name}'")

    clear_conflict(repo_root)
    print(f"\nResolved. Run `skillfile install` to deploy '{entry.name}'.")


def cmd_resolve(args: argparse.Namespace, repo_root: Path) -> None:
    manifest = parse_manifest(repo_root / MANIFEST_NAME)
    entry = find_entry(args.name, repo_root / MANIFEST_NAME)

    conflict = read_conflict(repo_root)
    if conflict is None or conflict.entry != args.name:
        raise ManifestError(
            f"no pending conflict for '{args.name}' — "
            "`skillfile resolve` is only available after a conflict is detected by `skillfile install --update`"
        )

    strategy = STRATEGIES[entry.source_type]
    if strategy.is_dir_entry(entry):
        _resolve_dir_entry(entry, manifest, conflict, repo_root)
    else:
        _resolve_single_file(entry, manifest, conflict, repo_root)
