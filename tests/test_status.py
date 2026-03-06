import argparse
import json
from unittest.mock import patch

import pytest

from skillfile.status import cmd_status


def write_manifest(tmp_path, content):
    p = tmp_path / "Skillfile"
    p.write_text(content)
    return p


def write_lock(tmp_path, locked: dict):
    (tmp_path / "Skillfile.lock").write_text(json.dumps(locked))


def write_meta(tmp_path, entity_type, name, sha):
    vdir = tmp_path / ".skillfile" / f"{entity_type}s" / name
    vdir.mkdir(parents=True, exist_ok=True)
    (vdir / ".meta").write_text(json.dumps({"sha": sha}))


def _make_args(check_upstream=False):
    return argparse.Namespace(check_upstream=check_upstream)


# ---------------------------------------------------------------------------
# No manifest
# ---------------------------------------------------------------------------

def test_cmd_status_no_manifest(tmp_path, capsys):
    with pytest.raises(SystemExit):
        cmd_status(_make_args(), tmp_path)
    assert "not found" in capsys.readouterr().err


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
