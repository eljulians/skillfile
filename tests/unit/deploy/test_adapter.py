"""Unit tests for the platform adapter pattern."""

from pathlib import Path

import pytest

from skillfile.deploy.adapter import (
    ADAPTERS,
    KNOWN_ADAPTERS,
    EntityConfig,
    FileSystemAdapter,
    PlatformAdapter,
)

# ---------------------------------------------------------------------------
# Protocol compliance — every registered adapter satisfies PlatformAdapter
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("adapter_name", ["claude-code", "gemini-cli", "codex"])
def test_adapters_satisfy_protocol(adapter_name):
    assert isinstance(ADAPTERS[adapter_name], PlatformAdapter)


@pytest.mark.parametrize("adapter_name", ["claude-code", "gemini-cli", "codex"])
def test_adapters_are_filesystem_adapters(adapter_name):
    assert isinstance(ADAPTERS[adapter_name], FileSystemAdapter)


# ---------------------------------------------------------------------------
# Registry completeness
# ---------------------------------------------------------------------------


def test_known_adapters_contains_all():
    assert set(KNOWN_ADAPTERS) == {"claude-code", "gemini-cli", "codex"}


@pytest.mark.parametrize("adapter_name", ["claude-code", "gemini-cli", "codex"])
def test_adapter_name_matches_registry_key(adapter_name):
    assert ADAPTERS[adapter_name].name == adapter_name


# ---------------------------------------------------------------------------
# supports()
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "adapter_name,entity_type",
    [
        ("claude-code", "agent"),
        ("claude-code", "skill"),
        ("gemini-cli", "agent"),
        ("gemini-cli", "skill"),
        ("codex", "skill"),
    ],
)
def test_adapter_supports_entity_type(adapter_name, entity_type):
    assert ADAPTERS[adapter_name].supports(entity_type)


def test_codex_does_not_support_agents():
    """Codex has no agent directory — agent entries must be skipped."""
    assert not ADAPTERS["codex"].supports("agent")


@pytest.mark.parametrize("adapter_name", ["claude-code", "gemini-cli", "codex"])
def test_adapter_does_not_support_unknown_entity(adapter_name):
    assert not ADAPTERS[adapter_name].supports("hook")


# ---------------------------------------------------------------------------
# target_dir() — local scope
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "adapter_name,entity_type,expected_suffix",
    [
        ("claude-code", "agent", ".claude/agents"),
        ("claude-code", "skill", ".claude/skills"),
        ("gemini-cli", "agent", ".gemini/agents"),
        ("gemini-cli", "skill", ".gemini/skills"),
        ("codex", "skill", ".codex/skills"),
    ],
)
def test_local_target_dir(tmp_path, adapter_name, entity_type, expected_suffix):
    adapter = ADAPTERS[adapter_name]
    assert isinstance(adapter, FileSystemAdapter)
    assert adapter.target_dir(entity_type, "local", tmp_path) == tmp_path / expected_suffix


# ---------------------------------------------------------------------------
# target_dir() — global scope
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "adapter_name,entity_type,expected_suffix",
    [
        ("claude-code", "agent", ".claude/agents"),
        ("claude-code", "skill", ".claude/skills"),
        ("gemini-cli", "agent", ".gemini/agents"),
        ("gemini-cli", "skill", ".gemini/skills"),
        ("codex", "skill", ".codex/skills"),
    ],
)
def test_global_target_dir_is_absolute_under_home(adapter_name, entity_type, expected_suffix):
    adapter = ADAPTERS[adapter_name]
    assert isinstance(adapter, FileSystemAdapter)
    result = adapter.target_dir(entity_type, "global", Path("/tmp"))
    assert result.is_absolute()
    assert str(result).endswith(expected_suffix)


# ---------------------------------------------------------------------------
# dir_mode via EntityConfig
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "adapter_name,entity_type,expected_mode",
    [
        ("claude-code", "agent", "flat"),
        ("claude-code", "skill", "nested"),
        ("gemini-cli", "agent", "flat"),
        ("gemini-cli", "skill", "nested"),
        ("codex", "skill", "nested"),
    ],
)
def test_entity_config_dir_mode(adapter_name, entity_type, expected_mode):
    adapter = ADAPTERS[adapter_name]
    assert isinstance(adapter, FileSystemAdapter)
    assert adapter._entities[entity_type].dir_mode == expected_mode


# ---------------------------------------------------------------------------
# Custom adapter — extensibility
# ---------------------------------------------------------------------------


def test_custom_filesystem_adapter():
    """Third-party tools can create a FileSystemAdapter instance with zero subclassing."""
    adapter = FileSystemAdapter(
        "my-tool",
        {
            "skill": EntityConfig("~/.my-tool/skills", ".my-tool/skills"),
        },
    )
    assert isinstance(adapter, PlatformAdapter)
    assert adapter.supports("skill")
    assert not adapter.supports("agent")


def test_custom_protocol_adapter():
    """Non-filesystem tools can implement PlatformAdapter directly."""
    from skillfile.core.models import Entry, InstallOptions

    class ApiAdapter:
        name = "my-api"

        def supports(self, entity_type: str) -> bool:
            return entity_type == "skill"

        def deploy_entry(
            self,
            entry: Entry,
            source: Path,
            scope: str,
            repo_root: Path,
            opts: InstallOptions,
        ) -> dict[str, Path]:
            return {}  # would POST to API

        def installed_path(self, entry: Entry, scope: str, repo_root: Path) -> Path:
            return Path("/dev/null")

        def installed_dir_files(self, entry: Entry, scope: str, repo_root: Path) -> dict[str, Path]:
            return {}

    assert isinstance(ApiAdapter(), PlatformAdapter)
