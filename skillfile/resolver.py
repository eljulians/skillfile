import json
import os
import subprocess
import urllib.error
import urllib.request

from .exceptions import NetworkError


def _github_token() -> str | None:
    """Return a GitHub token from the environment or gh CLI, if available."""
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token:
        return token
    try:
        result = subprocess.run(
            ["gh", "auth", "token"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if result.returncode == 0:
            return result.stdout.strip() or None
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass
    return None


def _base_headers() -> dict[str, str]:
    headers = {"User-Agent": "skillfile/0.2"}
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


def list_github_dir_recursive(owner_repo: str, base_path: str, ref: str) -> list[dict]:
    """Recursively list all files under base_path.

    Returns [{"relative_path": str, "download_url": str}, ...] where
    relative_path is relative to base_path (e.g. "SKILL.md", "resources/playbook.md").
    """
    result = []
    stack = [base_path]
    while stack:
        current = stack.pop()
        for item in _list_dir_contents(owner_repo, current, ref):
            if item["type"] == "file":
                relative_path = item["path"][len(base_path) :].lstrip("/")
                result.append({"relative_path": relative_path, "download_url": item["download_url"]})
            elif item["type"] == "dir":
                stack.append(item["path"])
    return result
