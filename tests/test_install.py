import argparse
from pathlib import Path

import pytest

from skillfile.exceptions import ManifestError
from skillfile.install import cmd_install, install_entry, resolve_target_dir
from skillfile.models import Entry, InstallTarget


def make_agent_entry(name="my-agent"):
    return Entry(
        source_type="github",
        entity_type="agent",
        name=name,
        owner_repo="owner/repo",
        path_in_repo="agents/agent.md",
        ref="main",
    )


def make_skill_entry(name="my-skill"):
    return Entry(
        source_type="skill",
        entity_type="skill",
        name=name,
        local_path="skills/my-skill.md",
    )


def make_local_entry(name="my-skill", local_path="skills/my-skill.md"):
    return Entry(
        source_type="local",
        entity_type="skill",
        name=name,
        local_path=local_path,
    )


def make_target(adapter="claude-code", scope="local"):
    return InstallTarget(adapter=adapter, scope=scope)


# ---------------------------------------------------------------------------
# resolve_target_dir
# ---------------------------------------------------------------------------


def test_resolve_target_dir_global_agent(tmp_path):
    d = resolve_target_dir("claude-code", "agent", "global", tmp_path)
    assert d == Path("~/.claude/agents").expanduser()


def test_resolve_target_dir_local_agent(tmp_path):
    d = resolve_target_dir("claude-code", "agent", "local", tmp_path)
    assert d == tmp_path / ".claude" / "agents"


def test_resolve_target_dir_global_skill(tmp_path):
    d = resolve_target_dir("claude-code", "skill", "global", tmp_path)
    assert d == Path("~/.claude/skills").expanduser()


def test_resolve_target_dir_local_skill(tmp_path):
    d = resolve_target_dir("claude-code", "skill", "local", tmp_path)
    assert d == tmp_path / ".claude" / "skills"


# ---------------------------------------------------------------------------
# install_entry — local source
# ---------------------------------------------------------------------------


def test_install_local_entry_symlink(tmp_path):
    source_file = tmp_path / "skills" / "my-skill.md"
    source_file.parent.mkdir(parents=True)
    source_file.write_text("# My Skill")

    entry = make_local_entry(local_path="skills/my-skill.md")
    target = make_target(scope="local")

    install_entry(entry, target, tmp_path, link_mode=True, dry_run=False)

    dest = tmp_path / ".claude" / "skills" / "my-skill.md"
    assert dest.is_symlink()
    assert dest.read_text() == "# My Skill"


def test_install_local_entry_copy(tmp_path):
    source_file = tmp_path / "skills" / "my-skill.md"
    source_file.parent.mkdir(parents=True)
    source_file.write_text("# My Skill")

    entry = make_local_entry(local_path="skills/my-skill.md")
    target = make_target(scope="local")

    install_entry(entry, target, tmp_path, dry_run=False)

    dest = tmp_path / ".claude" / "skills" / "my-skill.md"
    assert dest.exists()
    assert not dest.is_symlink()
    assert dest.read_text() == "# My Skill"


def test_install_entry_dry_run_no_write(tmp_path):
    source_file = tmp_path / "skills" / "my-skill.md"
    source_file.parent.mkdir(parents=True)
    source_file.write_text("# My Skill")

    entry = make_local_entry(local_path="skills/my-skill.md")
    target = make_target(scope="local")

    install_entry(entry, target, tmp_path, dry_run=True)

    dest = tmp_path / ".claude" / "skills" / "my-skill.md"
    assert not dest.exists()


def test_install_entry_overwrites_existing_symlink(tmp_path):
    source_file = tmp_path / "skills" / "my-skill.md"
    source_file.parent.mkdir(parents=True)
    source_file.write_text("# New content")

    dest_dir = tmp_path / ".claude" / "skills"
    dest_dir.mkdir(parents=True)
    old_target = tmp_path / "old.md"
    old_target.write_text("# Old content")
    dest = dest_dir / "my-skill.md"
    dest.symlink_to(old_target)

    entry = make_local_entry(local_path="skills/my-skill.md")
    target = make_target(scope="local")

    install_entry(entry, target, tmp_path, dry_run=False)

    assert dest.read_text() == "# New content"


# ---------------------------------------------------------------------------
# install_entry — github (vendored) source
# ---------------------------------------------------------------------------


def test_install_github_entry_symlink(tmp_path):
    vdir = tmp_path / ".skillfile" / "agents" / "my-agent"
    vdir.mkdir(parents=True)
    (vdir / "agent.md").write_text("# Agent")

    entry = make_agent_entry()
    target = make_target(adapter="claude-code", scope="local")

    install_entry(entry, target, tmp_path, link_mode=True, dry_run=False)

    dest = tmp_path / ".claude" / "agents" / "my-agent.md"
    assert dest.is_symlink()
    assert dest.read_text() == "# Agent"


