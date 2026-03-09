"""Source strategy implementations.

Each strategy encapsulates all behavior for one source type:
parse (from manifest), from_args (from CLI add), format_parts (to manifest),
content_file (vendor filename), is_dir_entry, fetch_original, and sync (fetch + cache).

Adding a new source type means adding one class and one entry in STRATEGIES.
No other module needs to change.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Protocol

from .models import Entry, LockEntry
from .resolver import _get, fetch_github_file, list_github_dir_recursive, resolve_github_sha

DEFAULT_REF = "main"


def _infer_name(path_or_url: str) -> str:
    """Infer entry name from a path or URL (filename stem)."""
    stem = Path(path_or_url).stem
    return stem if stem and stem != "." else "content"


def meta_sha(vdir: Path) -> str | None:
    """Return the SHA recorded in .meta, or None if missing/unreadable."""
    meta_path = vdir / ".meta"
    if not meta_path.exists():
        return None
    try:
        return json.loads(meta_path.read_text()).get("sha")
    except (json.JSONDecodeError, OSError):
        return None


class SourceStrategy(Protocol):
    """Interface all source type strategies must implement."""

    def parse(self, parts: list[str], lineno: int) -> Entry | None:
        """Parse a Skillfile line's fields into an Entry."""
        ...

    def from_args(self, args: argparse.Namespace, entity_type: str) -> Entry:
        """Construct an Entry from `skillfile add` CLI args."""
        ...

    def format_parts(self, entry: Entry) -> list[str]:
        """Return source-type-specific Skillfile fields (after source_type and entity_type)."""
        ...

    def content_file(self, entry: Entry) -> str:
        """Return the expected filename in the vendor cache dir. Empty for dir entries."""
        ...

    def is_dir_entry(self, entry: Entry) -> bool:
        """True when the entry represents a directory of files rather than a single file."""
        ...

    def fetch_original(self, entry: Entry, sha: str) -> str:
        """Re-fetch the upstream file at the given SHA and return its text content."""
        ...

    def fetch_dir_files(self, entry: Entry, sha: str) -> dict[str, str]:
        """Re-fetch all files for a dir entry at the given SHA. Returns {filename: content}."""
        ...

    def sync(
        self,
        entry: Entry,
        vdir: Path,
        key: str,
        label: str,
        dry_run: bool,
        locked: dict[str, LockEntry],
        update: bool,
    ) -> dict[str, LockEntry]:
        """Fetch and cache the entry. Returns updated lock dict."""
        ...


class GithubStrategy:
    def parse(self, parts: list[str], lineno: int) -> Entry | None:
        # With explicit name:  github  agent  name  owner/repo  path  [ref]
        # With inferred name:  github  agent  owner/repo  path  [ref]
        # Detection: field[2] contains '/' → owner/repo; otherwise → name.
        if "/" in parts[2]:
            if len(parts) < 4:
                print(f"warning: line {lineno}: github entry needs at least: owner/repo path", file=sys.stderr)
                return None
            owner_repo = parts[2]
            path_in_repo = parts[3]
            ref = parts[4] if len(parts) > 4 else DEFAULT_REF
            name = _infer_name(path_in_repo)
        else:
            if len(parts) < 5:
                print(f"warning: line {lineno}: github entry needs at least: name owner/repo path", file=sys.stderr)
                return None
            name = parts[2]
            owner_repo = parts[3]
            path_in_repo = parts[4]
            ref = parts[5] if len(parts) > 5 else DEFAULT_REF
        return Entry("github", parts[1], name, owner_repo=owner_repo, path_in_repo=path_in_repo, ref=ref)

    def from_args(self, args: argparse.Namespace, entity_type: str) -> Entry:
        path = args.path
        name = args.name or _infer_name(path)
        ref = args.ref or DEFAULT_REF
        return Entry("github", entity_type, name, owner_repo=args.owner_repo, path_in_repo=path, ref=ref)

    def format_parts(self, entry: Entry) -> list[str]:
        parts: list[str] = []
        if entry.name != _infer_name(entry.path_in_repo):
            parts.append(entry.name)
        parts.append(entry.owner_repo)
        parts.append(entry.path_in_repo)
        if entry.ref != DEFAULT_REF:
            parts.append(entry.ref)
        return parts

    def content_file(self, entry: Entry) -> str:
        if self.is_dir_entry(entry):
            return ""
        effective_path = "SKILL.md" if entry.path_in_repo == "." else entry.path_in_repo
        return Path(effective_path).name

    def is_dir_entry(self, entry: Entry) -> bool:
        return entry.path_in_repo != "." and not entry.path_in_repo.endswith(".md")

    def fetch_original(self, entry: Entry, sha: str) -> str:
        return fetch_github_file(entry.owner_repo, entry.path_in_repo, sha).decode()

    def fetch_dir_files(self, entry: Entry, sha: str) -> dict[str, str]:
        files = list_github_dir_recursive(entry.owner_repo, entry.path_in_repo, sha)
        return {f["relative_path"]: _get(f["download_url"]).decode() for f in files}

    def sync(
        self,
        entry: Entry,
        vdir: Path,
        key: str,
        label: str,
        dry_run: bool,
        locked: dict[str, LockEntry],
        update: bool,
    ) -> dict[str, LockEntry]:
        locked_sha = None if update else (locked[key].sha if key in locked else None)
        meta = meta_sha(vdir)

        if self.is_dir_entry(entry):
            content_exists = (
                (vdir.is_dir() and any(f for f in vdir.iterdir() if f.name != ".meta")) if vdir.exists() else False
            )
        else:
            cf = self.content_file(entry)
            content_exists = bool(cf) and (vdir / cf).exists()

        if locked_sha and meta == locked_sha and content_exists:
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

        vdir.mkdir(parents=True, exist_ok=True)

        if self.is_dir_entry(entry):
            files = list_github_dir_recursive(entry.owner_repo, entry.path_in_repo, sha)
            for file_info in files:
                dest = vdir / file_info["relative_path"]
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(_get(file_info["download_url"]))
            raw_url = f"https://api.github.com/repos/{entry.owner_repo}/contents/{entry.path_in_repo}?ref={sha}"
            print(f"-> {vdir}/ ({len(files)} files)")
        else:
            content = fetch_github_file(entry.owner_repo, entry.path_in_repo, sha)
            effective_path = "SKILL.md" if entry.path_in_repo == "." else entry.path_in_repo
            filename = Path(effective_path).name
            (vdir / filename).write_bytes(content)
            raw_url = f"https://raw.githubusercontent.com/{entry.owner_repo}/{sha}/{effective_path}"
            print(f"-> {vdir / filename}")

        meta_data = {
            "source_type": "github",
            "owner_repo": entry.owner_repo,
            "path_in_repo": entry.path_in_repo,
            "ref": entry.ref,
            "sha": sha,
            "raw_url": raw_url,
        }
        (vdir / ".meta").write_text(json.dumps(meta_data, indent=2) + "\n")
        locked[key] = LockEntry(sha=sha, raw_url=raw_url)
        return locked


