"""Unit tests for sources/resolver.py — token, headers, error paths."""

import os
import urllib.error
from unittest.mock import patch

import pytest

from skillfile.exceptions import NetworkError
from skillfile.sources import resolver
from skillfile.sources.resolver import (
    _base_headers,
    _get,
    _github_token,
    list_github_dir,
    list_github_dir_recursive,
    resolve_github_sha,
)


@pytest.fixture(autouse=True)
def _reset_caches():
    """Reset module-level caches between tests."""
    resolver._token_cache = None
    resolver._token_checked = False
    resolver._tree_cache.clear()
    yield
    resolver._token_cache = None
    resolver._token_checked = False
    resolver._tree_cache.clear()


def test_github_token_from_env():
    with patch.dict(os.environ, {"GITHUB_TOKEN": "test-token"}, clear=False):
        assert _github_token() == "test-token"


def test_github_token_from_gh_token_env():
    with patch.dict(os.environ, {"GH_TOKEN": "gh-token"}, clear=True):
        assert _github_token() == "gh-token"


def test_github_token_from_gh_cli():
    with patch.dict(os.environ, {}, clear=True):
        env = {k: v for k, v in os.environ.items() if k not in ("GITHUB_TOKEN", "GH_TOKEN")}
        with patch.dict(os.environ, env, clear=True):
            with patch("skillfile.sources.resolver.subprocess.run") as mock_run:
                mock_run.return_value.returncode = 0
                mock_run.return_value.stdout = "cli-token\n"
                assert _github_token() == "cli-token"


def test_github_token_gh_cli_not_found():
    with patch.dict(os.environ, {}, clear=True):
        env = {k: v for k, v in os.environ.items() if k not in ("GITHUB_TOKEN", "GH_TOKEN")}
        with patch.dict(os.environ, env, clear=True):
            with patch("skillfile.sources.resolver.subprocess.run", side_effect=FileNotFoundError):
                assert _github_token() is None


def test_base_headers_includes_user_agent():
    with patch("skillfile.sources.resolver._github_token", return_value=None) as mock_token:
        headers = _base_headers()
    mock_token.assert_called_once()
    assert "User-Agent" in headers
    assert "Authorization" not in headers


def test_base_headers_includes_auth_when_token():
    with patch("skillfile.sources.resolver._github_token", return_value="my-token") as mock_token:
        headers = _base_headers()
    mock_token.assert_called_once()
    assert headers["Authorization"] == "Bearer my-token"


def test_get_raises_network_error_on_http_error():
    with patch("skillfile.sources.resolver.urllib.request.urlopen") as mock_urlopen:
        mock_urlopen.side_effect = urllib.error.HTTPError("url", 404, "Not Found", {}, None)
        with pytest.raises(NetworkError, match="HTTP 404"):
            _get("https://example.com/file")


def test_get_raises_network_error_on_url_error():
    with patch("skillfile.sources.resolver.urllib.request.urlopen") as mock_urlopen:
        mock_urlopen.side_effect = urllib.error.URLError("Connection refused")
        with pytest.raises(NetworkError, match="Connection refused"):
            _get("https://example.com/file")


def test_resolve_github_sha_raises_on_non_fallback_ref():
    """Non-main/master ref that fails should raise NetworkError."""
    with patch("skillfile.sources.resolver.urllib.request.urlopen") as mock_urlopen:
        mock_urlopen.side_effect = urllib.error.HTTPError("url", 404, "Not Found", {}, None)
        with pytest.raises(NetworkError, match="could not resolve"):
            resolve_github_sha("owner/repo", "v1.0.0")


def test_list_github_dir():
    items = [
        {"type": "file", "name": "a.md", "path": "a.md"},
        {"type": "dir", "name": "sub", "path": "sub"},
        {"type": "file", "name": "b.md", "path": "b.md"},
    ]
    with patch("skillfile.sources.resolver._list_dir_contents", return_value=items) as mock_list:
        result = list_github_dir("owner/repo", ".", "main")
    mock_list.assert_called_once_with("owner/repo", ".", "main")
    assert len(result) == 2
    assert all(r["type"] == "file" for r in result)


def test_list_github_dir_recursive():
    """Trees API: _fetch_tree returns full tree, list_github_dir_recursive filters by path prefix."""
    fake_tree = [
        {"type": "blob", "path": "base/a.md"},
        {"type": "tree", "path": "base/sub"},
        {"type": "blob", "path": "base/sub/b.md"},
        {"type": "blob", "path": "other/c.md"},  # outside prefix, should be excluded
    ]
    with patch("skillfile.sources.resolver._fetch_tree", return_value=fake_tree) as mock_fetch:
        result = list_github_dir_recursive("owner/repo", "base", "abc123")
    mock_fetch.assert_called_once_with("owner/repo", "abc123")
    assert len(result) == 2
    paths = {r["relative_path"] for r in result}
    assert "a.md" in paths
    assert "sub/b.md" in paths
    # Verify download URLs point to raw.githubusercontent.com
    for item in result:
        assert "raw.githubusercontent.com" in item["download_url"]
        assert "abc123" in item["download_url"]
