from __future__ import annotations

import argparse
from pathlib import Path

from .exceptions import ManifestError
from .install import install_entry, installed_dir_files, installed_path
from .lock import lock_key, read_lock
from .models import Entry, Manifest
from .parser import MANIFEST_NAME, parse_manifest
from .patch import (
    generate_patch,
    has_dir_patch,
    has_patch,
    remove_all_dir_patches,
    remove_dir_patch,
    remove_patch,
    write_dir_patch,
    write_patch,
)
from .strategies import STRATEGIES
from .sync import vendor_dir_for


def _pin_dir_entry(entry: Entry, manifest: Manifest, repo_root: Path) -> str:
    """Pin a directory entry by comparing installed vs vendor cache. Local, no network."""
    vdir = vendor_dir_for(entry, repo_root)
    if not vdir.is_dir():
        raise ManifestError(f"'{entry.name}' is not cached — run `skillfile install` first")

    installed = installed_dir_files(entry, manifest, repo_root)
    if not installed:
        raise ManifestError(f"'{entry.name}' is not installed — run `skillfile install` first")

    pinned: list[str] = []
    for cache_file in sorted(vdir.rglob("*")):
        if cache_file.is_dir() or cache_file.name == ".meta":
            continue
        filename = str(cache_file.relative_to(vdir))
        inst_path = installed.get(filename)
        if inst_path is None or not inst_path.exists() or inst_path.is_symlink():
            continue
        original_text = cache_file.read_text()
        patch_text = generate_patch(original_text, inst_path.read_text(), filename)
        if patch_text:
            write_dir_patch(entry, filename, patch_text, repo_root)
            pinned.append(filename)
        else:
            remove_dir_patch(entry, filename, repo_root)

    if pinned:
        return f"Pinned '{entry.name}' ({', '.join(pinned)})"
    return f"'{entry.name}' matches upstream — nothing to pin"


def _pin_entry(entry: Entry, manifest: Manifest, repo_root: Path) -> str:
    """Pin one entry. Returns a status string. Raises ManifestError on hard errors."""
    if entry.source_type == "local":
        return f"'{entry.name}' is a local entry — skipped"

    strategy = STRATEGIES[entry.source_type]

    locked = read_lock(repo_root)
    key = lock_key(entry)
    if key not in locked:
        raise ManifestError(f"'{entry.name}' is not locked — run `skillfile install` first")

    if strategy.is_dir_entry(entry):
        return _pin_dir_entry(entry, manifest, repo_root)

    # Single-file: read from vendor cache (local, no network)
    vdir = vendor_dir_for(entry, repo_root)
    content_file = strategy.content_file(entry)
    cache_file = vdir / content_file if content_file else None
    if not cache_file or not cache_file.exists():
        raise ManifestError(f"'{entry.name}' is not cached — run `skillfile install` first")

    dest = installed_path(entry, manifest, repo_root)
    if not dest.exists() or dest.is_symlink():
        raise ManifestError(f"'{entry.name}' is not installed — run `skillfile install` first")

    label = f"{entry.name}.md"
    patch_text = generate_patch(cache_file.read_text(), dest.read_text(), label)

    if not patch_text:
        return f"'{entry.name}' matches upstream — nothing to pin"

    write_patch(entry, patch_text, repo_root)
    return f"Pinned '{entry.name}'"


def cmd_pin(args: argparse.Namespace, repo_root: Path) -> None:
    manifest = parse_manifest(repo_root / MANIFEST_NAME)
    matching = [e for e in manifest.entries if e.name == args.name]
    if not matching:
        raise ManifestError(f"no entry named '{args.name}' in {MANIFEST_NAME}")
    entry = matching[0]

    result = _pin_entry(entry, manifest, repo_root)
    if result.startswith("Pinned"):
        print(result + " — customisations saved to Skillfile.patches/")
    else:
        print(result)


def cmd_unpin(args: argparse.Namespace, repo_root: Path) -> None:
    manifest = parse_manifest(repo_root / MANIFEST_NAME)
    matching = [e for e in manifest.entries if e.name == args.name]
    if not matching:
        raise ManifestError(f"no entry named '{args.name}' in {MANIFEST_NAME}")
    entry = matching[0]

    single = has_patch(entry, repo_root)
    directory = has_dir_patch(entry, repo_root)

    if not single and not directory:
        print(f"'{args.name}' is not pinned")
        return

    if single:
        remove_patch(entry, repo_root)
    if directory:
        remove_all_dir_patches(entry, repo_root)

    # Restore pristine upstream from vendor cache immediately.
    # install_entry with overwrite=True will copy cache → installed path.
    # Patches are already removed above, so no patch is re-applied.
    for target in manifest.install_targets:
        install_entry(entry, target, repo_root, link_mode=False, dry_run=False, overwrite=True)

    print(f"Unpinned '{args.name}' — restored to upstream version")
