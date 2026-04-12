# Changelog

All notable changes to skillfile are documented here.

---

## v1.5.0 - 12-04-2026

### Added

- **Google Antigravity adapter** - skillfile can now deploy skills to Antigravity. Local installs go to `.agents/skills/`; global installs go to `~/.gemini/antigravity/skills`.

### Changed

- **Skills now deploy in a normalized directory layout across platforms** - single-file skills are now installed as `<platform>/skills/<name>/SKILL.md` instead of flat `<name>.md` files. This makes skill layouts consistent with directory-based skills and better matches how platforms like Claude Code and Copilot load skills.
- **Automatic migration from legacy flat skill files** - re-running `skillfile install` now cleans up orphaned legacy `<name>.md` skill files after deploying the normalized directory layout, preventing duplicate skill loading after upgrades.
- **`skillfile status` now prints a summary block** - the command now ends with totals for skills and agents, plus pinned/modified counts and the active install targets, making larger Skillfiles easier to scan.
- **Pre-commit formatting hook included** - the repo now ships a `.pre-commit-config.yaml` that runs `cargo fmt --all` before commit, reducing formatting churn in PRs.

### Fixed

- **Local drift detection now checks every install target** - `status` no longer misses modified skills or agents just because the first configured install target happens to be unchanged. This also fixes patch-aware comparisons for normalized nested skill installs.

## v1.4.2 - 31-03-2026

### Security

- **GitHub tokens no longer sent to third-party registries** - `build_get` and `build_post` unconditionally attached `Authorization: Bearer` headers to every HTTP request, sending GitHub tokens to agentskill.sh, skills.sh, and skillhub.club. Tokens are now wrapped in an opaque `GithubToken` newtype whose only extraction method gates on an exact-host allowlist (`api.github.com`, `raw.githubusercontent.com`). The inner field is private with no `Debug`/`Clone`/`Deref`, making accidental leakage structurally impossible.

### Changed

- **Dependencies updated** - bumped `console` (0.16.3), `cliclack` (0.5.2), `crossterm` (0.29.0), `insta` (1.47.1), and `assert_cmd` (2.2.0).
- **CI reliability** - upstream GitHub API tests now run sequentially with exponential backoff, eliminating rate-limit flakes on PRs.

---

## v1.4.1 - 23-03-2026

### Fixed

- **`cargo install` binary bloat** - `cargo install skillfile` produced a 7.9 MB binary (nearly double the GitHub Release binary) because workspace-level `[profile.release]` settings are not included in published crates. Added the release profile directly to the CLI crate's `Cargo.toml` so all install methods now produce the same ~4 MB optimized binary with LTO and strip enabled.

---

## v1.4.0 - 23-03-2026

### Added

- **One-line installer** - `curl -fsSL .../install.sh | sh` detects OS and architecture, downloads the right binary to `~/.local/bin/`, and prints a PATH hint if needed. Supports `SKILLFILE_VERSION` and `SKILLFILE_INSTALL_DIR` overrides. Served as a release asset so it never reflects in-progress work on master.
- **`cargo-binstall` support** - `cargo binstall skillfile` now downloads the pre-built binary in seconds instead of compiling from source. Zero config needed, piggybacks on existing release assets.
- **Windows support** - pre-built `x86_64-pc-windows-msvc` binary in releases. Path separators normalized to forward slashes in deploy keys. CRLF line endings handled in patch application. Windows CI test job added.
- **Local path drift in status** - `skillfile status` now shows `✗ path missing: <path>` for local entries whose file or directory no longer exists on disk. Previously all local entries showed `local` regardless of path validity.
- **GitHub token config + init wizard** - `skillfile init` now walks you through GitHub token setup after platform selection. Detects existing tokens from env vars, `gh` CLI, or config file. Offers paste-and-validate or skip. Token is saved to `~/.config/skillfile/config.toml` with `0o600` permissions. The token discovery chain now checks: `GITHUB_TOKEN` env → `GH_TOKEN` env → config file → `gh auth token`.
- **SKILL.md preview in search TUI** - the search TUI preview pane now shows rendered SKILL.md content. Each registry fetches content through its own strategy: agentskill.sh extracts from Nuxt hydration data, skills.sh tries raw GitHub URLs with page-scrape fallback. Frontmatter fields (name, description, risk, source) render as a metadata header above the body.

### Changed

