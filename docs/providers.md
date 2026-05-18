# Provider notes

agentdeck is provider-agnostic — it just spawns whatever you point it at. But because each provider's CLI has its own auth model, its own subscription terms, and its own quirks under a non-interactive parent process, here's what to know.

## General principle

**agentdeck talks to the local CLI, never to the provider directly.** That means:

- Subscription tier, seat assignment, rate limits, and ToS are enforced exactly the same as if you ran the CLI standalone — agentdeck cannot evade them and would not want to.
- Whatever auth the CLI uses (OAuth, API key, session cookie) lives in whatever file the CLI manages it in. agentdeck does not read, store, or rotate those files.
- If a provider's CLI prohibits or rate-limits concurrent sessions on the same account, that limit applies to agentdeck just as it would to multiple shell tabs. **Always consult your provider's terms before running multiple instances under one account.**

## Claude Code

CLI command: `claude` (from `@anthropic-ai/claude-code`).

| Note |
| --- |
| Auth lives in `~/.claude/.credentials.json`. agentdeck does not touch this file. |
| Each agentdeck-launched `claude` process is a completely separate conversation. Context is not shared with other instances. |
| Pro and Max subscriptions cover Claude Code usage; usage limits apply per the [subscription terms](https://www.anthropic.com/pricing). Running many concurrent sessions does not raise the cap. |
| Useful flags to pin per agent: `--model <id>`, `--working-directory <dir>` (or just set `cwd` in the config). |
| To run agentdeck *inside* the [dev-sandbox](https://github.com/alexander-matthew/dev-sandbox) devcontainer so Claude can't reach host secrets, install `@anthropic-ai/claude-code` in the container image and point `command` at it there. |

## Codex CLI

CLI command: `codex` (from `@openai/codex`).

| Note |
| --- |
| Auth flow is OpenAI account / ChatGPT subscription based; tokens are managed by the `codex` CLI itself. |
| Concurrent sessions are typically fine, but heavy parallel use can trip rate limits — the CLI surfaces these as visible errors inside the session, not at the agentdeck layer. |
| First run inside any new environment (including a fresh devcontainer) needs a `codex login`. |

## Gemini CLI

CLI command: `gemini` (Google's official CLI).

| Note |
| --- |
| Auth is via Google account OAuth; tokens cached under `~/.config/gemini` (or platform equivalent). |
| Free-tier daily quotas are tight; if you run several instances expect to hit them. |
| The CLI uses fairly standard ANSI; renders cleanly under agentdeck's PTY. |

## Anything else

Any other agent-style CLI works as long as it:

1. Renders to a terminal (not a separate window),
2. Treats its stdin as a TTY (which a PTY satisfies),
3. Exits cleanly on SIGHUP / SIGKILL.

To add one, just add another `[[agent]]` block with `provider = "other"` and your `command` of choice. There is no plugin API to write — agentdeck doesn't need to know about the provider for it to work.

## Caveats and gotchas

- **Shell aliases don't apply.** agentdeck does not source your `.bashrc`/`.zshrc`. If you wrap the CLI in a shell function, replace that function with a real script and point `command` at the script.
- **MCP servers and tool configs.** These are managed by each CLI; nothing in agentdeck changes them. Concurrent agents sharing an MCP server on the same machine will compete for that server's resources.
- **Working directory matters.** Many agent CLIs anchor their context to the cwd. Set `cwd` per agent if you want one focused on `~/code/A` and another on `~/code/B`.
- **Terminal size.** Each PTY is sized to the host terminal at startup, and resized when you change the terminal during attach (polled at ~200 ms). While in the overview the PTY size is held constant at `(host_rows - 3) × host_cols` so the agent's render fits inside the preview pane's expected geometry.
