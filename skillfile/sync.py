import argparse
import json
import sys
from pathlib import Path

from .lock import lock_key, read_lock, write_lock
from .models import Entry, LockEntry
from .parser import MANIFEST_NAME, parse_manifest
from .resolver import _get, fetch_github_file, resolve_github_sha

VENDOR_DIR = ".skillfile"


def vendor_dir_for(entry: Entry, repo_root: Path) -> Path:
    return repo_root / VENDOR_DIR / f"{entry.entity_type}s" / entry.name


def _meta_sha(vdir: Path) -> str | None:
    """Return the SHA recorded in .meta, or None if missing/unreadable."""
    meta_path = vdir / ".meta"
    if not meta_path.exists():
        return None
    try:
        return json.loads(meta_path.read_text()).get("sha")
    except (json.JSONDecodeError, OSError):
        return None


def _content_file(entry: Entry) -> str:
    """Return the expected content filename for a vendored entry."""
    if entry.source_type == "github":
        effective_path = "SKILL.md" if entry.path_in_repo == "." else entry.path_in_repo
        return Path(effective_path).name
    if entry.source_type == "url":
        return Path(entry.url).name or "content.md"
    return ""


def sync_entry(
    entry: Entry,
    repo_root: Path,
    dry_run: bool,
    locked: dict[str, LockEntry],
    update: bool,
) -> dict[str, LockEntry]:
    """Sync a single entry. Returns updated locked dict."""
    label = f"  {entry.source_type}/{entry.entity_type}/{entry.name}"

    if entry.source_type == "local":
        print(f"{label}: local — skipping")
        return locked

    vdir = vendor_dir_for(entry, repo_root)
    key = lock_key(entry)

    if entry.source_type == "github":
        locked_sha = None if update else (locked[key].sha if key in locked else None)
        meta_sha = _meta_sha(vdir)

        content_exists = (vdir / _content_file(entry)).exists()
        if locked_sha and meta_sha == locked_sha and content_exists:
            print(f"{label}: up to date (sha={locked_sha[:12]})")
            return locked

        if locked_sha and not update:
            print(f"{label}: re-fetching (locked sha={locked_sha[:12]}) ...", end=" ", flush=True)
            sha = locked_sha
        else:
            print(f"{label}: resolving {entry.owner_repo}@{entry.ref} ...", end=" ", flush=True)
            if dry_run:
                print("[dry-run]")
                return locked
            sha = resolve_github_sha(entry.owner_repo, entry.ref)
            print(f"sha={sha[:12]}", end=" ", flush=True)

        if dry_run:
            print("[dry-run]")
            return locked

        content = fetch_github_file(entry.owner_repo, entry.path_in_repo, sha)

        effective_path = "SKILL.md" if entry.path_in_repo == "." else entry.path_in_repo
        filename = Path(effective_path).name

        vdir.mkdir(parents=True, exist_ok=True)
        (vdir / filename).write_bytes(content)

        raw_url = f"https://raw.githubusercontent.com/{entry.owner_repo}/{sha}/{effective_path}"
        meta = {
            "source_type": "github",
            "owner_repo": entry.owner_repo,
            "path_in_repo": entry.path_in_repo,
            "ref": entry.ref,
            "sha": sha,
            "raw_url": raw_url,
        }
        (vdir / ".meta").write_text(json.dumps(meta, indent=2) + "\n")

        locked[key] = LockEntry(sha=sha, raw_url=raw_url)
        print(f"-> {vdir / filename}")

    elif entry.source_type == "url":
        print(f"{label}: fetching {entry.url} ...", end=" ", flush=True)

        if dry_run:
            print("[dry-run]")
            return locked

        content = _get(entry.url)
        filename = Path(entry.url).name or "content.md"

        vdir.mkdir(parents=True, exist_ok=True)
        (vdir / filename).write_bytes(content)

        meta = {
            "source_type": "url",
            "url": entry.url,
        }
        (vdir / ".meta").write_text(json.dumps(meta, indent=2) + "\n")

        print(f"-> {vdir / filename}")

    return locked


def cmd_sync(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        print(f"error: {MANIFEST_NAME} not found in {repo_root}", file=sys.stderr)
        sys.exit(1)

    entries = parse_manifest(manifest_path).entries

    if args.entry:
        entries = [e for e in entries if e.name == args.entry]
        if not entries:
            print(f"error: no entry named '{args.entry}' in {MANIFEST_NAME}", file=sys.stderr)
            sys.exit(1)

    if not entries:
        print(f"No entries found in {MANIFEST_NAME}.")
        return

    mode = " [dry-run]" if args.dry_run else ""
    print(f"Syncing {len(entries)} entr{'y' if len(entries) == 1 else 'ies'}{mode}...")

    locked = read_lock(repo_root)
    update = getattr(args, "update", False)

    for entry in entries:
        locked = sync_entry(entry, repo_root, args.dry_run, locked, update)

    if not args.dry_run:
        write_lock(repo_root, locked)
        print("Done.")
