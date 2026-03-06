import sys
from pathlib import Path

from .models import Entry, InstallTarget, Manifest

MANIFEST_NAME = "Skillfile"
DEFAULT_REF = "main"


def _infer_name(path_or_url: str) -> str:
    """Infer entry name from a path or URL (filename stem)."""
    stem = Path(path_or_url).stem
    return stem if stem and stem != "." else "content"


def _parse_github(parts: list[str], lineno: int) -> Entry | None:
    """Parse a github entry with optional name and ref.

    With explicit name:  github  agent  name  owner/repo  path  [ref]
    With inferred name:  github  agent  owner/repo  path  [ref]
    Detection: if field[2] contains '/' it is owner/repo; otherwise it is name.
    """
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


def _parse_local(parts: list[str], lineno: int) -> Entry | None:
    """Parse a local entry with optional name.

    With explicit name:  local  skill  name  path
    With inferred name:  local  skill  path       (path ends in .md or contains /)
    """
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


def _parse_url(parts: list[str], lineno: int) -> Entry | None:
    """Parse a url entry with optional name.

    With explicit name:  url  skill  name  https://...
    With inferred name:  url  skill  https://...
    """
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


def parse_manifest(manifest_path: Path) -> Manifest:
    entries: list[Entry] = []
    install_targets: list[InstallTarget] = []

    with open(manifest_path) as f:
        for lineno, raw in enumerate(f, 1):
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            if len(parts) < 2:
                print(f"warning: line {lineno}: too few fields, skipping", file=sys.stderr)
                continue

            source_type = parts[0]

            if source_type == "install":
                if len(parts) < 3:
                    print(f"warning: line {lineno}: install line needs: adapter scope", file=sys.stderr)
                    continue
                install_targets.append(InstallTarget(adapter=parts[1], scope=parts[2]))

            elif source_type in ("local", "github", "url"):
                if len(parts) < 3:
                    print(f"warning: line {lineno}: too few fields, skipping", file=sys.stderr)
                    continue
                if source_type == "github":
                    entry = _parse_github(parts, lineno)
                elif source_type == "local":
                    entry = _parse_local(parts, lineno)
                else:
                    entry = _parse_url(parts, lineno)
                if entry is not None:
                    entries.append(entry)

            else:
                print(f"warning: line {lineno}: unknown source type '{source_type}', skipping", file=sys.stderr)

    return Manifest(entries=entries, install_targets=install_targets)
