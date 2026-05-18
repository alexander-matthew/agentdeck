# agentdeck

A small Rust TUI for managing several AI-agent CLIs (Claude Code, Codex CLI, Gemini CLI, …) at once — like a "control room" with a 1000-foot overview and the ability to dive into any agent's native session.

```
┌─ agents ────────────────────────┐ ┌─ preview · Claude · running ──────────┐
│  1 ● Claude         claude      │ │ > write me a small fn that…           │
│ ▶2 ● Codex          codex       │ │ Sure — here's a sketch:               │
│  3 ● Gemini         gemini      │ │   fn solve(x: i32) -> i32 {           │
│  4 ○ exploration    claude      │ │       …                               │
└─────────────────────────────────┘ └───────────────────────────────────────┘
 ↑/↓ select   1-9 attach   r restart   k kill   q quit
```

Press `Enter` (or a number) to take over the real terminal with that agent's full native TUI. Press `Ctrl-A d` to come back to the overview.

## Why this exists

The agent CLIs (`claude`, `codex`, `gemini`) are full-screen TUIs in their own right, and running more than one at a time means juggling terminal tabs, tmux windows, or hoping you remember which agent is in which pane. agentdeck gives you a single entrypoint that:

- spawns each agent in its own PTY,
- shows them all in a status list with a live preview pane,
- lets you "attach" to any one of them as if you'd opened it yourself,
- and keeps each agent's chat context completely isolated from the others — no cross-contamination, no shared transcript, no extra tokens spent on orchestration.

## How it talks to providers

agentdeck **never speaks to any provider API directly**. It only shells out to the native CLI you already have logged in (`claude`, `codex`, `gemini`, or anything else). That means:

- Whatever subscription, OAuth login, or seat assignment you have stays in force — agentdeck doesn't see your tokens, doesn't store them, doesn't need an API key.
- Provider-imposed rate limits, usage caps, and ToS are enforced by the underlying CLI exactly as they would be if you'd run it standalone.
- If a provider's CLI doesn't allow concurrent sessions on one account, agentdeck can't bypass that — that's a provider policy decision, not something this tool overrides.

This also means agentdeck has near-zero auth surface area of its own.

## Install

Requires Rust 1.85+ (the binary builds clean on 1.93). On Ubuntu 24.04+:

```sh
sudo apt install -y rustc cargo
cargo install --git https://github.com/alexander-matthew/agentdeck
```

Or from a clone:

```sh
git clone https://github.com/alexander-matthew/agentdeck && cd agentdeck
cargo install --path .
```

`agentdeck` ends up in `~/.cargo/bin/` (or wherever your `CARGO_HOME/bin` is). Make sure that's on `PATH`.

## First run

```sh
agentdeck
```

On first launch, agentdeck writes a default config to `~/.config/agentdeck/config.toml` with profiles for `claude`, `codex`, and `gemini`. Edit it, then re-run. To inspect what config it would use:

```sh
agentdeck --print-config
```

### Config example

```toml
[settings]
prefix_byte  = 1     # 0x01 = Ctrl-A. Set to 2 for Ctrl-B (tmux-style).
detach_key   = "d"

[[agent]]
id        = "claude-main"
name      = "Claude — main"
provider  = "claude"
command   = "claude"
args      = []
cwd       = "~/code"

[[agent]]
id        = "claude-review"
name      = "Claude — review lane"
provider  = "claude"
command   = "claude"
args      = ["--model", "claude-sonnet-4-6"]
cwd       = "~/code/some-project"

[[agent]]
id        = "codex"
provider  = "codex"
command   = "codex"

[[agent]]
id        = "gemini-readonly"
provider  = "gemini"
command   = "gemini"
manual    = true   # don't auto-spawn; open later when needed
```

You can run **multiple instances of the same provider** by giving each its own `id` and `cwd`. Each is fully isolated.

## Keys

### Overview
| Key | Action |
| --- | --- |
| `↑` / `k`, `↓` / `j` | move cursor |
| `1`–`9` | attach to that agent |
| `Enter` | attach to highlighted agent |
| `Shift-K` | kill highlighted agent (SIGKILL) |
| `q`, `Ctrl-C` | quit (kills all child agents) |

### Attached
The agent's full native TUI controls the terminal. The only intercepted chord is:
| Key | Action |
| --- | --- |
| `Ctrl-A d` (default) | detach back to overview |
| `Ctrl-A Ctrl-A` | send a literal `Ctrl-A` through to the agent |

Change the prefix via `settings.prefix_byte` in the config.

## Design notes

- **One PTY per agent.** Each child process gets a real pseudo-terminal, so its full-screen UI (alt screen, cursor moves, colors, mouse) renders normally on attach.
- **vt100 parser per agent** for the preview pane — we don't try to repaint the agent through ratatui when you're attached; the bytes go straight to your real terminal.
- **No shared context.** agentdeck itself doesn't run an LLM, doesn't keep a transcript, doesn't summarize anything. Each agent's conversation history lives entirely inside that agent's own process.
- **Logs** go to `~/.local/state/agentdeck/agentdeck.log` so they never collide with the TUI. Tail with `tail -f ~/.local/state/agentdeck/agentdeck.log`. Set `AGENTDECK_LOG=debug` for verbose output.

## Security

- agentdeck spawns whatever you tell it to. The config file is plain TOML on your disk — treat it as you would your shell's `.profile`.
- It does **not** inherit or forward your shell aliases. If you usually run `claude` via a wrapper, point `command` at the wrapper script.
- The repo intentionally has restricted write permissions: main is protected, and direct pushes from non-maintainers are rejected. Send a PR.

## Status

Early. Works for the happy path on Linux. Not yet tested on macOS (the PTY layer is cross-platform via [`portable-pty`](https://docs.rs/portable-pty/), so it should mostly Just Work — PRs welcome). Not Windows.

## Documentation

- [Configuration reference](docs/configuration.md) — every config field, with recipes.
- [Provider notes](docs/providers.md) — per-provider quirks and subscription caveats.
- [Architecture](docs/architecture.md) — internals walkthrough for contributors.
- [Contributing guide](CONTRIBUTING.md) — local dev, CI gate, PR flow.
- [Security policy](SECURITY.md) — threat model and reporting.
- [Changelog](CHANGELOG.md).

## License

MIT — see [LICENSE](LICENSE).
