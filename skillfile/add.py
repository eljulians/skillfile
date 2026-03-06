import argparse
import sys
from pathlib import Path

from .install import install_entry, resolve_target_dir, ADAPTER_PATHS
from .lock import read_lock, write_lock
from .models import Entry
from .parser import DEFAULT_REF, MANIFEST_NAME, _infer_name, parse_manifest
from .sync import sync_entry


def _format_line(entry: Entry) -> str:
    """Format an entry as a minimal Skillfile line, omitting default name and ref."""
    parts = [entry.source_type, entry.entity_type]

    if entry.source_type == "github":
        if entry.name != _infer_name(entry.path_in_repo):
            parts.append(entry.name)
        parts.append(entry.owner_repo)
        parts.append(entry.path_in_repo)
        if entry.ref != DEFAULT_REF:
            parts.append(entry.ref)

    elif entry.source_type == "local":
        if entry.name != _infer_name(entry.local_path):
            parts.append(entry.name)
        parts.append(entry.local_path)

    elif entry.source_type == "url":
        if entry.name != _infer_name(entry.url):
            parts.append(entry.name)
        parts.append(entry.url)

    return "  ".join(parts)


def cmd_add(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        print(f"error: {MANIFEST_NAME} not found in {repo_root}", file=sys.stderr)
        sys.exit(1)

    source_type = args.add_source
    entity_type = args.entity_type

    if source_type == "github":
        path = args.path
        name = args.name or _infer_name(path)
        ref = args.ref or DEFAULT_REF
        entry = Entry("github", entity_type, name,
                      owner_repo=args.owner_repo,
                      path_in_repo=path,
                      ref=ref)
    elif source_type == "local":
        name = args.name or _infer_name(args.path)
        entry = Entry("local", entity_type, name, local_path=args.path)
    elif source_type == "url":
        name = args.name or _infer_name(args.url)
        entry = Entry("url", entity_type, name, url=args.url)
    else:
        print(f"error: unknown source type '{source_type}'", file=sys.stderr)
        sys.exit(1)

    manifest = parse_manifest(manifest_path)
    existing = {e.name for e in manifest.entries}
    if entry.name in existing:
        print(f"error: entry '{entry.name}' already exists in {MANIFEST_NAME}", file=sys.stderr)
        sys.exit(1)

    line = _format_line(entry)
    original = manifest_path.read_text()
    with open(manifest_path, "a") as f:
        f.write(line + "\n")

    print(f"Added: {line}")

    manifest = parse_manifest(manifest_path)
    if not manifest.install_targets:
        print("No install targets configured — run `skillfile init` then `skillfile install` to deploy.")
        return

    try:
        locked = read_lock(repo_root)
        locked = sync_entry(entry, repo_root, dry_run=False, locked=locked, update=False)
        write_lock(repo_root, locked)
        for target in manifest.install_targets:
            if target.adapter in ADAPTER_PATHS:
                install_entry(entry, target, repo_root, copy_mode=False, dry_run=False)
    except SystemExit:
        manifest_path.write_text(original)
        print(f"Rolled back: removed '{entry.name}' from {MANIFEST_NAME}", file=sys.stderr)
        raise
