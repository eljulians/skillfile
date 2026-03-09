import argparse
import textwrap

import pytest

from skillfile.exceptions import ManifestError
from skillfile.lock import write_lock
from skillfile.models import Entry, LockEntry
from skillfile.patch import dir_patch_path, generate_patch, has_patch, read_patch, write_patch
from skillfile.pin import cmd_pin, cmd_unpin

from .helpers import make_github_entry, write_manifest

ORIGINAL = textwrap.dedent("""\
    # Agent

    Original content.
""")

MODIFIED = textwrap.dedent("""\
    # Agent

    Original content.

    ## Custom Section

    Added by user.
""")

SHA = "a" * 40


def make_args(name: str) -> argparse.Namespace:
    return argparse.Namespace(name=name)


def setup_lock(tmp_path, entry, sha: str = SHA) -> None:
    write_lock(
        tmp_path,
        {f"{entry.source_type}/{entry.entity_type}/{entry.name}": LockEntry(sha=sha, raw_url="https://example.com")},
    )


def setup_installed(tmp_path, name: str, content: str, entity_type: str = "agent") -> None:
    d = tmp_path / ".claude" / f"{entity_type}s"
    d.mkdir(parents=True, exist_ok=True)
    (d / f"{name}.md").write_text(content)


def setup_vendor_cache(
    tmp_path, name: str, content: str, entity_type: str = "agent", content_file: str = "test.md"
) -> None:
    """Write a single-file entry's content to the vendor cache."""
    vdir = tmp_path / ".skillfile" / f"{entity_type}s" / name
    vdir.mkdir(parents=True, exist_ok=True)
    (vdir / content_file).write_text(content)


def setup_vendor_cache_dir(tmp_path, name: str, files: dict[str, str], entity_type: str = "skill") -> None:
    """Write a dir entry's files to the vendor cache. files = {relative_path: content}."""
    vdir = tmp_path / ".skillfile" / f"{entity_type}s" / name
    vdir.mkdir(parents=True, exist_ok=True)
    for filename, content in files.items():
        f = vdir / filename
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(content)


# ---------------------------------------------------------------------------
# cmd_pin
# ---------------------------------------------------------------------------


def test_pin_skips_local_entry(tmp_path, capsys):
    write_manifest(
        tmp_path,
        """\
        install claude-code local
        local agent my-agent agents/my-agent.md
        """,
    )
    cmd_pin(make_args("my-agent"), tmp_path)
    assert "local entry" in capsys.readouterr().out


def test_pin_raises_on_unknown_entry(tmp_path):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    with pytest.raises(ManifestError, match="no entry"):
        cmd_pin(make_args("does-not-exist"), tmp_path)


def test_pin_dir_entry_raises_if_not_locked(tmp_path):
    write_manifest(
        tmp_path,
        "install claude-code local\ngithub agent lang-pack owner/repo categories/lang-pack\n",
    )
    with pytest.raises(ManifestError, match="not locked"):
        cmd_pin(make_args("lang-pack"), tmp_path)


def test_pin_dir_entry_raises_if_not_installed(tmp_path):
    write_manifest(
        tmp_path,
        "install claude-code local\ngithub skill arch-patterns owner/repo categories/arch\n",
    )
    write_lock(
        tmp_path,
        {"github/skill/arch-patterns": LockEntry(sha=SHA, raw_url="https://example.com")},
    )
    setup_vendor_cache_dir(tmp_path, "arch-patterns", {"SKILL.md": ORIGINAL})
    with pytest.raises(ManifestError, match="not installed"):
        cmd_pin(make_args("arch-patterns"), tmp_path)


def test_pin_dir_entry_writes_per_file_patch(tmp_path, capsys):
    write_manifest(
        tmp_path,
        "install claude-code local\ngithub skill arch-patterns owner/repo categories/arch\n",
    )
    write_lock(
        tmp_path,
        {"github/skill/arch-patterns": LockEntry(sha=SHA, raw_url="https://example.com")},
    )
    setup_vendor_cache_dir(tmp_path, "arch-patterns", {"SKILL.md": ORIGINAL})
    installed_dir = tmp_path / ".claude" / "skills" / "arch-patterns"
    installed_dir.mkdir(parents=True)
    (installed_dir / "SKILL.md").write_text(MODIFIED)

    cmd_pin(make_args("arch-patterns"), tmp_path)

    entry = Entry(
        "github", "skill", "arch-patterns", owner_repo="owner/repo", path_in_repo="categories/arch", ref="main"
    )
    p = dir_patch_path(entry, "SKILL.md", tmp_path)
    assert p.exists()
    assert "+" in p.read_text()
    assert "Pinned" in capsys.readouterr().out


