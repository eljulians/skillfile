"""End-to-end tests for the install --update → conflict → resolve cycle.

Tests both the clean-merge (compatible changes) and conflict (incompatible changes)
paths without any network calls.
"""

import argparse
import json
import textwrap
from unittest.mock import patch as mock_patch

import pytest

from skillfile.conflict import has_conflict, read_conflict
from skillfile.exceptions import InstallError
from skillfile.install import cmd_install
from skillfile.models import LockEntry
from skillfile.patch import has_patch, read_patch
from skillfile.resolve import cmd_resolve
from skillfile.status import cmd_status

from .helpers import make_github_entry, write_manifest

# ---------------------------------------------------------------------------
# Content fixtures — mirrors a real upstream update (frontmatter metadata added)
# ---------------------------------------------------------------------------

BASE = textwrap.dedent("""\
    ---
    name: test-agent
    description: "A test agent"
    ---

    # Test Agent

    Use this for testing.

    ## Usage

    Follow these steps.
""")

# Upstream adds metadata to the frontmatter (top of file).
UPSTREAM_NEW = textwrap.dedent("""\
    ---
    name: test-agent
    description: "A test agent"
    risk: unknown
    source: community
    date_added: "2026-02-27"
    ---

    # Test Agent

    Use this for testing.

    ## Usage

    Follow these steps.
""")

# User adds a section at the bottom — non-overlapping with the upstream change.
USER_EDIT_COMPATIBLE = textwrap.dedent("""\
    ---
    name: test-agent
    description: "A test agent"
    ---

    # Test Agent

    Use this for testing.

    ## Usage

    Follow these steps.

    ## My Custom Notes

    Added by user.
""")

# User changes the description line — the context window overlaps with
# upstream's added lines, so the patch can't be applied to new upstream.
USER_EDIT_INCOMPATIBLE = textwrap.dedent("""\
    ---
    name: test-agent
    description: "My custom description"
    ---

    # Test Agent

    Use this for testing.

    ## Usage

    Follow these steps.
""")

SHA_OLD = "a" * 40
SHA_NEW = "b" * 40

# Expected merge result when the user manually resolves the conflict: keep custom
# description AND upstream metadata additions.
MANUAL_MERGE_RESULT = textwrap.dedent("""\
    ---
    name: test-agent
    description: "My custom description"
    risk: unknown
    source: community
    date_added: "2026-02-27"
    ---

    # Test Agent

    Use this for testing.

    ## Usage

    Follow these steps.
""")


def _make_args(dry_run=False, link=False, update=False):
    return argparse.Namespace(dry_run=dry_run, link=link, update=update)


def _make_resolve_args(name):
    return argparse.Namespace(name=name)


def _make_status_args(check_upstream=False):
    return argparse.Namespace(check_upstream=check_upstream)


def _setup_clean_state(tmp_path):
    """Install test-agent at SHA_OLD with BASE content. Returns entry."""
    write_manifest(
        tmp_path,
        "install claude-code local\ngithub agent test-agent owner/repo agents/agent.md main\n",
    )
    lock = {"github/agent/test-agent": {"sha": SHA_OLD, "raw_url": "https://example.com"}}
    (tmp_path / "Skillfile.lock").write_text(json.dumps(lock))

    vdir = tmp_path / ".skillfile" / "agents" / "test-agent"
    vdir.mkdir(parents=True)
    (vdir / "agent.md").write_text(BASE)
    (vdir / ".meta").write_text(json.dumps({"sha": SHA_OLD}))

    installed = tmp_path / ".claude" / "agents"
    installed.mkdir(parents=True)
    (installed / "test-agent.md").write_text(BASE)

    return make_github_entry()


def _mock_sync_update(entry_name, new_content, old_sha, new_sha):
    """Return a sync_entry function that simulates re-fetching with new upstream."""

    def _sync(entry, repo_root, dry_run, locked, update):
        key = f"{entry.source_type}/{entry.entity_type}/{entry.name}"
        if entry.name == entry_name and update and not dry_run:
            vdir = repo_root / ".skillfile" / f"{entry.entity_type}s" / entry.name
            if vdir.exists():
                # Simulate re-fetch: overwrite cache with new upstream content
                from skillfile.strategies import STRATEGIES

                cf = STRATEGIES[entry.source_type].content_file(entry)
                if cf:
                    (vdir / cf).write_text(new_content)
                (vdir / ".meta").write_text(json.dumps({"sha": new_sha}))
            locked = dict(locked)
            locked[key] = LockEntry(sha=new_sha, raw_url="https://example.com")
        return locked

    return _sync


# ---------------------------------------------------------------------------
# Test 1: Compatible changes — clean merge via install --update
# ---------------------------------------------------------------------------


