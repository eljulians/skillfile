import argparse
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from skillfile.core.lock import lock_key, read_lock, write_lock
from skillfile.core.models import Entry, LockEntry, SyncContext
from skillfile.core.parser import MANIFEST_NAME, parse_manifest
from skillfile.exceptions import ManifestError
from skillfile.sources.strategies import STRATEGIES

VENDOR_DIR = ".skillfile/cache"

# Cap for I/O-bound network threads. Python's default is min(32, os.cpu_count()+4).
# We use a lower cap since each worker may spawn its own ThreadPool for dir-entry downloads.
_MAX_SYNC_WORKERS = 16


def vendor_dir_for(entry: Entry, repo_root: Path) -> Path:
    return repo_root / VENDOR_DIR / f"{entry.entity_type}s" / entry.name


def sync_entry(
    entry: Entry,
    ctx: SyncContext,
    locked: dict[str, LockEntry],
) -> dict[str, LockEntry]:
    """Sync a single entry. Returns updated locked dict."""
    label = f"  {entry.source_type}/{entry.entity_type}/{entry.name}"
    vdir = vendor_dir_for(entry, ctx.repo_root)
    key = lock_key(entry)
    return STRATEGIES[entry.source_type].sync(entry, vdir, key, label, ctx, locked)


def sync_entries_parallel(
    entries: list[Entry],
    ctx: SyncContext,
    locked: dict[str, LockEntry],
) -> dict[str, LockEntry]:
    """Sync multiple entries in parallel. Returns updated locked dict.

    Each entry runs in its own thread. Lock updates are merged after all complete.
    """
    if not entries:
        return locked

    # For dry-run or single entry, run sequentially.
    if ctx.dry_run or len(entries) <= 1:
        for entry in entries:
            locked = sync_entry(entry, ctx, locked)
        return locked

    def _sync_one(entry: Entry) -> tuple[str, LockEntry | None]:
        """Sync one entry. Returns (key, lock_entry_or_None)."""
        key = lock_key(entry)
        local_locked = dict(locked)
        new_locked = sync_entry(entry, ctx, local_locked)
        return key, new_locked.get(key)

    results: list[tuple[str, LockEntry | None]] = [None] * len(entries)

    with ThreadPoolExecutor(max_workers=_MAX_SYNC_WORKERS) as pool:
        futures = [(i, pool.submit(_sync_one, entry)) for i, entry in enumerate(entries)]
        for i, future in futures:
            results[i] = future.result()

    # Merge lock updates.
    for key, lock_entry in results:
        if lock_entry is not None:
            locked[key] = lock_entry

    return locked


def cmd_sync(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        raise ManifestError(f"{MANIFEST_NAME} not found in {repo_root}")

    entries = parse_manifest(manifest_path).entries

    if args.entry:
        entries = [e for e in entries if e.name == args.entry]
        if not entries:
            raise ManifestError(f"no entry named '{args.entry}' in {MANIFEST_NAME}")

    if not entries:
        print(f"No entries found in {MANIFEST_NAME}.")
        return

    mode = " [dry-run]" if args.dry_run else ""
    print(f"Syncing {len(entries)} entr{'y' if len(entries) == 1 else 'ies'}{mode}...")

    locked = read_lock(repo_root)
    update = getattr(args, "update", False)
    ctx = SyncContext(repo_root=repo_root, dry_run=args.dry_run, update=update)

    locked = sync_entries_parallel(entries, ctx, locked)

    if not args.dry_run:
        write_lock(repo_root, locked)
        print("Done.")
