# skillfile

[![CI](https://github.com/eljulians/skillfile/actions/workflows/ci.yml/badge.svg)](https://github.com/eljulians/skillfile/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/skillfile)](https://crates.io/crates/skillfile)
[![Latest Release](https://img.shields.io/github/v/release/eljulians/skillfile)](https://github.com/eljulians/skillfile/releases/latest)
[![MSRV](https://img.shields.io/badge/MSRV-1.82-blue)](https://github.com/eljulians/skillfile)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![platform](https://img.shields.io/badge/platform-linux%20%7C%20macOS-lightgrey.svg)]()

**Track AI skills and agents declaratively, like dependencies. Pin them. Patch them. Deploy everywhere.**

Declare everything in one file. Lock to exact commits. Sync across 8 agentic coding platforms. Customize without losing upstream updates. And even browse for +110k community skills without leaving the CLI!

![demo](https://github.com/eljulians/skillfile/raw/master/docs/demo.gif)

You found a great skill on GitHub and copied the markdown into `.claude/skills/`. Or maybe you found one on [agentskill.sh](https://agentskill.sh) and used their installer. Either way:

- Each hub has its own install tool
- Nothing tracks what you installed
- No way to update when the author improves it
- Edit it? Your changes vanish on reinstall
- Switch to Cursor or Gemini CLI? Copy everything again

skillfile fixes all of that. One manifest, one lock file, every platform.

## Install

```
cargo install skillfile
```

Or download a pre-built binary from [GitHub Releases](https://github.com/eljulians/skillfile/releases). Or build from source:

```
git clone https://github.com/eljulians/skillfile.git && cargo install --path crates/cli
```

Single binary, 3.5 MB, no runtime dependencies.

> **GitHub token recommended.** skillfile uses the GitHub API to resolve commits. Without a token you're limited to 60 req/hour. Set `GITHUB_TOKEN`, `GH_TOKEN`, or run `gh auth login`.

## Quick Start

```bash
skillfile init                          # pick your platforms
skillfile search "code review"          # browse 110K+ community skills
skillfile add github skill owner/repo skills/my-skill/SKILL.md
skillfile install                       # fetch + deploy everywhere
```

On a fresh clone, `skillfile install` reads `Skillfile.lock` and reproduces the exact same install, every file pinned to its commit SHA.

## Search 110K+ community skills

Browse [agentskill.sh](https://agentskill.sh/), [skills.sh](https://skills.sh/), and [skillhub.club](https://skillhub.club/) without leaving the terminal. Results are sorted by popularity, deduplicated, and include security scores.

```bash
skillfile search "code review"                    # interactive TUI with preview
skillfile search docker --min-score 80            # only high-trust results
skillfile search testing --registry agentskill.sh # target a single registry
skillfile search testing --json                   # machine-readable output
```

Select a result and skillfile walks you through adding it to your manifest.

## Bulk-add from any repo

Point at a directory in a GitHub repo and skillfile discovers every skill inside it, even in deeply nested author-namespaced structures. Pick what you want from a split-pane TUI with SKILL.md preview, and all selected entries get added and installed in one shot.

```bash
# Discover all skills under skills/ in a repo (note the trailing /)
skillfile add github skill aiskillstore/marketplace skills/

#   Found 422 skills under skills/
#   ┌──────────────────────────────────────────────────┐
#   │ Select skills          │ SKILL.md preview         │
#   │ [x] browser            │ # Browser Skill          │
#   │ [x] code-review        │ Expert code review...    │
#   │ [ ] commit             │                          │
#   │ [ ] debugging          │                          │
#   └──────────────────────────────────────────────────┘
#   Added 2 entries to Skillfile.
```

Or just run `skillfile add` with no arguments for a guided wizard that walks you through every source type.

Each discovered skill becomes a normal, independent manifest line. No magic expansion, no hidden state. Pin, update, and remove them individually.

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

## Security

skillfile is a file manager. It downloads markdown from sources you specify and places it where your AI tools expect it. It does not execute, verify, or sandbox the content.

The lock file pins entries to exact commit SHAs. The same SHA always produces the same bytes. `install --dry-run` lets you review what will be fetched. Patches make all local modifications visible in version control. But none of this tells you whether the content is safe.

Review what you install. The risk profile is the same as `git clone`.

## Contributing

```bash
cargo test --workspace                     # unit + integration tests
cargo test --test functional -- --ignored  # network tests (needs GITHUB_TOKEN)
cargo clippy --all-targets -- -D warnings  # lint
cargo fmt --check                          # format check
```

## License

[Apache 2.0](LICENSE)
