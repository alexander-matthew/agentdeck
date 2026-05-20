//! Centralized usage dashboard.
//!
//! Each provider can have a shell command in `[usage_commands]` (config). We
//! run those commands on a fixed cadence in a background thread and route the
//! captured stdout/stderr back to the main loop via [`UsageEvent`]. The main
//! loop merges results into [`UsageState`], which the UI renders as a card per
//! provider.
//!
//! Commands run under `sh -c`, with a per-run timeout so a hung subprocess
//! can't starve the refresh thread.

use crossbeam_channel::Sender;
use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// One provider's most recent usage result.
#[derive(Debug, Clone)]
pub struct UsageEntry {
    pub provider: String,
    pub command: String,
    pub last_run_at: Option<Instant>,
    pub last_output: Option<String>,
    pub last_error: Option<String>,
    /// True while a refresh is in flight for this provider.
    pub refreshing: bool,
}

impl UsageEntry {
    fn new(provider: String, command: String) -> Self {
        Self {
            provider,
            command,
            last_run_at: None,
            last_output: None,
            last_error: None,
            refreshing: false,
        }
    }
}

/// Aggregate state for the dashboard, keyed by provider tag.
#[derive(Debug, Clone, Default)]
pub struct UsageState {
    pub entries: BTreeMap<String, UsageEntry>,
}

impl UsageState {
    pub fn from_commands(cmds: &BTreeMap<String, String>) -> Self {
        let mut entries = BTreeMap::new();
        for (provider, command) in cmds {
            let trimmed = command.trim();
            if trimmed.is_empty() {
                continue;
            }
            entries.insert(
                provider.clone(),
                UsageEntry::new(provider.clone(), trimmed.to_string()),
            );
        }
        Self { entries }
    }

