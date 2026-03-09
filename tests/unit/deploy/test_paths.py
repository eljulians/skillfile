"""Unit tests for deploy/paths.py — path resolution and installed path lookups."""

import pytest

from skillfile.core.models import Entry, InstallTarget, Manifest
from skillfile.deploy.paths import (
    KNOWN_ADAPTERS,
    _source_path,
    installed_dir_files,
    installed_path,
    resolve_target_dir,
)
from skillfile.exceptions import ManifestError


def test_resolve_target_dir_global(tmp_path):
    result = resolve_target_dir("claude-code", "agent", "global", tmp_path)
    assert str(result).endswith(".claude/agents")


def test_resolve_target_dir_local(tmp_path):
    result = resolve_target_dir("claude-code", "agent", "local", tmp_path)
    assert result == tmp_path / ".claude" / "agents"


def test_installed_path_no_targets():
    entry = Entry("github", "agent", "test")
    manifest = Manifest(entries=[entry], install_targets=[])
    with pytest.raises(ManifestError, match="no install targets"):
        installed_path(entry, manifest, "/tmp")


def test_installed_path_unknown_adapter():
    entry = Entry("github", "agent", "test")
    manifest = Manifest(entries=[entry], install_targets=[InstallTarget("unknown", "global")])
    with pytest.raises(ManifestError, match="unknown adapter"):
        installed_path(entry, manifest, "/tmp")


def test_installed_path_returns_correct_path(tmp_path):
    entry = Entry("github", "agent", "test")
    manifest = Manifest(entries=[entry], install_targets=[InstallTarget("claude-code", "local")])
    result = installed_path(entry, manifest, tmp_path)
    assert result == tmp_path / ".claude" / "agents" / "test.md"


def test_installed_dir_files_no_targets():
    entry = Entry("github", "agent", "test", path_in_repo="agents")
    manifest = Manifest(entries=[entry], install_targets=[])
    with pytest.raises(ManifestError, match="no install targets"):
        installed_dir_files(entry, manifest, "/tmp")


def test_installed_dir_files_skill_dir(tmp_path):
    """Skill dirs are installed as a whole directory."""
    entry = Entry("github", "skill", "my-skill", owner_repo="o/r", path_in_repo="skills")
    manifest = Manifest(entries=[entry], install_targets=[InstallTarget("claude-code", "local")])
    skill_dir = tmp_path / ".claude" / "skills" / "my-skill"
    skill_dir.mkdir(parents=True)
    (skill_dir / "SKILL.md").write_text("# Skill\n")
    result = installed_dir_files(entry, manifest, tmp_path)
    assert "SKILL.md" in result


def test_installed_dir_files_agent_dir(tmp_path):
    """Agent dirs are exploded: each .md at target_dir/filename."""
    entry = Entry("github", "agent", "my-agents", owner_repo="o/r", path_in_repo="agents")
    manifest = Manifest(entries=[entry], install_targets=[InstallTarget("claude-code", "local")])
    # Create vendor cache
    vdir = tmp_path / ".skillfile" / "cache" / "agents" / "my-agents"
    vdir.mkdir(parents=True)
    (vdir / "a.md").write_text("# A\n")
    (vdir / "b.md").write_text("# B\n")
    # Create installed copies
    agents_dir = tmp_path / ".claude" / "agents"
    agents_dir.mkdir(parents=True)
    (agents_dir / "a.md").write_text("# A\n")
    (agents_dir / "b.md").write_text("# B\n")
    result = installed_dir_files(entry, manifest, tmp_path)
    assert len(result) == 2


def test_source_path_local(tmp_path):
    entry = Entry("local", "skill", "test", local_path="skills/test.md")
    result = _source_path(entry, tmp_path)
    assert result == tmp_path / "skills" / "test.md"


def test_source_path_github_single(tmp_path):
    entry = Entry("github", "agent", "test", owner_repo="o/r", path_in_repo="agents/test.md")
    vdir = tmp_path / ".skillfile" / "cache" / "agents" / "test"
    vdir.mkdir(parents=True)
    (vdir / "test.md").write_text("# Test\n")
    result = _source_path(entry, tmp_path)
    assert result == vdir / "test.md"


def test_known_adapters():
    assert "claude-code" in KNOWN_ADAPTERS
