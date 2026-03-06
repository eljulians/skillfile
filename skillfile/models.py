from dataclasses import dataclass, field


@dataclass
class LockEntry:
    sha: str
    raw_url: str


@dataclass
class Entry:
    source_type: str   # local | github | url
    entity_type: str   # skill | agent
    name: str
    # github
    owner_repo: str = ""
    path_in_repo: str = ""
    ref: str = ""
    # local
    local_path: str = ""
    # url
    url: str = ""


@dataclass
class InstallTarget:
    adapter: str   # e.g. "claude-code"
    scope: str     # "global" | "local"


@dataclass
class Manifest:
    entries: list[Entry] = field(default_factory=list)
    install_targets: list[InstallTarget] = field(default_factory=list)