class LocalStrategy:
    def parse(self, parts: list[str], lineno: int) -> Entry | None:
        # With explicit name:  local  skill  name  path
        # With inferred name:  local  skill  path   (path ends in .md or contains /)
        if parts[2].endswith(".md") or "/" in parts[2]:
            local_path = parts[2]
            name = _infer_name(local_path)
        else:
            if len(parts) < 4:
                print(f"warning: line {lineno}: local entry needs: name path", file=sys.stderr)
                return None
            name = parts[2]
            local_path = parts[3]
        return Entry("local", parts[1], name, local_path=local_path)

    def from_args(self, args: argparse.Namespace, entity_type: str) -> Entry:
        name = args.name or _infer_name(args.path)
        return Entry("local", entity_type, name, local_path=args.path)

    def format_parts(self, entry: Entry) -> list[str]:
        parts: list[str] = []
        if entry.name != _infer_name(entry.local_path):
            parts.append(entry.name)
        parts.append(entry.local_path)
        return parts

    def content_file(self, entry: Entry) -> str:
        return ""

    def is_dir_entry(self, entry: Entry) -> bool:
        return False

    def fetch_original(self, entry: Entry, sha: str) -> str:
        raise NotImplementedError("local entries have no upstream to fetch")

    def fetch_dir_files(self, entry: Entry, sha: str) -> dict[str, str]:
        raise NotImplementedError("local entries have no upstream to fetch")

    def sync(
        self,
        entry: Entry,
        vdir: Path,
        key: str,
        label: str,
        dry_run: bool,
        locked: dict[str, LockEntry],
        update: bool,
    ) -> dict[str, LockEntry]:
        print(f"{label}: local — skipping")
        return locked


class UrlStrategy:
    def parse(self, parts: list[str], lineno: int) -> Entry | None:
        # With explicit name:  url  skill  name  https://...
        # With inferred name:  url  skill  https://...
        if parts[2].startswith("http"):
            url = parts[2]
            name = _infer_name(url)
        else:
            if len(parts) < 4:
                print(f"warning: line {lineno}: url entry needs: name url", file=sys.stderr)
                return None
            name = parts[2]
            url = parts[3]
        return Entry("url", parts[1], name, url=url)

    def from_args(self, args: argparse.Namespace, entity_type: str) -> Entry:
        name = args.name or _infer_name(args.url)
        return Entry("url", entity_type, name, url=args.url)

    def format_parts(self, entry: Entry) -> list[str]:
        parts: list[str] = []
        if entry.name != _infer_name(entry.url):
            parts.append(entry.name)
        parts.append(entry.url)
        return parts

    def content_file(self, entry: Entry) -> str:
        return Path(entry.url).name or "content.md"

    def is_dir_entry(self, entry: Entry) -> bool:
        return False

    def fetch_original(self, entry: Entry, sha: str) -> str:
        # URL entries have no SHA versioning — re-fetch current content.
        return _get(entry.url).decode()

    def fetch_dir_files(self, entry: Entry, sha: str) -> dict[str, str]:
        raise NotImplementedError("url entries cannot be directory entries")

    def sync(
        self,
        entry: Entry,
        vdir: Path,
        key: str,
        label: str,
        dry_run: bool,
        locked: dict[str, LockEntry],
        update: bool,
    ) -> dict[str, LockEntry]:
        print(f"{label}: fetching {entry.url} ...", end=" ", flush=True)

        if dry_run:
            print("[dry-run]")
            return locked

        content = _get(entry.url)
        filename = Path(entry.url).name or "content.md"

        vdir.mkdir(parents=True, exist_ok=True)
        (vdir / filename).write_bytes(content)

        meta_data = {"source_type": "url", "url": entry.url}
        (vdir / ".meta").write_text(json.dumps(meta_data, indent=2) + "\n")
        print(f"-> {vdir / filename}")
        return locked


STRATEGIES: dict[str, SourceStrategy] = {
    "github": GithubStrategy(),
    "local": LocalStrategy(),
    "url": UrlStrategy(),
}
