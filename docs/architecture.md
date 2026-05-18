# Architecture

A walkthrough of how agentdeck is put together internally. Aimed at contributors and the curious. Code references are file:line where useful.

## High level

```
                       ┌──────────────────────────────────┐
                       │             main loop            │
                       │  (single-threaded event pump)    │
                       └──────────────────────────────────┘
                          ▲   ▲                  │     │
       agent_rx           │   │                  │     │ overview draws
       (crossbeam,        │   │                  │     ▼
        unbounded)        │   │              ┌───────────────┐
                          │   │              │   ratatui     │
                          │   │              │  CrosstermBE  │
       ┌──────────────────┤   │              └───────────────┘
       │                  │   │                  ▲      ▲
  ┌────┴────┐       ┌─────┴───┐                  │      │  attached mode
  │ reader  │  …    │ reader  │   per-agent      │      │  bypasses ratatui:
  │ thread  │       │ thread  │   threads        │      │  raw bytes →
  └────┬────┘       └────┬────┘                  │      │  stdout
       │                 │                       │      │
  ┌────▼────┐       ┌────▼────┐                  │      │
  │ PTY     │       │ PTY     │  one PTY per     │      │
  │ master  │       │ master  │  agent           │      │
  └────┬────┘       └────┬────┘                  │      │
       │                 │                       │      │
   slave fd ─► child  slave fd ─► child          │      │
   (claude)            (codex)                   │      │
                                                 │      │
              ┌──────────────────┐               │      │
              │  stdin reader    │ ─ input_rx ──►│      │
              │  (only while     │ (crossbeam,   │      │
              │   attached)      │  bounded 256) │      │
              └──────────────────┘               │      │
                       ▲                         │      │
                       │ raw bytes               │      │
                  /dev/tty (stdin in raw mode)   │      │
                                                 │      │
              ┌──────────────────────────────────┘      │
              │   user's real terminal                  │
              └─────────────────────────────────────────┘
```

The whole orchestration is **single-threaded** at the decision-making level. Threads are only used to convert blocking I/O (PTY reads, stdin reads while attached) into channel messages.

## Module map

| File | Role |
| --- | --- |
| `src/main.rs` | CLI parsing (`clap`), tracing setup, config resolution, dispatch to `app::run`. |
| `src/config.rs` | `Config`, `Settings`, `AgentConfig`, `Provider`. Load-or-init logic, path expansion. |
| `src/agent.rs` | `Agent` struct, PTY spawn, reader thread, vt100 parser, status polling. |
| `src/ui.rs` | Ratatui overview rendering (header, agent list, preview pane). Attached mode does NOT render here — it writes bytes straight to stdout. |
| `src/app.rs` | The event loop, the mode state machine, attach/detach orchestration, stdin reader thread for attached mode. |

## Data flow

### Agent → terminal

1. A child process (e.g. `claude`) writes bytes to its slave PTY fd.
2. agentdeck's per-agent **reader thread** (`agent.rs`) reads up to 8 KiB at a time from the master end and sends an `AgentEvent::Output { agent_idx, bytes }` over a single `unbounded` channel shared by all agents.
3. The main loop drains this channel non-blocking on every tick.
4. For every event:
   - Bytes are fed into that agent's `vt100::Parser` so the preview pane has the latest screen state.
   - **If currently attached to this agent**, the same bytes are also written straight to `stdout`. This is what gives you native rendering — no re-parsing, no re-emission, no fidelity loss.

### Terminal → agent

There are two input pipelines depending on mode.

**Overview mode:** `crossterm::event::poll(50ms)` + `event::read()`. We translate key events into UI actions (move cursor, attach, kill, quit) and update local state. No PTY traffic.

**Attached mode:** crossterm is bypassed for input. A dedicated `agentdeck-stdin` thread does blocking reads from `std::io::stdin()` (which is in raw mode, so reads return raw bytes). It runs a small state machine:

```
        prefix-armed?         on byte b:
            no    →    if b == prefix_byte:  set armed
                       else:                 send b
            yes   →    if b == detach_byte:  emit Detach, exit
                       if b == prefix_byte:  send literal prefix_byte
                       otherwise:            send prefix_byte then b
                       (always clear armed after)
```

Output bytes are batched into an `InputEvt::Bytes(Vec<u8>)` and pushed through a bounded crossbeam channel to the main loop, which writes them to the focused agent's PTY master.

