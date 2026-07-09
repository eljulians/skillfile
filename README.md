# skillfile

[![CI](https://img.shields.io/github/actions/workflow/status/eljulians/skillfile/ci.yml?style=flat-square&label=CI)](https://github.com/eljulians/skillfile/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/skillfile?style=flat-square)](https://crates.io/crates/skillfile)
[![Coverage](https://img.shields.io/codecov/c/github/eljulians/skillfile?style=flat-square)](https://codecov.io/gh/eljulians/skillfile)

**Track AI skills and agents declaratively, like dependencies. Pin them. Patch them. Deploy everywhere.**

Fetch from GitHub, GitLab, local files, or direct URLs. Lock to exact revisions. Deploy to Claude Code, Codex, Cursor, Copilot, Factory, Gemini CLI, Junie, Opencode, Windsurf, and Antigravity.

skillfile is a file manager, not a framework. It keeps your installed prompts reproducible and lets you carry local edits forward with patch files.

![demo](https://github.com/eljulians/skillfile/raw/master/docs/demo.gif)

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

The same flow works for GitLab:

```bash
skillfile add gitlab skill my-group/my-project skills/
skillfile install
```

`Skillfile.lock` pins upstream content to exact SHAs or refs so another machine gets the same files.

## What it manages

Sources:

- `github`
- `gitlab`
- `local`
- `url`

Entities:

- `skill`
- `agent`

GitLab project paths may include subgroups. Self-hosted GitLab is supported through `GITLAB_HOST`.

## Example Skillfile

```text
install  claude-code  global
install  codex        global

github  skill  anthropics/skills  skills/slack-gif-creator
gitlab  skill  my-group/platform-skills  skills/release
local   skill  skills/team/reviewer/SKILL.md
url     agent  triager  https://example.com/agents/triager.md
```

Format details live in [SPEC.md](SPEC.md).

## Common commands

```bash
skillfile init
skillfile add
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

## Patches

Edit an installed file, then pin it:

```bash
skillfile pin browser
skillfile install --update
```

Pinned changes are stored in `.skillfile/patches/`. If upstream changes conflict, use `skillfile diff` and `skillfile resolve`.

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

## Notes

> [!IMPORTANT]
> skillfile downloads markdown and installs it where your AI tools expect it. It does not sandbox or verify the content.

Shell completions are available via `skillfile completions <bash|zsh|fish|powershell|elvish>`.
