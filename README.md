# skillfile

[![CI](https://github.com/eljulians/skillfile/actions/workflows/ci.yml/badge.svg)](https://github.com/eljulians/skillfile/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/skillfile)](https://crates.io/crates/skillfile)
[![Latest Release](https://img.shields.io/github/v/release/eljulians/skillfile)](https://github.com/eljulians/skillfile/releases/latest)
[![MSRV](https://img.shields.io/badge/MSRV-1.82-blue)](https://github.com/eljulians/skillfile)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![platform](https://img.shields.io/badge/platform-linux%20%7C%20macOS-lightgrey.svg)]()

One-stop shop (and file) for AI skills and agents.

Search, install, and track them declaratively across all major AI coding tools. Customize without losing upstream updates.i

![demo](https://github.com/eljulians/skillfile/raw/master/docs/demo.gif)

Community skills and agents are popping up everywhere ([agentskill.sh](https://agentskill.sh/), [skills.sh](https://skills.sh/), GitHub repos, raw URLs). Installing them usually means `npx` one-liners, copy-pasting markdown, or running tool-specific plugins. Nothing tracks what you installed, there's no lock file, no way to update, and if you tweak a skill you lose your changes the next time you reinstall.

`skillfile` gives you a single config file (`Skillfile`) that declares everything. Run `skillfile search` to browse 110K+ community skills right from your terminal! Including popularity and security scores. Or add entries by hand for repos you already know. `skillfile install` fetches your skills and agents, locks them to exact commit SHAs, and deploys them where your AI tools expect them (7 platforms supported, including Claude Code or Codex). Edit an installed skill? `skillfile pin` captures your changes as a patch so they survive upstream updates. You stay in sync with the source without losing your customizations.

Not a framework. Does not run agents. Just manages the markdown files that frameworks consume.

## Install

### From crates.io

```
cargo install skillfile
```

### Pre-built binaries

Download from [GitHub Releases](https://github.com/eljulians/skillfile/releases).

### From source

```
git clone https://github.com/eljulians/skillfile.git
cd skillfile
cargo install --path crates/cli
```

Run `skillfile --help` for the full command list.

## Quick Start

```bash
# 1. Configure which platforms to deploy for (creates Skillfile + .gitignore)
skillfile init

# 2. Add entries (automatically fetched and deployed)
skillfile add github skill obra/superpowers skills/requesting-code-review
skillfile add github agent iannuttall/claude-agents agents/code-refactorer.md
```

On a fresh clone, `skillfile install` reads `Skillfile.lock` and fetches the exact pinned content -- fully reproducible.

> **Note:** skillfile uses the GitHub API to resolve commits. Without a token you'll hit the 60 requests/hour rate limit fast. Set `GITHUB_TOKEN`, `GH_TOKEN`, or run `gh auth login` before using it. See [Environment Variables](#environment-variables) for details.

## Key Features

- [**Declarative manifest**](#skillfile-format) — one config file declares all your skills, agents, and platform targets
- [**Reproducible installs**](#lock-file) — every entry pinned to an exact commit SHA via `Skillfile.lock`
- [**Cross-platform deploy**](#supported-platforms) — one manifest, seven AI platforms
- [**Pinning & patching**](#pinning--patching) — customize installed skills without losing upstream updates
- [**Search & discovery**](#search--discovery) — find skills across multiple registries from the CLI

## Skillfile Format

```
# Platform targets (written by `skillfile init`)
install  claude-code  global
install  gemini-cli   local

# GitHub entries: github  <type>  [name]  <owner/repo>  <path>  [ref]
github  agent  VoltAgent/awesome-claude-code-subagents  categories/01-core-development/backend-developer.md
github  skill  obra/superpowers  skills/requesting-code-review

# Local entries: local  <type>  [name]  <path>
local  skill  skills/git/commit.md

# URL entries: url  <type>  [name]  <url>
url  skill  https://example.com/browser-skill.md
```

Line-oriented, space-delimited, human-editable. No YAML, no TOML. Names are inferred from filename stems when omitted. See [SPEC.md](SPEC.md) for the full format specification.

| Field | Description |
|---|---|
| `type` | Source type: `local`, `github`, `url` |
| `entity-type` | `skill` or `agent` |
| `name` | Logical name (inferred from filename if omitted). Must match `[a-zA-Z0-9._-]`. |
| `owner/repo` | (github) GitHub repository identifier |
| `path` | Path to the `.md` file (local: relative to repo root, github: within the repo) |
| `ref` | (github) Branch, tag, or commit SHA. Defaults to `main`. |
| `url` | (url) Direct URL to raw markdown file |

## Lock File

`skillfile install` resolves every GitHub entry to an exact commit SHA and writes it to `Skillfile.lock`. Commit this file — on a fresh clone, `skillfile install` fetches the exact same bytes.

`skillfile install --update` re-resolves to the latest SHA upstream. Your lock file diffs cleanly in code review, so you always see what changed.

## Pinning & Patching

Edit an installed skill, then `pin` it to survive upstream updates:

```bash
# 1. Edit the deployed file directly
vim ~/.claude/skills/browser/SKILL.md

# 2. Capture your changes as a patch
skillfile pin browser

# 3. Update to latest upstream -- your patch is reapplied automatically
skillfile install --update

# 4. If upstream conflicts with your patch, resolve it
skillfile resolve browser     # opens $MERGETOOL or $EDITOR
skillfile resolve --abort     # or discard the conflict
```

Patches are stored in `.skillfile/patches/` and committed to version control.

## Search & Discovery

Find skills and agents across multiple registries without leaving the terminal:

```bash
# Interactive TUI (default in a terminal)
skillfile search "code review"

# Plain text output
skillfile search docker --no-interactive

# Filter by registry or minimum security score
skillfile search testing --registry agentskill.sh
skillfile search docker --min-score 80

# JSON output for scripting
skillfile search testing --json | jq '.items[].name'
```

Searches [agentskill.sh](https://agentskill.sh/), [skills.sh](https://skills.sh/), and [skillhub.club](https://skillhub.club/) in parallel. Results are sorted by popularity and deduplicated.

## Supported Platforms

| Platform | Skills directory | Agents directory | Scopes |
|---|---|---|---|
| `claude-code` | `.claude/skills/` / `~/.claude/skills/` | `.claude/agents/` / `~/.claude/agents/` | local, global |
| `gemini-cli` | `.gemini/skills/` / `~/.gemini/skills/` | `.gemini/agents/` / `~/.gemini/agents/` | local, global |
| `codex` | `.codex/skills/` / `~/.codex/skills/` | -- | local, global |
| `cursor` | `.cursor/skills/` / `~/.cursor/skills/` | `.cursor/agents/` / `~/.cursor/agents/` | local, global |
| `windsurf` | `.windsurf/skills/` / `~/.codeium/windsurf/skills/` | -- | local, global |
| `opencode` | `.opencode/skills/` / `~/.config/opencode/skills/` | `.opencode/agents/` / `~/.config/opencode/agents/` | local, global |
| `copilot` | `.github/skills/` / `~/.copilot/skills/` | `.github/agents/` / `~/.copilot/agents/` | local, global |

Multiple platforms can be configured simultaneously. Each `install` line in the Skillfile adds a deployment target.

## Directory Layout

```
Skillfile                    # manifest (committed)
Skillfile.lock               # pinned SHAs (committed)
.skillfile/
  cache/                     # fetched upstream files (gitignored)
    skills/
      browser/
        SKILL.md
        .meta
    agents/
      code-refactorer/
        code-refactorer.md
        .meta
  patches/                   # your customisations (committed)
    skills/
      browser.patch
  conflict                   # pending conflict state (gitignored)
skills/                      # your own local skill definitions (committed)
agents/                      # your own local agent definitions (committed)
```

## Security

Skillfile is a file manager. It downloads content from sources you specify and places it where your AI tools expect it. It does not analyze, verify, or sandbox the content it manages.

The lock file pins entries to exact commit SHAs, giving you reproducibility -- the same SHA always produces the same bytes. `install --dry-run` lets you review what will be fetched. Patches make all local modifications visible in version control. But none of this tells you whether the content is safe to use.

Review what you install. The risk profile is the same as `git clone`.

## Environment Variables

| Variable | Description |
|---|---|
| `GITHUB_TOKEN` / `GH_TOKEN` | GitHub API token for SHA resolution and private repos. **Strongly recommended** -- unauthenticated requests hit GitHub's API rate limit (60 req/hour) very quickly. Set a token or run `gh auth login` to get 5,000 req/hour. |
| `MERGETOOL` | Merge tool for `skillfile resolve` |
| `EDITOR` | Fallback editor for `skillfile resolve` |

## Contributing

```bash
# Unit tests (all crates, in src/)
cargo test --workspace --lib

# CLI integration tests (no network)
cargo test --test cli

# Functional tests (hits GitHub API, needs token)
# Set GITHUB_TOKEN or GH_TOKEN, or run `gh auth login` first
cargo test --test functional

# All of the above
cargo test --workspace

# Lint
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
