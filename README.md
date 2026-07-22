# skillfile

[![CI](https://img.shields.io/github/actions/workflow/status/eljulians/skillfile/ci.yml?style=flat-square&label=CI)](https://github.com/eljulians/skillfile/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/skillfile?style=flat-square)](https://crates.io/crates/skillfile)
[![License](https://img.shields.io/github/license/eljulians/skillfile?style=flat-square)](LICENSE)
[![Coverage](https://img.shields.io/codecov/c/github/eljulians/skillfile?style=flat-square)](https://codecov.io/gh/eljulians/skillfile)
<br>
[![GitHub release downloads](https://img.shields.io/github/downloads/eljulians/skillfile/total?style=flat-square&label=GitHub%20release%20downloads)](https://github.com/eljulians/skillfile/releases)
[![crates.io downloads](https://img.shields.io/crates/d/skillfile?style=flat-square&label=crates.io%20downloads)](https://crates.io/crates/skillfile)

**One AI setup, everywhere. Pin it. Patch it. Deploy everywhere.**

Use the same skills and agents on your work laptop, personal machine, servers, and whichever AI tools you use. Put the setup in a repo when you want to share it with a team. skillfile locks upstream versions, preserves your edits on update, and installs to Claude Code, Codex, Cursor, Antigravity, custom filesystem paths, and more. No runtime or framework required.

![demo](https://github.com/eljulians/skillfile/raw/master/docs/demo.gif)

Add a skill once, deploy it to your configured tools, and carry your local edits forward when the upstream skill changes.

## Stop manually downloading and copying AI instructions across tools and machines

Without a manager, skills and agents end up as copied markdown files: different versions on every machine, separate copies for every AI tool, and local improvements lost during updates.

Manage them like dependencies:

- Define skills and agents in one `Skillfile`
- Lock exact upstream revisions in `Skillfile.lock`
- Deploy the same setup to the AI tools you already use
- Preserve local improvements as patches when upstream content changes
- Fetch skills and agents from GitHub, GitLab, local files, or URLs

Share skills and agents without sharing install targets. Each machine can keep its platform choices in user config; `skillfile init` can set them. Add `install` lines when the project should use the same targets.

### For individuals

Keep your preferred skills consistent across Claude Code, Codex, Cursor, and other supported tools without manually copying files.

### For engineering teams

Commit a `Skillfile` to a project to share skills and agents. Developers can install them into whichever tools they prefer. If everyone should use the same targets, commit `install` lines too.

## Install

```bash
curl -fsSL https://github.com/eljulians/skillfile/releases/latest/download/install.sh | sh
```

Or:

```bash
cargo install skillfile
```

Prebuilt binaries are published on [GitHub Releases](https://github.com/eljulians/skillfile/releases/latest).

## Quick start

```bash
skillfile init
skillfile add github skill anthropics/skills skills/
skillfile install
```

For GitLab:

```bash
skillfile add gitlab skill my-group/my-project skills/
skillfile install
```

`Skillfile.lock` pins upstream content to exact SHAs or refs so another machine gets the same files.

## Update without losing local edits

Edit an installed file, then pin it:

```bash
skillfile pin browser
skillfile install --update
```

Pinned changes are stored in `.skillfile/patches/`. If upstream changes conflict, use `skillfile diff` to review the conflict and `skillfile resolve` to choose the result.

## What it manages

Sources:

- `github`
- `gitlab`
- `local`
- `url`

Entities:

- `skill`
- `agent`

Install targets:

- Built-in AI tool directories with `install`
- Any filesystem directory with `install-path`

GitLab project paths may include subgroups. Self-hosted GitLab is supported through `GITLAB_HOST`.

## Example Skillfile

```text
install  claude-code  global
install  codex        global
install-path  openclaw  skill  ~/.openclaw/skills

github  skill  anthropics/skills  skills/slack-gif-creator
gitlab  skill  my-group/platform-skills  skills/release
local   skill  skills/team/reviewer/SKILL.md
url     agent  triager  https://example.com/agents/triager.md
```

The `install-path` line sends all three skill entries above to `~/.openclaw/skills` in addition to the compatible built-in targets. `openclaw` is only the target's display label. See [Custom install paths](#custom-install-paths) for layouts and a complete example.

Format details live in [SPEC.md](SPEC.md).

## Common commands

```bash
skillfile init
skillfile add
skillfile list
skillfile install
skillfile install --update
skillfile status
skillfile diff <name>
skillfile pin <name>
skillfile resolve <name>
```

Run `skillfile add` with no arguments for the interactive picker, or use explicit subcommands such as:

```bash
skillfile add github skill owner/repo skills/SKILL.md
skillfile add gitlab skill group/project skills/SKILL.md
skillfile add local skill skills/my-skill/SKILL.md
skillfile add url agent https://example.com/agent.md --name my-agent
```

## Auth

GitHub:

- `GITHUB_TOKEN`
- `GH_TOKEN`

GitLab:

- `GITLAB_TOKEN`
- `GITLAB_PRIVATE_TOKEN`
- `GITLAB_HOST` for self-hosted instances - a bare hostname such as `gitlab.example.com` (a `https://` prefix or trailing slash is accepted and normalized away)

`skillfile init` can also save token settings in user config.

## Platforms

Supported install targets:

- `claude-code`
- `codex`
- `cursor`
- `copilot`
- `factory`
- `gemini-cli`
- `junie`
- `opencode`
- `windsurf`
- `antigravity`

Some platforms support skills only; see `skillfile init` or `skillfile --help` for the exact target behavior.

### Custom install paths

Use `install-path <label> <entity-type> <directory>` when a tool is not built in or reads skills or agents from a nonstandard directory. This creates an install target; it does not add a skill or agent by itself.

For example, this `Skillfile` declares one custom skill target and two skills:

```text
install-path  openclaw  skill  ~/.openclaw/skills

github  skill  reviewer       acme/skills  reviewer/SKILL.md
local   skill  release-notes  skills/release-notes/SKILL.md
```

Running `skillfile install` installs both skill entries into the target:

```text
~/.openclaw/skills/reviewer/SKILL.md
~/.openclaw/skills/release-notes/SKILL.md
```

In other words, every entry matching the target's entity type is installed there. If the `Skillfile` also contained agents, they would not be installed into this skill-only target. Add another target for them:

```text
install-path  openclaw-agents  agent  ~/.openclaw/agents
```

The label (`openclaw` or `openclaw-agents` above) is only a name shown in command output. It does not enable an OpenClaw integration or choose which entries to install. Each target uses these layouts:

| Entity | Installed layout |
|---|---|
| `skill` | `<path>/<name>/SKILL.md` for a single-file skill, or `<path>/<name>/...` for a directory skill |
| `agent` | `<path>/<name>.md` |

Relative paths are resolved from the repository root. `~` and paths beginning with `~/` use your home directory. Custom targets can be mixed with built-in `install` targets; matching entries are installed to every configured target that supports their entity type.

`skillfile validate` rejects duplicate destinations for the same entity type. Install and read operations also refuse to traverse user-controlled symlinked path components so a declared target cannot escape through a symlink.

## Notes

> [!IMPORTANT]
> skillfile downloads markdown and installs it where your AI tools expect it. It does not sandbox or verify the content.

Shell completions are available via `skillfile completions <bash|zsh|fish|powershell|elvish>`.

## License

Apache-2.0. See [LICENSE](LICENSE).
