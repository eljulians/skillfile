import sys
from pathlib import Path

from .models import Entry, InstallTarget, Manifest
from .strategies import STRATEGIES

MANIFEST_NAME = "Skillfile"


def find_entry(name: str, manifest_path: Path) -> Entry:
    from .exceptions import ManifestError

    manifest = parse_manifest(manifest_path)
    matching = [e for e in manifest.entries if e.name == name]
    if not matching:
        raise ManifestError(f"no entry named '{name}' in {MANIFEST_NAME}")
    return matching[0]


def parse_manifest(manifest_path: Path) -> Manifest:
    from .models import Entry

    entries: list[Entry] = []
    install_targets: list[InstallTarget] = []

    with open(manifest_path) as f:
        for lineno, raw in enumerate(f, 1):
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            if len(parts) < 2:
                print(f"warning: line {lineno}: too few fields, skipping", file=sys.stderr)
                continue

            source_type = parts[0]

            match source_type:
                case "install":
                    if len(parts) < 3:
                        print(f"warning: line {lineno}: install line needs: adapter scope", file=sys.stderr)
                    else:
                        install_targets.append(InstallTarget(adapter=parts[1], scope=parts[2]))

                case _ if source_type in STRATEGIES:
                    if len(parts) < 3:
                        print(f"warning: line {lineno}: too few fields, skipping", file=sys.stderr)
                    else:
                        entry = STRATEGIES[source_type].parse(parts, lineno)
                        if entry is not None:
                            entries.append(entry)

                case _:
                    print(f"warning: line {lineno}: unknown source type '{source_type}', skipping", file=sys.stderr)

    return Manifest(entries=entries, install_targets=install_targets)
