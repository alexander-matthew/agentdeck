# Architecture

A walkthrough of how agentdeck is put together internally. Aimed at contributors and the curious. Code references are `file:line` where useful.

## High level

```
                  ┌──────────────────────────────────┐
                  │             main loop            │
                  │  (single-threaded event pump)    │
                  └──────────────────────────────────┘
                     ▲   ▲                  │
       agent_rx      │   │                  │  draws every ~50 ms
       (crossbeam,   │   │                  ▼
        unbounded)   │   │             ┌───────────────┐
                     │   │             │   ratatui     │
                     │   │             │  CrosstermBE  │
       ┌─────────────┤   │             │ owns screen   │
       │             │   │             │  end-to-end   │
  ┌────┴────┐   ┌────┴───┐             └───────────────┘
  │ reader  │   │ reader │   per-agent       ▲
  │ thread  │…  │ thread │   threads         │
  └────┬────┘   └────┬───┘                   │ key events,
       │             │                       │ resize, mouse
  ┌────▼────┐   ┌────▼───┐                   │
  │ PTY     │   │ PTY    │                   │
  │ master  │   │ master │                   │
  └────┬────┘   └────┬───┘            ┌──────┴───────┐
       │             │                │ crossterm    │
   slave fd       slave fd            │ event::poll  │
       │             │                └──────────────┘
       ▼             ▼                       ▲
   child           child                     │
   (claude)        (codex)                   │
                                             │
                            ┌────────────────┘
                            │  raw stdin in raw mode
                            │  (managed by crossterm)
                            ▼
                       real terminal
```

The whole orchestration is **single-threaded** at the decision-making level. The only threads are per-agent PTY readers — they convert blocking `read()` on each master fd into `AgentEvent::Output { rid, bytes }` messages that the main loop drains every tick.

There is no separate stdin reader thread anymore. The main loop pulls everything through `crossterm::event::poll`, including the bytes we forward to the focused agent.

## Module map

| File | Role |
| --- | --- |
| `src/main.rs` | CLI parsing (`clap`), tracing setup, config resolution, dispatch to `app::run`. |
| `src/config.rs` | `Config`, `Settings`, `AgentConfig`, `Provider`, and the `usage_commands` map. Load-or-init logic, path expansion. |
| `src/agent.rs` | `Agent` struct, PTY spawn, reader thread, vt100 parser, activity timestamps, exit polling. |
| `src/state.rs` | `LiveState` enum and `detect()` function: combines activity windows with provider-specific terminal-output heuristics to label what an agent is doing. |
| `src/keymap.rs` | Centralized key handling: mapping UI actions for the deck AND serializing agent input back to PTY bytes. |
| `src/usage.rs` | Centralized usage dashboard: `UsageState`, `UsageEvent`, and the per-provider `spawn_refresh()` that runs shell commands in background threads with a timeout and an output cap. |
| `src/ui.rs` | All rendering: header bar, sidebar (deck) with status badges, single-agent pane, multi-pane grid layout, usage dashboard cards, add/rename modals. `ViewMode` enum lives here. |
| `src/app.rs` | The core `App` struct and event loop, view-mode + focus management, smart-focus auto-switching, usage poller orchestration, add/rename modal state. |

## Data flow

### Agent → screen

1. The child process (e.g. `claude`) writes bytes to its slave PTY fd.
2. agentdeck's **per-agent reader thread** reads up to 8 KiB at a time from the master end and sends `AgentEvent::Output { rid, bytes }` over a single `unbounded` crossbeam channel shared by all agents.
3. The main loop drains the channel non-blocking on every tick. For each event:
   - Bytes are fed into that agent's `vt100::Parser` (so its `Screen` is always up to date).
   - Activity timestamps and the rolling 500 ms byte counter on the `Agent` are updated.
4. On the next draw, `ui::render_agent_cell` reads each visible agent's `vt100::Screen` and converts each cell into a styled ratatui `Span` (fg/bg color, bold/italic/underline/inverse). The input-target cell's cursor is given to ratatui via `set_cursor_position`. In single-pane mode there's one such cell; in grid mode `render_agent_grid` lays out `grid_rows × grid_cols` cells and calls `render_agent_cell` for each.

### Terminal → agent

1. `crossterm::event::poll` returns parsed `Event`s (key, resize, mouse).
2. `KeyEvent`s when focus is on the agent are sent through `keymap::key_event_to_bytes`, which serializes them back to the byte sequences the inner CLI expects: e.g. `KeyCode::Up` → `\x1b[A`, `Ctrl-C` → `\x03`, `Alt-x` → `\x1b x`, modified arrows → `\x1b[1;<mods><letter>`.
3. The resulting bytes are written to the focused agent's PTY master via `Agent::write`.
4. When focus is on the deck, key events drive deck actions (`j`/`k`, `1-9`, `a`, `x`, `q`) via `keymap::map_deck_key` and never reach an agent.

The single key reserved at the agentdeck layer is `Ctrl-Space` (configurable via `[settings] toggle_key`) — it is consumed before either branch above runs, and toggles `Focus::Deck` ↔ `Focus::Agent`. None of the supported agent CLIs bind Ctrl-Space, and no OS or window manager grabs it first, which is why it's the default.

## Focus model

