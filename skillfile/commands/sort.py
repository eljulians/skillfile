import argparse
from pathlib import Path

from skillfile.commands.add import _format_line
from skillfile.core.parser import MANIFEST_NAME, parse_manifest
from skillfile.exceptions import ManifestError

# Auto-generated section headers — always written by sort, never hand-edited.
_INSTALL_COMMENT = "# install  <platform>  <scope>"
_SECTION_HEADERS = {
    "agent": [
        "# --- Agents ---",
        "# github  agent  [name]  <owner/repo>  <path-or-dir>  [ref]",
    ],
    "skill": [
        "# --- Skills ---",
        "# github  skill  [name]  <owner/repo>  <path-or-dir>  [ref]",
    ],
}


def _sort_key(entry):
    repo = entry.owner_repo if entry.source_type == "github" else ""
    path = (
        entry.path_in_repo
        if entry.source_type == "github"
        else entry.local_path
        if entry.source_type == "local"
        else entry.url or ""
    )
    return (entry.source_type, repo, path)


def _group_by_repo(entries):
    """Split sorted entries into sub-lists by (source_type, owner_repo)."""
    groups = []
    current_key = None
    current_group = []
    for entry in entries:
        key = (entry.source_type, getattr(entry, "owner_repo", "") or "")
        if key != current_key:
            if current_group:
                groups.append(current_group)
            current_group = [entry]
            current_key = key
        else:
            current_group.append(entry)
    if current_group:
        groups.append(current_group)
    return groups


def _extract_entry_comments(raw_text: str) -> dict[str, list[str]]:
    """Return a mapping of entry line → comment lines immediately preceding it.

    A comment block is attached to an entry only when there is no blank line
    between the last comment and the entry.  Section/header comments separated
    by blank lines are not attached to any entry and are dropped.
    """
    lines = raw_text.splitlines()
    attached: dict[str, list[str]] = {}
    pending: list[str] = []

    for line in lines:
        stripped = line.strip()
        if stripped.startswith("#"):
            pending.append(line.rstrip())
        elif stripped == "" or stripped.startswith("install"):
            pending = []
        else:
            if pending:
                attached[stripped] = list(pending)
            pending = []

    return attached


def sorted_manifest_text(manifest, raw_text: str = "") -> str:
    entry_comments = _extract_entry_comments(raw_text) if raw_text else {}
    lines = []

    # Install targets section
    if manifest.install_targets:
        lines.append(_INSTALL_COMMENT)
        for target in manifest.install_targets:
            lines.append(f"install  {target.adapter}  {target.scope}")

    agents = sorted([e for e in manifest.entries if e.entity_type == "agent"], key=_sort_key)
    skills = sorted([e for e in manifest.entries if e.entity_type == "skill"], key=_sort_key)

    for entity_type, group in [("agent", agents), ("skill", skills)]:
        if not group:
            continue
        lines.append("")
        lines.extend(_SECTION_HEADERS[entity_type])
        for repo_group in _group_by_repo(group):
            lines.append("")
            for entry in repo_group:
                formatted = _format_line(entry)
                lines.extend(entry_comments.get(formatted, []))
                lines.append(formatted)

    return "\n".join(lines) + "\n"


def cmd_sort(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        raise ManifestError(f"{MANIFEST_NAME} not found in {repo_root}")

    manifest = parse_manifest(manifest_path)
    raw_text = manifest_path.read_text()
    text = sorted_manifest_text(manifest, raw_text)

    if getattr(args, "dry_run", False):
        print(text, end="")
        return

    manifest_path.write_text(text)
    n = len(manifest.entries)
    print(f"Sorted {n} entr{'y' if n == 1 else 'ies'}.")
