import argparse
import textwrap
from unittest.mock import patch as mock_patch

import pytest

from skillfile.conflict import ConflictState, write_conflict
from skillfile.diff import cmd_diff
from skillfile.exceptions import ManifestError
from skillfile.lock import write_lock
from skillfile.models import LockEntry

from .helpers import write_manifest

OLD_CONTENT = textwrap.dedent("""\
    # Agent

    Original upstream content.
""")

NEW_CONTENT = textwrap.dedent("""\
    # Agent

    Updated upstream content.
""")

INSTALLED_CONTENT = textwrap.dedent("""\
    # Agent

    Original upstream content.

    ## Custom Section

    Added by user.
""")

SHA_OLD = "a" * 40
SHA_NEW = "b" * 40


def make_args(name: str) -> argparse.Namespace:
    return argparse.Namespace(name=name)


def setup_conflict(tmp_path, entry_name: str = "test-agent", old_sha: str = SHA_OLD, new_sha: str = SHA_NEW) -> None:
    write_conflict(tmp_path, ConflictState(entry=entry_name, entity_type="agent", old_sha=old_sha, new_sha=new_sha))


def setup_lock(tmp_path, name: str = "test-agent", sha: str = SHA_OLD) -> None:
    write_lock(
        tmp_path,
        {f"github/agent/{name}": LockEntry(sha=sha, raw_url="https://example.com")},
    )


def setup_installed(tmp_path, name: str, content: str, entity_type: str = "agent") -> None:
    d = tmp_path / ".claude" / f"{entity_type}s"
    d.mkdir(parents=True, exist_ok=True)
    (d / f"{name}.md").write_text(content)


def setup_vendor_cache(
    tmp_path, name: str, content: str, entity_type: str = "agent", content_file: str = "test.md"
) -> None:
    """Write a single-file entry to the vendor cache."""
    vdir = tmp_path / ".skillfile" / f"{entity_type}s" / name
    vdir.mkdir(parents=True, exist_ok=True)
    (vdir / content_file).write_text(content)


# ---------------------------------------------------------------------------
# Non-conflict mode: local changes
# ---------------------------------------------------------------------------


def test_diff_no_conflict_shows_local_changes(tmp_path, capsys):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    setup_lock(tmp_path)
    setup_vendor_cache(tmp_path, "test-agent", OLD_CONTENT)
    setup_installed(tmp_path, "test-agent", INSTALLED_CONTENT)

    cmd_diff(make_args("test-agent"), tmp_path)

    out = capsys.readouterr().out
    assert "---" in out or "+++" in out


def test_diff_no_conflict_clean_entry_prints_clean(tmp_path, capsys):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    setup_lock(tmp_path)
    setup_vendor_cache(tmp_path, "test-agent", OLD_CONTENT)
    setup_installed(tmp_path, "test-agent", OLD_CONTENT)

    cmd_diff(make_args("test-agent"), tmp_path)

    assert "clean" in capsys.readouterr().out


def test_diff_no_conflict_local_entry_prints_message(tmp_path, capsys):
    write_manifest(tmp_path, "install claude-code local\nlocal agent my-agent agents/my-agent.md\n")
    cmd_diff(make_args("my-agent"), tmp_path)
    assert "local entry" in capsys.readouterr().out


def test_diff_no_conflict_unlocked_raises(tmp_path):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    # No lock file → unlocked
    with pytest.raises(ManifestError, match="not locked"):
        cmd_diff(make_args("test-agent"), tmp_path)


def test_diff_no_conflict_not_installed_raises(tmp_path):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    setup_lock(tmp_path)
    setup_vendor_cache(tmp_path, "test-agent", OLD_CONTENT)
    # No installed file
    with pytest.raises(ManifestError, match="not installed"):
        cmd_diff(make_args("test-agent"), tmp_path)


def test_diff_no_conflict_uses_upstream_and_installed_labels(tmp_path, capsys):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    setup_lock(tmp_path)
    setup_vendor_cache(tmp_path, "test-agent", OLD_CONTENT)
    setup_installed(tmp_path, "test-agent", INSTALLED_CONTENT)

    cmd_diff(make_args("test-agent"), tmp_path)

    out = capsys.readouterr().out
    assert "upstream" in out
    assert "installed" in out


# ---------------------------------------------------------------------------
# Conflict mode: upstream delta (existing tests — unchanged)
# ---------------------------------------------------------------------------


def test_diff_prints_upstream_delta(tmp_path, capsys):
    write_manifest(tmp_path, "github agent test-agent owner/repo agents/test.md\n")
    setup_conflict(tmp_path)

    with mock_patch(
        "skillfile.strategies.fetch_github_file",
        side_effect=[OLD_CONTENT.encode(), NEW_CONTENT.encode()],
    ):
        cmd_diff(make_args("test-agent"), tmp_path)

    out = capsys.readouterr().out
    assert "---" in out or "+++" in out


def test_diff_reports_no_upstream_change_when_shas_identical(tmp_path, capsys):
    write_manifest(tmp_path, "github agent test-agent owner/repo agents/test.md\n")
    setup_conflict(tmp_path)

    # Both SHAs return same content → no upstream delta
    with mock_patch("skillfile.strategies.fetch_github_file", return_value=OLD_CONTENT.encode()):
        cmd_diff(make_args("test-agent"), tmp_path)

    assert "No upstream changes" in capsys.readouterr().out


def test_diff_fetches_at_correct_shas(tmp_path):
    write_manifest(tmp_path, "github agent test-agent owner/repo agents/test.md\n")
    setup_conflict(tmp_path, old_sha=SHA_OLD, new_sha=SHA_NEW)

    with mock_patch("skillfile.strategies.fetch_github_file", return_value=OLD_CONTENT.encode()) as mock_fetch:
        cmd_diff(make_args("test-agent"), tmp_path)

    calls = mock_fetch.call_args_list
    assert len(calls) == 2
    # First call uses old SHA, second uses new SHA
    assert calls[0].args[2] == SHA_OLD
    assert calls[1].args[2] == SHA_NEW


def test_diff_conflict_for_different_entry_falls_through_to_local(tmp_path):
    """Conflict for another entry → diff shows local changes for requested entry."""
    write_manifest(
        tmp_path,
        "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n",
    )
    setup_conflict(tmp_path, entry_name="other-agent")
    setup_lock(tmp_path)
    setup_vendor_cache(tmp_path, "test-agent", OLD_CONTENT)
    setup_installed(tmp_path, "test-agent", OLD_CONTENT)

    # No conflict for test-agent → local path → clean (OLD_CONTENT == OLD_CONTENT)
    cmd_diff(make_args("test-agent"), tmp_path)
