import argparse

import pytest

from skillfile.validate import cmd_validate


def write_manifest(tmp_path, content):
    p = tmp_path / "Skillfile"
    p.write_text(content)
    return p


def _make_args():
    return argparse.Namespace()


# ---------------------------------------------------------------------------
# No manifest
# ---------------------------------------------------------------------------

def test_no_manifest(tmp_path, capsys):
    with pytest.raises(SystemExit):
        cmd_validate(_make_args(), tmp_path)
    assert "not found" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# Valid manifests
# ---------------------------------------------------------------------------

def test_valid_empty_manifest(tmp_path, capsys):
    write_manifest(tmp_path, "")
    cmd_validate(_make_args(), tmp_path)
    assert "OK" in capsys.readouterr().out


def test_valid_github_entry(tmp_path, capsys):
    write_manifest(tmp_path, "github  agent  owner/repo  agents/agent.md\n")
    cmd_validate(_make_args(), tmp_path)
    assert "OK" in capsys.readouterr().out


def test_valid_local_entry_existing_path(tmp_path, capsys):
    source = tmp_path / "skills" / "foo.md"
    source.parent.mkdir()
    source.write_text("# Foo")
    write_manifest(tmp_path, "local  skill  skills/foo.md\n")
    cmd_validate(_make_args(), tmp_path)
    assert "OK" in capsys.readouterr().out


def test_valid_with_known_install_target(tmp_path, capsys):
    write_manifest(tmp_path, "install  claude-code  global\n")
    cmd_validate(_make_args(), tmp_path)
    assert "OK" in capsys.readouterr().out


# ---------------------------------------------------------------------------
# Duplicate names
# ---------------------------------------------------------------------------

def test_duplicate_name_errors(tmp_path, capsys):
    write_manifest(tmp_path, (
        "local  skill  skills/foo.md\n"
        "github  agent  owner/repo  skills/foo.md\n"
    ))
    with pytest.raises(SystemExit):
        cmd_validate(_make_args(), tmp_path)
    assert "duplicate" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# Missing local path
# ---------------------------------------------------------------------------

def test_missing_local_path_errors(tmp_path, capsys):
    write_manifest(tmp_path, "local  skill  skills/nonexistent.md\n")
    with pytest.raises(SystemExit):
        cmd_validate(_make_args(), tmp_path)
    assert "not found" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# Unknown platform
# ---------------------------------------------------------------------------

def test_unknown_platform_errors(tmp_path, capsys):
    write_manifest(tmp_path, "install  unknown-platform  global\n")
    with pytest.raises(SystemExit):
        cmd_validate(_make_args(), tmp_path)
    assert "unknown platform" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# Multiple errors reported
# ---------------------------------------------------------------------------

def test_multiple_errors_all_reported(tmp_path, capsys):
    write_manifest(tmp_path, (
        "install  unknown-platform  global\n"
        "local  skill  skills/missing.md\n"
    ))
    with pytest.raises(SystemExit):
        cmd_validate(_make_args(), tmp_path)
    err = capsys.readouterr().err
    assert "unknown platform" in err
    assert "not found" in err
