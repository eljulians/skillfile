import argparse

import pytest

from skillfile.sort import cmd_sort, sorted_manifest_text
from skillfile.parser import parse_manifest, MANIFEST_NAME


def write_manifest(tmp_path, content=""):
    p = tmp_path / MANIFEST_NAME
    p.write_text(content)
    return p


def _args(dry_run=False):
    return argparse.Namespace(dry_run=dry_run)


def parse_and_sort(tmp_path, content):
    write_manifest(tmp_path, content)
    manifest = parse_manifest(tmp_path / MANIFEST_NAME)
    return sorted_manifest_text(manifest, content)


# ---------------------------------------------------------------------------
# Section headers always generated
# ---------------------------------------------------------------------------

def test_install_comment_generated(tmp_path):
    text = parse_and_sort(tmp_path,
        "install  claude-code  global\n"
        "github  skill  a/repo  a.md\n"
    )
    assert "# install  <platform>  <scope>" in text
    lines = text.splitlines()
    assert lines[0] == "# install  <platform>  <scope>"
    assert lines[1] == "install  claude-code  global"


def test_agents_section_header_generated(tmp_path):
    text = parse_and_sort(tmp_path, "github  agent  owner/repo  agent.md\n")
    assert "# --- Agents ---" in text


def test_skills_section_header_generated(tmp_path):
    text = parse_and_sort(tmp_path, "github  skill  owner/repo  skill.md\n")
    assert "# --- Skills ---" in text


def test_section_format_hint_generated(tmp_path):
    text = parse_and_sort(tmp_path,
        "github  agent  owner/repo  agent.md\n"
        "github  skill  owner/repo  skill.md\n"
    )
    assert "# github  agent  [name]  <owner/repo>  <path-or-dir>  [ref]" in text
    assert "# github  skill  [name]  <owner/repo>  <path-or-dir>  [ref]" in text


def test_no_install_section_when_no_targets(tmp_path):
    text = parse_and_sort(tmp_path, "github  skill  a/repo  a.md\n")
    assert "install" not in text


# ---------------------------------------------------------------------------
# Repo grouping with blank lines
# ---------------------------------------------------------------------------

def test_entries_grouped_by_repo_with_blank_lines(tmp_path):
    text = parse_and_sort(tmp_path,
        "github  skill  b/repo  b.md\n"
        "github  skill  a/repo  a.md\n"
        "github  skill  a/repo  z.md\n"
    )
    lines = text.splitlines()
    skill_lines = [l for l in lines if l.startswith("github  skill")]
    assert skill_lines[0] == "github  skill  a/repo  a.md"
    assert skill_lines[1] == "github  skill  a/repo  z.md"
    assert skill_lines[2] == "github  skill  b/repo  b.md"
    # blank line between a/repo group and b/repo group
    idx_z = lines.index("github  skill  a/repo  z.md")
    idx_b = lines.index("github  skill  b/repo  b.md")
    assert lines[idx_z + 1] == ""


def test_agents_before_skills(tmp_path):
    text = parse_and_sort(tmp_path,
        "github  skill  owner/repo  skill.md\n"
        "github  agent  owner/repo  agent.md\n"
    )
    assert text.index("# --- Agents ---") < text.index("# --- Skills ---")


# ---------------------------------------------------------------------------
# Comment preservation (entry-adjacent only)
# ---------------------------------------------------------------------------

def test_entry_adjacent_comment_preserved(tmp_path):
    text = parse_and_sort(tmp_path,
        "github  skill  z/repo  z.md\n"
        "# my annotation\n"
        "github  skill  a/repo  a.md\n"
    )
    lines = text.splitlines()
    idx = lines.index("# my annotation")
    assert lines[idx + 1] == "github  skill  a/repo  a.md"


def test_section_comment_dropped(tmp_path):
    text = parse_and_sort(tmp_path,
        "# old section header\n"
        "\n"
        "github  skill  b/repo  b.md\n"
        "github  skill  a/repo  a.md\n"
    )
    assert "old section header" not in text


# ---------------------------------------------------------------------------
# cmd_sort — writes in-place
# ---------------------------------------------------------------------------

def test_cmd_sort_rewrites_file(tmp_path):
    write_manifest(tmp_path,
        "github  skill  z/repo  z.md\n"
        "github  skill  a/repo  a.md\n"
    )
    cmd_sort(_args(), tmp_path)
    text = (tmp_path / MANIFEST_NAME).read_text()
    skill_lines = [l for l in text.splitlines() if l.startswith("github")]
    assert skill_lines[0] == "github  skill  a/repo  a.md"
    assert skill_lines[1] == "github  skill  z/repo  z.md"


def test_cmd_sort_dry_run_does_not_write(tmp_path, capsys):
    original = "github  skill  z/repo  z.md\ngithub  skill  a/repo  a.md\n"
    write_manifest(tmp_path, original)
    cmd_sort(_args(dry_run=True), tmp_path)
    assert (tmp_path / MANIFEST_NAME).read_text() == original
    assert "a/repo" in capsys.readouterr().out


def test_cmd_sort_no_manifest(tmp_path, capsys):
    with pytest.raises(SystemExit):
        cmd_sort(_args(), tmp_path)
    assert "not found" in capsys.readouterr().err
