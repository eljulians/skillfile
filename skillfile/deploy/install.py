"""Entry deployment: copy/symlink to platform directories, apply patches."""

import argparse
import sys
from pathlib import Path

from skillfile.core.conflict import ConflictState, read_conflict, write_conflict
from skillfile.core.lock import lock_key, read_lock, write_lock
from skillfile.core.models import Entry, InstallOptions, InstallTarget, Manifest, SyncContext
from skillfile.core.parser import MANIFEST_NAME, parse_manifest
from skillfile.deploy.adapter import ADAPTERS
from skillfile.deploy.paths import _source_path, installed_dir_files, installed_path
from skillfile.exceptions import InstallError, ManifestError
from skillfile.patch.patch import (
    PatchConflictError,
    apply_patch_pure,
    dir_patch_path,
    generate_patch,
    has_patch,
    patches_root,
    read_patch,
    write_dir_patch,
    write_patch,
)
from skillfile.sources.strategies import STRATEGIES
from skillfile.sources.sync import sync_entries_parallel, vendor_dir_for

# ---------------------------------------------------------------------------
# Patch application helpers — apply patches to installed copies, NOT the cache
# ---------------------------------------------------------------------------


def _apply_patch_to_file(target: Path, patch_text: str) -> None:
    """Apply a unified diff patch to target in-place using pure Python. Raises PatchConflictError."""
    original = target.read_text()
    patched = apply_patch_pure(original, patch_text)
    target.write_text(patched)


def _apply_single_file_patch(entry: Entry, dest: Path, source: Path, repo_root: Path) -> None:
    """Apply stored patch (if any) to the installed file. Raises PatchConflictError on failure.

    After successful application, rebases the stored patch against the current cache
    so that status comparisons work correctly after upstream updates.
    """
    if has_patch(entry, repo_root):
        _apply_patch_to_file(dest, read_patch(entry, repo_root))
        # Rebase: regenerate patch against the new cache content.
        new_patch = generate_patch(source.read_text(), dest.read_text(), f"{entry.name}.md")
        if new_patch:
            write_patch(entry, new_patch, repo_root)
        else:
            from skillfile.patch.patch import remove_patch

            remove_patch(entry, repo_root)


def _apply_dir_patches(entry: Entry, installed_files: dict[str, Path], source_dir: Path, repo_root: Path) -> None:
    """Apply per-file patches to installed copies of a directory entry.

    installed_files: maps relative_path → installed Path (computed by caller
    based on entity type: skill dirs keep structure, agent dirs explode flat).
    """
    patches_dir = patches_root(repo_root) / f"{entry.entity_type}s" / entry.name
    if not patches_dir.is_dir():
        return
    for patch_file in sorted(patches_dir.rglob("*.patch")):
        rel_str = str(patch_file.relative_to(patches_dir))[: -len(".patch")]
        target = installed_files.get(rel_str)
        if target is None or not target.exists():
            continue
        cache_file = source_dir / rel_str
        _apply_patch_to_file(target, patch_file.read_text())
        if cache_file.exists():
            new_patch = generate_patch(cache_file.read_text(), target.read_text(), rel_str)
            if new_patch:
                write_dir_patch(entry, rel_str, new_patch, repo_root)
            else:
                patch_file.unlink()


# ---------------------------------------------------------------------------
# Public deploy entry point
# ---------------------------------------------------------------------------


def install_entry(
    entry: Entry,
    target: InstallTarget,
    repo_root: Path,
    opts: InstallOptions | None = None,
) -> None:
    """Deploy one entry to its installed path via the platform adapter.

    The adapter owns all platform-specific logic (target dirs, flat vs. nested).
    This function only handles cross-cutting concerns: source resolution,
    missing-source warnings, and patch application.

    Raises PatchConflictError if a stored patch fails to apply.
    """
    if opts is None:
        opts = InstallOptions()

    adapter = ADAPTERS.get(target.adapter)
    if adapter is None or not adapter.supports(entry.entity_type):
        return

    source = _source_path(entry, repo_root)
    if source is None or not source.exists():
        print(f"  warning: source missing for {entry.name}, skipping", file=sys.stderr)
        return

    is_dir = STRATEGIES[entry.source_type].is_dir_entry(entry)
    installed = adapter.deploy_entry(entry, source, target.scope, repo_root, opts)

    if installed and not opts.dry_run and not opts.link_mode:
        if is_dir:
            _apply_dir_patches(entry, installed, source, repo_root)
        else:
            _apply_single_file_patch(entry, installed[f"{entry.name}.md"], source, repo_root)


# ---------------------------------------------------------------------------
# Auto-pin helpers — run before sync on install --update
# ---------------------------------------------------------------------------


