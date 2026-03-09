# Changelog

All notable changes to skillfile are documented here.

---

## v0.8.0 — 2026-03-09

### Changed

- All machine-managed state now lives under `.skillfile/` instead of being scattered at the repo root:
  - Upstream cache: `.skillfile/cache/` (was `.skillfile/`)
  - Pending conflict state: `.skillfile/conflict` (was `Skillfile.conflict`)
  - Your customisations: `.skillfile/patches/` (was `Skillfile.patches/`)
- `skillfile init` now writes the correct `.gitignore` entries automatically — no manual `.gitignore` editing required

---

## v0.7.0 — 2026-03-09

### Added

- **Gemini CLI adapter** — skills deploy to `.gemini/skills/`, agents to `.gemini/agents/`
- **Codex adapter** — skills deploy to `.codex/skills/` (Codex has no agent directory)
- One `Skillfile`, three tools: `claude-code`, `gemini-cli`, `codex`
- `skillfile init` now lists all registered adapters automatically

---

## v0.6.0 — 2026-03-09

### Changed

- Parallel file downloads — multiple entries are fetched concurrently
- SHA resolution cache — entries sharing the same repo+ref make only one API call
- Internal package restructure (no user-facing changes)
- Test suite restructured into unit / integration / functional layers

---

## v0.5.0 — 2026-03-09

### Added

- **`skillfile pin <name>`** — capture your edits to an installed upstream entry; changes survive future upstream updates
- **`skillfile unpin <name>`** — discard your customisations; revert to pure upstream on next install
- **`skillfile diff <name>`** — after a conflict, show what changed on the upstream side
- **`skillfile resolve <name>`** — three-way merge your customisations with upstream changes in `$MERGETOOL`
- `install` now silently applies patches after fetching and aborts loudly if upstream changes conflict with your customisations
- `install --update` auto-captures any local edits before re-fetching upstream
- `status` shows `[pinned]` and `[modified]` labels per entry

---

## v0.4.0 — 2026-03-09

### Added

- **`skillfile add`** — add an entry to the Skillfile without hand-editing it
- **`skillfile remove`** — remove an entry and clear its cache
- **`skillfile validate`** — check the Skillfile for syntax errors, unknown platforms, and duplicate names
- **`skillfile sort`** — sort and normalise the Skillfile
- Directory entries — a GitHub path can now point to a directory; all files are fetched and deployed individually

---

## v0.3.0 — 2026-03-09

### Added

- **`skillfile install`** — fetch and deploy in one step
- **`skillfile init`** — interactive wizard to configure which platforms to install for
- `install  <platform>  <scope>` lines in the Skillfile configure deploy targets (written by `init`)
- Global scope (`~/.claude/`) and local scope (`.claude/` relative to repo root) both supported
- `--copy` flag to copy files instead of symlinking (now the default since v0.5.0)

---

## v0.2.0 — 2026-03-09

### Added

- **`Skillfile.lock`** — records the exact commit SHA for every upstream entry
- `sync` skips entries whose SHA already matches the lock
- **`skillfile status`** — shows which entries are locked, unlocked, or outdated
- Setup is now fully reproducible: `git clone` + `skillfile install` gives byte-identical results

---

## v0.1.0 — 2026-03-09

### Added

- `Skillfile` manifest format — line-oriented, space-delimited, human-editable
- `github`, `local`, and `url` source types
- `skill` and `agent` entity types
- **`skillfile sync`** — fetch community entries into `.skillfile/cache/`, write `.meta` files
- GitHub source fetches from `raw.githubusercontent.com` — no cloning required
