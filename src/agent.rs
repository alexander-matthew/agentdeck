use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};

use crate::config::{AgentConfig, Provider};

/// One running provider CLI inside its own PTY.
pub struct Agent {
    #[allow(dead_code)] // surfaced via logging only
    pub id: String,
    pub name: String,
    pub provider: Provider,
    pub status: Status,

    pub parser: vt100::Parser,

    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    #[allow(dead_code)] // tracked for future resize diff logic
    size: PtySize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // SpawnFailed reserved for future per-agent placeholder rows
pub enum Status {
    Running,
    Exited(i32),
    SpawnFailed,
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
    Output { agent_idx: usize, bytes: Vec<u8> },
    /// Reader hit EOF (process likely exited).
    ReaderClosed { agent_idx: usize },
}

impl Agent {
    pub fn spawn(
        cfg: &AgentConfig,
        agent_idx: usize,
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
            .name(format!("agent-reader-{}", cfg.id))
            .spawn(move || {
                let mut buf = vec![0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            let _ = tx.send(AgentEvent::ReaderClosed { agent_idx });
                            break;
                        }
                        Ok(n) => {
                            if tx
                                .send(AgentEvent::Output {
                                    agent_idx,
                                    bytes: buf[..n].to_vec(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = tx.send(AgentEvent::ReaderClosed { agent_idx });
                            break;
                        }
                    }
                }
            })
            .context("spawn reader thread")?;

        Ok(Self {
            id: cfg.id.clone(),
            name: cfg.display_name().to_string(),
            provider: cfg.provider,
            status: Status::Running,
            parser: vt100::Parser::new(size.rows, size.cols, 1000),
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

    pub fn current_screen_bytes(&self) -> Vec<u8> {
        // Bytes that, when written to a terminal, recreate the current screen state.
        // Used at attach-time to paint the agent's current frame onto our real terminal.
        self.parser.screen().contents_formatted()
    }
}
