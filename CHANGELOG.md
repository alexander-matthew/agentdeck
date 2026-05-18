# Changelog

All notable changes to agentdeck will be documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Initial documentation set: `CONTRIBUTING.md`, `SECURITY.md`, `CHANGELOG.md`, `docs/configuration.md`, `docs/providers.md`, `docs/architecture.md`, PR template.
- Provider-grouped overview list with non-selectable section headings (`claude`, `codex`, `gemini`, `other`). Navigation skips headings; number-key bindings index selectable rows top-to-bottom.
- Live status badges per agent: `starting`, `working`, `thinking`, `idle`, `waiting`, `stuck`, `exited`. Combines activity timestamps with provider-specific terminal-output heuristics (`src/state.rs`).
- Runtime add (`a` or `+`) and remove (`x`) of agents. `a` opens a small modal prefilled with the highlighted agent's cwd; `Enter` spawns. Ephemeral — not persisted to config.
- Stable per-process `RuntimeId` for agent event routing, so events survive list mutation.
- README install section now documents `cargo install --path .` ergonomics and PATH setup.

### Changed
- License simplified from `MIT OR Apache-2.0` dual to MIT-only. The repo had no external contributions yet, so no relicensing of third-party work was needed.
- `AgentEvent` carries `rid: RuntimeId` instead of `agent_idx: usize` — breaking change for any out-of-tree consumers (there are none).
- Agent list pane widened to 42 cols to fit status badges.

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
