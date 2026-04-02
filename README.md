# skillfile

[![CI](https://img.shields.io/github/actions/workflow/status/eljulians/skillfile/ci.yml?style=flat-square&label=CI)](https://github.com/eljulians/skillfile/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/codecov/c/github/eljulians/skillfile?style=flat-square)](https://codecov.io/gh/eljulians/skillfile)
[![Crates.io](https://img.shields.io/crates/v/skillfile?style=flat-square)](https://crates.io/crates/skillfile)
[![MSRV](https://img.shields.io/badge/MSRV-1.82-blue?style=flat-square)](https://github.com/eljulians/skillfile)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue?style=flat-square)](https://opensource.org/licenses/Apache-2.0)
[![platform](https://img.shields.io/badge/platform-linux%20%7C%20macOS%20%7C%20Windows-lightgrey?style=flat-square)]()

**Track AI skills and agents declaratively, like dependencies. Pin them. Patch them. Deploy everywhere.**

Declare your skills in a manifest. Pinned to exact commits. Deploys to Claude Code, Cursor, Gemini, and 5 more. Customize without losing upstream updates. Search 110K+ skills from the terminal.

![demo](https://github.com/eljulians/skillfile/raw/master/docs/demo.gif)

You found a great skill on GitHub and copied the markdown into `.claude/skills/`. Maybe you grabbed one from [agentskill.sh](https://agentskill.sh). So what now?

- Each hub has its own install tool
- Nothing tracks what you installed
- No way to update when the author improves it
- Edit it? Your changes vanish on reinstall
- Switch to Cursor or Gemini CLI? Copy everything again

skillfile fixes all of that. One manifest, one lock file, every platform.

## Install

```bash
curl -fsSL https://github.com/eljulians/skillfile/releases/latest/download/install.sh | sh
```

Or with cargo:

```bash
cargo install skillfile
```

Pre-built binaries for Linux (x86_64, aarch64), macOS (x86_64, aarch64), and Windows (x86_64) are available on [GitHub Releases](https://github.com/eljulians/skillfile/releases/latest). Single binary, no runtime dependencies.

> **GitHub token recommended.** skillfile uses the GitHub API to resolve commits. Without a token you're limited to 60 req/hour. Set `GITHUB_TOKEN`, `GH_TOKEN`, or run `gh auth login`.

## Quick Start

```bash
skillfile init                                           # pick your platforms
skillfile add                                            # guided wizard
skillfile add github skill anthropics/skills skills/    # or add directly
```

That's it. Skills are fetched, locked to exact SHAs, and deployed. On a fresh clone, `skillfile install` reproduces the exact same setup.

> Looking for skills to install? See [awesome-agents-and-skills](https://github.com/eljulians/awesome-agents-and-skills) for a curated collection with a ready-to-use Skillfile.

## Add skills from anywhere

Run `skillfile add` for a guided wizard, or use the explicit CLI for any source:

```bash
skillfile add                                            # wizard: GitHub, search, local, URL
skillfile add github skill anthropics/skills            # discover all skills in a repo
skillfile add github skill anthropics/skills slack-gif-creator  # add one specific skill
skillfile add local skill skills/my-custom/SKILL.md      # track a local file
skillfile add url skill https://example.com/skill.md     # add from a URL
```

When discovering from a GitHub repo, skillfile opens a split-pane TUI where you can browse entries, preview SKILL.md content, and multi-select what to install. Each selected skill becomes an independent manifest line you can pin, update, or remove individually.

## Search community registries

Browse [agentskill.sh](https://agentskill.sh/), [skills.sh](https://skills.sh/), and [skillhub.club](https://skillhub.club/) without leaving the terminal. Results are sorted by popularity, deduplicated, and include security scores.

```bash
skillfile search "code review"                    # interactive TUI with preview
skillfile search docker --min-score 80            # only high-trust results
skillfile search testing --registry agentskill.sh # target a single registry
```

## Customize without losing upstream updates

Edit an installed skill to fit your workflow, then `pin` it. Your changes survive upstream updates automatically.

```bash
vim ~/.claude/skills/browser/SKILL.md     # edit the deployed file
skillfile pin browser                      # capture your diff as a patch
skillfile install --update                 # update upstream, patch reapplied

# If upstream conflicts with your changes:
skillfile diff browser                     # see what changed
skillfile resolve browser                  # three-way merge in $MERGETOOL
```

Patches live in `.skillfile/patches/` and are committed to version control. Your whole team gets the same customizations.

## 8 platforms, one manifest

Write your `Skillfile` once. Deploy to every AI coding tool you use.

| Platform | Skills | Agents | Scopes |
|---|---|---|---|
| **claude-code** | `.claude/skills/` | `.claude/agents/` | local, global |
| **codex** | `.codex/skills/` | - | local, global |
| **copilot** | `.github/skills/` | `.github/agents/` | local, global |
| **cursor** | `.cursor/skills/` | `.cursor/agents/` | local, global |
| **factory** | `.factory/skills/` | `.factory/droids/` | local, global |
| **gemini-cli** | `.gemini/skills/` | `.gemini/agents/` | local, global |
| **opencode** | `.opencode/skills/` | `.opencode/agents/` | local, global |
| **windsurf** | `.windsurf/skills/` | - | local, global |

Configure multiple platforms at once. `skillfile install` deploys to all of them.

## Reproducible installs

Every GitHub entry is pinned to an exact commit SHA in `Skillfile.lock`. Commit this file. On any machine, `skillfile install` fetches the exact same bytes. `install --update` re-resolves to the latest upstream, and the lock diff shows exactly what changed in code review.

```bash
skillfile install                 # fetch locked content
skillfile install --update        # update to latest upstream
skillfile install --dry-run       # preview without fetching
skillfile status --check-upstream # see which entries have updates
```

## Team workflow

Commit `Skillfile`, `Skillfile.lock`, and `.skillfile/patches/` to your repo. That's it.

The most common team setup: write your own skills in the repo and mix in community ones. Everyone gets the same AI behavior.

```bash
# Write an in-house skill and track it
vim skills/our-coding-standards/SKILL.md
skillfile add local skill skills/our-coding-standards/SKILL.md

# Pull in a community skill too
skillfile add github skill anthropics/skills skills/slack-gif-creator

# Commit everything
git add Skillfile Skillfile.lock skills/
git commit -m "Add coding standards and research skills"
git push
```

```bash
# Teammate clones and gets everything
git pull
skillfile install    # deploys all skills to their platform
```

Local skills live in your repo. You write and version them with git. GitHub skills are pinned to exact SHAs in the lock file. Both deploy to every configured platform. New teammate joins, runs `skillfile install`, and gets the same AI setup as everyone else.

## Skillfile format

Line-oriented, space-delimited, human-editable. No YAML, no TOML.

```
# Platform targets
install  claude-code  global
install  gemini-cli   local

# GitHub-hosted skills and agents
github  skill  obra/superpowers  skills/requesting-code-review
github  agent  reviewer  owner/repo  agents/reviewer.md  v2.0

# Local files
local  skill  skills/git/commit.md

# Direct URLs
url  agent  my-agent  https://example.com/agent.md
```

Names are inferred from filenames when omitted. Full format specification in [SPEC.md](SPEC.md).

## All commands

| Command | What it does |
|---|---|
| `init` | Configure platform targets interactively |
| `add` | Add entries (or run bare for guided wizard) |
| `remove` | Remove an entry, its lock record, and cache |
| `install` | Fetch and deploy everything |
| `sync` | Fetch into cache without deploying |
| `search` | Browse community registries |
| `status` | Show state of all entries |
| `validate` | Check for errors in the Skillfile |
| `format` | Sort and canonicalize the Skillfile |
| `pin` | Capture local edits as a patch |
| `unpin` | Discard pinned customizations |
| `diff` | Show local changes vs upstream |
| `resolve` | Three-way merge after a conflict |

## Environment variables

| Variable | Description |
|---|---|
| `GITHUB_TOKEN` / `GH_TOKEN` | GitHub API token. **Recommended** - without it, you're limited to 60 req/hour. Set a token or run `gh auth login` for 5,000 req/hour. |
| `MERGETOOL` | Merge tool for `skillfile resolve` |
| `EDITOR` | Fallback editor for `skillfile resolve` |
| `SKILLFILE_QUIET` | Suppress progress output (same as `--quiet`) |

## Shell completions

Generate completions for your shell, then source or install them:

```bash
# Bash
skillfile completions bash > ~/.local/share/bash-completion/completions/skillfile

# Zsh
skillfile completions zsh > ~/.zfunc/_skillfile
# (add `fpath+=~/.zfunc` to .zshrc before compinit)

# Fish
skillfile completions fish > ~/.config/fish/completions/skillfile.fish
```

Tab completion covers all commands, flags, and entry names (for `remove`, `pin`, `unpin`, `diff`, `resolve`).

## Security

skillfile is a file manager. It downloads markdown from sources you specify and places it where your AI tools expect it. It does not execute, verify, or sandbox the content.

The lock file pins entries to exact commit SHAs. The same SHA always produces the same bytes. `install --dry-run` lets you review what will be fetched. Patches make all local modifications visible in version control. But none of this tells you whether the content is safe.

Review what you install. The risk profile is the same as `git clone`.

## Development
```bash
cargo test --workspace                     # unit + integration + upstream tests
cargo test --test upstream                 # upstream API health tests (needs GITHUB_TOKEN)
cargo clippy --all-targets -- -D warnings  # lint
cargo fmt --check                          # format check
```

### Pre-commit Hooks

This project includes a `.pre-commit-config.yaml` that runs `cargo fmt --all` automatically before each commit, so formatting issues never reach CI.

**First-time setup:**
```bash
pip install pre-commit    # or: brew install pre-commit
pre-commit install        # install the git hook (once per clone)
```

To run manually:
```bash
pre-commit run --all-files
```

PRs are very welcome!