def _auto_pin_entry(entry: Entry, manifest: Manifest, repo_root: Path, locked: dict) -> None:
    """Compare installed vs cache; write patch if they differ. Silent on any missing prerequisite."""
    if entry.source_type == "local":
        return

    strategy = STRATEGIES[entry.source_type]
    key = lock_key(entry)
    if key not in locked:
        return

    vdir = vendor_dir_for(entry, repo_root)

    if strategy.is_dir_entry(entry):
        _auto_pin_dir_entry(entry, manifest, repo_root, vdir)
        return

    content_file = strategy.content_file(entry)
    if not content_file:
        return
    cache_file = vdir / content_file
    if not cache_file.exists():
        return

    dest = installed_path(entry, manifest, repo_root)
    if not dest.exists() or dest.is_symlink():
        return

    cache_text = cache_file.read_text()
    installed_text = dest.read_text()

    # If already pinned, check whether the stored patch still describes the installed
    # content exactly (cache + patch = installed → no new edits to save).
    # If apply_patch_pure raises, the cache is inconsistent with the stored patch
    # (e.g. the vendor cache was manually edited).  In both cases, keep the existing
    # patch rather than overwriting it with one generated from a bad cache.
    if has_patch(entry, repo_root):
        try:
            expected = apply_patch_pure(cache_text, read_patch(entry, repo_root))
            if installed_text == expected:
                return  # no new edits beyond the stored pin
            # installed has additional edits on top of the pin — fall through to re-pin
        except PatchConflictError:
            return  # cache inconsistent with stored patch — preserve existing patch

    patch_text = generate_patch(cache_text, installed_text, f"{entry.name}.md")
    if patch_text:
        write_patch(entry, patch_text, repo_root)
        print(f"  {entry.name}: local changes auto-saved to Skillfile.patches/")


def _auto_pin_dir_entry(entry: Entry, manifest: Manifest, repo_root: Path, vdir: Path) -> None:
    """Auto-pin each modified file in a dir entry's installed copy."""
    if not vdir.is_dir():
        return

    installed = installed_dir_files(entry, manifest, repo_root)
    if not installed:
        return

    pinned: list[str] = []
    for cache_file in sorted(vdir.rglob("*")):
        if cache_file.is_dir() or cache_file.name == ".meta":
            continue
        filename = str(cache_file.relative_to(vdir))
        inst_path = installed.get(filename)
        if inst_path is None or not inst_path.exists() or inst_path.is_symlink():
            continue
        cache_text = cache_file.read_text()
        installed_text = inst_path.read_text()
        p = dir_patch_path(entry, filename, repo_root)
        if p.exists():
            try:
                expected = apply_patch_pure(cache_text, p.read_text())
                if installed_text == expected:
                    continue  # no new edits
            except PatchConflictError:
                continue  # cache inconsistent — preserve existing patch

        patch_text = generate_patch(cache_text, installed_text, filename)
        if patch_text:
            write_dir_patch(entry, filename, patch_text, repo_root)
            pinned.append(filename)

    if pinned:
        print(f"  {entry.name}: local changes auto-saved to Skillfile.patches/ ({', '.join(pinned)})")


# ---------------------------------------------------------------------------
# cmd_install
# ---------------------------------------------------------------------------


def _check_preconditions(manifest: Manifest, repo_root: Path) -> None:
    """Raise on missing install targets or pending conflict."""
    if not manifest.install_targets:
        raise ManifestError("No install targets configured. Run `skillfile init` first.")

    conflict = read_conflict(repo_root)
    if conflict:
        raise InstallError(
            f"pending conflict for '{conflict.entry}' — "
            f"run `skillfile diff {conflict.entry}` to review, "
            f"or `skillfile resolve {conflict.entry}` to merge"
        )


def _deploy_all(
    manifest: Manifest,
    repo_root: Path,
    opts: InstallOptions,
    locked: dict,
    old_locked: dict,
) -> None:
    """Deploy all entries to all install targets. Handles PatchConflictError → conflict state."""
    mode = " [dry-run]" if opts.dry_run else ""
    for target in manifest.install_targets:
        if target.adapter not in ADAPTERS:
            print(f"warning: unknown platform '{target.adapter}', skipping", file=sys.stderr)
            continue
        print(f"Installing for {target.adapter} ({target.scope}){mode}...")
        for entry in manifest.entries:
            try:
                install_entry(entry, target, repo_root, opts)
            except PatchConflictError:
                key = lock_key(entry)
                old_sha = old_locked[key].sha if key in old_locked else (locked[key].sha if key in locked else "")
                new_sha = locked[key].sha if key in locked else old_sha
                write_conflict(
                    repo_root,
                    ConflictState(
                        entry=entry.name,
                        entity_type=entry.entity_type,
                        old_sha=old_sha,
                        new_sha=new_sha,
                    ),
                )
                raise InstallError(
                    f"upstream changes to '{entry.name}' conflict with your customisations.\n"
                    f"Run `skillfile diff {entry.name}` to review what changed upstream.\n"
                    f"Run `skillfile resolve {entry.name}` when ready to merge."
                )


def cmd_install(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        raise ManifestError(f"{MANIFEST_NAME} not found in {repo_root}")

    manifest = parse_manifest(manifest_path)
    _check_preconditions(manifest, repo_root)

    dry_run = getattr(args, "dry_run", False)
    update = getattr(args, "update", False)
    link_mode = getattr(args, "link", False)

    locked = read_lock(repo_root)
    old_locked = dict(locked)

    # Auto-pin local edits before re-fetching upstream (--update only).
    if update and not dry_run:
        for entry in manifest.entries:
            _auto_pin_entry(entry, manifest, repo_root, locked)

    # Fetch any missing or stale entries.
    ctx = SyncContext(repo_root=repo_root, dry_run=dry_run, update=update)
    locked = sync_entries_parallel(manifest.entries, ctx, locked)
    if not dry_run:
        write_lock(repo_root, locked)

    # Deploy to all configured platform targets.
    opts = InstallOptions(link_mode=link_mode, dry_run=dry_run, overwrite=update)
    _deploy_all(manifest, repo_root, opts, locked, old_locked)

    if not dry_run:
        print("Done.")
