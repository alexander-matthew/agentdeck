# Security policy

## Reporting a vulnerability

Please **do not** open a public GitHub issue for security problems. Email a private report to `alex.matt.007@gmail.com` with:

- a description of the issue,
- a minimal reproduction (config snippet, exact commands, terminal type),
- the impact you think it has.

You'll get an acknowledgement within a few days. Fixes are released as patch versions.

## Threat model

agentdeck is a local-only orchestrator. It runs in your terminal session, spawns child processes, and proxies your terminal to one of them at a time. It has these properties by design:

- **No network code.** agentdeck never opens a socket. All network activity comes from the agent CLIs it spawns.
- **No provider credentials.** It does not read, store, or forward API tokens. Provider auth lives in whatever location the underlying CLI manages (e.g. `~/.claude/.credentials.json` for Claude Code, the analogous file for Codex, OAuth tokens for Gemini).
- **No transcript collection.** It does not persist agent output anywhere except the in-memory `vt100` parser used for preview rendering. The only thing written to disk by agentdeck itself is the config file and `~/.local/state/agentdeck/agentdeck.log` (which logs orchestration events, not agent content).
- **Untrusted PTY output is not parsed as a control channel.** Bytes coming from the agent are either fed to `vt100` (parsed against the VT100/xterm grammar) or written verbatim to your terminal in attach mode. There is no codepath where the agent can influence agentdeck's own control flow.

What agentdeck *can* see, and therefore what someone compromising the agentdeck process could see:

- The bytes of every agent's terminal — its prompt, the user's typing, the model's replies.
- Whatever environment variables and working-directory paths are set in your config.

What agentdeck *cannot* see:

- The contents of `~/.ssh`, `~/.config/gh`, `~/.claude/.credentials.json` and similar — agentdeck never reads these files. Child processes can read them per normal Unix permissions, but that's no different from running the CLI directly.

## Operational guidance

- The config file (`~/.config/agentdeck/config.toml`) controls what gets executed. Treat it like a shell rc file.
- If you want stronger isolation, run agentdeck inside the [dev-sandbox](https://github.com/alexander-matthew/dev-sandbox) devcontainer or any other restricted container — the agent CLIs will read auth from the container's filesystem, not your host's.
- The release binary is built with `lto = "thin"` and `strip = true`; no debug symbols are shipped.

## Supported versions

Only the latest `0.x` patch series is supported with security fixes during this pre-1.0 phase.