def test_pin_dir_entry_nothing_when_clean(tmp_path, capsys):
    write_manifest(
        tmp_path,
        "install claude-code local\ngithub skill arch-patterns owner/repo categories/arch\n",
    )
    write_lock(
        tmp_path,
        {"github/skill/arch-patterns": LockEntry(sha=SHA, raw_url="https://example.com")},
    )
    setup_vendor_cache_dir(tmp_path, "arch-patterns", {"SKILL.md": ORIGINAL})
    installed_dir = tmp_path / ".claude" / "skills" / "arch-patterns"
    installed_dir.mkdir(parents=True)
    (installed_dir / "SKILL.md").write_text(ORIGINAL)

    cmd_pin(make_args("arch-patterns"), tmp_path)

    assert "nothing to pin" in capsys.readouterr().out


def test_pin_raises_if_not_locked(tmp_path):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    with pytest.raises(ManifestError, match="not locked"):
        cmd_pin(make_args("test-agent"), tmp_path)


def test_pin_raises_if_not_installed(tmp_path):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    entry = make_github_entry()
    setup_lock(tmp_path, entry)
    setup_vendor_cache(tmp_path, "test-agent", ORIGINAL)
    with pytest.raises(ManifestError, match="not installed"):
        cmd_pin(make_args("test-agent"), tmp_path)


def test_pin_writes_patch_when_modified(tmp_path):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    entry = make_github_entry()
    setup_lock(tmp_path, entry)
    setup_vendor_cache(tmp_path, "test-agent", ORIGINAL)
    setup_installed(tmp_path, "test-agent", MODIFIED)

    cmd_pin(make_args("test-agent"), tmp_path)

    assert has_patch(entry, tmp_path)
    patch_text = read_patch(entry, tmp_path)
    assert "+" in patch_text


def test_pin_nothing_when_matches_upstream(tmp_path, capsys):
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    entry = make_github_entry()
    setup_lock(tmp_path, entry)
    setup_vendor_cache(tmp_path, "test-agent", ORIGINAL)
    setup_installed(tmp_path, "test-agent", ORIGINAL)

    cmd_pin(make_args("test-agent"), tmp_path)

    assert not has_patch(entry, tmp_path)
    assert "nothing to pin" in capsys.readouterr().out


# ---------------------------------------------------------------------------
# cmd_unpin
# ---------------------------------------------------------------------------


def test_unpin_prints_not_pinned_when_no_patch(tmp_path, capsys):
    write_manifest(tmp_path, "github agent test-agent owner/repo agents/test.md\n")
    cmd_unpin(make_args("test-agent"), tmp_path)
    assert "not pinned" in capsys.readouterr().out


def test_unpin_removes_patch(tmp_path, capsys):
    write_manifest(tmp_path, "github agent test-agent owner/repo agents/test.md\n")
    entry = make_github_entry()
    write_patch(entry, generate_patch(ORIGINAL, MODIFIED, "test-agent.md"), tmp_path)

    cmd_unpin(make_args("test-agent"), tmp_path)

    assert not has_patch(entry, tmp_path)
    assert "Unpinned" in capsys.readouterr().out


def test_unpin_restores_installed_file_from_cache(tmp_path, capsys):
    """After unpin, installed file is restored to pristine upstream from vendor cache."""
    write_manifest(tmp_path, "install claude-code local\ngithub agent test-agent owner/repo agents/test.md\n")
    entry = make_github_entry()
    write_patch(entry, generate_patch(ORIGINAL, MODIFIED, "test-agent.md"), tmp_path)
    setup_vendor_cache(tmp_path, "test-agent", ORIGINAL)
    setup_installed(tmp_path, "test-agent", MODIFIED)

    cmd_unpin(make_args("test-agent"), tmp_path)

    assert not has_patch(entry, tmp_path)
    installed = tmp_path / ".claude" / "agents" / "test-agent.md"
    assert installed.read_text() == ORIGINAL
    assert "restored to upstream version" in capsys.readouterr().out
