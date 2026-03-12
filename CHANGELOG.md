# Changelog

All notable changes to skillfile are documented here.

---

## v1.1.0

### Added

- **Search & discovery** - find skills and agents across [agentskill.sh](https://agentskill.sh/), [skills.sh](https://skills.sh/), and [skillhub.club](https://skillhub.club/) from the CLI. Results are sorted by popularity and deduplicated across registries.
  - Interactive TUI with split-pane preview (default in a terminal)
  - Plain text and JSON output modes (`--no-interactive`, `--json`)
  - Filter by registry (`--registry`) or minimum security score (`--min-score`)
  - Select a result to add it directly to your Skillfile

- **4 new platform adapters** - deploy to 7 AI platforms from a single manifest:
  - Cursor (`.cursor/skills/`, `.cursor/agents/`)
  - Windsurf (`.windsurf/skills/`)
  - OpenCode (`.opencode/skills/`, `.opencode/agents/`)
  - GitHub Copilot (`.github/skills/`, `.github/agents/`)

- **Update notifications** - checks GitHub Releases in the background and shows a notice when a newer version is available. Cached for 24 hours, opt out with `SKILLFILE_NO_UPDATE_NOTIFIER=1`.

### Changed

- **README restructured** - replaced command reference tables with a Key Features section linking to dedicated docs. Added GitHub token callout in Quick Start, new Lock File and Search & Discovery sections, all 7 platforms in the Supported Platforms table.
- **Search results default to 20** (was 10) for better coverage across registries.
- **Per-registry result limiting removed** - search now fetches up to 100 results from each registry before applying the global limit, so results from smaller registries aren't drowned out.

### Fixed

- **Update notification now reliably displays** - switched from non-blocking `try_recv` to a 2-second timeout so the background check has time to complete before the process exits.
- **Removed sentinel cache write** that prevented update notices from appearing on subsequent runs within 24 hours.
- **Fixed test against wrong repo** - `list_repo_skill_entries_real_another_repo` was testing against a Jupyter notebook repo with no `.md` skill files.

---

## v1.0.1 — 2026-03-11

### Fixed

- **Local directory entries now deploy correctly** — `is_dir_entry()` only inspected GitHub `path_in_repo`, so local directory sources were silently treated as single files. `fs::copy(dir, file.md)` failed without error, and `install` printed success with nothing written. Now uses filesystem truth (`source.is_dir()`) as a fallback.
- **Renamed GitHub repos no longer fail silently** — when a repository has been renamed, `resolve_github_sha` now detects the rename via the GitHub API and tells you the new name, instead of a generic "could not resolve" error.

### Changed

- **Parallel sync** — `skillfile sync` and `skillfile install` now resolve SHAs and fetch entries in parallel using scoped threads. Manifests with many entries sync significantly faster.
- **HTTP redirect auth headers preserved** — ureq now keeps the `Authorization` header on same-host HTTPS redirects. This fixes 401 errors when GitHub returns a 301 for renamed repositories.
- Progress output is now atomic (full lines via `eprintln!`) — no more garbled interleaved output when entries sync in parallel.

---

## v0.9.0 — 2026-03-09

### Added

- **`skillfile pin --dry-run`** — preview what would be pinned without writing patches
- **`skillfile resolve --abort`** — clear pending conflict state without merging (escape hatch if you kill the editor mid-resolve)
- Quoted fields in the Skillfile — paths with spaces now work: `github skill "path with spaces/foo.md"`
- Inline comments — `github skill owner/repo path  # my note` now works correctly
- Duplicate entry name warnings during parsing
- Orphaned lock entry detection in `validate`
- Duplicate install target detection in `validate`

### Changed

- **Symlink mode removed** — `--link` flag is gone; all installs are now copy-only. This simplifies the patch system and the upcoming Rust rewrite.
- Lock file keys are now sorted deterministically (no more spurious git diffs when entries sync in different order)
- Better error on upstream 404 — now suggests checking that the path exists in the repo
- "Skillfile not found" errors now suggest running `skillfile init`
- Conflict errors now include SHA context and mention `resolve --abort`
- Entry names validated as filesystem-safe (`[a-zA-Z0-9._-]` only)
- Install scope validated (`global` or `local` only — unknown scopes now error instead of silently defaulting)
- UTF-8 BOM handling — Skillfiles saved with a byte order mark (common on Windows) now parse correctly
- Binary files in directory entries no longer crash sync

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
- **`skillfile format`** — format and sort the Skillfile
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
