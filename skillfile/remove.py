import argparse
import shutil
import sys
from pathlib import Path

from .lock import lock_key, read_lock, write_lock
from .parser import (
    MANIFEST_NAME,
    _parse_github,
    _parse_local,
    _parse_url,
    parse_manifest,
)
from .sync import vendor_dir_for


def _name_from_parts(parts: list[str]) -> str | None:
    """Return the entry name if parts parse as a valid entry, else None."""
    if len(parts) < 3:
        return None
    source_type = parts[0]
    if source_type == "github":
        e = _parse_github(parts, 0)
    elif source_type == "local":
        e = _parse_local(parts, 0)
    elif source_type == "url":
        e = _parse_url(parts, 0)
    else:
        return None
    return e.name if e else None


def cmd_remove(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        print(f"error: {MANIFEST_NAME} not found in {repo_root}", file=sys.stderr)
        sys.exit(1)

    name = args.name
    manifest = parse_manifest(manifest_path)
    matching = [e for e in manifest.entries if e.name == name]

    if not matching:
        print(f"error: no entry named '{name}' in {MANIFEST_NAME}", file=sys.stderr)
        sys.exit(1)

    entry = matching[0]

    # Remove the matching line from Skillfile.
    lines = manifest_path.read_text().splitlines(keepends=True)
    new_lines = []
    removed = False
    for line in lines:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            new_lines.append(line)
            continue
        parts = stripped.split()
        if not removed and _name_from_parts(parts) == name:
            removed = True
            continue
        new_lines.append(line)
    manifest_path.write_text("".join(new_lines))

    # Remove from lock.
    locked = read_lock(repo_root)
    key = lock_key(entry)
    if key in locked:
        del locked[key]
        write_lock(repo_root, locked)

    # Remove cache directory.
    vdir = vendor_dir_for(entry, repo_root)
    if vdir.exists():
        shutil.rmtree(vdir)
        print(f"Removed cache: {vdir}")

    print(f"Removed: {name}")
