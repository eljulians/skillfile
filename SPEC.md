# Skillfile Format Specification

Version: 1.0

## Overview

A **Skillfile** is a declarative manifest for managing AI skill and agent definitions across tools. It describes what to fetch, where it comes from, and where to deploy it. This document specifies the file format, lock file, and patch directory structure.

## File Name

The manifest file is named `Skillfile` (no extension), located at the repository root.

## Encoding

UTF-8. A UTF-8 BOM (`U+FEFF`) at the start of the file is tolerated and stripped during parsing.

## Line Types

A Skillfile consists of lines, each of which is one of:

- **Blank line** — ignored
- **Comment line** — starts with `#` (after optional whitespace), ignored
- **`install` line** — declares a deployment target
- **Entry line** — declares a skill or agent to manage (`local`, `github`, or `url`)

### Inline Comments

Inline comments are supported. A `#` preceded by whitespace strips the `#` and everything after it:

```
github  skill  owner/repo  path.md  # this is stripped
```

If `#` appears inside a quoted field, it is not treated as a comment.

### Quoting

Fields are parsed using POSIX shell quoting rules (shlex). Quoted strings preserve spaces and special characters:

```
local  skill  "my skills/git commit.md"
```

## Install Lines

```
install  <platform>  <scope>
```

| Field | Values | Description |
|---|---|---|
| `platform` | `claude-code`, `gemini-cli`, `codex` | Target AI tool |
| `scope` | `global`, `local` | Where to deploy (user-wide or project-local) |

Multiple install lines are allowed (one per platform+scope combination). Duplicate install targets produce a warning during validation.

## Entry Lines

### Local

```
local  <entity-type>  [name]  <path>
```

| Field | Description |
|---|---|
| `entity-type` | `skill` or `agent` |
| `name` | Optional logical name. If omitted, inferred from the filename stem of `path`. |
| `path` | Path to the `.md` file, relative to the repository root |

### GitHub

```
github  <entity-type>  [name]  <owner/repo>  <path-in-repo>  [ref]
```

| Field | Description |
|---|---|
| `entity-type` | `skill` or `agent` |
| `name` | Optional logical name. If omitted, inferred from the filename stem of `path-in-repo`. |
| `owner/repo` | GitHub repository identifier (e.g. `VoltAgent/awesome-claude-code-subagents`) |
| `path-in-repo` | Path to the `.md` file within the repo. Use `.` if SKILL.md is at the repo root. |
| `ref` | Branch, tag, or commit SHA. Defaults to `main` if omitted. |

### URL

```
url  <entity-type>  [name]  <url>
```

| Field | Description |
|---|---|
| `entity-type` | `skill` or `agent` |
| `name` | Optional logical name. If omitted, inferred from the URL filename stem. |
| `url` | Direct URL to the raw markdown file |

## Name Rules

### Valid Characters

Names must match the pattern `[a-zA-Z0-9._-]+`. They become directory names and filenames on disk.

### Name Inference

When the `name` field is omitted, it is inferred from the source path:

1. Take the last path component (filename)
2. Strip the `.md` extension if present
3. The result is the name

For GitHub entries, a field containing `/` is treated as `owner/repo`, not as a name. This disambiguates the positional fields.

### Uniqueness

Entry names must be unique across the entire Skillfile regardless of entity type or source type. Duplicate names produce a warning during validation.

## Directory Entry Detection

An entry is treated as a **directory entry** (multi-file) when its path does not end with `.md`. For example:

- `skills/browser.md` — single-file entry
- `skills/browser` — directory entry (fetches all files in that directory)

## Lock File

### File Name

`Skillfile.lock`, located at the repository root. This file should be committed to version control.

### Format

JSON with sorted keys and 2-space indentation:

```json
{
  "github/agent/code-refactorer": {
    "owner_repo": "owner/repo",
    "path": "agents/code-refactorer.md",
    "ref": "main",
    "sha": "abc123def456..."
  },
  "local/skill/commit": {
    "path": "skills/git/commit.md"
  },
  "url/skill/browser": {
    "url": "https://example.com/browser-skill.md"
  }
}
```

### Key Format

Lock keys follow the pattern: `<source-type>/<entity-type>/<name>`

### Fields

**GitHub entries:**
- `owner_repo` — repository identifier
- `path` — path within the repo
- `ref` — the ref that was resolved
- `sha` — the resolved commit SHA (40-character hex string)

**Local entries:**
- `path` — path relative to the repository root

**URL entries:**
- `url` — the source URL

### Purpose

The lock file is the primary reproducibility and security primitive. When present, `install` fetches content at the exact locked SHA rather than re-resolving the ref. This prevents supply chain drift.

## Patch Directory

### Location

`.skillfile/patches/` — committed to version control, machine-managed.

### Structure

```
.skillfile/patches/
  skills/
    browser.patch           # single-file entry patch
    language-specialists/   # directory entry: one patch per file
      python.md.patch
      typescript.md.patch
  agents/
    code-refactorer.patch
```

### Patch Format

Unified diff format. Patches are generated by diffing the installed copy against the cached upstream version.

### Lifecycle

- `skillfile pin <name>` — creates a patch
- `skillfile unpin <name>` — removes the patch
- `skillfile install` — applies patches after deployment
- `skillfile install --update` — detects conflicts when upstream changes and a patch exists

## Cache Directory

### Location

`.skillfile/cache/` — gitignored, reconstructed by `install` or `sync`.

### Structure

```
.skillfile/cache/
  skills/
    browser/
      SKILL.md
      .meta
  agents/
    code-refactorer/
      code-refactorer.md
      .meta
```

Each entry directory contains the fetched files and a `.meta` JSON file with source metadata (URL, ref, resolved SHA).

## Conflict State

### Location

`.skillfile/conflict` — gitignored, deleted on resolve.

### Purpose

When `install --update` detects that upstream changed for a pinned entry, it writes conflict state to this file. The user must then run `skillfile resolve` to perform a three-way merge, or `skillfile resolve --abort` to discard the conflict.

## Forward Compatibility

The format is designed for forward compatibility:

1. **New first words** (e.g. `gitlab` as a new source type) — old parsers warn and skip the line. No breakage.
2. **New entity types** (e.g. `rule`) — flow through as strings. No breakage.
3. **Extra trailing fields** — ignored by parsers that don't understand them. No breakage.
4. **Per-entry options** — if needed, use new line types (e.g. `set foo disabled=true`) rather than trailing key=value on entry lines.

**Rule: never change the meaning of existing positional fields.**

No format version header is required because of these compatibility guarantees.
