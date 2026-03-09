import argparse
import sys
from pathlib import Path

from .exceptions import ManifestError, SkillfileError
from .install import ADAPTER_PATHS, install_entry
from .lock import read_lock, write_lock
from .models import Entry
from .parser import MANIFEST_NAME, parse_manifest
from .strategies import STRATEGIES
from .sync import sync_entry


def _format_line(entry: Entry) -> str:
    """Format an entry as a Skillfile line."""
    parts = [entry.source_type, entry.entity_type]
    parts.extend(STRATEGIES[entry.source_type].format_parts(entry))
    return "  ".join(parts)


def cmd_add(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        raise ManifestError(f"{MANIFEST_NAME} not found in {repo_root}")

    source_type = args.add_source
    if source_type not in STRATEGIES:
        raise ManifestError(f"unknown source type '{source_type}'")

    entity_type = args.entity_type
    entry = STRATEGIES[source_type].from_args(args, entity_type)

    manifest = parse_manifest(manifest_path)
    existing = {e.name for e in manifest.entries}
    if entry.name in existing:
        raise ManifestError(f"entry '{entry.name}' already exists in {MANIFEST_NAME}")

    line = _format_line(entry)
    original_manifest = manifest_path.read_text()
    with open(manifest_path, "a") as f:
        f.write(line + "\n")

    print(f"Added: {line}")

    manifest = parse_manifest(manifest_path)
    if not manifest.install_targets:
        print("No install targets configured — run `skillfile init` then `skillfile install` to deploy.")
        return

    lock_path = repo_root / "Skillfile.lock"
    original_lock = lock_path.read_text() if lock_path.exists() else None

    try:
        locked = read_lock(repo_root)
        locked = sync_entry(entry, repo_root, dry_run=False, locked=locked, update=False)
        write_lock(repo_root, locked)
        for target in manifest.install_targets:
            if target.adapter in ADAPTER_PATHS:
                install_entry(entry, target, repo_root, dry_run=False)
    except SkillfileError:
        manifest_path.write_text(original_manifest)
        if original_lock is None:
            lock_path.unlink(missing_ok=True)
        else:
            lock_path.write_text(original_lock)
        print(f"Rolled back: removed '{entry.name}' from {MANIFEST_NAME}", file=sys.stderr)
        raise
