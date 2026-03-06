import argparse
import shutil
import sys
from pathlib import Path

from .lock import read_lock, write_lock
from .models import Entry, InstallTarget
from .parser import MANIFEST_NAME, parse_manifest
from .sync import _content_file, _is_dir_entry, sync_entry, vendor_dir_for

# Adapter target directories.
# Paths starting with '~' are global (expanded at runtime).
# Relative paths are local (resolved from repo_root).
ADAPTER_PATHS: dict[str, dict[str, dict[str, str]]] = {
    "claude-code": {
        "agent": {"global": "~/.claude/agents",  "local": ".claude/agents"},
        "skill": {"global": "~/.claude/skills",   "local": ".claude/skills"},
    },
}

KNOWN_ADAPTERS = list(ADAPTER_PATHS.keys())


def resolve_target_dir(adapter: str, entity_type: str, scope: str, repo_root: Path) -> Path:
    paths = ADAPTER_PATHS[adapter][entity_type]
    raw = paths[scope]
    if raw.startswith("~"):
        return Path(raw).expanduser()
    return repo_root / raw


def _source_path(entry: Entry, repo_root: Path) -> Path | None:
    """Return the path to the source file or directory for an entry."""
    if entry.source_type == "local":
        return repo_root / entry.local_path
    vdir = vendor_dir_for(entry, repo_root)
    if _is_dir_entry(entry):
        return vdir if vdir.exists() else None
    filename = _content_file(entry)
    if not filename:
        return None
    return vdir / filename


def install_entry(
    entry: Entry,
    target: InstallTarget,
    repo_root: Path,
    copy_mode: bool,
    dry_run: bool,
) -> None:
    if entry.entity_type not in ADAPTER_PATHS.get(target.adapter, {}):
        return

    source = _source_path(entry, repo_root)
    if source is None or not source.exists():
        print(f"  warning: source missing for {entry.name}, skipping", file=sys.stderr)
        return

    target_dir = resolve_target_dir(target.adapter, entry.entity_type, target.scope, repo_root)

    if _is_dir_entry(entry) and entry.entity_type == "agent":
        # A directory of agents: each .md file is a separate agent, deployed individually.
        # (Skills are a coherent unit — a skill IS its directory. Agents are not.)
        _install_dir_exploded(source, target_dir, copy_mode, dry_run)
    else:
        # Single file (any entity type) or skill directory.
        dest = target_dir / (entry.name if _is_dir_entry(entry) else f"{entry.name}.md")
        _install_one(source, dest, is_dir=_is_dir_entry(entry), copy_mode=copy_mode, dry_run=dry_run)


def _install_one(source: Path, dest: Path, is_dir: bool, copy_mode: bool, dry_run: bool) -> None:
    label = f"  {source.name} -> {dest}"
    if dry_run:
        print(f"{label} [{'copy' if copy_mode else 'symlink'}, dry-run]")
        return
    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.exists() or dest.is_symlink():
        shutil.rmtree(dest) if (dest.is_dir() and not dest.is_symlink()) else dest.unlink()
    if copy_mode:
        shutil.copytree(source, dest) if is_dir else shutil.copy2(source, dest)
    else:
        dest.symlink_to(source.resolve())
    print(label)


def _install_dir_exploded(source_dir: Path, target_dir: Path, copy_mode: bool, dry_run: bool) -> None:
    """Install each .md file in source_dir as a separate entity in target_dir."""
    md_files = sorted(f for f in source_dir.iterdir() if f.suffix == ".md")
    if dry_run:
        for src in md_files:
            print(f"  {src.name} -> {target_dir / src.name} [{'copy' if copy_mode else 'symlink'}, dry-run]")
        return
    target_dir.mkdir(parents=True, exist_ok=True)
    for src in md_files:
        dest = target_dir / src.name
        if dest.exists() or dest.is_symlink():
            dest.unlink()
        if copy_mode:
            shutil.copy2(src, dest)
        else:
            dest.symlink_to(src.resolve())
        print(f"  {src.name} -> {dest}")


def cmd_install(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        print(f"error: {MANIFEST_NAME} not found in {repo_root}", file=sys.stderr)
        sys.exit(1)

    manifest = parse_manifest(manifest_path)

    if not manifest.install_targets:
        print("No install targets configured. Run `skillfile init` first.", file=sys.stderr)
        sys.exit(1)

    copy_mode = getattr(args, "copy", False)
    dry_run = getattr(args, "dry_run", False)
    update = getattr(args, "update", False)
    mode = " [dry-run]" if dry_run else ""

    # Fetch any missing or stale entries before deploying.
    locked = read_lock(repo_root)
    for entry in manifest.entries:
        locked = sync_entry(entry, repo_root, dry_run=dry_run, locked=locked, update=update)
    if not dry_run:
        write_lock(repo_root, locked)

    # Deploy to all configured platform targets.
    for target in manifest.install_targets:
        if target.adapter not in ADAPTER_PATHS:
            print(f"warning: unknown platform '{target.adapter}', skipping", file=sys.stderr)
            continue
        print(f"Installing for {target.adapter} ({target.scope}){mode}...")
        for entry in manifest.entries:
            install_entry(entry, target, repo_root, copy_mode, dry_run)

    if not dry_run:
        print("Done.")
