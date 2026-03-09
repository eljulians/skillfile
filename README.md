# skillfile

Declarative manager for AI skills and agents - the Brewfile for your AI tooling.

## What it is

AI frameworks like Claude Code, Gemini CLI, and Codex consume markdown files that define skills and agents. There is no standard way to manage these across tools or machines. You end up copying files by hand, losing track of upstream versions, and having no reproducibility across machines.

`skillfile` fixes this. You declare what you want in a `Skillfile`, run `skillfile install`, and your skills and agents are fetched, pinned to an exact commit SHA, and placed where your platform expects them.

It is not a framework. It does not run agents. It only manages the markdown files that frameworks consume.

## Status

**v0.8.0** — sync, lock, install, pin/patch, and three platform adapters all work.

## Workflow

```
skillfile init       # once: configure which platforms to install for
skillfile install    # fetch any missing entries, deploy to platform directories
```

That's it. On a fresh clone, `skillfile install` reads `Skillfile.lock`, fetches the exact pinned content, and deploys.

## Usage

```
skillfile install               # fetch + deploy
skillfile install --dry-run     # show what would change
skillfile install --update      # re-resolve all refs and update the lock
skillfile install --link        # symlink files instead of copying

skillfile sync                  # fetch only, don't deploy
skillfile status                # show locked/unlocked/pinned state of all entries

skillfile add github skill browser agentskills/browser .
skillfile remove browser
skillfile validate

skillfile pin <name>            # capture local edits, survive future upstream updates
skillfile unpin <name>          # discard edits, revert to pure upstream
skillfile diff <name>           # show what changed upstream (after a conflict)
skillfile resolve <name>        # three-way merge upstream changes with your edits
```

## Skillfile format

```
# install lines first — written by `skillfile init`
install  claude-code  global

# <source>  <type>  [name]  [source fields...]
# name defaults to filename stem, ref defaults to main

# GitHub
github  agent  VoltAgent/awesome-claude-code-subagents  categories/01-core-development/backend-developer.md

# Local
local  skill  skills/git/commit.md

# Direct URL
url  skill  https://example.com/skill.md
```

Line-oriented, space-delimited, human-editable. No YAML, no TOML.

## Directory layout

```
Skillfile                    ← manifest (committed)
Skillfile.lock               ← pinned SHAs (committed)
.skillfile/
  cache/                     ← fetched upstream files (gitignored)
    agents/
      backend-developer/
        backend-developer.md
        .meta
  patches/                   ← your customisations of upstream files (committed)
    agents/
      backend-developer.patch
  conflict                   ← pending conflict state (gitignored)
skills/                      ← your own local skill definitions (committed)
agents/                      ← your own local agent definitions (committed)
```

`skillfile init` writes the correct `.gitignore` entries automatically — `.skillfile/cache/` and `.skillfile/conflict` are ignored; `.skillfile/patches/` is tracked.

## Requirements

Python 3.10+, stdlib only. No dependencies.