- **HTML entity decoding** - replaced hand-rolled 8-entity decoder with the `html-escape` crate, covering all 2,231+ named, numeric, and hex HTML entities. Fixes garbled characters in skills.sh preview content.
- **Shell completions** - `skillfile completions <shell>` generates static completion scripts for bash, zsh, fish, and PowerShell. Dynamic completions via `eval "$(COMPLETE=bash skillfile)"` add entry name completion for `remove`, `pin`, `unpin`, `diff`, and `resolve` by reading the Skillfile at tab-press time.

### Fixed

- **Update notice missing on remaining commands** - clap's `parse()` called `process::exit(0)` internally, bypassing the update notification at the end of `main()`. Replaced with `try_parse()` so that the update notice always prints.
- **Search-to-add broken for agentskill.sh monorepo entries** - registry slugs (e.g. `openclaw/k8s-config-gen`) were leaking into `source_repo` and being treated as GitHub `owner/repo` values. The Tree API would fail, leading to a confusing "Path in repo:" prompt. Now `source_repo` is only set when real GitHub coordinates exist.
- **Coordinate resolution for agentskill.sh skills** - when the detail API can't find a skill by name (common for large registries), the system now scrapes the skill page HTML for GitHub coordinates as a fallback. Resolves repo and path without user intervention.
- **Directory sync fails silently on massive repos** - `list_github_dir_recursive` used the recursive Tree API which truncates at ~7000 entries. Skills deep in the alphabet (e.g. `skills/ivangdavila/k8s` in `openclaw/skills`) would silently get zero content. Now falls back to the GitHub Contents API for the specific directory when the tree returns empty.

---

## v1.3.0 - 18-03-2026

### Added

- **Interactive add wizard** - `skillfile add` with no subcommand launches a guided wizard. Pick a source (GitHub, search, local, URL), answer a few prompts, and skills get added and installed. Zero CLI syntax to memorize.
- **Split-pane TUI for bulk add** - discovering skills from a GitHub repo now opens a full-screen split-pane browser matching the `search` experience. Left pane: multi-select list with checkboxes. Right pane: live SKILL.md preview with name, description, risk level, source, GitHub URL, and the first 20 lines of body content. Filter by typing, toggle all with `a`, scroll preview with Tab/Shift+Tab.
- **Bulk add from any GitHub repo** - `skillfile add github skill owner/repo` discovers every skill in the repo via the Tree API. Works with repos of any depth, including author-namespaced structures. Each selected entry becomes an independent Skillfile line.
- **Factory (Droid) platform adapter** - 8th supported platform. Skills deploy to `.factory/skills/` (nested), agents to `.factory/droids/` (flat).
- **Repo validation** - both the wizard and the explicit CLI validate that the GitHub repo exists before attempting Tree API discovery. Typos now produce "repository not found" instead of the misleading "no skills found."
- **Path hints in wizard** - after entering a repo, the wizard scans for top-level directories and shows them as hints (e.g. "found: skills, agents (or . for all)").
- **6 new clippy restriction lints** at deny level: `empty_structs_with_brackets`, `rest_pat_in_fully_bound_structs`, `format_push_string`, `needless_raw_strings`, `verbose_file_reads`, `string_lit_chars_any`. Plus 2 new `clippy.toml` thresholds: `too-large-for-stack = 512`, `enum-variant-size-threshold = 256`.
- **Fuzzer upgraded** - 7 semantic invariants (up from crash-freedom only): name validity, source field non-empty, `owner_repo` contains `/`, install targets, warnings, idempotency, unique names. Fuzz job moved from daily schedule to PR CI pipeline.

### Changed

- **`path` is now optional in `skillfile add github`** - omitting the path defaults to full-repo discovery (`.`). No more clap error when you forget the path argument.
- **Single-entry discovery skips the TUI** - when exactly one entry is found, it's added directly without opening the multi-select browser.
- **Skill boundary detection rewritten** - `collapse_to_entries()` now uses SKILL.md markers to detect skill roots at any depth. A directory containing `SKILL.md` claims all descendant files. Directories without markers fall back to the immediate-parent heuristic.
- **Parser validates `owner_repo` format** - `parse_github_entry` now rejects `owner_repo` values without `/` in the explicit-name branch. Found by the fuzzer.
- **Non-ASCII path support** - file paths with Unicode characters (Chinese filenames, spaces) are now percent-encoded for `raw.githubusercontent.com` fetches.
- **README revamped** - new Quick Start showing the wizard, updated examples to use `anthropics/courses`, new "Team workflow" section documenting in-house skills + community skills committed together.

### Fixed

