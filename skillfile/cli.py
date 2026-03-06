import argparse
import sys
from pathlib import Path

from .init import cmd_init
from .install import cmd_install
from .status import cmd_status
from .sync import cmd_sync


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


if __name__ == "__main__":
    main()
