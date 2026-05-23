//! PTY-backed agent runtime.
//!
//! Each [`Agent`] owns a provider CLI running in its own pseudo-terminal.
//! A dedicated reader thread owns the PTY master read half: it pumps bytes
//! off the PTY and forwards them as [`AgentEvent::Output`] on a shared
//! crossbeam channel back to the main loop. The `vt100::Parser` lives on
//! the `Agent` itself and is owned and mutated exclusively by the main
//! loop, which calls `parser.process(&bytes)` as it drains output events
//! (see `src/app.rs`). The parser is never touched from the reader thread,
//! so the UI layer can read it via immutable reference without locking.
//!
//! [`RuntimeId`] is a process-stable u64 minted by the app, distinct from
//! the user-facing [`AgentConfig`] `id` (which is a config-file string and
//! may be reused, renamed, or shared across config reloads). Channel keys
//! and event routing use `RuntimeId` so events stay correct across
//! reordering, removal, and respawn of agents.

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::time::Instant;

use crate::config::{AgentConfig, Provider};

/// Stable per-process identifier minted by the app, separate from the user-facing
/// config `id`. Used as the channel key so events survive agent reordering/removal.
pub type RuntimeId = u64;

/// One running provider CLI inside its own PTY.
pub struct Agent {
    /// Stable runtime id minted by the app — unique for the lifetime of the process.
    pub rid: RuntimeId,
    pub name: String,
    pub provider: Provider,
    pub status: Status,

    pub parser: vt100::Parser,
    pub scroll_offset: u16,
    /// Source config kept around so we can clone-spawn new instances under the
    /// same provider without going back to disk.
    pub template: AgentConfig,
    pub cwd_label: Option<String>,

    pub spawned_at: Instant,
    pub last_output_at: Instant,
    /// Rolling activity window: bytes received since `recent_window_start`.
    pub recent_bytes: u64,
    pub recent_window_start: Instant,

    pub master: Box<dyn MasterPty + Send>,
    pub writer: Box<dyn Write + Send>,
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
    #[allow(dead_code)] // tracked for future resize diff logic
    pub size: PtySize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // SpawnFailed reserved for future per-agent placeholder rows
pub enum Status {
    Running,
    Exited(i32),
    SpawnFailed,
}

/// A configured agent that failed to spawn at startup. Surfaced to the user
/// via a one-shot modal so a typo'd `command =` in config.toml doesn't
/// silently drop the agent — the underlying error otherwise only reaches
/// the tracing log at `~/.local/state/agentdeck/agentdeck.log`.
#[derive(Debug, Clone)]
pub struct SpawnFailure {
    pub id: String,
    pub provider: Provider,
    pub error: String,
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Status::Running => "running".into(),
            Status::Exited(code) => format!("exited ({code})"),
            Status::SpawnFailed => "spawn failed".into(),
        }
    }
}

/// Message types sent from per-agent reader threads to the main loop.
#[derive(Debug)]
pub enum AgentEvent {
    /// Bytes read from the agent's PTY master.
    Output { rid: RuntimeId, bytes: Vec<u8> },
    /// Reader hit EOF (process likely exited).
    ReaderClosed { rid: RuntimeId },
}

impl Agent {
    pub fn spawn(
        cfg: &AgentConfig,
        rid: RuntimeId,
        size: PtySize,
        tx: Sender<AgentEvent>,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size).context("openpty for new agent")?;

