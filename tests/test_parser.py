import textwrap

from skillfile.models import InstallTarget
from skillfile.parser import parse_manifest


def write_manifest(tmp_path, content):
    p = tmp_path / "Skillfile"
    p.write_text(textwrap.dedent(content))
    return p


# ---------------------------------------------------------------------------
# Existing entry types (explicit name + ref)
# ---------------------------------------------------------------------------

def test_github_entry_explicit_name_and_ref(tmp_path):
    p = write_manifest(tmp_path, """\
        github  agent  backend-dev  owner/repo  path/to/agent.md  main
    """)
    m = parse_manifest(p)
    assert len(m.entries) == 1
    e = m.entries[0]
    assert e.source_type == "github"
    assert e.entity_type == "agent"
    assert e.name == "backend-dev"
    assert e.owner_repo == "owner/repo"
    assert e.path_in_repo == "path/to/agent.md"
    assert e.ref == "main"


def test_local_entry_explicit_name(tmp_path):
    p = write_manifest(tmp_path, """\
        local  skill  git-commit  skills/git/commit.md
    """)
    m = parse_manifest(p)
    assert len(m.entries) == 1
    e = m.entries[0]
    assert e.source_type == "local"
    assert e.entity_type == "skill"
    assert e.name == "git-commit"
    assert e.local_path == "skills/git/commit.md"


def test_url_entry_explicit_name(tmp_path):
    p = write_manifest(tmp_path, """\
        url  skill  my-skill  https://example.com/skill.md
    """)
    m = parse_manifest(p)
    assert len(m.entries) == 1
    e = m.entries[0]
    assert e.source_type == "url"
    assert e.name == "my-skill"
    assert e.url == "https://example.com/skill.md"


# ---------------------------------------------------------------------------
# Optional name inference
# ---------------------------------------------------------------------------

def test_github_entry_inferred_name(tmp_path):
    p = write_manifest(tmp_path, """\
        github  agent  owner/repo  path/to/agent.md  main
    """)
    m = parse_manifest(p)
    assert len(m.entries) == 1
    e = m.entries[0]
    assert e.name == "agent"
    assert e.owner_repo == "owner/repo"
    assert e.path_in_repo == "path/to/agent.md"
    assert e.ref == "main"


def test_local_entry_inferred_name_from_path(tmp_path):
    p = write_manifest(tmp_path, """\
        local  skill  skills/git/commit.md
    """)
    m = parse_manifest(p)
    assert len(m.entries) == 1
    e = m.entries[0]
    assert e.name == "commit"
    assert e.local_path == "skills/git/commit.md"


def test_local_entry_inferred_name_from_md_extension(tmp_path):
    p = write_manifest(tmp_path, """\
        local  skill  commit.md
    """)
    m = parse_manifest(p)
    assert len(m.entries) == 1
    e = m.entries[0]
    assert e.name == "commit"


def test_url_entry_inferred_name(tmp_path):
    p = write_manifest(tmp_path, """\
        url  skill  https://example.com/my-skill.md
    """)
    m = parse_manifest(p)
    assert len(m.entries) == 1
    e = m.entries[0]
    assert e.name == "my-skill"
    assert e.url == "https://example.com/my-skill.md"


# ---------------------------------------------------------------------------
# Optional ref (defaults to main)
# ---------------------------------------------------------------------------

def test_github_entry_inferred_name_default_ref(tmp_path):
    p = write_manifest(tmp_path, """\
        github  agent  owner/repo  path/to/agent.md
    """)
    m = parse_manifest(p)
    assert m.entries[0].ref == "main"


def test_github_entry_explicit_name_default_ref(tmp_path):
    p = write_manifest(tmp_path, """\
        github  agent  my-agent  owner/repo  path/to/agent.md
    """)
    m = parse_manifest(p)
    assert m.entries[0].ref == "main"


# ---------------------------------------------------------------------------
# Install targets
# ---------------------------------------------------------------------------

def test_install_target_parsed(tmp_path):
    p = write_manifest(tmp_path, """\
        install  claude-code  global
    """)
    m = parse_manifest(p)
    assert len(m.install_targets) == 1
    assert m.install_targets[0] == InstallTarget(adapter="claude-code", scope="global")


def test_multiple_install_targets(tmp_path):
    p = write_manifest(tmp_path, """\
        install  claude-code  global
        install  claude-code  local
    """)
    m = parse_manifest(p)
    assert len(m.install_targets) == 2
    assert m.install_targets[0].scope == "global"
    assert m.install_targets[1].scope == "local"


def test_install_targets_not_in_entries(tmp_path):
    p = write_manifest(tmp_path, """\
        install  claude-code  global
        github  agent  owner/repo  path/to/agent.md
    """)
    m = parse_manifest(p)
    assert len(m.entries) == 1
    assert len(m.install_targets) == 1


# ---------------------------------------------------------------------------
# Comments, blanks, errors
# ---------------------------------------------------------------------------

def test_comments_and_blanks_skipped(tmp_path):
    p = write_manifest(tmp_path, """\
        # this is a comment

        # another comment
        local  skill  foo  skills/foo.md
    """)
    m = parse_manifest(p)
    assert len(m.entries) == 1


def test_malformed_too_few_fields(tmp_path, capsys):
    p = write_manifest(tmp_path, """\
        github  agent
    """)
    m = parse_manifest(p)
    assert m.entries == []
    captured = capsys.readouterr()
    assert "warning" in captured.err


def test_unknown_source_type_skipped(tmp_path, capsys):
    p = write_manifest(tmp_path, """\
        svn  skill  foo  some/path
    """)
    m = parse_manifest(p)
    assert m.entries == []
    captured = capsys.readouterr()
    assert "warning" in captured.err
    assert "svn" in captured.err
