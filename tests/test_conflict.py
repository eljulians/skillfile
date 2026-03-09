import json

import pytest

from skillfile.conflict import ConflictState, clear_conflict, has_conflict, read_conflict, write_conflict

CONFLICT_FILE = "Skillfile.conflict"


def make_state(**kwargs) -> ConflictState:
    defaults: dict[str, str] = {
        "entry": "foo",
        "entity_type": "agent",
        "old_sha": "a" * 40,
        "new_sha": "b" * 40,
    }
    return ConflictState(**{**defaults, **kwargs})


# ---------------------------------------------------------------------------
# read_conflict
# ---------------------------------------------------------------------------


def test_read_missing_returns_none(tmp_path):
    assert read_conflict(tmp_path) is None


def test_write_then_read_roundtrip(tmp_path):
    state = make_state()
    write_conflict(tmp_path, state)
    assert read_conflict(tmp_path) == state


# ---------------------------------------------------------------------------
# write_conflict
# ---------------------------------------------------------------------------


def test_write_produces_valid_json_structure(tmp_path):
    state = make_state(entry="bar", entity_type="skill")
    write_conflict(tmp_path, state)
    data = json.loads((tmp_path / CONFLICT_FILE).read_text())
    assert data == {
        "entry": "bar",
        "entity_type": "skill",
        "old_sha": state.old_sha,
        "new_sha": state.new_sha,
    }


def test_write_creates_file(tmp_path):
    write_conflict(tmp_path, make_state())
    assert (tmp_path / CONFLICT_FILE).exists()


# ---------------------------------------------------------------------------
# has_conflict
# ---------------------------------------------------------------------------


def test_has_conflict_false_when_missing(tmp_path):
    assert not has_conflict(tmp_path)


def test_has_conflict_true_after_write(tmp_path):
    write_conflict(tmp_path, make_state())
    assert has_conflict(tmp_path)


# ---------------------------------------------------------------------------
# clear_conflict
# ---------------------------------------------------------------------------


def test_clear_removes_file(tmp_path):
    write_conflict(tmp_path, make_state())
    clear_conflict(tmp_path)
    assert not has_conflict(tmp_path)
    assert not (tmp_path / CONFLICT_FILE).exists()


def test_clear_noop_when_missing(tmp_path):
    clear_conflict(tmp_path)  # must not raise


# ---------------------------------------------------------------------------
# ConflictState immutability
# ---------------------------------------------------------------------------


def test_frozen_prevents_mutation():
    state = make_state()
    with pytest.raises(AttributeError):
        state.entry = "mutated"  # type: ignore[misc]