```
                      ┌─────────────────┐
                      │  Focus::Agent   │   ← default
                      │  (typing goes   │
                      │   to selected   │
                      │   agent)        │
                      └────────┬────────┘
                               │  Ctrl-Space
                               ▼
                      ┌─────────────────┐
                      │  Focus::Deck    │
                      │  (j/k/1-9/a/x/q │
                      │   operate on    │
                      │   the sidebar)  │
                      └────────┬────────┘
                               │  Enter / digit / Ctrl-Space
                               ▼
                      ┌─────────────────┐
                      │  Focus::Agent   │
                      └─────────────────┘
```

Modal overlays (`Adding` and `Renaming` state) take precedence over both focus values — when present, every key feeds that overlay's input box until `Enter` (commit) or `Esc` (cancel).

The usage dashboard (`u`) is a *third* render path for the right pane: it doesn't change `Focus`, but while `showing_usage` is true the key handler dispatches to `handle_usage_key` (which understands `r` / `u` / `Esc`) instead of forwarding to the focused agent.

## View mode

The right-hand pane area can be in one of three render modes, picked in this priority order each frame:

1. **Usage dashboard** (`showing_usage == true`, toggled by `u`). Renders `UsageState` as a stack of provider cards.
2. **Single** (`view_mode == Single`, default). Renders the selected agent full-pane.
3. **Grid** (`view_mode == Grid`, toggled by `g`). Renders the page of agents containing the selected one in a `grid_rows × grid_cols` mosaic. The page is `selected / (grid_rows * grid_cols)`, so navigation always keeps the selected agent visible.

Toggling `view_mode` or resizing the terminal recomputes the per-agent PTY size via `agent_pane_size(view, grid, term_cols, term_rows)` and calls `Agent::resize` on every agent, so child TUIs redraw to fit their cell.

## Usage poller

`src/usage.rs` exposes `spawn_refresh(provider, command, tx)`. The main loop calls it from `App::refresh_usage_all` either on the configured cadence (`usage_refresh_secs`, clamped to ≥ 5 s) or when the user presses `r` inside the dashboard. Each call:

1. Sends a `UsageEvent::Started { provider }` so the UI can render a "refreshing…" title.
2. Spawns a short-lived thread that runs `sh -c <command>` with stdout/stderr piped, polling `try_wait` until exit or a 20 s timeout. stdout is captured up to 64 KB.
3. Sends a `UsageEvent::Result { provider, output, error, at }` back to the main loop, which folds it into `UsageState`.

Failures (non-zero exit, timeout, spawn error) attach an `error` message and don't overwrite the previous good output, so a transient breakage doesn't blank the card.

## Threading rules

- **Per-agent reader thread**: owns its `BoxedReader`, never touches anything else. Lives until EOF or send error.
- **Main thread**: owns all `Agent` instances, the agent_rx channel, and the terminal handle. Performs all writes to PTYs, all drawing, all event handling. Never blocks on PTY I/O.

Because all PTY writes happen on the main thread, there is no need for `Mutex` around any `Agent` field — the struct is `!Sync` and never escapes the main thread.

## Why we re-render the agent through ratatui

You might wonder why we don't just hand the terminal to the focused agent as raw bytes (which is what the old "attach" mode did). Two reasons:

1. **The deck has to stay visible.** A pure passthrough would let the agent's full-screen TUI repaint the entire terminal, including the area we want to keep reserved for the sidebar. We'd have to choose between "deck visible" and "agent renders natively" — and our users want the deck.
2. **One frame, one source of truth.** With the vt100 parser sitting between the agent and the screen, the same data drives both the status badge (`state::detect`) and the displayed grid. There's no chance of the badge showing one thing while the user's view shows something else.

The cost is fidelity: vt100 implements VT100/xterm sequences but not exotic protocols (sixels, Kitty graphics, ITerm2 image protocol, partial mouse-tracking flavours). For the agent CLIs we care about today, that's not a real loss.

## Resize handling

A single `Event::Resize(cols, rows)` recomputes `agent_pane_size` and calls `Agent::resize` on every agent, which:

- calls `master.resize(PtySize)` so the slave fd's `TIOCGWINSZ` reflects the new size (modern TUIs redraw on SIGWINCH),
- calls `parser.set_size` so our in-memory grid matches.

We only resize when the *pane* size actually changes, not on every redraw.

## Live-state detection

`src/state.rs` produces a `LiveState` per agent each frame. The signal hierarchy:

1. **Process exited?** → `Exited(code)`.
2. **Spawned <800 ms ago?** → `Starting`.
3. **Recent activity** (bytes in the last 500 ms): we either return `Working`, or — if the bottom third of the screen contains a known spinner glyph (`⠋⠙⠹…`, `◐◓…`) — `Thinking`.
4. **Quiet ≥ 4 s** and provider-specific awaiting-input pattern matched → `Waiting`. The pattern check lives in `provider_awaiting_input()` and is the one place that knows about Claude Code's `│ >` input frame, Codex's `▌` cursor, Gemini's `>` line, etc.
5. **Quiet ≥ 45 s** with no prompt match → `Stuck`.
6. Otherwise → `Idle`.

These thresholds are tuned for "user can read the badge change as it happens" rather than instant reaction; bumping them too low makes badges flicker between Working and Idle mid-stream.

When upstream CLIs redesign their UI, the provider-specific helper for that CLI is the only place that needs updating. Falling back to the `Idle` badge if a heuristic stops matching is intentional — better a vague but truthful badge than a confidently wrong one.

## Logging

`tracing` is configured in `main.rs` to write to `~/.local/state/agentdeck/agentdeck.log` with `with_ansi(false)`. The TUI never logs to stdout/stderr — that would corrupt the screen. Log level is controlled by `AGENTDECK_LOG`.
