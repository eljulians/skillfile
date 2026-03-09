import json
import os
import subprocess
import urllib.error
import urllib.request

from skillfile.exceptions import NetworkError

_token_cache: str | None = None
_token_checked = False


def _github_token() -> str | None:
    """Return a GitHub token from the environment or gh CLI, if available. Cached."""
    global _token_cache, _token_checked
    if _token_checked:
        return _token_cache
    _token_checked = True
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token:
        _token_cache = token
        return token
    try:
        result = subprocess.run(
            ["gh", "auth", "token"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if result.returncode == 0:
            _token_cache = result.stdout.strip() or None
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass
    return _token_cache


def _base_headers() -> dict[str, str]:
    headers = {"User-Agent": "skillfile/0.6"}
    token = _github_token()
    if token:
        headers["Authorization"] = f"Bearer {token}"
    return headers


def _get(url: str) -> bytes:
    req = urllib.request.Request(url, headers=_base_headers())
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.read()
    except urllib.error.HTTPError as e:
        raise NetworkError(f"HTTP {e.code} fetching {url}") from e
    except urllib.error.URLError as e:
        raise NetworkError(f"{e.reason} fetching {url}") from e


def _try_resolve_sha(owner_repo: str, ref: str) -> str | None:
    """Try to resolve a ref to a commit SHA. Returns None on 4xx."""
    url = f"https://api.github.com/repos/{owner_repo}/commits/{ref}"
    headers = _base_headers()
    headers["Accept"] = "application/vnd.github.v3+json"
    req = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read())["sha"]
    except urllib.error.HTTPError as e:
        if 400 <= e.code < 500:
            return None
        raise NetworkError(f"could not resolve {owner_repo}@{ref}: HTTP {e.code}") from e


def resolve_github_sha(owner_repo: str, ref: str) -> str:
    """Resolve a branch/tag/SHA ref to a full commit SHA via GitHub API.

    When ref is 'main' and the repo uses 'master', falls back automatically.
    """
    sha = _try_resolve_sha(owner_repo, ref)
    if sha is not None:
        return sha
    # Fall back: main -> master (and vice-versa)
    fallback = "master" if ref == "main" else "main" if ref == "master" else None
    if fallback:
        sha = _try_resolve_sha(owner_repo, fallback)
        if sha is not None:
            return sha
    raise NetworkError(f"could not resolve {owner_repo}@{ref}")


def fetch_github_file(owner_repo: str, path_in_repo: str, sha: str) -> bytes:
    """Fetch raw file bytes from raw.githubusercontent.com."""
    effective_path = "SKILL.md" if path_in_repo == "." else path_in_repo
    url = f"https://raw.githubusercontent.com/{owner_repo}/{sha}/{effective_path}"
    return _get(url)


# --- Trees API (replaces Contents API for directory listing) ---

_tree_cache: dict[tuple[str, str], list[dict]] = {}


def _fetch_tree(owner_repo: str, sha: str) -> list[dict]:
    """Fetch full recursive tree via Git Trees API. Cached per (repo, sha)."""
    cache_key = (owner_repo, sha)
    if cache_key in _tree_cache:
        return _tree_cache[cache_key]
    url = f"https://api.github.com/repos/{owner_repo}/git/trees/{sha}?recursive=1"
    data = json.loads(_get(url))
    tree = data.get("tree", [])
    _tree_cache[cache_key] = tree
    return tree


def list_github_dir_recursive(owner_repo: str, base_path: str, ref: str) -> list[dict]:
    """List all files under base_path using Git Trees API.

    Returns [{"relative_path": str, "download_url": str}, ...] where
    relative_path is relative to base_path (e.g. "SKILL.md", "resources/playbook.md").

    Uses a single cached Trees API call per (repo, ref) instead of N Contents API calls.
    """
    tree = _fetch_tree(owner_repo, ref)
    prefix = base_path.rstrip("/") + "/"
    result = []
    for item in tree:
        if item["type"] == "blob" and item["path"].startswith(prefix):
            relative_path = item["path"][len(prefix) :]
            download_url = f"https://raw.githubusercontent.com/{owner_repo}/{ref}/{item['path']}"
            result.append({"relative_path": relative_path, "download_url": download_url})
    return result


# --- Legacy Contents API (kept for list_github_dir, used by non-recursive callers) ---


def _list_dir_contents(owner_repo: str, path: str, ref: str) -> list[dict]:
    """Fetch one level of a GitHub directory via Contents API."""
    url = f"https://api.github.com/repos/{owner_repo}/contents/{path}?ref={ref}"
    headers = _base_headers()
    headers["Accept"] = "application/vnd.github.v3+json"
    req = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.loads(resp.read())
    except urllib.error.HTTPError as e:
        raise NetworkError(f"HTTP {e.code} fetching {url}") from e
    except urllib.error.URLError as e:
        raise NetworkError(f"{e.reason} fetching {url}") from e
    return data if isinstance(data, list) else []


def list_github_dir(owner_repo: str, path: str, ref: str) -> list[dict]:
    """Return top-level file items in a GitHub directory via Contents API."""
    return [item for item in _list_dir_contents(owner_repo, path, ref) if item["type"] == "file"]
