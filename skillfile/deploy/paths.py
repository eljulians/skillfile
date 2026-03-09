"""Manifest-aware path helpers — bridge between Manifest install targets and adapters.

These functions look up the first install target from a Manifest and delegate
to the appropriate PlatformAdapter. They exist so callers in pin/diff/resolve/
status don't have to repeat the "get first target, look up adapter" boilerplate.
"""

from pathlib import Path

from skillfile.core.models import Entry, Manifest
from skillfile.deploy.adapter import ADAPTERS, KNOWN_ADAPTERS, FileSystemAdapter
from skillfile.exceptions import ManifestError
from skillfile.sources.strategies import STRATEGIES
from skillfile.sources.sync import vendor_dir_for

__all__ = ["KNOWN_ADAPTERS", "resolve_target_dir", "installed_path", "installed_dir_files", "_source_path"]


def resolve_target_dir(adapter: str, entity_type: str, scope: str, repo_root: Path) -> Path:
    """Resolve absolute deploy directory for (adapter, entity_type, scope).

    Convenience used by tests and validate. Requires a FileSystemAdapter.
    """
    a = ADAPTERS[adapter]
    assert isinstance(a, FileSystemAdapter)
    return a.target_dir(entity_type, scope, repo_root)


def installed_path(entry: Entry, manifest: Manifest, repo_root: Path) -> Path:
    """Installed path for a single-file entry (first install target)."""
    target = _first_target(manifest)
    return target.installed_path(entry, manifest.install_targets[0].scope, repo_root)


def installed_dir_files(entry: Entry, manifest: Manifest, repo_root: Path) -> dict[str, Path]:
    """Installed files for a directory entry (first install target)."""
    target = _first_target(manifest)
    return target.installed_dir_files(entry, manifest.install_targets[0].scope, repo_root)


def _source_path(entry: Entry, repo_root: Path) -> Path | None:
    """Resolve the cache or local source path for an entry."""
    strategy = STRATEGIES[entry.source_type]
    if entry.source_type == "local":
        return repo_root / entry.local_path
    vdir = vendor_dir_for(entry, repo_root)
    if strategy.is_dir_entry(entry):
        return vdir if vdir.exists() else None
    filename = strategy.content_file(entry)
    if not filename:
        return None
    return vdir / filename


def _first_target(manifest: Manifest):
    """Return the PlatformAdapter for the first install target, or raise."""
    if not manifest.install_targets:
        raise ManifestError("no install targets configured — run `skillfile install` first")
    t = manifest.install_targets[0]
    adapter = ADAPTERS.get(t.adapter)
    if adapter is None:
        raise ManifestError(f"unknown adapter '{t.adapter}'")
    return adapter