def test_install_update_clean_merge_both_changes_present(tmp_path, capsys):
    """User edits bottom, upstream changes top → install --update applies cleanly."""
    entry = _setup_clean_state(tmp_path)

    # User edits the installed file (adds section at bottom)
    installed_file = tmp_path / ".claude" / "agents" / "test-agent.md"
    installed_file.write_text(USER_EDIT_COMPATIBLE)

    # Pin the edit
    from skillfile.pin import cmd_pin

    cmd_pin(argparse.Namespace(name="test-agent"), tmp_path)
    assert has_patch(entry, tmp_path)
    capsys.readouterr()  # clear output

    # install --update with mock sync that updates vendor cache to UPSTREAM_NEW
    sync_fn = _mock_sync_update("test-agent", UPSTREAM_NEW, SHA_OLD, SHA_NEW)
    with mock_patch("skillfile.install.sync_entry", side_effect=sync_fn):
        cmd_install(_make_args(update=True), tmp_path)

    # Installed file should have BOTH upstream metadata AND user's custom notes
    result = installed_file.read_text()
    assert "risk: unknown" in result, "upstream metadata should be present"
    assert "source: community" in result, "upstream metadata should be present"
    assert "## My Custom Notes" in result, "user's edit should be preserved"
    assert "Added by user." in result, "user's edit should be preserved"

    # Entry should still be pinned, not modified
    assert has_patch(entry, tmp_path)
    assert not has_conflict(tmp_path)


def test_install_update_clean_merge_status_shows_pinned(tmp_path, capsys):
    """After clean merge, status shows [pinned] and NOT [modified]."""
    _setup_clean_state(tmp_path)
    installed_file = tmp_path / ".claude" / "agents" / "test-agent.md"
    installed_file.write_text(USER_EDIT_COMPATIBLE)

    from skillfile.pin import cmd_pin

    cmd_pin(argparse.Namespace(name="test-agent"), tmp_path)
    capsys.readouterr()

    sync_fn = _mock_sync_update("test-agent", UPSTREAM_NEW, SHA_OLD, SHA_NEW)
    with mock_patch("skillfile.install.sync_entry", side_effect=sync_fn):
        cmd_install(_make_args(update=True), tmp_path)
    capsys.readouterr()

    cmd_status(_make_status_args(), tmp_path)
    out = capsys.readouterr().out
    assert "[pinned]" in out
    assert "[modified]" not in out


# ---------------------------------------------------------------------------
# Test 2: Incompatible changes — conflict → diff → resolve cycle
# ---------------------------------------------------------------------------


def test_install_update_conflict_raises_and_writes_conflict_state(tmp_path, capsys):
    """User changes description (near upstream's metadata addition) → PatchConflictError."""
    entry = _setup_clean_state(tmp_path)
    installed_file = tmp_path / ".claude" / "agents" / "test-agent.md"
    installed_file.write_text(USER_EDIT_INCOMPATIBLE)

    from skillfile.pin import cmd_pin

    cmd_pin(argparse.Namespace(name="test-agent"), tmp_path)
    assert has_patch(entry, tmp_path)
    capsys.readouterr()

    sync_fn = _mock_sync_update("test-agent", UPSTREAM_NEW, SHA_OLD, SHA_NEW)
    with mock_patch("skillfile.install.sync_entry", side_effect=sync_fn):
        with pytest.raises(InstallError, match="conflict"):
            cmd_install(_make_args(update=True), tmp_path)

    # Conflict state should be written
    assert has_conflict(tmp_path)
    conflict = read_conflict(tmp_path)
    assert conflict.entry == "test-agent"
    assert conflict.old_sha == SHA_OLD
    assert conflict.new_sha == SHA_NEW


def test_install_blocked_while_conflict_pending(tmp_path, capsys):
    """install raises hard error while Skillfile.conflict exists."""
    _setup_clean_state(tmp_path)
    installed_file = tmp_path / ".claude" / "agents" / "test-agent.md"
    installed_file.write_text(USER_EDIT_INCOMPATIBLE)

    from skillfile.pin import cmd_pin

    cmd_pin(argparse.Namespace(name="test-agent"), tmp_path)
    capsys.readouterr()

    sync_fn = _mock_sync_update("test-agent", UPSTREAM_NEW, SHA_OLD, SHA_NEW)
    with mock_patch("skillfile.install.sync_entry", side_effect=sync_fn):
        with pytest.raises(InstallError, match="conflict"):
            cmd_install(_make_args(update=True), tmp_path)

    # Now try plain install — should be blocked
    with pytest.raises(InstallError, match="pending conflict"):
        cmd_install(_make_args(), tmp_path)


