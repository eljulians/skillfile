import argparse
import textwrap
from unittest.mock import patch as mock_patch

import pytest

from skillfile.conflict import ConflictState, has_conflict, write_conflict
from skillfile.exceptions import ManifestError
from skillfile.patch import generate_patch, has_patch, write_patch
from skillfile.resolve import _three_way_merge, cmd_resolve

from .helpers import make_github_entry, write_manifest

# Use two independent sections so theirs and yours touch non-overlapping
# regions — guaranteeing a clean three-way merge.

# Common ancestor.
BASE = textwrap.dedent("""\
    # Section A

    Original A.

    # Section B

    Original B.
""")

# Upstream changed Section A only.
THEIRS = textwrap.dedent("""\
    # Section A

    Updated A by upstream.

    # Section B

    Original B.
""")

# User customised Section B only (built on BASE).
YOURS = textwrap.dedent("""\
    # Section A

    Original A.

    # Section B

    Original B with my addition.
""")

SHA_OLD = "a" * 40
SHA_NEW = "b" * 40


def make_args(name: str) -> argparse.Namespace:
    return argparse.Namespace(name=name)


def setup_conflict(tmp_path, entry: str = "test-agent", **kwargs) -> None:
    defaults = {"entity_type": "agent", "old_sha": SHA_OLD, "new_sha": SHA_NEW}
    write_conflict(tmp_path, ConflictState(entry=entry, **{**defaults, **kwargs}))


def setup_installed(tmp_path, content: str, name: str = "test-agent", entity_type: str = "agent") -> None:
    d = tmp_path / ".claude" / f"{entity_type}s"
    d.mkdir(parents=True, exist_ok=True)
    (d / f"{name}.md").write_text(content)


# ---------------------------------------------------------------------------
# _three_way_merge (real git)
# ---------------------------------------------------------------------------


def test_three_way_merge_clean_incorporates_both_sides():
    merged, has_conflicts = _three_way_merge(BASE, THEIRS, YOURS, "test.md")
    assert not has_conflicts
    assert "Updated A by upstream" in merged
    assert "my addition" in merged


def test_three_way_merge_detects_conflicts_on_same_line():
    base = "# Hello\n\nShared line.\n"
    theirs = "# Hello\n\nChanged by upstream.\n"
    yours = "# Hello\n\nChanged by me.\n"
    merged, has_conflicts = _three_way_merge(base, theirs, yours, "test.md")
    assert has_conflicts
    assert "<<<<<<" in merged


def test_three_way_merge_raises_if_git_not_found():
    with mock_patch("skillfile.resolve.subprocess.run", side_effect=FileNotFoundError):
        with pytest.raises(Exception, match="git.*not found"):
            _three_way_merge(BASE, THEIRS, YOURS, "test.md")


# ---------------------------------------------------------------------------
# cmd_resolve — pre-conditions
# ---------------------------------------------------------------------------


def test_resolve_raises_if_no_conflict(tmp_path):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    with pytest.raises(ManifestError, match="no pending conflict"):
        cmd_resolve(make_args("test-agent"), tmp_path)


def test_resolve_raises_if_conflict_for_different_entry(tmp_path):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    setup_conflict(tmp_path, entry="other-agent")
    with pytest.raises(ManifestError, match="no pending conflict"):
        cmd_resolve(make_args("test-agent"), tmp_path)


def test_resolve_raises_if_not_installed(tmp_path):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    setup_conflict(tmp_path)
    with mock_patch("skillfile.strategies.fetch_github_file", return_value=BASE.encode()):
        with pytest.raises(ManifestError, match="not installed"):
            cmd_resolve(make_args("test-agent"), tmp_path)


# ---------------------------------------------------------------------------
# cmd_resolve — clean merge
# ---------------------------------------------------------------------------


def test_resolve_clean_merge_writes_patch(tmp_path):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    setup_conflict(tmp_path)
    setup_installed(tmp_path, YOURS)
    entry = make_github_entry()

    with mock_patch("skillfile.strategies.fetch_github_file", side_effect=[BASE.encode(), THEIRS.encode()]):
        cmd_resolve(make_args("test-agent"), tmp_path)

    assert has_patch(entry, tmp_path)


def test_resolve_clean_merge_clears_conflict(tmp_path):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    setup_conflict(tmp_path)
    setup_installed(tmp_path, YOURS)

    with mock_patch("skillfile.strategies.fetch_github_file", side_effect=[BASE.encode(), THEIRS.encode()]):
        cmd_resolve(make_args("test-agent"), tmp_path)

    assert not has_conflict(tmp_path)


def test_resolve_removes_pin_when_merged_matches_upstream(tmp_path):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    setup_conflict(tmp_path)
    # yours already matches theirs → merged == theirs → no diff → no patch
    setup_installed(tmp_path, THEIRS)
    entry = make_github_entry()

    with mock_patch("skillfile.strategies.fetch_github_file", side_effect=[BASE.encode(), THEIRS.encode()]):
        cmd_resolve(make_args("test-agent"), tmp_path)

    assert not has_patch(entry, tmp_path)
    assert not has_conflict(tmp_path)


# ---------------------------------------------------------------------------
# cmd_resolve — conflict path
# ---------------------------------------------------------------------------


def test_resolve_aborts_if_conflict_markers_remain(tmp_path, capsys):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    setup_conflict(tmp_path)
    setup_installed(tmp_path, YOURS)

    markers = "<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> upstream\n"

    with mock_patch("skillfile.strategies.fetch_github_file", side_effect=[BASE.encode(), THEIRS.encode()]):
        with mock_patch("skillfile.resolve._three_way_merge", return_value=(markers, True)):
            with mock_patch("skillfile.resolve.subprocess.run"):  # editor no-op
                cmd_resolve(make_args("test-agent"), tmp_path)

    assert "conflict markers" in capsys.readouterr().err
    # Conflict state must NOT be cleared — user still needs to resolve
    assert has_conflict(tmp_path)


def test_resolve_existing_pin_replaced_after_resolve(tmp_path):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    setup_conflict(tmp_path)
    setup_installed(tmp_path, YOURS)
    entry = make_github_entry()

    # Write a stale patch first
    write_patch(entry, generate_patch(BASE, YOURS, "test-agent.md"), tmp_path)

    with mock_patch("skillfile.strategies.fetch_github_file", side_effect=[BASE.encode(), THEIRS.encode()]):
        cmd_resolve(make_args("test-agent"), tmp_path)

    # Patch should now reflect merge of YOURS into THEIRS, not the old one
    assert has_patch(entry, tmp_path)
    assert not has_conflict(tmp_path)
