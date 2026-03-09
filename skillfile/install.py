import argparse
import shutil
import sys
from pathlib import Path

from .conflict import ConflictState, read_conflict, write_conflict
from .exceptions import InstallError, ManifestError
from .lock import lock_key, read_lock, write_lock
from .models import Entry, InstallTarget, Manifest
from .parser import MANIFEST_NAME, parse_manifest
from .patch import (
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
from .strategies import STRATEGIES
from .sync import sync_entry, vendor_dir_for

# Adapter target directories.
# Paths starting with '~' are global (expanded at runtime).
# Relative paths are local (resolved from repo_root).
ADAPTER_PATHS: dict[str, dict[str, dict[str, str]]] = {
    "claude-code": {
        "agent": {"global": "~/.claude/agents", "local": ".claude/agents"},
        "skill": {"global": "~/.claude/skills", "local": ".claude/skills"},
    },
}

KNOWN_ADAPTERS = list(ADAPTER_PATHS.keys())


def resolve_target_dir(adapter: str, entity_type: str, scope: str, repo_root: Path) -> Path:
    paths = ADAPTER_PATHS[adapter][entity_type]
    raw = paths[scope]
    if raw.startswith("~"):
        return Path(raw).expanduser()
    return repo_root / raw


def installed_path(entry: Entry, manifest: Manifest, repo_root: Path) -> Path:
    """Return the platform-side installed path for a single-file entry (first install target)."""
    if not manifest.install_targets:
        raise ManifestError("no install targets configured — run `skillfile install` first")
    target = manifest.install_targets[0]
    if target.adapter not in ADAPTER_PATHS:
        raise ManifestError(f"unknown adapter '{target.adapter}'")
    target_dir = resolve_target_dir(target.adapter, entry.entity_type, target.scope, repo_root)
    return target_dir / f"{entry.name}.md"


def installed_dir_files(entry: Entry, manifest: Manifest, repo_root: Path) -> dict[str, Path]:
    """Return {relative_path: installed_path} for a directory entry's installed files."""
    if not manifest.install_targets:
        raise ManifestError("no install targets configured — run `skillfile install` first")
    target = manifest.install_targets[0]
    if target.adapter not in ADAPTER_PATHS:
        raise ManifestError(f"unknown adapter '{target.adapter}'")
    target_dir = resolve_target_dir(target.adapter, entry.entity_type, target.scope, repo_root)

    if entry.entity_type == "skill":
        # Skill dirs are installed as a whole directory: target_dir/name/
        installed_dir = target_dir / entry.name
        if not installed_dir.is_dir():
            return {}
        return {str(f.relative_to(installed_dir)): f for f in installed_dir.rglob("*") if f.is_file()}
    else:
        # Agent dirs are exploded: each .md file at target_dir/filename (flat, recursive).
        # Key by relative path within vendor dir so pin/diff lookups match cache_file.relative_to(vdir).
        vdir = vendor_dir_for(entry, repo_root)
        if not vdir.is_dir():
            return {}
        result = {}
        for f in vdir.rglob("*.md"):
            relative_key = str(f.relative_to(vdir))
            installed = target_dir / f.name
            if installed.exists():
                result[relative_key] = installed
        return result


def _source_path(entry: Entry, repo_root: Path) -> Path | None:
    """Return the path to the source file or directory for an entry."""
    strategy = STRATEGIES[entry.source_type]
    if entry.source_type == "local":
        return repo_root / entry.local_path
    vdir = vendor_dir_for(entry, repo_root)
    if strategy.is_dir_entry(entry):
        return vdir if vdir.exists() else None
    filename = strategy.content_file(entry)
    if not filename:
        return None
    return vdir / filename


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
            from .patch import remove_patch

            remove_patch(entry, repo_root)


def _apply_skill_dir_patches(entry: Entry, installed_dir: Path, source_dir: Path, repo_root: Path) -> None:
    """Apply per-file patches to an installed skill directory copy."""
    patches_dir = patches_root(repo_root) / f"{entry.entity_type}s" / entry.name
    if not patches_dir.is_dir():
        return
    for patch_file in sorted(patches_dir.rglob("*.patch")):
        rel_str = str(patch_file.relative_to(patches_dir))[: -len(".patch")]
        target = installed_dir / rel_str
        cache_file = source_dir / rel_str
        if target.exists():
            _apply_patch_to_file(target, patch_file.read_text())
            # Rebase patch against new cache content.
            if cache_file.exists():
                new_patch = generate_patch(cache_file.read_text(), target.read_text(), rel_str)
                if new_patch:
                    write_dir_patch(entry, rel_str, new_patch, repo_root)
                else:
                    patch_file.unlink()


def _apply_agent_dir_patches(entry: Entry, target_dir: Path, source_dir: Path, repo_root: Path) -> None:
    """Apply per-file patches to installed agent files (exploded dir)."""
    patches_dir = patches_root(repo_root) / f"{entry.entity_type}s" / entry.name
    if not patches_dir.is_dir():
        return
    for patch_file in sorted(patches_dir.rglob("*.patch")):
        filename = str(patch_file.relative_to(patches_dir))[: -len(".patch")]
        target = target_dir / filename
        cache_file = source_dir / filename
        if target.exists():
            _apply_patch_to_file(target, patch_file.read_text())
            # Rebase patch against new cache content.
            if cache_file.exists():
                new_patch = generate_patch(cache_file.read_text(), target.read_text(), filename)
                if new_patch:
                    write_dir_patch(entry, filename, new_patch, repo_root)
                else:
                    patch_file.unlink()


# ---------------------------------------------------------------------------
# Low-level deploy helpers
# ---------------------------------------------------------------------------


def _install_one(source: Path, dest: Path, is_dir: bool, link_mode: bool, dry_run: bool, overwrite: bool) -> bool:
    """Copy (or link) source to dest. Returns True if deployed, False if skipped.

    Skips if overwrite=False and dest already exists as a regular file/dir (not a symlink).
    Symlinks are always replaced (migration from old symlink-based installs).
    """
    if not overwrite and not dry_run:
        if is_dir and dest.is_dir() and not dest.is_symlink():
            return False
        if not is_dir and dest.is_file() and not dest.is_symlink():
            return False

    label = f"  {source.name} -> {dest}"
    if dry_run:
        print(f"{label} [{'link' if link_mode else 'copy'}, dry-run]")
        return True

    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.exists() or dest.is_symlink():
        shutil.rmtree(dest) if (dest.is_dir() and not dest.is_symlink()) else dest.unlink()

    if link_mode:
        dest.symlink_to(source.resolve())
    else:
        shutil.copytree(source, dest) if is_dir else shutil.copy2(source, dest)

    print(label)
    return True


def _install_dir_exploded(source_dir: Path, target_dir: Path, link_mode: bool, dry_run: bool, overwrite: bool) -> None:
    """Install each .md file in source_dir as a separate entity in target_dir (flat, recursive)."""
    md_files = sorted(source_dir.rglob("*.md"))
    if dry_run:
        for src in md_files:
            print(f"  {src.name} -> {target_dir / src.name} [{'link' if link_mode else 'copy'}, dry-run]")
        return
    target_dir.mkdir(parents=True, exist_ok=True)
    for src in md_files:
        dest = target_dir / src.name
        if not overwrite and dest.is_file() and not dest.is_symlink():
            continue
        if dest.exists() or dest.is_symlink():
            dest.unlink()
        if link_mode:
            dest.symlink_to(src.resolve())
        else:
            shutil.copy2(src, dest)
        print(f"  {src.name} -> {dest}")


# ---------------------------------------------------------------------------
# Public deploy entry point
# ---------------------------------------------------------------------------


def install_entry(
    entry: Entry,
    target: InstallTarget,
    repo_root: Path,
    link_mode: bool = False,
    dry_run: bool = False,
    overwrite: bool = True,
) -> None:
    """Deploy one entry to its installed path.

    link_mode: use symlinks instead of copies (opt-in; tradeoffs apply).
    overwrite: if False, skip entries already installed as regular files.
    Raises PatchConflictError if a patch fails to apply.
    """
    if entry.entity_type not in ADAPTER_PATHS.get(target.adapter, {}):
        return

    source = _source_path(entry, repo_root)
    if source is None or not source.exists():
        print(f"  warning: source missing for {entry.name}, skipping", file=sys.stderr)
        return

    target_dir = resolve_target_dir(target.adapter, entry.entity_type, target.scope, repo_root)
    is_dir = STRATEGIES[entry.source_type].is_dir_entry(entry)

    if is_dir and entry.entity_type == "agent":
        # Agent dir: each .md file is a separate agent, deployed individually.
        _install_dir_exploded(source, target_dir, link_mode, dry_run, overwrite)
        # Apply per-file patches to installed copies (copy mode only — cache stays pristine).
        if not dry_run and not link_mode:
            _apply_agent_dir_patches(entry, target_dir, source, repo_root)
    else:
        # Single file or skill directory.
        dest = target_dir / (entry.name if is_dir else f"{entry.name}.md")
        deployed = _install_one(source, dest, is_dir=is_dir, link_mode=link_mode, dry_run=dry_run, overwrite=overwrite)
        # Apply patch to the installed copy (cache stays pristine).
        if deployed and not dry_run and not link_mode:
            if is_dir:
                _apply_skill_dir_patches(entry, dest, source, repo_root)
            else:
                _apply_single_file_patch(entry, dest, source, repo_root)


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


def cmd_install(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        raise ManifestError(f"{MANIFEST_NAME} not found in {repo_root}")

    manifest = parse_manifest(manifest_path)

    if not manifest.install_targets:
        raise ManifestError("No install targets configured. Run `skillfile init` first.")

    # Block if a conflict is pending — must be resolved before installing.
    conflict = read_conflict(repo_root)
    if conflict:
        raise InstallError(
            f"pending conflict for '{conflict.entry}' — "
            f"run `skillfile diff {conflict.entry}` to review, "
            f"or `skillfile resolve {conflict.entry}` to merge"
        )

    link_mode = getattr(args, "link", False)
    dry_run = getattr(args, "dry_run", False)
    update = getattr(args, "update", False)
    mode = " [dry-run]" if dry_run else ""

    # Snapshot lock before sync so we have old SHAs for conflict state if needed.
    locked = read_lock(repo_root)
    old_locked = dict(locked)  # copy — sync mutates locked in-place

    # Auto-pin local edits before re-fetching upstream (--update only).
    # This preserves any user modifications to installed files as patches,
    # so they survive the upcoming cache refresh.
    if update and not dry_run:
        for entry in manifest.entries:
            _auto_pin_entry(entry, manifest, repo_root, locked)

    # Fetch any missing or stale entries before deploying.
    for entry in manifest.entries:
        locked = sync_entry(entry, repo_root, dry_run=dry_run, locked=locked, update=update)
    if not dry_run:
        write_lock(repo_root, locked)

    # Deploy to all configured platform targets.
    # With --update: overwrite=True (always redeploy with merged upstream).
    # Without --update: overwrite=False (skip existing regular files, preserving user edits).
    for target in manifest.install_targets:
        if target.adapter not in ADAPTER_PATHS:
            print(f"warning: unknown platform '{target.adapter}', skipping", file=sys.stderr)
            continue
        print(f"Installing for {target.adapter} ({target.scope}){mode}...")
        for entry in manifest.entries:
            try:
                install_entry(entry, target, repo_root, link_mode, dry_run, overwrite=update)
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

    if not dry_run:
        print("Done.")