- **`is_discovery_path()` case sensitivity** - used `ends_with(".md")` instead of `Path::extension()` with case-insensitive comparison.
- **Invalid URL for root path entries** - `build_github_url` generated `/blob/{ref_}/.` for root path `"."` instead of `/tree/{ref_}`.
- **Render loop allocation** - `draw_list` cloned `owner_repo` on every frame; now uses `as_str()`.
- **Scroll reset allocation** - `reset_scroll_if_changed` allocated a String every tick to compare highlighted paths; now compares indices.
- **Primitive obsession in frontmatter parser** - `parse_frontmatter_fields` returned a positionally-coupled 4-tuple; now returns `PreviewContent` directly.

---

## v1.2.2 - 17-03-2026

### Changed

- **`EntityType` is now a proper enum** - entity types (`skill`, `agent`) were stringly-typed `&str` values throughout the codebase. Now a dedicated `EntityType` enum with `parse()` and `as_str()`, making illegal states unrepresentable.

### Fixed

- **Local entries with bare directory names** - `local skill bash-craftsman` produced a spurious "local entry needs: name path" warning because the parser only recognized paths ending in `.md` or containing `/`. Bare directory names are now correctly treated as paths when no explicit name is given.
- **GitHub API 403 no longer disguised as "repo not found"** - HTTP 403 (rate limit) and 401 (bad credentials) were silently treated the same as 404, producing a misleading "could not resolve owner/repo@main" error. Now surfaces a clear message pointing to `GITHUB_TOKEN` or `gh auth login`.

---

## v1.2.1 - 14-03-2026

### Added

- **Personal platform config** - platform preferences can now be stored in a user-global TOML config file (`~/.config/skillfile/config.toml`) instead of the committed Skillfile. Useful in shared repos where each developer uses different AI tools.
  - `skillfile init` now asks where to store platform config: personal (recommended for shared repos) or Skillfile (shared with team)
  - Existing config from both sources shown during `init` with labels ("Skillfile" / "personal config")
  - Precedence rule: Skillfile install targets always override personal config
  - All commands (`status`, `diff`, `pin`, `unpin`, `resolve`) fall back to personal config when Skillfile has no install lines
  - `install` prints "Using platform targets from personal config" when falling back
- **Smarter init outro** - when the Skillfile already has entries (e.g. after cloning), the wizard now tells you how many and suggests `skillfile install` as the next step.

### Changed

- **Binary size reduced** - release profile now uses `opt-level = "s"` (optimize for size), bringing the binary from ~4.2 MB to ~3.4 MB.
- **Personal config is the default init choice** - the destination picker now lists personal config first with a tip about avoiding merge conflicts in shared repos.
- **Status formatting extracted** - `format_entry_status` is now a standalone function, no behavior change.

### Fixed

- **"No install targets" in shared repos** - if a Skillfile had entries but no `install` lines (common when teams don't want to commit platform preferences), every command failed. Now falls back to personal config so each developer can set their own platforms without touching the shared Skillfile.

---

## v1.2.0 - 13-03-2026

### Added

- **Interactive init wizard** — `skillfile init` now uses a modern setup wizard (cliclack) with arrow-key navigation and space-to-toggle platform selection. No more typing platform names manually.
  - Scope picker: choose `local`, `global`, or `both` (adds both scopes per platform)
  - Entity type hints next to each platform ("skill, agent" or "skill only")
  - Existing platforms pre-selected when re-running `init` on a repo that already has a Skillfile
- **Clone flow guidance** — when you run `skillfile install` for the first time after cloning a repo, it now shows which platforms are configured and suggests `skillfile init` to add yours.

### Changed

- **`init` requires an interactive terminal** — running `init` in CI or piped input now errors with a clear message pointing to `skillfile add` as the non-interactive alternative.
- **CI pipeline restructured** — static checks (fmt + clippy) now gate test jobs, saving CI minutes on bad pushes. Functional tests run under coverage. macOS gets its own test job.
- **`registry.rs` split into modules** — the 1479-line file is now `registry/mod.rs`, `registry/agentskill.rs`, `registry/skillssh.rs`, `registry/skillhub.rs`.
- **Workspace-level clippy lints** — complexity thresholds (cognitive complexity, line count, nesting depth, argument count) are now enforced via `[workspace.lints.clippy]`.
- **Test crate restructured** — subprocess-based tests moved to a dedicated `tests/` workspace crate with shared binary-resolution helpers. Network tests wrapped with retry (3 attempts, 2s delay).

### Fixed

- **Stale binary in CI** — functional tests spawn the compiled `skillfile` binary as a subprocess. `cargo test` does not produce this binary, and the CI cache (keyed on `Cargo.lock`, not source hash) could serve one compiled from a previous commit. A pre-build step now ensures the binary is always fresh.

---

## v1.1.0 - 12-03-2026

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