def test_install_github_dir_entry_symlinks_directory(tmp_path):
    vdir = tmp_path / ".skillfile" / "skills" / "python-pro"
    vdir.mkdir(parents=True)
    (vdir / "SKILL.md").write_text("# Python Pro")
    (vdir / "examples.md").write_text("# Examples")

    entry = Entry(
        source_type="github",
        entity_type="skill",
        name="python-pro",
        owner_repo="owner/repo",
        path_in_repo="skills/python-pro",
        ref="main",
    )
    target = make_target(adapter="claude-code", scope="local")

    install_entry(entry, target, tmp_path, link_mode=True, dry_run=False)

    dest = tmp_path / ".claude" / "skills" / "python-pro"
    assert dest.is_symlink()
    assert (dest / "SKILL.md").read_text() == "# Python Pro"


def test_install_github_dir_entry_copy_mode(tmp_path):
    vdir = tmp_path / ".skillfile" / "skills" / "python-pro"
    vdir.mkdir(parents=True)
    (vdir / "SKILL.md").write_text("# Python Pro")

    entry = Entry(
        source_type="github",
        entity_type="skill",
        name="python-pro",
        owner_repo="owner/repo",
        path_in_repo="skills/python-pro",
        ref="main",
    )
    target = make_target(adapter="claude-code", scope="local")

    install_entry(entry, target, tmp_path, dry_run=False)

    dest = tmp_path / ".claude" / "skills" / "python-pro"
    assert dest.is_dir()
    assert not dest.is_symlink()
    assert (dest / "SKILL.md").read_text() == "# Python Pro"


def test_install_agent_dir_entry_explodes_to_individual_files(tmp_path):
    vdir = tmp_path / ".skillfile" / "agents" / "core-dev"
    vdir.mkdir(parents=True)
    (vdir / "backend-developer.md").write_text("# Backend")
    (vdir / "frontend-developer.md").write_text("# Frontend")
    (vdir / ".meta").write_text("{}")

    entry = Entry(
        source_type="github",
        entity_type="agent",
        name="core-dev",
        owner_repo="owner/repo",
        path_in_repo="categories/core-dev",
        ref="main",
    )
    target = make_target(adapter="claude-code", scope="local")
    install_entry(entry, target, tmp_path, link_mode=True, dry_run=False)

    agents_dir = tmp_path / ".claude" / "agents"
    assert (agents_dir / "backend-developer.md").is_symlink()
    assert (agents_dir / "frontend-developer.md").is_symlink()
    assert (agents_dir / "backend-developer.md").read_text() == "# Backend"
    assert not (agents_dir / "core-dev").exists()


def test_install_agent_dir_entry_copy_mode(tmp_path):
    vdir = tmp_path / ".skillfile" / "agents" / "core-dev"
    vdir.mkdir(parents=True)
    (vdir / "backend-developer.md").write_text("# Backend")

    entry = Entry(
        source_type="github",
        entity_type="agent",
        name="core-dev",
        owner_repo="owner/repo",
        path_in_repo="categories/core-dev",
        ref="main",
    )
    target = make_target(adapter="claude-code", scope="local")
    install_entry(entry, target, tmp_path, dry_run=False)

    dest = tmp_path / ".claude" / "agents" / "backend-developer.md"
    assert dest.exists() and not dest.is_symlink()


def test_install_entry_missing_source_warns(tmp_path, capsys):
    # vendor file does not exist
    entry = make_agent_entry()
    target = make_target(scope="local")

    install_entry(entry, target, tmp_path, dry_run=False)

    captured = capsys.readouterr()
    assert "warning" in captured.err
    assert "my-agent" in captured.err


def test_install_entry_unknown_entity_type_skipped(tmp_path):
    entry = Entry(
        source_type="local",
        entity_type="hook",  # unknown for claude-code adapter
        name="my-hook",
        local_path="hooks/my-hook.md",
    )
    target = make_target(scope="local")

    # Should return without error or writing anything
    install_entry(entry, target, tmp_path, dry_run=False)

    assert not (tmp_path / ".claude").exists()


# ---------------------------------------------------------------------------
# cmd_install
# ---------------------------------------------------------------------------


def _make_args(dry_run=False, link=False, update=False):
    args = argparse.Namespace()
    args.dry_run = dry_run
    args.link = link
    args.update = update
    return args


def test_cmd_install_no_manifest(tmp_path):
    with pytest.raises(ManifestError, match="not found"):
        cmd_install(_make_args(), tmp_path)