        let mut cmd = CommandBuilder::new(&cfg.command);
        for a in &cfg.args {
            cmd.arg(a);
        }
        if let Some(cwd) = cfg.resolved_cwd() {
            cmd.cwd(cwd);
        }
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        // Hint to programs that a real terminal is on the other end.
        cmd.env(
            "TERM",
            std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
        );

        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("spawn `{}`", cfg.command))?;
        drop(pair.slave); // we don't need the slave fd in this process anymore

        let mut reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let writer = pair.master.take_writer().context("take pty writer")?;

        // Reader thread: forward chunks to the main loop.
        std::thread::Builder::new()
            .name(format!("agent-reader-{}-{}", cfg.id, rid))
            .spawn(move || {
                let mut buf = vec![0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            let _ = tx.send(AgentEvent::ReaderClosed { rid });
                            break;
                        }
                        Ok(n) => {
                            if tx
                                .send(AgentEvent::Output {
                                    rid,
                                    bytes: buf[..n].to_vec(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = tx.send(AgentEvent::ReaderClosed { rid });
                            break;
                        }
                    }
                }
            })
            .context("spawn reader thread")?;

        let now = Instant::now();
        Ok(Self {
            rid,
            name: cfg.display_name().to_string(),
            provider: cfg.provider,
            status: Status::Running,
            parser: vt100::Parser::new(size.rows, size.cols, 1000),
            scroll_offset: 0,
            template: cfg.clone(),
            cwd_label: cfg.cwd.clone(),
            spawned_at: now,
            last_output_at: now,
            recent_bytes: 0,
            recent_window_start: now,
            master: pair.master,
            writer,
            child,
            size,
        })
    }

    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        let now = Instant::now();
        self.last_output_at = now;
        // Roll the 500ms activity window forward; we use byte count in that window
        // to distinguish active streaming from idle prompts in `state::detect`.
        if now.duration_since(self.recent_window_start) > std::time::Duration::from_millis(500) {
            self.recent_window_start = now;
            self.recent_bytes = 0;
        }
        self.recent_bytes = self.recent_bytes.saturating_add(bytes.len() as u64);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        if self.master.resize(size).is_ok() {
            self.size = size;
            self.parser.set_size(rows, cols);
        }
    }

    pub fn poll_exit(&mut self) {
        if matches!(self.status, Status::Running)
            && let Ok(Some(status)) = self.child.try_wait()
        {
            let code = status.exit_code() as i32;
            self.status = Status::Exited(code);
        }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    pub fn scroll_up(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(n).min(1000);
    }

    pub fn scroll_down(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use super::*;
    use crate::config::AgentConfig;
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    /// Build an `Agent` instance whose PTY is fully mocked. Suitable for state
    /// machine and rendering tests that never need to read or write the child.
    pub fn mock_agent(provider: Provider, name: &str) -> Agent {
        let cfg = AgentConfig {
            id: name.to_string(),
            name: Some(name.to_string()),
            provider,
            command: "true".into(),
            args: vec![],
            cwd: None,
            env: BTreeMap::new(),
            manual: false,
        };
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let now = Instant::now();
        // Subtract a comfortable margin so detectors past STARTUP_GRACE behave deterministically.
        let past = now - Duration::from_secs(10);
        Agent {
            rid: 1,
            name: name.to_string(),
            provider,
            status: Status::Running,
            parser: vt100::Parser::new(size.rows, size.cols, 1000),
            scroll_offset: 0,
            template: cfg,
            cwd_label: None,
            spawned_at: past,
            last_output_at: past,
            recent_bytes: 0,
            recent_window_start: past,
            master: Box::new(MockMaster),
            writer: Box::new(Vec::new()),
            child: Box::new(MockChild),
            size,
        }
    }

    pub struct MockMaster;
    impl portable_pty::MasterPty for MockMaster {
        fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> {
            Ok(())
        }
        fn get_size(&self) -> Result<PtySize, anyhow::Error> {
            Ok(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
        }
        fn try_clone_reader(&self) -> Result<Box<dyn std::io::Read + Send>, anyhow::Error> {
            Ok(Box::new(std::io::empty()))
        }
        fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
            Ok(Box::new(std::io::sink()))
        }
        fn process_group_leader(&self) -> Option<i32> {
            None
        }
        fn as_raw_fd(&self) -> Option<std::os::unix::io::RawFd> {
            None
        }
    }

    #[derive(Debug)]
    pub struct MockChild;
    impl portable_pty::ChildKiller for MockChild {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(MockKiller)
        }
    }
    impl portable_pty::Child for MockChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(None)
        }
        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            Ok(portable_pty::ExitStatus::with_exit_code(0))
        }
        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    #[derive(Debug)]
    pub struct MockKiller;
    impl portable_pty::ChildKiller for MockKiller {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(MockKiller)
        }
    }
}
