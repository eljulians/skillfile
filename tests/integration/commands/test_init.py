import argparse
from unittest.mock import patch

import pytest

from skillfile.commands.init import cmd_init
from skillfile.exceptions import ManifestError
from tests.helpers import write_manifest


def _make_args():
    return argparse.Namespace()


# ---------------------------------------------------------------------------
# cmd_init — no manifest
# ---------------------------------------------------------------------------


def test_cmd_init_no_manifest(tmp_path):
    with pytest.raises(ManifestError, match="not found"):
        cmd_init(_make_args(), tmp_path)


# ---------------------------------------------------------------------------
# cmd_init — fresh Skillfile (no existing install targets)
# ---------------------------------------------------------------------------


def test_cmd_init_writes_install_lines(tmp_path):
    write_manifest(
        tmp_path,
        """\
        local  skill  foo  skills/foo.md
    """,
    )

    # Simulate: adapter=claude-code, scope=global, no more adapters (n)
    inputs = iter(["claude-code", "global", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    text = (tmp_path / "Skillfile").read_text()
    assert "install  claude-code  global" in text


def test_cmd_init_install_lines_at_top(tmp_path):
    write_manifest(
        tmp_path,
        """\
        local  skill  foo  skills/foo.md
    """,
    )

    inputs = iter(["claude-code", "global", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    text = (tmp_path / "Skillfile").read_text()
    install_pos = text.index("install")
    entry_pos = text.index("local  skill")
    assert install_pos < entry_pos


def test_cmd_init_preserves_existing_entries(tmp_path):
    write_manifest(
        tmp_path,
        """\
        local  skill  foo  skills/foo.md
    """,
    )

    inputs = iter(["claude-code", "local", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    text = (tmp_path / "Skillfile").read_text()
    assert "local  skill  foo  skills/foo.md" in text
    assert "install  claude-code  local" in text


def test_cmd_init_scope_both_writes_two_lines(tmp_path):
    write_manifest(tmp_path, "")

    inputs = iter(["claude-code", "both", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    text = (tmp_path / "Skillfile").read_text()
    assert "install  claude-code  global" in text
    assert "install  claude-code  local" in text


def test_cmd_init_multiple_adapters(tmp_path):
    write_manifest(tmp_path, "")

    # First adapter, then add another, second adapter, stop
    inputs = iter(["claude-code", "global", "y", "claude-code", "local", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    text = (tmp_path / "Skillfile").read_text()
    assert text.count("install") == 2


# ---------------------------------------------------------------------------
# cmd_init — idempotency: existing install targets trigger confirmation
# ---------------------------------------------------------------------------


def test_cmd_init_existing_targets_confirmed(tmp_path):
    write_manifest(
        tmp_path,
        """\
        install  claude-code  global
        local  skill  foo  skills/foo.md
    """,
    )

    # Confirm replacement (y), then new config
    inputs = iter(["y", "claude-code", "local", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    text = (tmp_path / "Skillfile").read_text()
    # Old global replaced with new local
    assert "install  claude-code  local" in text
    assert "install  claude-code  global" not in text


def test_cmd_init_existing_targets_aborted(tmp_path):
    write_manifest(
        tmp_path,
        """\
        install  claude-code  global
    """,
    )

    # Decline replacement (n)
    inputs = iter(["n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    # Skillfile should be unchanged
    text = (tmp_path / "Skillfile").read_text()
    assert "install  claude-code  global" in text
    assert "install  claude-code  local" not in text


def test_cmd_init_replaces_only_install_lines(tmp_path):
    write_manifest(
        tmp_path,
        """\
        install  claude-code  global
        local  skill  foo  skills/foo.md
        github  agent  my-agent  owner/repo  agents/agent.md  main
    """,
    )

    inputs = iter(["y", "claude-code", "local", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    text = (tmp_path / "Skillfile").read_text()
    assert "local  skill  foo  skills/foo.md" in text
    assert "github  agent  my-agent  owner/repo  agents/agent.md  main" in text
    assert "install  claude-code  local" in text
    assert "install  claude-code  global" not in text


# ---------------------------------------------------------------------------
# cmd_init — .gitignore management
# ---------------------------------------------------------------------------


def test_cmd_init_creates_gitignore_when_missing(tmp_path):
    write_manifest(tmp_path, "")

    inputs = iter(["claude-code", "global", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    gitignore = (tmp_path / ".gitignore").read_text()
    assert ".skillfile/cache/" in gitignore
    assert ".skillfile/conflict" in gitignore


def test_cmd_init_appends_to_existing_gitignore(tmp_path):
    write_manifest(tmp_path, "")
    (tmp_path / ".gitignore").write_text("*.pyc\n")

    inputs = iter(["claude-code", "global", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    gitignore = (tmp_path / ".gitignore").read_text()
    assert "*.pyc" in gitignore
    assert ".skillfile/cache/" in gitignore
    assert ".skillfile/conflict" in gitignore


def test_cmd_init_gitignore_idempotent(tmp_path):
    write_manifest(tmp_path, "")
    (tmp_path / ".gitignore").write_text(".skillfile/cache/\n.skillfile/conflict\n")

    inputs = iter(["claude-code", "global", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    gitignore = (tmp_path / ".gitignore").read_text()
    assert gitignore.count(".skillfile/cache/") == 1
    assert gitignore.count(".skillfile/conflict") == 1


def test_cmd_init_patches_not_in_gitignore(tmp_path):
    """patches/ must be committed, so it must not appear in .gitignore."""
    write_manifest(tmp_path, "")

    inputs = iter(["claude-code", "global", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    gitignore = (tmp_path / ".gitignore").read_text()
    assert ".skillfile/patches" not in gitignore
