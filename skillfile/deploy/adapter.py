"""Platform adapters — per-tool deployment strategies.

Pattern
-------
PlatformAdapter (Protocol)      — the target interface install.py calls.
FileSystemAdapter               — parameterized implementation for tools that
                                  deploy markdown files to the local filesystem.
                                  All three built-in adapters are instances.

To add a new filesystem tool: create a FileSystemAdapter instance with the
tool's name and entity configs, register it in ADAPTERS.  No subclassing needed.

To add a tool with non-filesystem semantics (e.g. posting to an API): implement
PlatformAdapter directly, bypassing FileSystemAdapter entirely.
"""

from __future__ import annotations

import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Literal, Protocol, runtime_checkable

from skillfile.core.models import Entry, InstallOptions
from skillfile.sources.strategies import STRATEGIES
from skillfile.sources.sync import vendor_dir_for

DirInstallMode = Literal["flat", "nested"]
"""How a directory entry is deployed.

flat:   each .md placed individually in target_dir/ — used by tools that load
        one file per agent/skill at the top level (e.g. claude-code agents).
nested: directory placed as target_dir/<name>/ — used by tools that load a
        full SKILL.md directory as a unit (e.g. all skill adapters).
"""


@dataclass(frozen=True)
class EntityConfig:
    """Paths and install mode for one entity type within a platform."""

    global_path: str  # absolute; may start with ~
    local_path: str  # relative to repo_root
    dir_mode: DirInstallMode = "nested"


# ---------------------------------------------------------------------------
# Protocol — the seam between install.py and every platform adapter
# ---------------------------------------------------------------------------


@runtime_checkable
class PlatformAdapter(Protocol):
    """Contract every platform adapter must satisfy.

    install.py only calls methods defined here — it never inspects adapter
    internals, checks entity_type strings, or knows about target paths.
    """

    name: str

    def supports(self, entity_type: str) -> bool:
        """Return True if this adapter can deploy entity_type."""
        ...

    def deploy_entry(
        self,
        entry: Entry,
        source: Path,
        scope: str,
        repo_root: Path,
        opts: InstallOptions,
    ) -> dict[str, Path]:
        """Deploy source to the platform directory.

        Returns {relative_key: installed_path} for downstream patch application.
        Keys match the relative keys used in Skillfile.patches/ so patch lookups
        work without the caller knowing anything about the deploy layout.
        Returns an empty dict when nothing was written (dry-run, skipped).
        """
        ...

    def installed_path(self, entry: Entry, scope: str, repo_root: Path) -> Path:
        """Installed path for a single-file entry. Used by pin/diff/resolve/status."""
        ...

    def installed_dir_files(self, entry: Entry, scope: str, repo_root: Path) -> dict[str, Path]:
        """Installed files for a directory entry. Used by pin/diff/resolve/status."""
        ...


# ---------------------------------------------------------------------------
# Shared low-level IO (no domain knowledge)
# ---------------------------------------------------------------------------


def _place_file(source: Path, dest: Path, is_dir: bool, opts: InstallOptions) -> bool:
    """Copy or symlink source to dest. Returns True if placed, False if skipped."""
    if not opts.overwrite and not opts.dry_run:
        if is_dir and dest.is_dir() and not dest.is_symlink():
            return False
        if not is_dir and dest.is_file() and not dest.is_symlink():
            return False

    label = f"  {source.name} -> {dest}"
    if opts.dry_run:
        print(f"{label} [{'link' if opts.link_mode else 'copy'}, dry-run]")
        return True

    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.exists() or dest.is_symlink():
        shutil.rmtree(dest) if (dest.is_dir() and not dest.is_symlink()) else dest.unlink()

    if opts.link_mode:
        dest.symlink_to(source.resolve())
    else:
        shutil.copytree(source, dest) if is_dir else shutil.copy2(source, dest)

    print(label)
    return True


# ---------------------------------------------------------------------------
# FileSystemAdapter — parameterized, not subclassed
# ---------------------------------------------------------------------------


