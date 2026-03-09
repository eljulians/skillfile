"""Functional test: skillfile sync golden path."""

from tests.functional.conftest import run_sf


def test_sync_golden_path(repo, github_token):
    """sync fetches into cache and writes lock, but does NOT deploy."""
    r = run_sf("sync", cwd=repo)
    assert r.returncode == 0

    # Lock and cache exist
    assert (repo / "Skillfile.lock").exists()
    assert (repo / ".skillfile" / "cache" / "agents" / "code-refactorer").is_dir()

    # NOT deployed
    assert not (repo / ".claude").exists()