def test_resolve_recovers_user_edits_and_clears_conflict(tmp_path, capsys):
    """resolve reconstructs 'yours' from stored patch, three-way merge succeeds."""
    entry = _setup_clean_state(tmp_path)
    installed_file = tmp_path / ".claude" / "agents" / "test-agent.md"
    installed_file.write_text(USER_EDIT_INCOMPATIBLE)

    from skillfile.pin import cmd_pin

    cmd_pin(argparse.Namespace(name="test-agent"), tmp_path)
    capsys.readouterr()

    sync_fn = _mock_sync_update("test-agent", UPSTREAM_NEW, SHA_OLD, SHA_NEW)
    with mock_patch("skillfile.install.sync_entry", side_effect=sync_fn):
        with pytest.raises(InstallError, match="conflict"):
            cmd_install(_make_args(update=True), tmp_path)

    # Now resolve — mock fetch_github_file to return BASE (old) and UPSTREAM_NEW (new).
    # Mock _open_in_editor to simulate user manually merging the conflict.
    with (
        mock_patch(
            "skillfile.strategies.fetch_github_file",
            side_effect=[BASE.encode(), UPSTREAM_NEW.encode()],
        ),
        mock_patch(
            "skillfile.resolve._open_in_editor",
            return_value=MANUAL_MERGE_RESULT,
        ),
    ):
        cmd_resolve(_make_resolve_args("test-agent"), tmp_path)

    # Conflict should be cleared
    assert not has_conflict(tmp_path)

    # Installed file should have BOTH user's custom description AND upstream metadata
    result = installed_file.read_text()
    assert 'description: "My custom description"' in result, "user's edit should be preserved"
    assert "risk: unknown" in result, "upstream metadata should be present"
    assert "source: community" in result, "upstream metadata should be present"

    # Patch should be updated (diff between new upstream and merged result)
    assert has_patch(entry, tmp_path)


def test_install_succeeds_after_resolve(tmp_path, capsys):
    """After resolve, plain install succeeds with no errors."""
    _setup_clean_state(tmp_path)
    installed_file = tmp_path / ".claude" / "agents" / "test-agent.md"
    installed_file.write_text(USER_EDIT_INCOMPATIBLE)

    from skillfile.pin import cmd_pin

    cmd_pin(argparse.Namespace(name="test-agent"), tmp_path)
    capsys.readouterr()

    sync_fn = _mock_sync_update("test-agent", UPSTREAM_NEW, SHA_OLD, SHA_NEW)
    with mock_patch("skillfile.install.sync_entry", side_effect=sync_fn):
        with pytest.raises(InstallError, match="conflict"):
            cmd_install(_make_args(update=True), tmp_path)

    with (
        mock_patch(
            "skillfile.strategies.fetch_github_file",
            side_effect=[BASE.encode(), UPSTREAM_NEW.encode()],
        ),
        mock_patch(
            "skillfile.resolve._open_in_editor",
            return_value=MANUAL_MERGE_RESULT,
        ),
    ):
        cmd_resolve(_make_resolve_args("test-agent"), tmp_path)
    capsys.readouterr()

    # Plain install should succeed (no --update, no conflict)
    cmd_install(_make_args(), tmp_path)
    out = capsys.readouterr().out
    assert "Done." in out

    # Status should show [pinned], NOT [modified]
    cmd_status(_make_status_args(), tmp_path)
    out = capsys.readouterr().out
    assert "[pinned]" in out
    assert "[modified]" not in out


# ---------------------------------------------------------------------------
# Test 3: Auto-pin preserves correct patch when cache is inconsistent
# ---------------------------------------------------------------------------


def test_auto_pin_does_not_overwrite_existing_patch_with_bad_cache(tmp_path, capsys):
    """If vendor cache is manually modified, auto-pin keeps the existing correct patch."""
    entry = _setup_clean_state(tmp_path)
    installed_file = tmp_path / ".claude" / "agents" / "test-agent.md"
    installed_file.write_text(USER_EDIT_COMPATIBLE)

    from skillfile.pin import cmd_pin

    cmd_pin(argparse.Namespace(name="test-agent"), tmp_path)
    read_patch(entry, tmp_path)  # verify patch was written
    capsys.readouterr()

    # Corrupt the vendor cache (simulate what the user was doing in manual QA)
    cache_file = tmp_path / ".skillfile" / "agents" / "test-agent" / "agent.md"
    cache_file.write_text("CORRUPTED CACHE\n")

    # install --update with mock sync that ALSO updates to correct new upstream
    sync_fn = _mock_sync_update("test-agent", UPSTREAM_NEW, SHA_OLD, SHA_NEW)
    with mock_patch("skillfile.install.sync_entry", side_effect=sync_fn):
        cmd_install(_make_args(update=True), tmp_path)

    # The auto-pin should NOT have overwritten the patch with garbage
    # Instead it should have kept the existing patch (generated against real BASE)
    # and the patch should have applied cleanly to UPSTREAM_NEW
    result = installed_file.read_text()
    assert "## My Custom Notes" in result, "user's edit should be preserved"
    assert "risk: unknown" in result, "upstream metadata should be present"
