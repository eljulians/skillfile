import argparse
from pathlib import Path

from skillfile.core.parser import MANIFEST_NAME, parse_manifest
from skillfile.deploy.paths import KNOWN_ADAPTERS
from skillfile.exceptions import ManifestError


def _prompt(prompt: str, options: list[str] | None = None) -> str:
    """Prompt the user for input, optionally validating against a list of options."""
    while True:
        raw = input(prompt).strip()
        if options is None or raw in options:
            return raw
        print(f"  Please enter one of: {', '.join(options)}")


def _prompt_yn(prompt: str) -> bool:
    return _prompt(f"{prompt} [y/N] ", ["y", "Y", "n", "N", ""]).lower() == "y"


def _collect_targets() -> list[tuple[str, str]]:
    """Interactively collect (adapter, scope) pairs."""
    targets = []
    adapter_list = ", ".join(KNOWN_ADAPTERS)

    while True:
        print(f"\nKnown platforms: {adapter_list}")
        adapter = _prompt("Platform: ", KNOWN_ADAPTERS)

        scope = _prompt("Scope [global/local/both]: ", ["global", "local", "both"])
        if scope == "both":
            targets.append((adapter, "global"))
            targets.append((adapter, "local"))
        else:
            targets.append((adapter, scope))

        if not _prompt_yn("Add another platform?"):
            break

    return targets


def _rewrite_install_lines(manifest_path: Path, new_targets: list[tuple[str, str]]) -> None:
    """Replace all install lines in the Skillfile with new_targets."""
    lines = manifest_path.read_text().splitlines(keepends=True)
    non_install = [line for line in lines if not line.strip().startswith("install ") and not line.strip() == "install"]

    # Strip leading blank lines from remaining content
    while non_install and not non_install[0].strip():
        non_install.pop(0)

    new_lines = []
    for adapter, scope in new_targets:
        new_lines.append(f"install  {adapter}  {scope}\n")
    new_lines.append("\n")
    new_lines.extend(non_install)

    manifest_path.write_text("".join(new_lines))


_GITIGNORE_ENTRIES = [
    ".skillfile/cache/",
    ".skillfile/conflict",
]


def _update_gitignore(repo_root: Path) -> None:
    """Add missing skillfile entries to .gitignore (idempotent)."""
    gitignore = repo_root / ".gitignore"
    existing = gitignore.read_text().splitlines() if gitignore.exists() else []
    missing = [e for e in _GITIGNORE_ENTRIES if e not in existing]
    if not missing:
        return
    with gitignore.open("a") as f:
        if existing and existing[-1] != "":
            f.write("\n")
        f.write("# skillfile\n")
        for entry in missing:
            f.write(entry + "\n")
    print(f"\n.gitignore updated: {', '.join(missing)}")


def cmd_init(args: argparse.Namespace, repo_root: Path) -> None:
    manifest_path = repo_root / MANIFEST_NAME
    if not manifest_path.exists():
        raise ManifestError(f"{MANIFEST_NAME} not found in {repo_root}")

    manifest = parse_manifest(manifest_path)
    existing = manifest.install_targets

    if existing:
        print("Existing install config found:")
        for t in existing:
            print(f"  install  {t.adapter}  {t.scope}")
        print("This will be replaced.")
        if not _prompt_yn("Continue?"):
            print("Aborted.")
            return

    print("\nConfigure install targets.")
    new_targets = _collect_targets()

    _rewrite_install_lines(manifest_path, new_targets)
    _update_gitignore(repo_root)

    print("\nInstall config written to Skillfile:")
    for adapter, scope in new_targets:
        print(f"  install  {adapter}  {scope}")