class FileSystemAdapter:
    """Deploy implementation for tools that copy/symlink markdown files locally.

    Each instance is configured with a name and a dict of entity configs.
    All deploy logic lives here. Adding a new tool is a one-line registry entry.
    """

    def __init__(self, name: str, entities: dict[str, EntityConfig]) -> None:
        self.name = name
        self._entities = entities

    # -- PlatformAdapter protocol ----------------------------------------

    def supports(self, entity_type: str) -> bool:
        return entity_type in self._entities

    def deploy_entry(
        self,
        entry: Entry,
        source: Path,
        scope: str,
        repo_root: Path,
        opts: InstallOptions,
    ) -> dict[str, Path]:
        target_dir = self._target_dir(entry.entity_type, scope, repo_root)
        is_dir = STRATEGIES[entry.source_type].is_dir_entry(entry)

        if is_dir and self._entities[entry.entity_type].dir_mode == "flat":
            return self._deploy_flat(source, target_dir, opts)

        dest = target_dir / (entry.name if is_dir else f"{entry.name}.md")
        if not _place_file(source, dest, is_dir=is_dir, opts=opts) or opts.dry_run:
            return {}
        if is_dir:
            return {
                str(f.relative_to(source)): dest / f.relative_to(source)
                for f in source.rglob("*")
                if f.is_file() and f.name != ".meta"
            }
        return {f"{entry.name}.md": dest}

    def installed_path(self, entry: Entry, scope: str, repo_root: Path) -> Path:
        return self._target_dir(entry.entity_type, scope, repo_root) / f"{entry.name}.md"

    def installed_dir_files(self, entry: Entry, scope: str, repo_root: Path) -> dict[str, Path]:
        target_dir = self._target_dir(entry.entity_type, scope, repo_root)
        if self._entities[entry.entity_type].dir_mode == "nested":
            installed_dir = target_dir / entry.name
            if not installed_dir.is_dir():
                return {}
            return {str(f.relative_to(installed_dir)): f for f in installed_dir.rglob("*") if f.is_file()}
        # flat: keys are relative-from-vdir so they match patch lookup keys
        vdir = vendor_dir_for(entry, repo_root)
        if not vdir.is_dir():
            return {}
        return {
            str(f.relative_to(vdir)): target_dir / f.name for f in vdir.rglob("*.md") if (target_dir / f.name).exists()
        }

    # -- Exposed helper (not on Protocol — filesystem adapters only) -----

    def target_dir(self, entity_type: str, scope: str, repo_root: Path) -> Path:
        """Resolve the absolute deploy directory. For paths.py and tests."""
        return self._target_dir(entity_type, scope, repo_root)

    # -- Private ---------------------------------------------------------

    def _target_dir(self, entity_type: str, scope: str, repo_root: Path) -> Path:
        config = self._entities[entity_type]
        raw = config.global_path if scope == "global" else config.local_path
        return Path(raw).expanduser() if raw.startswith("~") else repo_root / raw

    def _deploy_flat(self, source_dir: Path, target_dir: Path, opts: InstallOptions) -> dict[str, Path]:
        """Deploy each .md in source_dir as an individual file in target_dir."""
        md_files = sorted(source_dir.rglob("*.md"))
        if opts.dry_run:
            for src in md_files:
                print(f"  {src.name} -> {target_dir / src.name} [{'link' if opts.link_mode else 'copy'}, dry-run]")
            return {}
        target_dir.mkdir(parents=True, exist_ok=True)
        result: dict[str, Path] = {}
        for src in md_files:
            dest = target_dir / src.name
            if not opts.overwrite and dest.is_file() and not dest.is_symlink():
                continue
            if dest.exists() or dest.is_symlink():
                dest.unlink()
            if opts.link_mode:
                dest.symlink_to(src.resolve())
            else:
                shutil.copy2(src, dest)
            print(f"  {src.name} -> {dest}")
            result[str(src.relative_to(source_dir))] = dest
        return result


# ---------------------------------------------------------------------------
# Registry — one instance per tool, configured with paths
# ---------------------------------------------------------------------------

ADAPTERS: dict[str, PlatformAdapter] = {
    "claude-code": FileSystemAdapter(
        "claude-code",
        {
            "agent": EntityConfig("~/.claude/agents", ".claude/agents", dir_mode="flat"),
            "skill": EntityConfig("~/.claude/skills", ".claude/skills", dir_mode="nested"),
        },
    ),
    "gemini-cli": FileSystemAdapter(
        "gemini-cli",
        {
            "agent": EntityConfig("~/.gemini/agents", ".gemini/agents", dir_mode="flat"),
            "skill": EntityConfig("~/.gemini/skills", ".gemini/skills", dir_mode="nested"),
        },
    ),
    # Codex has no separate agent directory — skills only.
    "codex": FileSystemAdapter(
        "codex",
        {
            "skill": EntityConfig("~/.codex/skills", ".codex/skills", dir_mode="nested"),
        },
    ),
}

KNOWN_ADAPTERS: list[str] = list(ADAPTERS)

# Backward compat alias — tests that imported BaseFileSystemAdapter still work.
BaseFileSystemAdapter = FileSystemAdapter
