import textwrap
from unittest.mock import patch as mock_patch

import pytest

from skillfile.exceptions import InstallError
from skillfile.models import Entry
from skillfile.patch import (
    PatchConflictError,
    apply_entry_patch,
    apply_patch,
    generate_patch,
    has_patch,
    patch_path,
    read_patch,
    remove_patch,
    write_patch,
)

from .helpers import make_github_entry

ORIGINAL = textwrap.dedent("""\
    # Agent

    This is the original content.
""")

MODIFIED = textwrap.dedent("""\
    # Agent

    This is the original content.

    ## Addition

    Custom section added by user.
""")


def make_local_entry(name: str = "my-skill") -> Entry:
    return Entry(source_type="local", entity_type="skill", name=name, local_path=f"skills/{name}.md")


def make_dir_entry(name: str = "lang-pack") -> Entry:
    return Entry(
        source_type="github",
        entity_type="skill",
        name=name,
        owner_repo="owner/repo",
        path_in_repo="skills/lang-pack",  # no .md → dir entry
        ref="main",
    )


# ---------------------------------------------------------------------------
# generate_patch
# ---------------------------------------------------------------------------


def test_generate_patch_empty_when_identical():
    assert generate_patch("same\n", "same\n", "label") == ""


def test_generate_patch_nonempty_when_different():
    diff = generate_patch(ORIGINAL, MODIFIED, "agent.md")
    assert diff
    assert "--- a/agent.md" in diff
    assert "+++ b/agent.md" in diff


def test_generate_patch_contains_additions():
    diff = generate_patch(ORIGINAL, MODIFIED, "x.md")
    assert "+" in diff


# ---------------------------------------------------------------------------
# write_patch / has_patch / read_patch
# ---------------------------------------------------------------------------


def test_write_patch_creates_file(tmp_path):
    entry = make_github_entry()
    patch_text = generate_patch(ORIGINAL, MODIFIED, "test-agent.md")
    write_patch(entry, patch_text, tmp_path)
    p = patch_path(entry, tmp_path)
    assert p.exists()
    assert p.read_text() == patch_text


def test_write_patch_creates_parent_dirs(tmp_path):
    entry = make_github_entry()
    write_patch(entry, generate_patch(ORIGINAL, MODIFIED, "x"), tmp_path)
    assert patch_path(entry, tmp_path).parent.is_dir()


def test_has_patch_false_when_missing(tmp_path):
    assert not has_patch(make_github_entry(), tmp_path)


def test_has_patch_true_after_write(tmp_path):
    entry = make_github_entry()
    write_patch(entry, generate_patch(ORIGINAL, MODIFIED, "x"), tmp_path)
    assert has_patch(entry, tmp_path)


def test_read_patch_returns_text(tmp_path):
    entry = make_github_entry()
    patch_text = generate_patch(ORIGINAL, MODIFIED, "x")
    write_patch(entry, patch_text, tmp_path)
    assert read_patch(entry, tmp_path) == patch_text


# ---------------------------------------------------------------------------
# remove_patch
# ---------------------------------------------------------------------------


def test_remove_patch_deletes_file(tmp_path):
    entry = make_github_entry()
    write_patch(entry, generate_patch(ORIGINAL, MODIFIED, "x"), tmp_path)
    remove_patch(entry, tmp_path)
    assert not has_patch(entry, tmp_path)


def test_remove_patch_cleans_empty_parent(tmp_path):
    entry = make_github_entry()
    write_patch(entry, generate_patch(ORIGINAL, MODIFIED, "x"), tmp_path)
    parent = patch_path(entry, tmp_path).parent
    remove_patch(entry, tmp_path)
    assert not parent.exists()


def test_remove_patch_keeps_parent_with_siblings(tmp_path):
    entry1 = make_github_entry(name="agent-one")
    entry2 = make_github_entry(name="agent-two")
    write_patch(entry1, generate_patch(ORIGINAL, MODIFIED, "x"), tmp_path)
    write_patch(entry2, generate_patch(ORIGINAL, MODIFIED, "x"), tmp_path)
    remove_patch(entry1, tmp_path)
    assert not has_patch(entry1, tmp_path)
    assert has_patch(entry2, tmp_path)
    assert patch_path(entry2, tmp_path).parent.exists()


def test_remove_patch_noop_when_missing(tmp_path):
    remove_patch(make_github_entry(), tmp_path)  # must not raise


# ---------------------------------------------------------------------------
# apply_patch
# ---------------------------------------------------------------------------


def test_apply_patch_modifies_file_in_place(tmp_path):
    target = tmp_path / "test.md"
    target.write_text(ORIGINAL)
    apply_patch(target, generate_patch(ORIGINAL, MODIFIED, "test.md"))
    assert target.read_text() == MODIFIED


def test_apply_patch_raises_on_content_mismatch(tmp_path):
    target = tmp_path / "test.md"
    target.write_text("# Completely different content\nNothing matches.\n")
    patch_text = generate_patch(ORIGINAL, MODIFIED, "test.md")
    with pytest.raises(PatchConflictError):
        apply_patch(target, patch_text)


def test_apply_patch_raises_install_error_if_patch_not_found(tmp_path):
    target = tmp_path / "test.md"
    target.write_text("content\n")
    with mock_patch("skillfile.patch.subprocess.run", side_effect=FileNotFoundError):
        with pytest.raises(InstallError, match="patch.*not found"):
            apply_patch(target, "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n")


# ---------------------------------------------------------------------------
# apply_entry_patch
# ---------------------------------------------------------------------------


def test_apply_entry_patch_skips_when_no_patch(tmp_path):
    entry = make_github_entry()
    apply_entry_patch(entry, tmp_path)  # no patch exists — must not raise or write


def test_apply_entry_patch_skips_local_entry(tmp_path):
    entry = make_local_entry()
    write_patch(entry, generate_patch(ORIGINAL, MODIFIED, "x"), tmp_path)
    with mock_patch("skillfile.patch.apply_patch") as mock_apply:
        apply_entry_patch(entry, tmp_path)
    mock_apply.assert_not_called()


def test_apply_entry_patch_skips_dir_entry(tmp_path):
    entry = make_dir_entry()
    write_patch(entry, generate_patch(ORIGINAL, MODIFIED, "x"), tmp_path)
    with mock_patch("skillfile.patch.apply_patch") as mock_apply:
        apply_entry_patch(entry, tmp_path)
    mock_apply.assert_not_called()


def test_apply_entry_patch_applies_to_vendor_file(tmp_path):
    entry = make_github_entry()
    vdir = tmp_path / ".skillfile" / "agents" / "test-agent"
    vdir.mkdir(parents=True)
    vendor_file = vdir / "test.md"
    vendor_file.write_text(ORIGINAL)

    write_patch(entry, generate_patch(ORIGINAL, MODIFIED, "test-agent.md"), tmp_path)
    apply_entry_patch(entry, tmp_path)

    assert vendor_file.read_text() == MODIFIED


def test_apply_entry_patch_raises_on_conflict(tmp_path):
    entry = make_github_entry()
    vdir = tmp_path / ".skillfile" / "agents" / "test-agent"
    vdir.mkdir(parents=True)
    (vdir / "test.md").write_text("# Completely different content\nNothing matches.\n")

    write_patch(entry, generate_patch(ORIGINAL, MODIFIED, "test-agent.md"), tmp_path)
    with pytest.raises(PatchConflictError):
        apply_entry_patch(entry, tmp_path)
