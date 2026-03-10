[![CI](https://github.com/eljulians/skillfile/actions/workflows/ci.yml/badge.svg)](https://github.com/eljulians/skillfile/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/skillfile)](https://crates.io/crates/skillfile)
[![Latest Release](https://img.shields.io/github/v/release/eljulians/skillfile)](https://github.com/eljulians/skillfile/releases/latest)
[![MSRV](https://img.shields.io/badge/MSRV-1.94-blue)](https://github.com/eljulians/skillfile)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

# skillfile

Declarative manager for AI skills and agents - the Brewfile for your AI tooling.

Community skills and agents are popping up everywhere ([agentskill.sh](https://agentskill.sh/), [skills.sh](https://skills.sh/), GitHub repos, raw URLs). Installing them usually means `npx` one-liners, copy-pasting markdown, or running tool-specific plugins. Nothing tracks what you installed, there's no lock file, no way to update, and if you tweak a skill you lose your changes the next time you reinstall.

`skillfile` gives you a single config file (`Skillfile`) that declares everything. Run `skillfile install` and it fetches your skills and agents, locks them to exact commit SHAs, and deploys them where Claude Code / Gemini CLI / Codex expect them. Edit an installed skill? `skillfile pin` captures your changes as a patch so they survive upstream updates — you stay in sync with the source without losing your customizations.

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

## Quick Start

```bash
# 1. Create a Skillfile in your project
touch Skillfile

# 2. Configure which platforms to deploy for
skillfile init

# 3. Add entries
skillfile add github skill obra/superpowers skills/requesting-code-review
skillfile add github agent iannuttall/claude-agents agents/code-refactorer.md

# 4. Fetch + deploy
skillfile install
```

On a fresh clone, `skillfile install` reads `Skillfile.lock` and fetches the exact pinned content -- fully reproducible.

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

## Commands

### Setup

| Command | Description | Key flags |
|---|---|---|
| `init` | Configure install targets interactively | |
| `add <source> <type> ...` | Add entry to the Skillfile | `--name` |
| `remove <name>` | Remove entry, lock record, and cache | |

### Workflow

| Command | Description | Key flags |
|---|---|---|
| `install` | Fetch + deploy to platform directories | `--dry-run`, `--update` |
| `sync` | Fetch only (no deploy) | `--dry-run`, `--entry NAME`, `--update` |
| `status` | Show entry states (locked/unlocked/pinned) | `--check-upstream` |

### Validation

| Command | Description | Key flags |
|---|---|---|
| `validate` | Check for syntax errors, duplicates, orphans | |
| `sort` | Sort entries into a standard order | `--dry-run` |

### Customization

| Command | Description | Key flags |
|---|---|---|
| `pin <name>` | Capture local edits as a patch | `--dry-run` |
| `unpin <name>` | Discard patch, restore upstream | |
| `diff <name>` | Show local or upstream delta | |
| `resolve [name]` | Three-way merge after a conflict | `--abort` |

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

## Supported Platforms

| Platform | Skills directory | Agents directory | Scopes |
|---|---|---|---|
| `claude-code` | `.claude/skills/` / `~/.claude/skills/` | `.claude/agents/` / `~/.claude/agents/` | local, global |
| `gemini-cli` | `.gemini/skills/` / `~/.gemini/skills/` | `.gemini/agents/` / `~/.gemini/agents/` | local, global |
| `codex` | `.codex/skills/` / `~/.codex/skills/` | -- | local, global |

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

## Security Model

- **Lock file is the primary security primitive.** Pinned SHAs prevent supply chain drift.
- **`install --dry-run`** enables pre-deployment review of what will be fetched and placed.
- **Patches are explicit** -- all modifications are visible in `.skillfile/patches/`.
- **File manager, not executor.** Skillfile never runs the content it manages. Threat model is analogous to `git clone`.

## Environment Variables

| Variable | Description |
|---|---|
| `GITHUB_TOKEN` / `GH_TOKEN` | GitHub API token for SHA resolution and private repos |
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
