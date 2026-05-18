# Configuration reference

agentdeck reads a single TOML file. By default:

```
$XDG_CONFIG_HOME/agentdeck/config.toml
# or, if XDG_CONFIG_HOME is unset:
~/.config/agentdeck/config.toml
```

Override with `agentdeck --config <path>`.

If the file doesn't exist on first run, agentdeck writes a default with profiles for `claude`, `codex`, and `gemini`. To see the current resolved config without launching the TUI:

```sh
agentdeck --print-config
```

Agents added at runtime via the `a` key are **not** written back to the config — they only live until you quit. To make a new agent permanent, edit `config.toml` and relaunch.

## File structure

The file has one optional `[settings]` table and any number of `[[agent]]` array-of-table entries:

```toml
[settings]
prefix_byte = 1
detach_key  = "d"

[[agent]]
id        = "claude"
provider  = "claude"
command   = "claude"
# … see field reference below
```

## `[settings]`

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `prefix_byte` | integer | `1` | **Deprecated, no-op.** Was the detach-chord prefix back when "attach" was a separate mode. The current model is split-view with `F1` as the only intercepted key, so this field is accepted-but-ignored. Safe to leave in old configs. |
| `detach_key` | string | `"d"` | **Deprecated, no-op.** Same story. |

The detach chord behaviour these used to control is no longer reachable: the deck is always visible and `F1` swaps focus between agent and deck instead.

## `[[agent]]`

One block per agent. The order in the file is the order shown in the overview list and the number-key binding (1 = first, 2 = second, …).

| Field | Type | Required | Default | Meaning |
| --- | --- | --- | --- | --- |
| `id` | string | yes | — | Stable identifier. Used in logs, the spawned thread name, and as the display name fallback. Should be unique. |
| `name` | string | no | falls back to `id` | Human-readable label shown in the overview list. Truncated to 14 chars on display. |
| `provider` | enum | yes | — | One of `claude`, `codex`, `gemini`, `other`. Currently only used as a display tag; future provider-specific hooks may key off it. |
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
