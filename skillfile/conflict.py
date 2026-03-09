from __future__ import annotations

import json
from dataclasses import asdict, dataclass
from pathlib import Path

CONFLICT_FILE = "Skillfile.conflict"


@dataclass(frozen=True)
class ConflictState:
    entry: str
    entity_type: str
    old_sha: str
    new_sha: str


def read_conflict(repo_root: Path) -> ConflictState | None:
    p = repo_root / CONFLICT_FILE
    if not p.exists():
        return None
    data = json.loads(p.read_text())
    return ConflictState(**data)


def write_conflict(repo_root: Path, state: ConflictState) -> None:
    p = repo_root / CONFLICT_FILE
    p.write_text(json.dumps(asdict(state), indent=2) + "\n")


def clear_conflict(repo_root: Path) -> None:
    (repo_root / CONFLICT_FILE).unlink(missing_ok=True)


def has_conflict(repo_root: Path) -> bool:
    return (repo_root / CONFLICT_FILE).exists()
