import argparse
from pathlib import Path

from .exceptions import ManifestError
from .install import installed_dir_files, installed_path
from .lock import lock_key, read_lock
from .models import Manifest
from .parser import MANIFEST_NAME, parse_manifest
from .patch import (
    dir_patch_path,
    generate_patch,
    has_dir_patch,
    has_patch,
    read_patch,
)
from .resolver import resolve_github_sha
from .strategies import STRATEGIES, meta_sha
from .sync import vendor_dir_for


def _is_modified_local(entry, manifest: Manifest, repo_root: Path) -> bool:
    """Return True if installed content differs from cache+patch. Fully local, no network."""
    strategy = STRATEGIES[entry.source_type]
    try:
        if strategy.is_dir_entry(entry):
            return _is_dir_modified_local(entry, manifest, repo_root)

        dest = installed_path(entry, manifest, repo_root)
        if not dest.exists() or dest.is_symlink():
            return False

        vdir = vendor_dir_for(entry, repo_root)
        content_file = strategy.content_file(entry)
        if not content_file:
            return False
        cache_file = vdir / content_file
        if not cache_file.exists():
            return False

        cache_text = cache_file.read_text()
        installed_text = dest.read_text()
        if has_patch(entry, repo_root):
            # Compare current diff against stored patch — same inputs → same output.
            # Avoids invoking the `patch` binary for a read-only check.
            current_patch = generate_patch(cache_text, installed_text, f"{entry.name}.md")
            return current_patch != read_patch(entry, repo_root)
        return installed_text != cache_text
    except Exception:
        return False


def _is_dir_modified_local(entry, manifest: Manifest, repo_root: Path) -> bool:
    """Return True if any installed dir file differs from cache+patch."""
    try:
        installed = installed_dir_files(entry, manifest, repo_root)
        if not installed:
            return False

        vdir = vendor_dir_for(entry, repo_root)
        if not vdir.is_dir():
            return False

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
                current_patch = generate_patch(cache_text, installed_text, filename)
                if current_patch != p.read_text():
                    return True
            elif installed_text != cache_text:
                return True
        return False
    except Exception:
        return False


def cmd_status(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        raise ManifestError(f"{MANIFEST_NAME} not found in {repo_root}")

    manifest = parse_manifest(manifest_path)
    entries = manifest.entries
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
        meta_sha_val = meta_sha(vdir)

        annotations = []
        if has_patch(entry, repo_root) or has_dir_patch(entry, repo_root):
            annotations.append("[pinned]")
        if entry.source_type != "local" and _is_modified_local(entry, manifest, repo_root):
            annotations.append("[modified]")
        annotation = ("  " + "  ".join(annotations)) if annotations else ""

        if meta_sha_val != sha:
            status = f"locked    sha={sha[:12]}  (missing or stale){annotation}"
        elif check_upstream and entry.source_type == "github":
            upstream_sha = resolve_github_sha(entry.owner_repo, entry.ref)
            if upstream_sha == sha:
                status = f"up to date  sha={sha[:12]}{annotation}"
            else:
                status = f"outdated    locked={sha[:12]}  upstream={upstream_sha[:12]}{annotation}"
        else:
            status = f"locked    sha={sha[:12]}{annotation}"

        print(f"{name:<{col_w}} {status}")
