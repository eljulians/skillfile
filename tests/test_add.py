import argparse
from unittest.mock import patch

import pytest

from skillfile.add import cmd_add


def write_manifest(tmp_path, content=""):
    p = tmp_path / "Skillfile"
    p.write_text(content)
    return p


def _github(entity_type, owner_repo, path, ref=None, name=None):
    return argparse.Namespace(
        add_source="github",
        entity_type=entity_type,
        owner_repo=owner_repo,
        path=path,
        ref=ref,
        name=name,
    )


def _local(entity_type, path, name=None):
    return argparse.Namespace(
        add_source="local",
        entity_type=entity_type,
        path=path,
        name=name,
    )


def _url(entity_type, url, name=None):
    return argparse.Namespace(
        add_source="url",
        entity_type=entity_type,
        url=url,
        name=name,
    )


# ---------------------------------------------------------------------------
# No manifest
# ---------------------------------------------------------------------------

def test_no_manifest(tmp_path, capsys):
    with pytest.raises(SystemExit):
        cmd_add(_github("agent", "owner/repo", "path/to/agent.md"), tmp_path)
    assert "not found" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# GitHub entries
# ---------------------------------------------------------------------------

def test_add_github_inferred_name(tmp_path):
    write_manifest(tmp_path)
    cmd_add(_github("agent", "owner/repo", "path/to/agent.md"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    assert "github  agent  owner/repo  path/to/agent.md\n" in text


def test_add_github_no_ref_when_default(tmp_path):
    write_manifest(tmp_path)
    cmd_add(_github("agent", "owner/repo", "path/to/agent.md", ref="main"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    assert "main" not in text


def test_add_github_explicit_ref(tmp_path):
    write_manifest(tmp_path)
    cmd_add(_github("agent", "owner/repo", "path/to/agent.md", ref="v1.2.0"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    assert "v1.2.0" in text


def test_add_github_explicit_name(tmp_path):
    write_manifest(tmp_path)
    cmd_add(_github("agent", "owner/repo", "path/to/agent.md", name="my-agent"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    assert "github  agent  my-agent  owner/repo  path/to/agent.md\n" in text


def test_add_github_explicit_name_omitted_when_matches_stem(tmp_path):
    write_manifest(tmp_path)
    cmd_add(_github("agent", "owner/repo", "path/to/agent.md", name="agent"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    # name matches stem — should be omitted
    assert "github  agent  owner/repo  path/to/agent.md\n" in text


# ---------------------------------------------------------------------------
# Local entries
# ---------------------------------------------------------------------------

def test_add_local_inferred_name(tmp_path):
    write_manifest(tmp_path)
    cmd_add(_local("skill", "skills/git/commit.md"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    assert "local  skill  skills/git/commit.md\n" in text


def test_add_local_explicit_name(tmp_path):
    write_manifest(tmp_path)
    cmd_add(_local("skill", "skills/git/commit.md", name="my-commit"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    assert "local  skill  my-commit  skills/git/commit.md\n" in text


# ---------------------------------------------------------------------------
# URL entries
# ---------------------------------------------------------------------------

def test_add_url_inferred_name(tmp_path):
    write_manifest(tmp_path)
    cmd_add(_url("skill", "https://example.com/my-skill.md"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    assert "url  skill  https://example.com/my-skill.md\n" in text


def test_add_url_explicit_name(tmp_path):
    write_manifest(tmp_path)
    cmd_add(_url("skill", "https://example.com/my-skill.md", name="custom-name"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    assert "url  skill  custom-name  https://example.com/my-skill.md\n" in text


# ---------------------------------------------------------------------------
# Duplicate name
# ---------------------------------------------------------------------------

def test_add_duplicate_name_errors(tmp_path, capsys):
    write_manifest(tmp_path, "local  skill  skills/commit.md\n")
    with pytest.raises(SystemExit):
        cmd_add(_local("skill", "skills/commit.md"), tmp_path)
    assert "already exists" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# Auto-install after add
# ---------------------------------------------------------------------------

def test_add_triggers_install_when_targets_configured(tmp_path):
    write_manifest(tmp_path, "install  claude-code  local\n")
    with patch("skillfile.add.sync_entry", return_value={}) as mock_sync:
        with patch("skillfile.add.install_entry") as mock_install:
            cmd_add(_local("skill", "skills/new.md"), tmp_path)
    mock_sync.assert_called_once()
    mock_install.assert_called_once()


def test_add_no_install_when_no_targets(tmp_path, capsys):
    write_manifest(tmp_path)
    with patch("skillfile.add.sync_entry") as mock_sync:
        cmd_add(_local("skill", "skills/new.md"), tmp_path)
    mock_sync.assert_not_called()
    assert "skillfile init" in capsys.readouterr().out


# ---------------------------------------------------------------------------
# Directory path entries
# ---------------------------------------------------------------------------

def test_add_github_directory_path_accepted(tmp_path):
    write_manifest(tmp_path)
    cmd_add(_github("skill", "owner/repo", "categories/01-core-development"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    assert "categories/01-core-development" in text


def test_add_github_directory_name_inferred_from_last_segment(tmp_path):
    write_manifest(tmp_path)
    cmd_add(_github("skill", "owner/repo", "categories/python-pro"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    # 'python-pro' inferred as name, path kept as-is, name omitted since it matches stem
    assert "github  skill  owner/repo  categories/python-pro\n" in text


# ---------------------------------------------------------------------------
# Rollback on install failure
# ---------------------------------------------------------------------------

def test_add_rollback_on_install_failure(tmp_path, capsys):
    write_manifest(tmp_path, "install  claude-code  local\n")

    def failing_install(*_args, **_kwargs):
        raise SystemExit(1)

    with patch("skillfile.add.sync_entry", side_effect=failing_install):
        with pytest.raises(SystemExit):
            cmd_add(_local("skill", "skills/new.md"), tmp_path)

    text = (tmp_path / "Skillfile").read_text()
    assert "skills/new.md" not in text
    assert "Rolled back" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# Appends without overwriting existing content
# ---------------------------------------------------------------------------

def test_add_appends_to_existing_content(tmp_path):
    write_manifest(tmp_path, "local  skill  skills/existing.md\n")
    cmd_add(_local("skill", "skills/new.md"), tmp_path)
    text = (tmp_path / "Skillfile").read_text()
    assert "existing" in text
    assert "new" in text