    pub fn apply(&mut self, event: UsageEvent) {
        match event {
            UsageEvent::Started { provider } => {
                if let Some(e) = self.entries.get_mut(&provider) {
                    e.refreshing = true;
                }
            }
            UsageEvent::Result {
                provider,
                output,
                error,
                at,
            } => {
                if let Some(e) = self.entries.get_mut(&provider) {
                    e.refreshing = false;
                    e.last_run_at = Some(at);
                    if let Some(err) = error {
                        e.last_error = Some(err);
                    } else {
                        e.last_error = None;
                        e.last_output = Some(output);
                    }
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug)]
pub enum UsageEvent {
    Started {
        provider: String,
    },
    Result {
        provider: String,
        output: String,
        error: Option<String>,
        at: Instant,
    },
}

/// Hard cap on how long a single usage command can run before we give up.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
/// Cap on captured output. Most tools fit in a few KB; anything bigger gets
/// truncated so a runaway script can't blow up memory.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Spawn a one-shot refresh for `provider`. Sends a `Started` immediately and
/// a `Result` when the command finishes (or times out).
pub fn spawn_refresh(provider: String, command: String, tx: Sender<UsageEvent>) {
    let _ = tx.send(UsageEvent::Started {
        provider: provider.clone(),
    });
    std::thread::Builder::new()
        .name(format!("usage-refresh-{provider}"))
        .spawn(move || {
            let (output, error) = run_command(&command);
            let _ = tx.send(UsageEvent::Result {
                provider,
                output,
                error,
                at: Instant::now(),
            });
        })
        .ok();
}

fn run_command(command: &str) -> (String, Option<String>) {
    let child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return (String::new(), Some(format!("spawn failed: {e}"))),
    };

    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                if let Some(mut s) = child.stdout.take() {
                    read_truncated(&mut s, &mut stdout);
                }
                let mut stderr = String::new();
                if let Some(mut s) = child.stderr.take() {
                    read_truncated(&mut s, &mut stderr);
                }
                if status.success() {
                    return (stdout, None);
                }
                let msg = if stderr.trim().is_empty() {
                    format!("exit {}", status.code().unwrap_or(-1))
                } else {
                    format!("exit {}: {}", status.code().unwrap_or(-1), stderr.trim())
                };
                return (stdout, Some(msg));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (
                        String::new(),
                        Some(format!("timed out after {COMMAND_TIMEOUT:?}")),
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return (String::new(), Some(format!("wait failed: {e}"))),
        }
    }
}

fn read_truncated<R: Read>(reader: &mut R, dest: &mut String) {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let remaining = MAX_OUTPUT_BYTES.saturating_sub(buf.len());
                if remaining == 0 {
                    break;
                }
                let take = n.min(remaining);
                buf.extend_from_slice(&chunk[..take]);
                if buf.len() >= MAX_OUTPUT_BYTES {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    dest.push_str(&String::from_utf8_lossy(&buf));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn from_commands_skips_empty_and_whitespace_values() {
        let state = UsageState::from_commands(&cmds(&[
            ("claude", "  ccusage  "),
            ("codex", ""),
            ("gemini", "   "),
        ]));
        assert_eq!(state.entries.len(), 1);
        let entry = state.entries.get("claude").expect("claude entry exists");
        assert_eq!(entry.provider, "claude");
        assert_eq!(entry.command, "ccusage");
        assert!(!state.entries.contains_key("codex"));
        assert!(!state.entries.contains_key("gemini"));
    }

    #[test]
    fn is_empty_reflects_entry_presence() {
        assert!(UsageState::default().is_empty());
        let state = UsageState::from_commands(&cmds(&[("claude", "ccusage")]));
        assert!(!state.is_empty());
    }

    #[test]
    fn started_event_sets_refreshing_only_for_known_provider() {
        let mut state = UsageState::from_commands(&cmds(&[("claude", "ccusage")]));
        state.apply(UsageEvent::Started {
            provider: "nonexistent".into(),
        });
        assert_eq!(state.entries.len(), 1);
        assert!(!state.entries["claude"].refreshing);

        state.apply(UsageEvent::Started {
            provider: "claude".into(),
        });
        assert!(state.entries["claude"].refreshing);
    }

    #[test]
    fn result_success_clears_error_and_populates_output() {
        let mut state = UsageState::from_commands(&cmds(&[("claude", "ccusage")]));
        // Seed prior error + refreshing state to verify reset behavior.
        {
            let entry = state.entries.get_mut("claude").unwrap();
            entry.last_error = Some("previous failure".into());
            entry.refreshing = true;
        }
        let now = Instant::now();
        state.apply(UsageEvent::Result {
            provider: "claude".into(),
            output: "tokens: 42".into(),
            error: None,
            at: now,
        });
        let entry = &state.entries["claude"];
        assert!(!entry.refreshing);
        assert_eq!(entry.last_output.as_deref(), Some("tokens: 42"));
        assert_eq!(entry.last_error, None);
        assert_eq!(entry.last_run_at, Some(now));
    }

    #[test]
    fn result_error_preserves_prior_output_and_stores_error() {
        let mut state = UsageState::from_commands(&cmds(&[("claude", "ccusage")]));
        state.apply(UsageEvent::Result {
            provider: "claude".into(),
            output: "tokens: 42".into(),
            error: None,
            at: Instant::now(),
        });
        state.apply(UsageEvent::Result {
            provider: "claude".into(),
            output: String::new(),
            error: Some("boom".into()),
            at: Instant::now(),
        });
        let entry = &state.entries["claude"];
        assert_eq!(entry.last_output.as_deref(), Some("tokens: 42"));
        assert_eq!(entry.last_error.as_deref(), Some("boom"));
        assert!(!entry.refreshing);
    }

    #[test]
    fn result_for_unknown_provider_is_a_noop() {
        let mut state = UsageState::from_commands(&cmds(&[("claude", "ccusage")]));
        state.apply(UsageEvent::Result {
            provider: "ghost".into(),
            output: "x".into(),
            error: None,
            at: Instant::now(),
        });
        assert_eq!(state.entries.len(), 1);
        let entry = &state.entries["claude"];
        assert!(entry.last_output.is_none());
        assert!(entry.last_run_at.is_none());
    }
}
