"""Functional test: skillfile pin + unpin."""

from tests.functional.conftest import run_sf


def test_pin_then_unpin(repo, github_token):
    """pin captures local edits; unpin reverts to upstream."""
    # Install first
    run_sf("install", cwd=repo)

    # Modify installed file (simulate user edit)
    agent_file = repo / ".claude" / "agents" / "code-refactorer.md"
    original = agent_file.read_text()
    agent_file.write_text(original + "\n## My custom section\n")

    # Pin
    r = run_sf("pin", "code-refactorer", cwd=repo)
    assert r.returncode == 0
    assert (repo / ".skillfile" / "patches" / "agents" / "code-refactorer.patch").exists()

    # Unpin
    r = run_sf("unpin", "code-refactorer", cwd=repo)
    assert r.returncode == 0
    assert not (repo / ".skillfile" / "patches" / "agents" / "code-refactorer.patch").exists()

    # Installed file should be back to original (unpin reinstalls)
    assert agent_file.read_text() == original
