# skillfile Development Guide

This file is for maintainers and contributors using Claude Code or similar coding agents in this repository. Keep changes aligned with the public project contract in `README.md` and `SPEC.md`.

## Project Shape

`skillfile` is a Rust workspace CLI for managing AI skill and agent markdown files. It reads a `Skillfile`, fetches upstream content, locks exact revisions, preserves local edits as patches, and deploys to supported AI tool directories.

Workspace dependency direction is strict:

```text
core <- sources <- deploy <- cli
```

- `crates/core`: models, parsing, lock files, conflicts, patches, output, errors.
- `crates/sources`: HTTP clients, GitHub/GitLab/registry resolution, sync logic.
- `crates/deploy`: platform adapters and install orchestration.
- `crates/cli`: clap commands, TUI flows, update checks, binary entrypoint.
- `tests`: workspace-level functional tests that spawn the CLI.
- `fuzz`: parser fuzz target, excluded from the normal workspace.

## Architecture Standards

- Keep domain models and format rules in `core`; do not let `core` depend on network, deployment, terminal UI, or command concerns.
- Keep source fetching and registry behavior in `sources`; do not bypass `HttpClient` when code needs to be testable.
- Keep platform-specific filesystem layout behind `PlatformAdapter`; avoid hardcoded per-tool branching in command code.
- Keep CLI code as orchestration and presentation. Reusable behavior belongs in the lower crate that owns the concept.
- Preserve backward compatibility for `Skillfile`, `Skillfile.lock`, patch paths, and installed layout unless `SPEC.md` is updated in the same change.
- Prefer typed domain errors in library crates via `SkillfileError`; reserve `anyhow` for top-level CLI reporting.
- Do not add broad abstractions for hypothetical future platforms, sources, or entity types. Add extension points when a concrete second use case exists.
- When changing a public contract, include the migration story, docs update, and tests that lock the new behavior.

## Fragility Controls

- Preserve user edits before remote updates. `install --update`, `pin`, `resolve`, conflict state, and patch rebasing are one workflow; do not change one piece without testing the full lifecycle.
- Filesystem writes that replace cache, installed content, lock files, or patches should avoid partial updates. Prefer staged writes, rollback, or cleanup-on-failure patterns already used in the repo.
- Never silently clobber user-managed installed files. Respect `InstallOptions`, existing destination behavior, and conflict records.
- Treat symlinks as hostile around install destinations and copied source trees. Do not follow symlinked destination roots or components when writing managed content.
- Keep lock output, patch keys, deploy-result keys, registry search output, and TUI snapshots deterministic. Use sorted maps/vectors when order reaches disk, tests, or UI snapshots.
- Preserve token boundaries for both GitHub and GitLab. Raw tokens should stay behind opaque token handles and host allowlists.
- Do not let registry-specific quirks leak into generic sync, deploy, or CLI contracts; isolate them in the registry/source module that owns them.
- Any new platform adapter must define support matrix, global/local paths, directory install mode, installed-path lookup, and patch-key behavior together.
- Any new source type must define parser fields, lock representation, cache layout, fetch/update semantics, auth boundaries, and mocked tests.

## Commands

Use the Rust toolchain from `rust-toolchain.toml`.

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build -p skillfile
cargo test --workspace
cargo test --test upstream
cargo deny check
cargo machete
shellcheck install.sh
```

Notes:
- Build `skillfile` before functional tests that spawn `target/debug/skillfile`.
- `tests/upstream.rs` uses real external APIs and needs `GITHUB_TOKEN` for GitHub-backed checks.
- Fuzzing uses nightly: `cargo +nightly fuzz run parse_manifest -- -max_total_time=60`.

## Core Invariants

- `SourceFields` keeps source-specific fields in enum variants so invalid source states are unrepresentable.
- Lock keys use `"{source_type}/{entity_type}/{name}"` and remain sorted through `BTreeMap`.
- Patch paths are part of the contract:
  - Single-file entry: `.skillfile/patches/<type>s/<name>.patch`
  - Directory entry: `.skillfile/patches/<type>s/<name>/<relative>.patch`
- Sync is two-phase: resolve SHAs first, then fetch files. Preserve deduplication by `(owner_repo, ref)`.
- `install --update` auto-pins local edits before fetching, then rebases patches after deploy.
- `cmd_add` must roll back manifest, lockfile, and cache changes if later steps fail.
- GitHub tokens must only be sent to exact GitHub hosts handled by the token newtype.
- For HTTP 4xx behavior, only 404 and 422 map to `Ok(None)`; 403 should surface rate-limit context.
- A directory containing `SKILL.md` is a skill root; descendants belong to that skill.

## Testing

Prefer the smallest test boundary that proves the behavior.

- Unit tests live in the same source file under `#[cfg(test)]`.
- Unit tests may call functions from the same file, construct public data types, use same-crate `#[cfg(test)]` support modules, and use real temp files.
- Unit tests must not call functions from other source files, other workspace crates, or re-exported cross-crate functions.
- Use manual fixtures for cross-boundary setup in unit tests: write manifests, lock JSON, patch files, and conflict state directly.
- Integration tests live in `crates/<crate>/tests/` when public APIs coordinate multiple modules.
- Functional tests live in workspace `tests/` and spawn the real binary.
- Mock HTTP in unit and integration tests through `HttpClient` test doubles. Only functional tests should hit real APIs.
- Every new command needs at least one functional golden-path test.

## Lint And Style

Workspace lints deny unsafe code and several Clippy complexity/style regressions. Keep functions small enough for the configured thresholds in `clippy.toml`; prefer extracting focused helpers over raising limits.

Comment policy:
- Keep comments that explain non-obvious invariants, return semantics, examples, or why code is shaped a certain way.
- Do not add doc comments that restate a function name, type name, field name, or signature.

For filesystem reads, prefer direct helpers such as `fs::read_to_string` where applicable. For string building, avoid `push_str(&format!(...))`; use `write!` into the string.

## Platform Scope

Supported install targets currently include Claude Code, Codex, Cursor, Copilot, Factory, Gemini CLI, Junie, Opencode, Windsurf, and Antigravity. Some targets support skills only; verify adapter behavior before changing deploy logic.

In scope:
- Skills with `SKILL.md`
- Agent markdown files
- Source resolution, pinning, patches, install adapters

Out of scope unless the project direction changes:
- MCP servers
- Hook systems
- Plugin bundles
- Generic context-file management

## Contribution Expectations

- Check `SPEC.md` before changing manifest syntax or user-facing semantics.
- Keep crate boundaries intact; avoid making downstream crates a dependency of upstream crates.
- Update `README.md`, `SPEC.md`, or `CHANGELOG.md` when behavior visible to users changes.
- When adding `CHANGELOG.md` entries for external contributions, end the entry with `by @handle` and no trailing full stop after the mention.
- Keep this file limited to repository-wide engineering guidance; individual contributor workflows should live outside the repository.
