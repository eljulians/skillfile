"""Shared fixtures and helpers for functional tests.

These tests invoke the CLI via subprocess and hit the real GitHub API.
They are skipped when no GitHub token is available.
"""

import os
import subprocess
from pathlib import Path

import pytest

GITHUB_TOKEN_VARS = ("GITHUB_TOKEN", "GH_TOKEN")


def _has_github_token() -> bool:
    """Check env vars, then fall back to `gh auth token`."""
    for var in GITHUB_TOKEN_VARS:
        if os.environ.get(var):
            return True
    try:
        r = subprocess.run(["gh", "auth", "token"], capture_output=True, text=True)
        return r.returncode == 0 and bool(r.stdout.strip())
    except FileNotFoundError:
        return False


@pytest.fixture(scope="session")
def github_token():
    """Fail hard if no GitHub token is available."""
    if not _has_github_token():
        pytest.fail("No GitHub token available — set GITHUB_TOKEN or run `gh auth login`")

# Minimal Skillfile for testing — uses real public repos confirmed in project's own Skillfile
TEST_SKILLFILE = """\
install  claude-code  local

# Single-file agent
github  agent  code-refactorer  iannuttall/claude-agents  agents/code-refactorer.md

# Single-file skill
github  skill  requesting-code-review  obra/superpowers  skills/requesting-code-review
"""


_PROJECT_ROOT = Path(__file__).parents[2]
# Coverage data files must land in the project root so pytest-cov can combine them.
# Each subprocess writes .coverage.<host>.<pid>.<rand> there via --parallel-mode.
_COVERAGE_FILE = str(_PROJECT_ROOT / ".coverage")


def run_sf(*args: str, cwd: Path, env: dict | None = None) -> subprocess.CompletedProcess:
    """Run `skillfile <args>` as a subprocess, returning CompletedProcess.

    Uses `coverage run --parallel-mode` so subprocess coverage is captured and
    combined by pytest-cov at the end of the session.
    """
    cmd = ["uv", "run", "coverage", "run", "--parallel-mode", "--source=skillfile", "-m", "skillfile.cli", *args]
    run_env = {**os.environ, "COVERAGE_FILE": _COVERAGE_FILE, **(env or {})}
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, env=run_env, timeout=120)


@pytest.fixture
def repo(tmp_path):
    """Create an isolated repo directory with a test Skillfile."""
    sf = tmp_path / "Skillfile"
    sf.write_text(TEST_SKILLFILE)
    return tmp_path
