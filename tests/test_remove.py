import argparse
import json

import pytest

from skillfile.remove import cmd_remove


def write_manifest(tmp_path, content):
    p = tmp_path / "Skillfile"
    p.write_text(content)
    return p


def write_lock(tmp_path, locked: dict):
    (tmp_path / "Skillfile.lock").write_text(json.dumps(locked))


def write_cache(tmp_path, entity_type, name, filename="agent.md"):
    vdir = tmp_path / ".skillfile" / f"{entity_type}s" / name
    vdir.mkdir(parents=True)
    (vdir / filename).write_text("# content")
    return vdir


def _make_args(name):
    return argparse.Namespace(name=name)


# ---------------------------------------------------------------------------
# No manifest
# ---------------------------------------------------------------------------

def test_no_manifest(tmp_path, capsys):
    with pytest.raises(SystemExit):
        cmd_remove(_make_args("foo"), tmp_path)
    assert "not found" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# Entry not found
# ---------------------------------------------------------------------------

def test_unknown_name_errors(tmp_path, capsys):
    write_manifest(tmp_path, "local  skill  skills/foo.md\n")
    with pytest.raises(SystemExit):
        cmd_remove(_make_args("nonexistent"), tmp_path)
    assert "no entry named" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# Removes line from Skillfile
# ---------------------------------------------------------------------------

def test_remove_github_entry_removes_line(tmp_path):
    write_manifest(tmp_path, (
        "github  agent  owner/repo  agents/my-agent.md\n"
        "local  skill  skills/foo.md\n"
    ))
    cmd_remove(_make_args("my-agent"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    assert "my-agent" not in text
    assert "skills/foo.md" in text


def test_remove_local_entry_removes_line(tmp_path):
    write_manifest(tmp_path, "local  skill  skills/foo.md\n")
    cmd_remove(_make_args("foo"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    assert "foo" not in text


def test_remove_preserves_comments_and_blanks(tmp_path):
    write_manifest(tmp_path, (
        "# My agents\n"
        "\n"
        "local  skill  skills/foo.md\n"
    ))
    cmd_remove(_make_args("foo"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    assert "# My agents" in text


# ---------------------------------------------------------------------------
# Clears cache directory
# ---------------------------------------------------------------------------

def test_remove_clears_cache(tmp_path):
    write_manifest(tmp_path, "github  agent  owner/repo  agents/my-agent.md\n")
    vdir = write_cache(tmp_path, "agent", "my-agent")
    cmd_remove(_make_args("my-agent"), tmp_path)
    assert not vdir.exists()


def test_remove_local_entry_no_cache_no_error(tmp_path):
    write_manifest(tmp_path, "local  skill  skills/foo.md\n")
    # No cache dir — should complete without error
    cmd_remove(_make_args("foo"), tmp_path)
    assert not (tmp_path / ".skillfile").exists()


# ---------------------------------------------------------------------------
# Updates lock file
# ---------------------------------------------------------------------------

def test_remove_updates_lock(tmp_path):
    write_manifest(tmp_path, "github  agent  owner/repo  agents/my-agent.md\n")
    write_lock(tmp_path, {
        "github/agent/my-agent": {"sha": "abc123", "raw_url": "https://example.com"},
        "github/agent/other": {"sha": "def456", "raw_url": "https://example.com/other"},
    })
    cmd_remove(_make_args("my-agent"), tmp_path)
    lock = json.loads((tmp_path / "Skillfile.lock").read_text())
    assert "github/agent/my-agent" not in lock
    assert "github/agent/other" in lock


def test_remove_no_lock_entry_no_error(tmp_path):
    write_manifest(tmp_path, "github  agent  owner/repo  agents/my-agent.md\n")
    # No lock file at all
    cmd_remove(_make_args("my-agent"), tmp_path)
