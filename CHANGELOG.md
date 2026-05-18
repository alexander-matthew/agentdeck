# Changelog

All notable changes to agentdeck will be documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Initial documentation set: `CONTRIBUTING.md`, `SECURITY.md`, `CHANGELOG.md`, `docs/configuration.md`, `docs/providers.md`, `docs/architecture.md`, PR template.
- Provider-grouped overview list with non-selectable section headings. Navigation skips headings; number-key bindings index selectable rows top-to-bottom.
- Live status badges per agent (`starting` / `working` / `thinking` / `idle` / `waiting` / `stuck` / `exited`). Combines activity timestamps with provider-specific terminal-output heuristics (`src/state.rs`).
- Runtime add (`a` or `+`) and remove (`x`) of agents via in-TUI modal. Ephemeral — not persisted to config.
- Stable per-process `RuntimeId` for agent event routing, so events survive list mutation.
- **Persistent split view**: sidebar (deck) and focused-agent pane are visible simultaneously. The agent's terminal grid is rendered through agentdeck's own UI (vt100 → ratatui with cell-level fg/bg/bold/italic/underline/inverse styling), with the agent's cursor positioned via ratatui's `set_cursor_position`.
- **`F1`** is the single global key reserved by agentdeck — toggles focus between deck and agent. None of the supported agent CLIs bind F1.
- `src/keymap.rs` serializes crossterm `KeyEvent`s back to PTY-bound byte sequences (chars, Alt-prefixed chars, every Ctrl-letter, arrows with modifiers, F2–F12, navigation cluster).
- README install section documents `cargo install --path .` ergonomics and PATH setup.

### Changed
- License simplified from `MIT OR Apache-2.0` dual to MIT-only.
- `AgentEvent` carries `rid: RuntimeId` instead of `agent_idx: usize`.
- **Replaced the modal attach model with persistent split view.** Pressing Enter / digit on the deck no longer hands the terminal to the agent full-screen; instead it returns focus to that agent inside the existing split. There is no more "detach back to the deck" — the deck never goes away.
- `[settings] prefix_byte` / `detach_key` are accepted-but-ignored. Their old purpose (the detach chord) is gone.

### Removed
- Raw-stdin reader thread and the LeaveAlternateScreen/EnterAlternateScreen handoff dance. ratatui now owns the screen for the whole session.
- `Agent::current_screen_bytes()` (was used by the old attach-snapshot path).

## [0.1.0] — 2026-05-17

### Added
- First public release.
- TUI overview listing all configured agents with live status (running / exited / failed) and a vt100-rendered preview pane for the selected agent.
- One PTY per agent via [`portable-pty`](https://docs.rs/portable-pty/), spawned with the user's `TERM` and per-agent `cwd`/`env`/`args`.
- Attach mode: leave the ratatui alt-screen and pipe raw bytes straight to the host terminal so each agent's native TUI renders without re-parsing. Detach via configurable chord (default Ctrl-A d).
- Doubled-prefix escape (default Ctrl-A Ctrl-A → literal Ctrl-A through to the agent).
- Configurable detach chord via `[settings] prefix_byte`/`detach_key`.
- TOML config at `~/.config/agentdeck/config.toml` with auto-init on first run; default profiles for `claude`, `codex`, and `gemini`.
- `agentdeck --print-config` for inspecting the resolved config.
- File-based tracing at `~/.local/state/agentdeck/agentdeck.log`; level controlled by `AGENTDECK_LOG`.
- CI workflow: `cargo fmt --check`, `cargo clippy -D warnings`, build, test.

### Security
- No network code in agentdeck itself. No provider API access. No transcript persistence. See [SECURITY.md](SECURITY.md) for the full threat model.

[Unreleased]: https://github.com/alexander-matthew/agentdeck/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/alexander-matthew/agentdeck/releases/tag/v0.1.0
