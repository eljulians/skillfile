import argparse
import json
import textwrap
from unittest.mock import patch

import pytest

from skillfile.exceptions import ManifestError
from skillfile.status import cmd_status

from .helpers import write_manifest

ORIGINAL = textwrap.dedent("""\
    # Agent

    Upstream content.
""")

MODIFIED = textwrap.dedent("""\
    # Agent

    Upstream content.

    ## Custom Section

    Added by user.
""")

SHA = "a" * 40


def write_lock(tmp_path, locked: dict):
    (tmp_path / "Skillfile.lock").write_text(json.dumps(locked))


def write_meta(tmp_path, entity_type, name, sha):
    vdir = tmp_path / ".skillfile" / f"{entity_type}s" / name
    vdir.mkdir(parents=True, exist_ok=True)
    (vdir / ".meta").write_text(json.dumps({"sha": sha}))


def write_vendor_content(tmp_path, entity_type, name, filename, content):
    """Write a vendor cache content file (for _is_modified_local to read)."""
    vdir = tmp_path / ".skillfile" / f"{entity_type}s" / name
    vdir.mkdir(parents=True, exist_ok=True)
    (vdir / filename).write_text(content)


def _make_args(check_upstream=False):
    return argparse.Namespace(check_upstream=check_upstream)


# ---------------------------------------------------------------------------
# No manifest
# ---------------------------------------------------------------------------


def test_cmd_status_no_manifest(tmp_path):
    with pytest.raises(ManifestError, match="not found"):
        cmd_status(_make_args(), tmp_path)


# ---------------------------------------------------------------------------
# Local entries
# ---------------------------------------------------------------------------


def test_local_entry_shows_local(tmp_path, capsys):
    write_manifest(tmp_path, "local  skill  foo  skills/foo.md\n")

    cmd_status(_make_args(), tmp_path)

    out = capsys.readouterr().out
    assert "foo" in out
    assert "local" in out


# ---------------------------------------------------------------------------
# Unlocked entries
# ---------------------------------------------------------------------------


def test_github_entry_unlocked(tmp_path, capsys):
    write_manifest(tmp_path, "github  agent  my-agent  owner/repo  agents/agent.md  main\n")

    cmd_status(_make_args(), tmp_path)

    out = capsys.readouterr().out
    assert "my-agent" in out
    assert "unlocked" in out


# ---------------------------------------------------------------------------
# Locked entries — vendor present
# ---------------------------------------------------------------------------


def test_github_entry_locked_vendor_matches(tmp_path, capsys):
    sha = "87321636a1c666283d8f17398b45c2644395044b"
    write_manifest(tmp_path, "github  agent  my-agent  owner/repo  agents/agent.md  main\n")
    write_lock(tmp_path, {"github/agent/my-agent": {"sha": sha, "raw_url": "https://example.com"}})
    write_meta(tmp_path, "agent", "my-agent", sha)

    cmd_status(_make_args(), tmp_path)

    out = capsys.readouterr().out
    assert "my-agent" in out
    assert "locked" in out
    assert sha[:12] in out


def test_github_entry_locked_vendor_missing(tmp_path, capsys):
    sha = "87321636a1c666283d8f17398b45c2644395044b"
    write_manifest(tmp_path, "github  agent  my-agent  owner/repo  agents/agent.md  main\n")
    write_lock(tmp_path, {"github/agent/my-agent": {"sha": sha, "raw_url": "https://example.com"}})
    # no .meta written

    cmd_status(_make_args(), tmp_path)

    out = capsys.readouterr().out
    assert "stale" in out or "missing" in out


# ---------------------------------------------------------------------------
# --check-upstream
# ---------------------------------------------------------------------------


def test_check_upstream_up_to_date(tmp_path, capsys):
    sha = "87321636a1c666283d8f17398b45c2644395044b"
    write_manifest(tmp_path, "github  agent  my-agent  owner/repo  agents/agent.md  main\n")
    write_lock(tmp_path, {"github/agent/my-agent": {"sha": sha, "raw_url": "https://example.com"}})
    write_meta(tmp_path, "agent", "my-agent", sha)

    with patch("skillfile.status.resolve_github_sha", return_value=sha):
        cmd_status(_make_args(check_upstream=True), tmp_path)

    out = capsys.readouterr().out
    assert "up to date" in out