The state machine is at `app.rs` in `stdin_reader`.

## Mode state machine

```
                  ┌─────────────────────┐
                  │       Overview      │
                  └──────────┬──────────┘
                             │ Enter / digit
                             ▼
                  ┌─────────────────────┐
                  │  attach(idx) side-  │
                  │  effects:           │
                  │  • LeaveAlternate   │
                  │  • Show cursor      │
                  │  • Clear+Move(0,0)  │
                  │  • write current    │
                  │    screen snapshot  │
                  │  • spawn stdin thr  │
                  └──────────┬──────────┘
                             │
                             ▼
                  ┌─────────────────────┐
                  │  Attached { idx }   │
                  └──────────┬──────────┘
                             │ Ctrl-A d
                             ▼
                  ┌─────────────────────┐
                  │  detach() side-     │
                  │  effects:           │
                  │  • drop input chan  │
                  │  • EnterAlternate   │
                  │  • Hide cursor      │
                  │  • terminal.clear() │
                  └──────────┬──────────┘
                             ▼
                  ┌─────────────────────┐
                  │       Overview      │
                  └─────────────────────┘
```

The key trick: when entering attached mode, we paint `parser.screen().contents_formatted()` to the real terminal. Those bytes deterministically reconstruct the agent's current screen state — colours, cursor position, alt-screen, the whole grid. So you don't see a blank screen waiting for the agent's next redraw.

When detaching, ratatui's terminal buffer is invalid (we wrote to stdout behind its back), so we call `terminal.clear()` to force a full repaint on the next tick.

## Threading rules

- **Per-agent reader thread**: owns its `BoxedReader`, never touches anything else. Lives until EOF or send error.
- **Stdin reader thread**: created at attach, exits voluntarily on detach (or on EOF/read error). Only one of these runs at a time.
- **Main thread**: owns all `Agent` instances, both channels, and the terminal handle. Performs all writes to PTYs and all writes to stdout. Never blocks on PTY I/O.

Because all PTY *writes* and all stdout writes happen on the main thread, there is no need for `Mutex` around the agent state — the `Agent` struct is `!Sync` and never escapes the main thread.

## vt100 vs raw passthrough — why both

You might wonder why we maintain a `vt100::Parser` per agent if attached mode just dumps bytes straight to the terminal. Two reasons:

1. **Preview pane.** We need a structured view of the agent's screen to render the right side of the overview. Without a parser, we'd be showing a stream of raw bytes including ANSI escapes — unreadable.
2. **Attach snapshot.** When you press Enter, the agent doesn't know to redraw. The parser has been keeping up the whole time, so we can emit `screen().contents_formatted()` to repaint the current state without involving the child process at all.

## Resize handling

- In overview mode: `crossterm::Event::Resize(cols, rows)` triggers `Agent::resize` on every PTY, which both calls `master.resize()` (so the slave sees the new size via TIOCGWINSZ) and `parser.set_size()`.
- In attached mode: we cannot poll crossterm events without stealing stdin bytes. Instead, every ~200 ms the main loop calls `crossterm::terminal::size()` (a syscall, not stdin) and resizes the focused PTY if the size changed.

A future improvement is to install a SIGWINCH handler via `signal-hook` so resizes are zero-latency in both modes.

## Logging

`tracing` is configured in `main.rs` to write to `~/.local/state/agentdeck/agentdeck.log` with `with_ansi(false)`. The TUI never logs to stdout/stderr — that would corrupt the screen. Log level is controlled by `AGENTDECK_LOG`.

## What's intentionally absent

- **No async runtime.** Tokio was considered and rejected. The whole loop fits in `std::thread` + crossbeam channels; an executor wouldn't earn its complexity.
- **No transcript persistence.** Each agent owns its own state. agentdeck adds zero session storage on top.
- **No provider abstraction layer.** The `Provider` enum is a display tag, nothing more. If we ever need provider-specific behaviour (e.g. distinct "is this output a tool call?" hooks), we'll add it; until then, every CLI is treated identically.
- **No multi-pane attached view.** You attach to *one* agent at a time. A future tiling mode is plausible but would require re-rendering all agents through vt100 + ratatui (Approach B in the design notes), which trades fidelity for layout flexibility.
