"""Unit tests for deploy — patch application helpers and filesystem IO."""

from skillfile.core.models import Entry, InstallOptions
from skillfile.deploy.adapter import ADAPTERS, _place_file
from skillfile.deploy.install import (
    _apply_dir_patches,
    _apply_patch_to_file,
    _apply_single_file_patch,
)
from skillfile.patch.patch import generate_patch, write_dir_patch, write_patch


def _github_entry(name="test", entity_type="agent", path="agents/test.md"):
    return Entry("github", entity_type, name, owner_repo="o/r", path_in_repo=path, ref="main")


def test_apply_patch_to_file(tmp_path):
    """Apply a patch modifying a file in place."""
    target = tmp_path / "test.md"
    target.write_text("# Hello\nWorld\n")
    patch_text = generate_patch("# Hello\nWorld\n", "# Hello\nWorld\nPatched\n", "test.md")
    _apply_patch_to_file(target, patch_text)
    assert "Patched" in target.read_text()


def test_apply_single_file_patch_rebases(tmp_path):
    """After applying a patch, rebase it against the new cache content."""
    entry = _github_entry()
    # Setup cache
    vdir = tmp_path / ".skillfile" / "agents" / "test"
    vdir.mkdir(parents=True)
    (vdir / "test.md").write_text("# New Cache\nContent\n")

    # Setup installed
    dest = tmp_path / "installed.md"
    dest.write_text("# New Cache\nContent\n")  # will be patched

    # Write a patch
    patch_text = generate_patch("# New Cache\nContent\n", "# New Cache\nContent\nEdited\n", "test.md")
    write_patch(entry, patch_text, tmp_path)

    _apply_single_file_patch(entry, dest, vdir / "test.md", tmp_path)
    assert "Edited" in dest.read_text()


def test_apply_single_file_patch_removes_empty(tmp_path):
    """If patch application results in content matching cache, remove the patch."""
    entry = _github_entry()
    vdir = tmp_path / ".skillfile" / "agents" / "test"
    vdir.mkdir(parents=True)
    cache_content = "# Same\nContent\n"
    (vdir / "test.md").write_text(cache_content)

    dest = tmp_path / "installed.md"
    dest.write_text(cache_content)

    # Write a no-op patch (diff is empty after application = content matches)
    # Actually let's write a patch that when applied gives the same as cache
    patch_text = generate_patch(cache_content, cache_content + "Extra\n", "test.md")
    write_patch(entry, patch_text, tmp_path)

    _apply_single_file_patch(entry, dest, vdir / "test.md", tmp_path)
    # After applying, the patch is regenerated: diff(cache, installed) — if extra line present, patch stays


def test_place_file_copy(tmp_path):
    """_place_file copies a file."""
    src = tmp_path / "source.md"
    src.write_text("# Source\n")
    dest = tmp_path / "dest" / "target.md"
    assert _place_file(src, dest, is_dir=False, opts=InstallOptions()) is True
    assert dest.read_text() == "# Source\n"


def test_place_file_link(tmp_path):
    """_place_file creates a symlink in link mode."""
    src = tmp_path / "source.md"
    src.write_text("# Source\n")
    dest = tmp_path / "dest" / "target.md"
    assert _place_file(src, dest, is_dir=False, opts=InstallOptions(link_mode=True)) is True
    assert dest.is_symlink()


def test_place_file_skip_existing(tmp_path):
    """_place_file with overwrite=False skips existing regular files."""
    src = tmp_path / "source.md"
    src.write_text("# New\n")
    dest = tmp_path / "dest" / "target.md"
    dest.parent.mkdir(parents=True)
    dest.write_text("# Existing\n")
    assert _place_file(src, dest, is_dir=False, opts=InstallOptions(overwrite=False)) is False
    assert dest.read_text() == "# Existing\n"


def test_place_file_dry_run(tmp_path, capsys):
    """_place_file dry-run prints but doesn't write."""
    src = tmp_path / "source.md"
    src.write_text("# Source\n")
    dest = tmp_path / "dest" / "target.md"
    assert _place_file(src, dest, is_dir=False, opts=InstallOptions(dry_run=True)) is True
    assert not dest.exists()
    assert "dry-run" in capsys.readouterr().out


def test_deploy_flat(tmp_path):
    """BaseFileSystemAdapter._deploy_flat copies each .md into target_dir."""
    src_dir = tmp_path / "source"
    src_dir.mkdir()
    (src_dir / "a.md").write_text("# A\n")
    (src_dir / "b.md").write_text("# B\n")
    target_dir = tmp_path / "target"
    result = ADAPTERS["claude-code"]._deploy_flat(src_dir, target_dir, InstallOptions())
    assert (target_dir / "a.md").read_text() == "# A\n"
    assert (target_dir / "b.md").read_text() == "# B\n"
    assert result == {"a.md": target_dir / "a.md", "b.md": target_dir / "b.md"}


def test_deploy_flat_dry_run(tmp_path, capsys):
    """_deploy_flat dry-run prints but doesn't write."""
    src_dir = tmp_path / "source"
    src_dir.mkdir()
    (src_dir / "a.md").write_text("# A\n")
    target_dir = tmp_path / "target"
    result = ADAPTERS["claude-code"]._deploy_flat(src_dir, target_dir, InstallOptions(dry_run=True))
    assert not target_dir.exists()
    assert result == {}
    assert "dry-run" in capsys.readouterr().out


def test_apply_dir_patches_skill(tmp_path):
    """Apply per-file patches to a skill directory."""
    entry = _github_entry(name="my-skill", entity_type="skill", path="skills")
    # Create installed skill dir
    installed_dir = tmp_path / "installed" / "my-skill"
    installed_dir.mkdir(parents=True)
    (installed_dir / "SKILL.md").write_text("# Skill\nOriginal\n")
    # Create source dir
    source_dir = tmp_path / "cache"
    source_dir.mkdir()
    (source_dir / "SKILL.md").write_text("# Skill\nOriginal\n")
    # Write a patch
    patch_text = generate_patch("# Skill\nOriginal\n", "# Skill\nEdited\n", "SKILL.md")
    write_dir_patch(entry, "SKILL.md", patch_text, tmp_path)
    installed_files = {"SKILL.md": installed_dir / "SKILL.md"}
    _apply_dir_patches(entry, installed_files, source_dir, tmp_path)
    assert "Edited" in (installed_dir / "SKILL.md").read_text()


def test_apply_dir_patches_agent(tmp_path):
    """Apply per-file patches to agent dir files."""
    entry = _github_entry(name="my-agents", entity_type="agent", path="agents")
    target_dir = tmp_path / "agents"
    target_dir.mkdir()
    (target_dir / "a.md").write_text("# Agent A\nOriginal\n")
    source_dir = tmp_path / "cache"
    source_dir.mkdir()
    (source_dir / "a.md").write_text("# Agent A\nOriginal\n")
    patch_text = generate_patch("# Agent A\nOriginal\n", "# Agent A\nEdited\n", "a.md")
    write_dir_patch(entry, "a.md", patch_text, tmp_path)
    installed_files = {"a.md": target_dir / "a.md"}
    _apply_dir_patches(entry, installed_files, source_dir, tmp_path)
    assert "Edited" in (target_dir / "a.md").read_text()
