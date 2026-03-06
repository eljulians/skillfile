import argparse
import sys
from pathlib import Path

from .install import ADAPTER_PATHS
from .parser import MANIFEST_NAME, parse_manifest


def cmd_validate(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        print(f"error: {MANIFEST_NAME} not found in {repo_root}", file=sys.stderr)
        sys.exit(1)

    # parse_manifest already emits warnings for malformed lines.
    manifest = parse_manifest(manifest_path)
    errors: list[str] = []

    # Duplicate entry names.
    seen: dict[str, str] = {}
    for entry in manifest.entries:
        if entry.name in seen:
            errors.append(
                f"duplicate name '{entry.name}' "
                f"({seen[entry.name]} and {entry.source_type})"
            )
        else:
            seen[entry.name] = entry.source_type

    # Missing local paths.
    for entry in manifest.entries:
        if entry.source_type == "local":
            p = repo_root / entry.local_path
            if not p.exists():
                errors.append(f"local path not found: '{entry.local_path}' (entry: {entry.name})")

    # Unknown platforms.
    for target in manifest.install_targets:
        if target.adapter not in ADAPTER_PATHS:
            errors.append(f"unknown platform: '{target.adapter}'")

    if errors:
        for msg in errors:
            print(f"error: {msg}", file=sys.stderr)
        sys.exit(1)

    n = len(manifest.entries)
    t = len(manifest.install_targets)
    print(f"Skillfile OK — {n} entr{'y' if n == 1 else 'ies'}, {t} install target{'s' if t != 1 else ''}")