def test_cmd_install_no_install_targets(tmp_path):
    (tmp_path / "Skillfile").write_text("local  skill  foo  skills/foo.md\n")
    with pytest.raises(ManifestError, match="No install targets"):
        cmd_install(_make_args(), tmp_path)


def test_cmd_install_unknown_adapter_warns(tmp_path, capsys):
    sf = tmp_path / "Skillfile"
    sf.write_text("install  unknown-adapter  global\nlocal  skill  foo  skills/foo.md\n")

    source_file = tmp_path / "skills" / "foo.md"
    source_file.parent.mkdir(parents=True)
    source_file.write_text("# Foo")

    cmd_install(_make_args(), tmp_path)
    assert "unknown platform" in capsys.readouterr().err


def test_cmd_install_dry_run_no_files(tmp_path):
    sf = tmp_path / "Skillfile"
    sf.write_text("install  claude-code  local\nlocal  skill  foo  skills/foo.md\n")

    source_file = tmp_path / "skills" / "foo.md"
    source_file.parent.mkdir(parents=True)
    source_file.write_text("# Foo")

    cmd_install(_make_args(dry_run=True), tmp_path)

    assert not (tmp_path / ".claude").exists()


# ---------------------------------------------------------------------------
# auto-pin on install --update
# ---------------------------------------------------------------------------


def test_cmd_install_update_auto_pins_modified_entry(tmp_path, capsys):
    """install --update auto-pins modified installed files before re-fetching."""
    from unittest.mock import patch as mock_patch

    from skillfile.lock import write_lock
    from skillfile.models import LockEntry
    from skillfile.patch import has_patch

    sf = tmp_path / "Skillfile"
    sf.write_text("install  claude-code  local\ngithub  agent  my-agent  owner/repo  agents/agent.md\n")

    sha = "a" * 40
    write_lock(tmp_path, {"github/agent/my-agent": LockEntry(sha=sha, raw_url="https://example.com")})

    # Set up vendor cache (pristine upstream)
    vdir = tmp_path / ".skillfile" / "agents" / "my-agent"
    vdir.mkdir(parents=True)
    (vdir / "agent.md").write_text("# Original\n")
    (vdir / ".meta").write_text('{"sha": "' + sha + '"}')

    # Installed file has user edits
    installed_dir = tmp_path / ".claude" / "agents"
    installed_dir.mkdir(parents=True)
    (installed_dir / "my-agent.md").write_text("# Original\n\n## My custom section\n")

    with mock_patch(
        "skillfile.install.sync_entry",
        return_value={"github/agent/my-agent": LockEntry(sha=sha, raw_url="https://example.com")},
    ):
        cmd_install(_make_args(update=True), tmp_path)

    # Patch should have been auto-generated
    from skillfile.models import Entry

    e = Entry("github", "agent", "my-agent", owner_repo="owner/repo", path_in_repo="agents/agent.md", ref="main")
    assert has_patch(e, tmp_path), "auto-pin should have written a patch for the modified entry"
    out = capsys.readouterr().out
    assert "auto-saved" in out


def test_cmd_install_update_no_auto_pin_for_clean_entry(tmp_path):
    """install --update does NOT auto-pin when installed matches cache."""
    from unittest.mock import patch as mock_patch

    from skillfile.lock import write_lock
    from skillfile.models import LockEntry
    from skillfile.patch import has_patch

    sf = tmp_path / "Skillfile"
    sf.write_text("install  claude-code  local\ngithub  agent  my-agent  owner/repo  agents/agent.md\n")

    sha = "a" * 40
    write_lock(tmp_path, {"github/agent/my-agent": LockEntry(sha=sha, raw_url="https://example.com")})

    vdir = tmp_path / ".skillfile" / "agents" / "my-agent"
    vdir.mkdir(parents=True)
    (vdir / "agent.md").write_text("# Original\n")
    (vdir / ".meta").write_text('{"sha": "' + sha + '"}')

    installed_dir = tmp_path / ".claude" / "agents"
    installed_dir.mkdir(parents=True)
    (installed_dir / "my-agent.md").write_text("# Original\n")  # matches cache exactly

    with mock_patch(
        "skillfile.install.sync_entry",
        return_value={"github/agent/my-agent": LockEntry(sha=sha, raw_url="https://example.com")},
    ):
        cmd_install(_make_args(update=True), tmp_path)

    from skillfile.models import Entry

    e = Entry("github", "agent", "my-agent", owner_repo="owner/repo", path_in_repo="agents/agent.md", ref="main")
    assert not has_patch(e, tmp_path), "no patch should be written when installed matches cache"
