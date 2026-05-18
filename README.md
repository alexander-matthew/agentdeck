# agentdeck

A small Rust TUI that wraps multiple AI-agent CLIs (Claude Code, Codex CLI, Gemini CLI, …) in a single split-pane view. The deck on the left is always visible; the focused agent fills the rest of the screen and you type into it the same way you'd type into the CLI on its own.

```
 agentdeck   focus: agent   [4 agents]    F1 to toggle focus
┌─ agents ─────────────────────────┐┌─ Claude · running · waiting ─────────┐
│ claude (2)                        ││ > write me a small fn that…          │
│  1 ● Claude              waiting  ││ Sure — here's a sketch:              │
│ ▶2 ● Claude · agentdeck  working  ││   fn solve(x: i32) -> i32 {          │
│ codex (1)                         ││       …                              │
│  3 ● Codex               thinking ││                                      │
│ gemini (1)                        ││                                      │
│  4 ● Gemini              idle     ││                                      │
└───────────────────────────────────┘└──────────────────────────────────────┘
 typing → focused agent   F1 → deck   Ctrl-C → interrupt agent
```

Default focus is the agent — every keystroke goes straight to it, including `Enter`, arrows, `Ctrl-C`, `Tab`, etc. The one key agentdeck reserves for itself is **F1**, which toggles focus between the agent and the deck. None of the supported agent CLIs bind F1, so this is the safest "always free" key.

When the deck has focus you can navigate with `↑/↓`, jump with `1`–`9`, spawn another agent under the highlighted provider with `a`, kill one with `x`, and quit with `q`. `Enter` (or any digit) returns focus to that agent.

## Why this exists

The agent CLIs (`claude`, `codex`, `gemini`) are full-screen TUIs in their own right, and running more than one at a time means juggling terminal tabs, tmux windows, or hoping you remember which agent is in which pane. agentdeck gives you a single entrypoint that:

- spawns each agent in its own PTY,
- shows them all together in a sidebar with live status badges,
- lets you type into any one of them as the focused pane,
- and keeps each agent's chat context completely isolated from the others — no cross-contamination, no shared transcript, no extra tokens spent on orchestration.

## How it talks to providers

agentdeck **never speaks to any provider API directly**. It only shells out to the native CLI you already have logged in (`claude`, `codex`, `gemini`, or anything else). That means:

- Whatever subscription, OAuth login, or seat assignment you have stays in force — agentdeck doesn't see your tokens, doesn't store them, doesn't need an API key.
- Provider-imposed rate limits, usage caps, and ToS are enforced by the underlying CLI exactly as they would be if you'd run it standalone.
- If a provider's CLI doesn't allow concurrent sessions on one account, agentdeck can't bypass that — that's a provider policy decision, not something this tool overrides.

This also means agentdeck has near-zero auth surface area of its own.

## Install

Requires Rust 1.85+ (builds clean on 1.93). On Ubuntu 24.04+:

```sh
sudo apt install -y rustc cargo
cargo install --git https://github.com/alexander-matthew/agentdeck
```

Or from a clone:

```sh
git clone https://github.com/alexander-matthew/agentdeck && cd agentdeck
cargo install --path .
```

The binary lands at `~/.cargo/bin/agentdeck`. If that's not on your `PATH`, add it:

```sh
# bash: in ~/.profile (login shells) or ~/.bashrc (interactive shells)
if [ -d "$HOME/.cargo/bin" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
fi
```

After that, `agentdeck` is just one word, the same as `claude` or `codex`.

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

The only key agentdeck reserves globally is **F1**, which swaps focus between the agent pane and the deck sidebar. Everything else depends on which pane has focus.

### Focus = agent (default)

All keystrokes are forwarded to the focused agent's PTY (chars, arrows, F2–F12, Ctrl-X letters, Backspace, Tab, Enter, Esc, Home/End/PgUp/PgDn/Del/Ins). Just type into Claude / Codex / Gemini the way you would if you'd run it standalone.

| Key | Action |
| --- | --- |
| anything | sent to the focused agent |
| `F1` | swap focus to the deck |

### Focus = deck

| Key | Action |
| --- | --- |
| `↑` / `k`, `↓` / `j` | move cursor (skips provider headings) |
| `1`–`9` | jump to that agent and return focus to it |
| `Enter` | return focus to the highlighted agent |
| `a` or `+` | spawn another agent under the highlighted agent's provider (ephemeral cwd prompt) |
| `x` | kill and remove the highlighted agent |
| `q`, `Ctrl-C` | quit (kills all child agents) |
| `F1` | swap focus back to the agent |

### Adding (cwd prompt)

| Key | Action |
| --- | --- |
| typing | edit the cwd |
| `←` / `→`, `Home` / `End` | move cursor in the field |
| `Backspace` | delete char left of cursor |
| `Enter` | spawn the new agent with this cwd |
| `Esc` or `Ctrl-C` | cancel |

## Status badges

agentdeck inspects each agent's terminal output and labels its current state. The badge color and word change as the agent moves between phases:

| Badge | Meaning |
| --- | --- |
| `starting` (cyan) | spawned in the last ~1s; first frame hasn't drawn yet |
| `working` (yellow) | bytes streaming right now — tokens, tool output, etc |
| `thinking` (magenta) | spinner glyphs detected in the bottom portion of the screen |
| `idle` (gray) | nothing happening but recently was, no detected prompt |
| `waiting` (bold green) | provider-specific prompt visible — **your turn** |
| `stuck` (bold red) | 45+ seconds of silence and the screen doesn't look like a prompt |
| `exited` (gray / red) | process exited; red if non-zero exit code |

Detection uses small provider-specific patterns (see `src/state.rs`). When a CLI redesigns its UI, that's the file to update.

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
