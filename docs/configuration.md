# Configuration reference

agentdeck reads a single TOML file. By default:

```
$XDG_CONFIG_HOME/agentdeck/config.toml
# or, if XDG_CONFIG_HOME is unset:
~/.config/agentdeck/config.toml
```

Override with `agentdeck --config <path>`.

If the file doesn't exist on first run, agentdeck writes a default with profiles for `claude`, `codex`, `gemini`, `aider`, and `shell`, plus a starter `[usage_commands]` entry for Claude. To see the current resolved config without launching the TUI:

```sh
agentdeck --print-config
```

Agents added or renamed at runtime via the `a` / `r` keys are **not** written back to the config — they only live until you quit. To make a change permanent, edit `config.toml` and relaunch.

## File structure

The file has one optional `[settings]` table, one optional `[usage_commands]` table, and any number of `[[agent]]` array-of-table entries:

```toml
[settings]
toggle_key         = "ctrl-space"
grid_rows          = 2
grid_cols          = 2
usage_refresh_secs = 60

[usage_commands]
claude = "npx -y ccusage@latest --json"

[[agent]]
id        = "claude"
provider  = "claude"
command   = "claude"
# … see field reference below
```

## `[settings]`

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `toggle_key` | string | `"ctrl-space"` | Key that swaps focus between the deck (sidebar) and the focused agent. Modifier syntax: `ctrl-`, `alt-`, `shift-`, `cmd-`, joined with a `-` (e.g. `"ctrl-space"`, `"alt-d"`, `"esc"`, `"f1"`). Named keys: `f1`–`f12`, `enter`, `tab`, `esc`, `space`, `up`/`down`/`left`/`right`, plus any single character. `Ctrl-Space` is the default because no supported agent CLI binds it and no OS/WM grabs it first — avoid `Cmd-Tab` / `Super-Tab` (app switcher) and `Alt-Tab` (window cycler). |
| `grid_rows` | integer | `2` | Rows in the multi-pane grid view (`g` from deck focus). Clamped to ≥ 1. |
| `grid_cols` | integer | `2` | Cols in the multi-pane grid view. Clamped to ≥ 1. The visible page contains the selected agent; selecting an agent on another page auto-scrolls. |
| `usage_refresh_secs` | integer | `60` | How often (in seconds) the dashboard re-runs each `[usage_commands]` entry. Clamped to ≥ 5. The dashboard runs an initial refresh at startup so the first `u` press shows something. |
| `prefix_byte` | integer | `1` | **Deprecated, no-op.** Was the detach-chord prefix back when "attach" was a separate mode. Accepted-but-ignored. |
| `detach_key` | string | `"d"` | **Deprecated, no-op.** Same story. |

## `[usage_commands]`

A map from provider tag (`claude`, `codex`, `gemini`, `aider`, `shell`, `other`) to a shell command. When the usage dashboard is open (`u`), each command is run as `sh -c <command>` in a background thread, with a 20 s timeout and a 64 KB output cap. Captured stdout is rendered as a card; non-zero exits or stderr land as a red error line. Empty / missing entries skip that provider.

```toml
[usage_commands]
claude = "npx -y ccusage@latest --json"        # default
codex  = "/home/me/bin/codex-usage.sh"          # any executable
gemini = ""                                     # skipped
```

**Security note.** These commands execute under your user with the same access agentdeck itself has. Treat the config file the way you treat your shell's `.profile`. Don't paste in a command you wouldn't run yourself.

## `[[agent]]`

One block per agent. The order in the file is the order shown in the overview list and the number-key binding (1 = first, 2 = second, …).