def test_check_upstream_outdated(tmp_path, capsys):
    locked_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    upstream_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    write_manifest(tmp_path, "github  agent  my-agent  owner/repo  agents/agent.md  main\n")
    write_lock(tmp_path, {"github/agent/my-agent": {"sha": locked_sha, "raw_url": "https://example.com"}})
    write_meta(tmp_path, "agent", "my-agent", locked_sha)

    with patch("skillfile.status.resolve_github_sha", return_value=upstream_sha):
        cmd_status(_make_args(check_upstream=True), tmp_path)

    out = capsys.readouterr().out
    assert "outdated" in out
    assert locked_sha[:12] in out
    assert upstream_sha[:12] in out


# ---------------------------------------------------------------------------
# [modified] annotation (always local, no flag needed)
# ---------------------------------------------------------------------------


def test_modified_shows_for_changed_installed_file(tmp_path, capsys):
    write_manifest(
        tmp_path,
        "install claude-code local\ngithub agent my-agent owner/repo agents/agent.md main\n",
    )
    write_lock(tmp_path, {"github/agent/my-agent": {"sha": SHA, "raw_url": "https://example.com"}})
    write_meta(tmp_path, "agent", "my-agent", SHA)
    write_vendor_content(tmp_path, "agent", "my-agent", "agent.md", ORIGINAL)
    installed = tmp_path / ".claude" / "agents"
    installed.mkdir(parents=True)
    (installed / "my-agent.md").write_text(MODIFIED)

    cmd_status(_make_args(), tmp_path)

    assert "[modified]" in capsys.readouterr().out


def test_modified_not_shown_for_clean_entry(tmp_path, capsys):
    write_manifest(
        tmp_path,
        "install claude-code local\ngithub agent my-agent owner/repo agents/agent.md main\n",
    )
    write_lock(tmp_path, {"github/agent/my-agent": {"sha": SHA, "raw_url": "https://example.com"}})
    write_meta(tmp_path, "agent", "my-agent", SHA)
    write_vendor_content(tmp_path, "agent", "my-agent", "agent.md", ORIGINAL)
    installed = tmp_path / ".claude" / "agents"
    installed.mkdir(parents=True)
    (installed / "my-agent.md").write_text(ORIGINAL)

    cmd_status(_make_args(), tmp_path)

    assert "[modified]" not in capsys.readouterr().out


def test_modified_not_shown_when_not_installed(tmp_path, capsys):
    write_manifest(
        tmp_path,
        "install claude-code local\ngithub agent my-agent owner/repo agents/agent.md main\n",
    )
    write_lock(tmp_path, {"github/agent/my-agent": {"sha": SHA, "raw_url": "https://example.com"}})
    write_meta(tmp_path, "agent", "my-agent", SHA)
    write_vendor_content(tmp_path, "agent", "my-agent", "agent.md", ORIGINAL)
    # No installed file

    cmd_status(_make_args(), tmp_path)

    assert "[modified]" not in capsys.readouterr().out


def test_modified_dir_entry_shows_modified(tmp_path, capsys):
    write_manifest(
        tmp_path,
        "install claude-code local\ngithub skill arch-patterns owner/repo categories/arch main\n",
    )
    write_lock(tmp_path, {"github/skill/arch-patterns": {"sha": SHA, "raw_url": "https://example.com"}})
    write_meta(tmp_path, "skill", "arch-patterns", SHA)
    write_vendor_content(tmp_path, "skill", "arch-patterns", "SKILL.md", ORIGINAL)
    installed_dir = tmp_path / ".claude" / "skills" / "arch-patterns"
    installed_dir.mkdir(parents=True)
    (installed_dir / "SKILL.md").write_text(MODIFIED)

    cmd_status(_make_args(), tmp_path)

    assert "[modified]" in capsys.readouterr().out


def test_modified_not_shown_without_vendor_cache(tmp_path, capsys):
    """Without a vendor cache, [modified] is never shown (graceful degradation)."""
    write_manifest(
        tmp_path,
        "install claude-code local\ngithub agent my-agent owner/repo agents/agent.md main\n",
    )
    write_lock(tmp_path, {"github/agent/my-agent": {"sha": SHA, "raw_url": "https://example.com"}})
    write_meta(tmp_path, "agent", "my-agent", SHA)
    installed = tmp_path / ".claude" / "agents"
    installed.mkdir(parents=True)
    (installed / "my-agent.md").write_text(MODIFIED)
    # No vendor cache file — _is_modified_local returns False gracefully

    cmd_status(_make_args(), tmp_path)

    assert "[modified]" not in capsys.readouterr().out
