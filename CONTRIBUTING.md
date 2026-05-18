# Contributing to agentdeck

Thanks for the interest! This is a small, opinionated tool; PRs that fit its design goals are very welcome.

## Ground rules

- **`main` is protected.** No direct pushes — open a PR.
- **Keep it minimal.** agentdeck is intentionally a thin orchestrator. It does not, and will not, talk to provider APIs directly, store transcripts, summarise sessions, or otherwise act like an agent itself. Features that violate that boundary will not be merged.
- **Cross-platform where cheap.** Linux is the primary target; macOS support is "should work via `portable-pty`" but untested. Windows is not in scope.
- **No new heavy deps without a good reason.** The current dep set is deliberate.

## Local development

You'll need:

- Rust 1.85+ (`apt install rustc cargo` on Ubuntu 26.04 is fine; otherwise [rustup](https://rustup.rs/))
- `rustfmt` and `clippy` (`apt install rustfmt rust-clippy` or `rustup component add`)
- A real TTY to run the binary (it won't function under `cargo run` redirected to a file)

```sh
git clone https://github.com/alexander-matthew/agentdeck
cd agentdeck
cargo build
./target/debug/agentdeck --print-config   # writes default ~/.config/agentdeck/config.toml
./target/debug/agentdeck                  # launch the TUI
```

Tail logs in another terminal while you work:

```sh
tail -f ~/.local/state/agentdeck/agentdeck.log
AGENTDECK_LOG=debug ./target/debug/agentdeck
```

## CI gate

Every PR runs:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo build --locked
cargo test --locked
```

Run these locally before pushing. The `-D warnings` is non-negotiable; if clippy is wrong, `#[allow(...)]` the specific lint at the call site with a one-line comment explaining why.

## Submitting changes

1. Branch from `main` with a descriptive name (`feat/multi-pane`, `fix/resize-deadlock`, `docs/provider-notes`).
2. Make focused commits. One logical change per commit.
3. Open a PR. The PR template prompts for the essentials.
4. CI must pass. Merge is squash by default to keep `main` linear.

### Commit message style

Short subject line, imperative mood. Body explains the *why* if it isn't obvious from the diff.

```
Forward host SIGWINCH to attached PTY

Previously the attached child saw the original size for its whole
lifetime, which broke claude-code's layout after a window resize.
```

## Adding a new provider

agentdeck doesn't hard-code provider behaviour beyond a display tag. To add one:

1. Add a variant to `Provider` in `src/config.rs` and update `Provider::tag()`.
2. Document any provider-specific quirks (subscription concurrency limits, env vars, flags) in `docs/providers.md`.
3. Add a sample `[[agent]]` block to the README config example.

No other changes are usually needed — the new provider's CLI Just Runs in a PTY.

## Reporting bugs and security issues

- **Bugs:** open a GitHub issue with the output of `agentdeck --print-config`, the relevant chunk of `~/.local/state/agentdeck/agentdeck.log`, and the terminal you're running in (`echo $TERM`, terminal app, OS).
- **Security:** see [SECURITY.md](SECURITY.md). Don't open a public issue for anything that looks like it could be exploited.
