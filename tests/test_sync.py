import json
from unittest.mock import patch

from skillfile.models import Entry, LockEntry
from skillfile.sync import sync_entry


def make_github_entry(name="test-agent"):
    return Entry(
        source_type="github",
        entity_type="agent",
        name=name,
        owner_repo="owner/repo",
        path_in_repo="agents/test.md",
        ref="main",
    )


def make_local_entry():
    return Entry(
        source_type="local",
        entity_type="skill",
        name="my-skill",
        local_path="skills/my-skill.md",
    )


def test_local_entry_no_network_no_vendor(tmp_path):
    entry = make_local_entry()
    with patch("skillfile.sync.resolve_github_sha") as mock_resolve:
        with patch("skillfile.sync.fetch_github_file") as mock_fetch:
            locked = sync_entry(entry, tmp_path, dry_run=False, locked={}, update=False)

    mock_resolve.assert_not_called()
    mock_fetch.assert_not_called()
    assert not (tmp_path / ".skillfile").exists()
    assert locked == {}


def test_github_entry_fetches_and_writes(tmp_path):
    entry = make_github_entry()
    sha = "87321636a1c666283d8f17398b45c2644395044b"
    content = b"# Agent content"

    with patch("skillfile.sync.resolve_github_sha", return_value=sha) as mock_resolve:
        with patch("skillfile.sync.fetch_github_file", return_value=content) as mock_fetch:
            locked = sync_entry(entry, tmp_path, dry_run=False, locked={}, update=False)

    mock_resolve.assert_called_once_with("owner/repo", "main")
    mock_fetch.assert_called_once_with("owner/repo", "agents/test.md", sha)

    vdir = tmp_path / ".skillfile" / "agents" / "test-agent"
    assert (vdir / "test.md").read_bytes() == content
    meta = json.loads((vdir / ".meta").read_text())
    assert meta["sha"] == sha

    key = "github/agent/test-agent"
    assert locked[key].sha == sha


def test_skip_when_locked_sha_matches_meta(tmp_path):
    entry = make_github_entry()
    sha = "87321636a1c666283d8f17398b45c2644395044b"

    vdir = tmp_path / ".skillfile" / "agents" / "test-agent"
    vdir.mkdir(parents=True)
    (vdir / ".meta").write_text(json.dumps({"sha": sha}))
    (vdir / "test.md").write_bytes(b"# existing content")

    locked = {"github/agent/test-agent": LockEntry(sha=sha, raw_url="https://example.com/test.md")}

    with patch("skillfile.sync.resolve_github_sha") as mock_resolve:
        with patch("skillfile.sync.fetch_github_file") as mock_fetch:
            result = sync_entry(entry, tmp_path, dry_run=False, locked=locked, update=False)

    mock_resolve.assert_not_called()
    mock_fetch.assert_not_called()
    assert result == locked


def test_fetch_using_locked_sha_when_vendor_missing(tmp_path):
    entry = make_github_entry()
    sha = "87321636a1c666283d8f17398b45c2644395044b"
    content = b"# Agent content"

    locked = {"github/agent/test-agent": LockEntry(sha=sha, raw_url="https://example.com/test.md")}

    with patch("skillfile.sync.resolve_github_sha") as mock_resolve:
        with patch("skillfile.sync.fetch_github_file", return_value=content) as mock_fetch:
            result = sync_entry(entry, tmp_path, dry_run=False, locked=locked, update=False)

    mock_resolve.assert_not_called()
    mock_fetch.assert_called_once_with("owner/repo", "agents/test.md", sha)


def make_dir_entry(name="python-pro"):
    return Entry(
        source_type="github",
        entity_type="skill",
        name=name,
        owner_repo="owner/repo",
        path_in_repo="skills/python-pro",
        ref="main",
    )


def test_github_dir_entry_fetches_all_files(tmp_path):
    entry = make_dir_entry()
    sha = "87321636a1c666283d8f17398b45c2644395044b"
    dir_listing = [
        {"name": "SKILL.md", "type": "file", "download_url": "https://raw.example.com/SKILL.md"},
        {"name": "examples.md", "type": "file", "download_url": "https://raw.example.com/examples.md"},
    ]

    def fake_get(url):
        if "SKILL.md" in url:
            return b"# SKILL content"
        return b"# examples content"

    with patch("skillfile.sync.resolve_github_sha", return_value=sha):
        with patch("skillfile.sync.list_github_dir", return_value=dir_listing):
            with patch("skillfile.sync._get", side_effect=fake_get):
                locked = sync_entry(entry, tmp_path, dry_run=False, locked={}, update=False)

    vdir = tmp_path / ".skillfile" / "skills" / "python-pro"
    assert (vdir / "SKILL.md").read_bytes() == b"# SKILL content"
    assert (vdir / "examples.md").read_bytes() == b"# examples content"
    assert locked["github/skill/python-pro"].sha == sha


def test_github_dir_entry_skip_when_up_to_date(tmp_path):
    entry = make_dir_entry()
    sha = "87321636a1c666283d8f17398b45c2644395044b"

    vdir = tmp_path / ".skillfile" / "skills" / "python-pro"
    vdir.mkdir(parents=True)
    (vdir / ".meta").write_text(json.dumps({"sha": sha}))
    (vdir / "SKILL.md").write_bytes(b"# existing")

    locked = {"github/skill/python-pro": LockEntry(sha=sha, raw_url="https://example.com")}

    with patch("skillfile.sync.resolve_github_sha") as mock_resolve:
        with patch("skillfile.sync.list_github_dir") as mock_list:
            result = sync_entry(entry, tmp_path, dry_run=False, locked=locked, update=False)

    mock_resolve.assert_not_called()
    mock_list.assert_not_called()
    assert result == locked


def test_update_flag_re_resolves_despite_lock(tmp_path):
    entry = make_github_entry()
    old_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    new_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    content = b"# Updated content"

    vdir = tmp_path / ".skillfile" / "agents" / "test-agent"
    vdir.mkdir(parents=True)
    (vdir / ".meta").write_text(json.dumps({"sha": old_sha}))
    (vdir / "test.md").write_bytes(b"# Old content")

    locked = {"github/agent/test-agent": LockEntry(sha=old_sha, raw_url="https://example.com/test.md")}

    with patch("skillfile.sync.resolve_github_sha", return_value=new_sha) as mock_resolve:
        with patch("skillfile.sync.fetch_github_file", return_value=content) as mock_fetch:
            result = sync_entry(entry, tmp_path, dry_run=False, locked=locked, update=True)

    mock_resolve.assert_called_once_with("owner/repo", "main")
    mock_fetch.assert_called_once_with("owner/repo", "agents/test.md", new_sha)
    assert result["github/agent/test-agent"].sha == new_sha
