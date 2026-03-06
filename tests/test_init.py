import argparse
import textwrap
from io import StringIO
from unittest.mock import patch

import pytest

from skillfile.init import cmd_init


def write_manifest(tmp_path, content):
    p = tmp_path / "Skillfile"
    p.write_text(textwrap.dedent(content))
    return p


def _make_args():
    return argparse.Namespace()


# ---------------------------------------------------------------------------
# cmd_init — no manifest
# ---------------------------------------------------------------------------

def test_cmd_init_no_manifest(tmp_path, capsys):
    with pytest.raises(SystemExit):
        cmd_init(_make_args(), tmp_path)
    assert "not found" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# cmd_init — fresh Skillfile (no existing install targets)
# ---------------------------------------------------------------------------

def test_cmd_init_writes_install_lines(tmp_path):
    write_manifest(tmp_path, """\
        local  skill  foo  skills/foo.md
    """)

    # Simulate: adapter=claude-code, scope=global, no more adapters (n)
    inputs = iter(["claude-code", "global", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    text = (tmp_path / "Skillfile").read_text()
    assert "install  claude-code  global" in text


def test_cmd_init_install_lines_at_top(tmp_path):
    write_manifest(tmp_path, """\
        local  skill  foo  skills/foo.md
    """)

    inputs = iter(["claude-code", "global", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    text = (tmp_path / "Skillfile").read_text()
    install_pos = text.index("install")
    entry_pos = text.index("local  skill")
    assert install_pos < entry_pos


def test_cmd_init_preserves_existing_entries(tmp_path):
    write_manifest(tmp_path, """\
        local  skill  foo  skills/foo.md
    """)

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
    write_manifest(tmp_path, """\
        install  claude-code  global
        local  skill  foo  skills/foo.md
    """)

    # Confirm replacement (y), then new config
    inputs = iter(["y", "claude-code", "local", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    text = (tmp_path / "Skillfile").read_text()
    # Old global replaced with new local
    assert "install  claude-code  local" in text
    assert "install  claude-code  global" not in text


def test_cmd_init_existing_targets_aborted(tmp_path):
    write_manifest(tmp_path, """\
        install  claude-code  global
    """)

    # Decline replacement (n)
    inputs = iter(["n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    # Skillfile should be unchanged
    text = (tmp_path / "Skillfile").read_text()
    assert "install  claude-code  global" in text
    assert "install  claude-code  local" not in text


def test_cmd_init_replaces_only_install_lines(tmp_path):
    write_manifest(tmp_path, """\
        install  claude-code  global
        local  skill  foo  skills/foo.md
        github  agent  my-agent  owner/repo  agents/agent.md  main
    """)

    inputs = iter(["y", "claude-code", "local", "n"])
    with patch("builtins.input", side_effect=lambda _: next(inputs)):
        cmd_init(_make_args(), tmp_path)

    text = (tmp_path / "Skillfile").read_text()
    assert "local  skill  foo  skills/foo.md" in text
    assert "github  agent  my-agent  owner/repo  agents/agent.md  main" in text
    assert "install  claude-code  local" in text
    assert "install  claude-code  global" not in text