| Field | Type | Required | Default | Meaning |
| --- | --- | --- | --- | --- |
| `id` | string | yes | — | Stable identifier. Used in logs, the spawned thread name, and as the display name fallback. Should be unique. |
| `name` | string | no | falls back to `id` | Human-readable label shown in the overview list. Truncated to 14 chars on display. |
| `provider` | enum | yes | — | One of `claude`, `codex`, `gemini`, `aider`, `shell`, `other`. Drives the display tag, the awaiting-input heuristic in `src/state.rs`, and the lookup key for `[usage_commands]`. |
| `command` | string | yes | — | The executable to spawn. Looked up on `PATH` if not absolute. |
| `args` | array of strings | no | `[]` | Arguments passed to the executable. |
| `cwd` | string | no | parent process's cwd | Working directory. `~` and environment variables are expanded via [shellexpand](https://docs.rs/shellexpand/). |
| `env` | table of strings | no | `{}` | Extra environment variables, merged on top of the inherited environment. |
| `manual` | bool | no | `false` | If `true`, the agent isn't auto-spawned at startup. (Reserved for a future "spawn from overview" key — currently this just means the slot is skipped.) |

## Recipes

### Multiple Claude instances pinned to different projects

```toml
[[agent]]
id       = "claude-personal-site"
name     = "Claude — personal-site"
provider = "claude"
command  = "claude"
cwd      = "~/code/personal-site"

[[agent]]
id       = "claude-agentdeck"
name     = "Claude — agentdeck"
provider = "claude"
command  = "claude"
cwd      = "~/code/agentdeck"
```

Each instance has its own conversation history, its own working directory, and is fully isolated from the others. Number keys `1` and `2` jump between them.

### Three providers side-by-side

```toml
[[agent]]
id       = "claude"
provider = "claude"
command  = "claude"
cwd      = "~/code"

[[agent]]
id       = "codex"
provider = "codex"
command  = "codex"
cwd      = "~/code"

[[agent]]
id       = "gemini"
provider = "gemini"
command  = "gemini"
cwd      = "~/code"
```

### Passing flags through

The agent CLI's flags go in `args`:

```toml
[[agent]]
id       = "claude-sonnet"
provider = "claude"
command  = "claude"
args     = ["--model", "claude-sonnet-4-6"]
```

### Wrapping the CLI

If you usually run `claude` via a shell wrapper or function, point `command` at a script:

```toml
[[agent]]
id       = "claude-wrapped"
provider = "claude"
command  = "/home/me/bin/claude-with-mcp.sh"
```

agentdeck does not source your shell rc, so aliases and functions from `.bashrc` are not visible — wrap them in a real file instead.

### Injecting env vars

```toml
[[agent]]
id       = "claude-debug"
provider = "claude"
command  = "claude"

[agent.env]
ANTHROPIC_LOG = "debug"
NO_COLOR      = "0"
```

### Holding a slot for later

```toml
[[agent]]
id       = "gemini-readonly"
provider = "gemini"
command  = "gemini"
manual   = true
```

`manual = true` skips auto-spawn at startup. (A future release will add an in-TUI key to start manual agents on demand; today, flip the flag and relaunch.)

## Environment variables agentdeck itself reads

| Var | Effect |
| --- | --- |
| `XDG_CONFIG_HOME` | Where to look for `agentdeck/config.toml`. |
| `XDG_STATE_HOME` | Where to write `agentdeck/agentdeck.log`. |
| `AGENTDECK_LOG` | Log level filter, in [`tracing-subscriber`](https://docs.rs/tracing-subscriber/) `EnvFilter` syntax. e.g. `info`, `debug`, `agentdeck=trace,portable_pty=warn`. |
| `HOME` | Used to derive defaults for the two `XDG_*` vars when unset. |
| `TERM` | Forwarded into each agent's PTY (defaults to `xterm-256color` if unset). |

## Where things end up on disk

| Path | Purpose |
| --- | --- |
| `~/.config/agentdeck/config.toml` | This config. |
| `~/.local/state/agentdeck/agentdeck.log` | Orchestration log (agent spawn/exit, resize events, errors). Never contains agent transcript content. |
| (none) | No transcript or cache files are ever written. Each agent's history lives entirely inside that agent's own process. |
