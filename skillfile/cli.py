import argparse
import sys
from pathlib import Path

from .add import cmd_add
from .init import cmd_init
from .install import cmd_install
from .remove import cmd_remove
from .sort import cmd_sort
from .status import cmd_status
from .sync import cmd_sync
from .validate import cmd_validate

ENTITY_TYPES = ["skill", "agent"]


def main() -> None:
    parser = argparse.ArgumentParser(
        prog="skillfile",
        description="Tool-agnostic AI skill & agent manager",
    )
    sub = parser.add_subparsers(dest="command")

    sync_p = sub.add_parser("sync", help="Fetch community entries into .skillfile/ (without deploying)")
    sync_p.add_argument("--dry-run", action="store_true", help="Show planned actions without fetching")
    sync_p.add_argument("--entry", metavar="NAME", help="Sync only this named entry")
    sync_p.add_argument("--update", action="store_true", help="Re-resolve all refs and update the lock")

    status_p = sub.add_parser("status", help="Show state of all entries")
    status_p.add_argument("--check-upstream", action="store_true", help="Check current upstream SHA (makes API calls)")

    sub.add_parser("init", help="Configure install targets interactively")

    install_p = sub.add_parser("install", help="Fetch entries and deploy to platform directories")
    install_p.add_argument("--dry-run", action="store_true", help="Show planned actions without fetching or installing")
    install_p.add_argument("--copy", action="store_true", help="Copy files instead of symlinking")
    install_p.add_argument("--update", action="store_true", help="Re-resolve all refs and update the lock")

    # add — subcommand per source type for discoverability
    add_p = sub.add_parser("add", help="Add an entry to the Skillfile")
    add_sub = add_p.add_subparsers(dest="add_source", metavar="SOURCE")

    gh = add_sub.add_parser("github", help="Add a GitHub-hosted entry")
    gh.add_argument("entity_type", choices=ENTITY_TYPES, metavar="TYPE", help="skill or agent")
    gh.add_argument("owner_repo", metavar="OWNER/REPO", help="GitHub repository (e.g. VoltAgent/repo)")
    gh.add_argument("path", metavar="PATH", help="Path to the .md file within the repo")
    gh.add_argument("ref", nargs="?", default=None, metavar="REF", help="Branch, tag, or SHA (default: main)")
    gh.add_argument("--name", metavar="NAME", help="Override name (default: filename stem)")

    loc = add_sub.add_parser("local", help="Add a local file entry")
    loc.add_argument("entity_type", choices=ENTITY_TYPES, metavar="TYPE", help="skill or agent")
    loc.add_argument("path", metavar="PATH", help="Path to the .md file relative to repo root")
    loc.add_argument("--name", metavar="NAME", help="Override name (default: filename stem)")

    url_p = add_sub.add_parser("url", help="Add a URL entry")
    url_p.add_argument("entity_type", choices=ENTITY_TYPES, metavar="TYPE", help="skill or agent")
    url_p.add_argument("url", metavar="URL", help="Direct URL to the .md file")
    url_p.add_argument("--name", metavar="NAME", help="Override name (default: filename stem)")

    remove_p = sub.add_parser("remove", help="Remove an entry from the Skillfile")
    remove_p.add_argument("name", help="Entry name to remove")

    sub.add_parser("validate", help="Check the Skillfile for errors")

    sort_p = sub.add_parser("sort", help="Sort and canonicalize the Skillfile in-place")
    sort_p.add_argument("--dry-run", action="store_true", help="Print sorted output without writing")

    args = parser.parse_args()
    if args.command is None:
        parser.print_help()
        sys.exit(1)

    repo_root = Path.cwd()

    if args.command == "sync":
        cmd_sync(args, repo_root)
    elif args.command == "status":
        cmd_status(args, repo_root)
    elif args.command == "init":
        cmd_init(args, repo_root)
    elif args.command == "install":
        cmd_install(args, repo_root)
    elif args.command == "add":
        if args.add_source is None:
            add_p.print_help()
            sys.exit(1)
        cmd_add(args, repo_root)
    elif args.command == "remove":
        cmd_remove(args, repo_root)
    elif args.command == "validate":
        cmd_validate(args, repo_root)
    elif args.command == "sort":
        cmd_sort(args, repo_root)


if __name__ == "__main__":
    main()
