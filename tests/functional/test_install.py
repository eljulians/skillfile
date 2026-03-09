"""Functional test: skillfile install golden path."""

from tests.functional.conftest import run_sf


def test_install_golden_path(repo, github_token):
    """install fetches entries, writes lock, deploys to .claude/."""
    r = run_sf("install", cwd=repo)
    assert r.returncode == 0, r.stderr

    # Lock file written
    assert (repo / "Skillfile.lock").exists()
    lock_text = (repo / "Skillfile.lock").read_text()
    assert "code-refactorer" in lock_text
    assert "requesting-code-review" in lock_text

    # Vendor cache populated
    assert (repo / ".skillfile" / "cache" / "agents" / "code-refactorer").is_dir()
    assert (repo / ".skillfile" / "cache" / "skills" / "requesting-code-review").is_dir()

    # Deployed to local .claude/
    assert (repo / ".claude" / "agents" / "code-refactorer.md").exists()

    # Content is real markdown
    content = (repo / ".claude" / "agents" / "code-refactorer.md").read_text()
    assert len(content) > 10
