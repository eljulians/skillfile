import argparse
import shutil
from pathlib import Path

from skillfile.core.lock import lock_key, read_lock, write_lock
from skillfile.core.parser import MANIFEST_NAME, find_entry_in, parse_manifest
from skillfile.exceptions import ManifestError
from skillfile.sources.strategies import STRATEGIES
from skillfile.sources.sync import vendor_dir_for


def _name_from_parts(parts: list[str]) -> str | None:
    """Return the entry name if parts parse as a valid entry, else None."""
    if len(parts) < 3:
        return None
    strategy = STRATEGIES.get(parts[0])
    if strategy is None:
        return None
    e = strategy.parse(parts, 0)
    return e.name if e else None


def cmd_remove(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        raise ManifestError(f"{MANIFEST_NAME} not found in {repo_root}")

    name = args.name
    manifest = parse_manifest(manifest_path)
    entry = find_entry_in(name, manifest)

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
