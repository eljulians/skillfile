import argparse
import sys
from pathlib import Path

from .lock import lock_key, read_lock
from .parser import MANIFEST_NAME, parse_manifest
from .resolver import resolve_github_sha
from .sync import _meta_sha, vendor_dir_for


def cmd_status(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        print(f"error: {MANIFEST_NAME} not found in {repo_root}", file=sys.stderr)
        sys.exit(1)

    entries = parse_manifest(manifest_path).entries
    locked = read_lock(repo_root)
    check_upstream = getattr(args, "check_upstream", False)

    col_w = max((len(e.name) for e in entries), default=10) + 2

    for entry in entries:
        key = lock_key(entry)
        name = entry.name

        if entry.source_type == "local":
            print(f"{name:<{col_w}} local")
            continue

        locked_info = locked.get(key)
        if not locked_info:
            print(f"{name:<{col_w}} unlocked")
            continue

        sha = locked_info.sha
        vdir = vendor_dir_for(entry, repo_root)
        meta_sha = _meta_sha(vdir)

        if meta_sha != sha:
            status = f"locked    sha={sha[:12]}  (missing or stale)"
        elif check_upstream and entry.source_type == "github":
            upstream_sha = resolve_github_sha(entry.owner_repo, entry.ref)
            if upstream_sha == sha:
                status = f"up to date  sha={sha[:12]}"
            else:
                status = f"outdated    locked={sha[:12]}  upstream={upstream_sha[:12]}"
        else:
            status = f"locked    sha={sha[:12]}"

        print(f"{name:<{col_w}} {status}")
