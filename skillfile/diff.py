from __future__ import annotations

import argparse
import difflib
import sys
from pathlib import Path

from .conflict import read_conflict
from .exceptions import ManifestError
from .install import installed_dir_files, installed_path
from .lock import lock_key, read_lock
from .models import Entry, Manifest
from .parser import MANIFEST_NAME, parse_manifest
from .strategies import STRATEGIES
from .sync import vendor_dir_for


def cmd_diff(args: argparse.Namespace, repo_root: Path) -> None:
    manifest = parse_manifest(repo_root / MANIFEST_NAME)
    matching = [e for e in manifest.entries if e.name == args.name]
    if not matching:
        raise ManifestError(f"no entry named '{args.name}' in {MANIFEST_NAME}")
    entry = matching[0]

    conflict = read_conflict(repo_root)
    if conflict is not None and conflict.entry == args.name:
        _diff_conflict(entry, args.name, conflict)
    else:
        _diff_local(entry, args.name, manifest, repo_root)


def _diff_conflict(entry: Entry, name: str, conflict) -> None:
    strategy = STRATEGIES[entry.source_type]

    if strategy.is_dir_entry(entry):
        _diff_conflict_dir(entry, name, conflict, strategy)
        return

    print(f"  fetching upstream at old sha={conflict.old_sha[:12]} ...", end=" ", flush=True)
    old_content = strategy.fetch_original(entry, conflict.old_sha)
    print("done")

    print(f"  fetching upstream at new sha={conflict.new_sha[:12]} ...", end=" ", flush=True)
    new_content = strategy.fetch_original(entry, conflict.new_sha)
    print("done\n")

    old_lines = old_content.splitlines(keepends=True)
    new_lines = new_content.splitlines(keepends=True)

    diff = list(
        difflib.unified_diff(
            old_lines,
            new_lines,
            fromfile=f"{name}.md (old upstream sha={conflict.old_sha[:12]})",
            tofile=f"{name}.md (new upstream sha={conflict.new_sha[:12]})",
        )
    )

    if not diff:
        print("No upstream changes detected (patch conflict may be due to local file drift).")
        return

    sys.stdout.writelines(diff)


def _diff_conflict_dir(entry: Entry, name: str, conflict, strategy) -> None:
    """Show per-file upstream deltas for a directory entry in conflict."""
    print(f"  fetching upstream at old sha={conflict.old_sha[:12]} ...", end=" ", flush=True)
    old_files = strategy.fetch_dir_files(entry, conflict.old_sha)
    print("done")

    print(f"  fetching upstream at new sha={conflict.new_sha[:12]} ...", end=" ", flush=True)
    new_files = strategy.fetch_dir_files(entry, conflict.new_sha)
    print("done\n")

    all_filenames = sorted(set(old_files) | set(new_files))
    any_diff = False
    for filename in all_filenames:
        old_content = old_files.get(filename, "")
        new_content = new_files.get(filename, "")
        diff = list(
            difflib.unified_diff(
                old_content.splitlines(keepends=True),
                new_content.splitlines(keepends=True),
                fromfile=f"{name}/{filename} (old upstream sha={conflict.old_sha[:12]})",
                tofile=f"{name}/{filename} (new upstream sha={conflict.new_sha[:12]})",
            )
        )
        if diff:
            any_diff = True
            sys.stdout.writelines(diff)

    if not any_diff:
        print("No upstream changes detected (patch conflict may be due to local file drift).")


def _diff_local(entry: Entry, name: str, manifest: Manifest, repo_root: Path) -> None:
    if entry.source_type == "local":
        print(f"'{name}' is a local entry — nothing to diff")
        return

    strategy = STRATEGIES[entry.source_type]

    locked = read_lock(repo_root)
    key = lock_key(entry)
    if key not in locked:
        raise ManifestError(f"'{name}' is not locked — run `skillfile install` first")

    sha = locked[key].sha

    if strategy.is_dir_entry(entry):
        _diff_local_dir(entry, name, sha, manifest, repo_root)
        return

    # Single-file: read from vendor cache (local, no network)
    vdir = vendor_dir_for(entry, repo_root)
    content_file = strategy.content_file(entry)
    cache_file = vdir / content_file if content_file else None
    if not cache_file or not cache_file.exists():
        raise ManifestError(f"'{name}' is not cached — run `skillfile install` first")

    dest = installed_path(entry, manifest, repo_root)
    if not dest.exists():
        raise ManifestError(f"'{name}' is not installed — run `skillfile install` first")

    upstream = cache_file.read_text()
    installed_text = dest.read_text()
    upstream_lines = upstream.splitlines(keepends=True)
    installed_lines = installed_text.splitlines(keepends=True)

    diff = list(
        difflib.unified_diff(
            upstream_lines,
            installed_lines,
            fromfile=f"a/{name}.md (upstream sha={sha[:12]})",
            tofile=f"b/{name}.md (installed)",
        )
    )

    if not diff:
        print(f"'{name}' is clean — no local modifications")
        return

    sys.stdout.writelines(diff)


def _diff_local_dir(entry: Entry, name: str, sha: str, manifest: Manifest, repo_root: Path) -> None:
    vdir = vendor_dir_for(entry, repo_root)
    if not vdir.is_dir():
        raise ManifestError(f"'{name}' is not cached — run `skillfile install` first")

    installed = installed_dir_files(entry, manifest, repo_root)
    if not installed:
        raise ManifestError(f"'{name}' is not installed — run `skillfile install` first")

    any_diff = False
    for cache_file in sorted(vdir.rglob("*")):
        if cache_file.is_dir() or cache_file.name == ".meta":
            continue
        filename = str(cache_file.relative_to(vdir))
        inst_path = installed.get(filename)
        if inst_path is None or not inst_path.exists():
            continue
        original_text = cache_file.read_text()
        installed_text = inst_path.read_text()
        diff = list(
            difflib.unified_diff(
                original_text.splitlines(keepends=True),
                installed_text.splitlines(keepends=True),
                fromfile=f"a/{name}/{filename} (upstream sha={sha[:12]})",
                tofile=f"b/{name}/{filename} (installed)",
            )
        )
        if diff:
            any_diff = True
            sys.stdout.writelines(diff)

    if not any_diff:
        print(f"'{name}' is clean — no local modifications")
