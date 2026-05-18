use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use portable_pty::PtySize;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{Read, Write, stdout};
use std::time::{Duration, Instant};

use crate::agent::{Agent, AgentEvent, Status};
use crate::config::Config;
use crate::ui::draw_overview;

#[derive(Debug)]
enum InputEvt {
    Bytes(Vec<u8>),
    Detach,
    Closed,
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    Overview,
    Attached { idx: usize },
}

pub fn run(cfg: Config) -> Result<()> {
    let mut stdout_handle = stdout();
    enable_raw_mode().context("enable raw mode")?;
    execute!(stdout_handle, EnterAlternateScreen, Hide).context("enter alt screen")?;

    let result = run_inner(cfg, &mut stdout_handle);

    // Always try to restore terminal state, even on error.
    execute!(stdout_handle, LeaveAlternateScreen, Show).ok();
    disable_raw_mode().ok();

    result
}

fn run_inner(cfg: Config, stdout_handle: &mut std::io::Stdout) -> Result<()> {
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).context("new terminal")?;

    let (term_cols, term_rows) = crossterm::terminal::size().context("query terminal size")?;
    let initial_size = PtySize {
        rows: term_rows.saturating_sub(3).max(10),
        cols: term_cols.max(40),
        pixel_width: 0,
        pixel_height: 0,
    };

    let (agent_tx, agent_rx) = unbounded::<AgentEvent>();
    let mut agents: Vec<Agent> = Vec::with_capacity(cfg.agents.len());
    for (idx, ac) in cfg.agents.iter().enumerate() {
        if ac.manual {
            tracing::info!(id = %ac.id, "skipping manual agent");
            continue;
        }
        match Agent::spawn(ac, idx, initial_size, agent_tx.clone()) {
            Ok(a) => agents.push(a),
            Err(e) => {
                tracing::error!(id = %ac.id, error = ?e, "failed to spawn agent");
                // Push a placeholder so indices line up with config order? For MVP, skip.
            }
        }
    }

    if agents.is_empty() {
        // Don't leave the user in an empty TUI with no exit hint — bail out with a message.
        execute!(stdout_handle, LeaveAlternateScreen, Show).ok();
        disable_raw_mode().ok();
        eprintln!(
            "agentdeck: no agents could be spawned. Check your config (run `agentdeck --print-config`)."
        );
        return Ok(());
    }

    let mut selected: usize = 0;
    let mut mode = Mode::Overview;
    let mut current_term_size = (term_cols, term_rows);
    let mut should_quit = false;
    let mut footer = " ↑/↓ select   1-9 attach   r restart   k kill   q quit ".to_string();
    let mut last_size_check = Instant::now();
    let prefix = cfg.settings.prefix_byte;
    let detach_byte = cfg.settings.detach_key as u8;

    // Channel used only while attached.
    let mut input_rx: Option<Receiver<InputEvt>> = None;
    let mut input_thread: Option<std::thread::JoinHandle<()>> = None;

    while !should_quit {
        // Drain agent output. In attached mode, write bytes for the focused agent
        // straight to the host terminal so the native TUI renders unchanged.
        while let Ok(ev) = agent_rx.try_recv() {
            match ev {
                AgentEvent::Output { agent_idx, bytes } => {
                    if let Some(a) = agents.get_mut(agent_idx) {
                        a.feed(&bytes);
                    }
                    if let Mode::Attached { idx } = mode
                        && idx == agent_idx
                    {
                        let _ = stdout_handle.write_all(&bytes);
                        let _ = stdout_handle.flush();
                    }
                }
                AgentEvent::ReaderClosed { agent_idx } => {
                    if let Some(a) = agents.get_mut(agent_idx) {
                        a.poll_exit();
                        if matches!(a.status, Status::Running) {
                            a.status = Status::Exited(0);
                        }
                    }
                }
            }
        }
        for a in agents.iter_mut() {
            a.poll_exit();
        }

        match mode {
            Mode::Overview => {
                terminal.draw(|f| draw_overview(f, &agents, selected, &footer))?;
                if event::poll(Duration::from_millis(50))? {
                    let ev = event::read()?;
                    if let Some(action) = handle_overview_event(ev, agents.len(), &mut selected) {
                        match action {
                            OverviewAction::Quit => should_quit = true,
                            OverviewAction::Attach(idx) => {
                                attach(
                                    stdout_handle,
                                    &mut terminal,
                                    &mut agents,
                                    idx,
                                    prefix,
                                    detach_byte,
                                    &mut input_rx,
                                    &mut input_thread,
                                )?;
                                mode = Mode::Attached { idx };
                                footer = format!(
                                    " attached · Ctrl-{} {} to detach ",
                                    char_for_ctrl(prefix),
                                    cfg.settings.detach_key
                                );
                            }
                            OverviewAction::Kill(idx) => {
                                if let Some(a) = agents.get_mut(idx) {
                                    a.kill();
                                }
                            }
                            OverviewAction::Resize(cols, rows) => {
                                current_term_size = (cols, rows);
                                let pty_rows = rows.saturating_sub(3).max(10);
                                for a in agents.iter_mut() {
                                    a.resize(pty_rows, cols);
                                }
                            }
                        }
                    }
                }
            }
            Mode::Attached { idx } => {
                // Pump input events from the dedicated stdin thread.
                if let Some(rx) = input_rx.as_ref() {
                    while let Ok(evt) = rx.try_recv() {
                        match evt {
                            InputEvt::Bytes(b) => {
                                if let Some(a) = agents.get_mut(idx) {
                                    let _ = a.write(&b);
                                }
                            }
                            InputEvt::Detach | InputEvt::Closed => {
                                detach(
                                    stdout_handle,
                                    &mut terminal,
                                    &mut input_rx,
                                    &mut input_thread,
                                )?;
                                mode = Mode::Overview;
                                footer = " ↑/↓ select   1-9 attach   r restart   k kill   q quit "
                                    .to_string();
                                break;
                            }
                        }
                    }
                }

                // Forward host-terminal resizes to the PTY by polling size
                // (we can't drain crossterm events here without stealing stdin bytes).
                if last_size_check.elapsed() > Duration::from_millis(200) {
                    if let Ok(sz) = crossterm::terminal::size()
                        && sz != current_term_size
                    {
                        current_term_size = sz;
                        if let Some(a) = agents.get_mut(idx) {
                            a.resize(sz.1, sz.0);
                        }
                    }
                    last_size_check = Instant::now();
                }

                // Light sleep so we don't pin a core while the agent is idle.
                std::thread::sleep(Duration::from_millis(8));
            }
        }
    }

    // Clean shutdown: kill all child processes.
    for a in agents.iter_mut() {
        a.kill();
    }
    Ok(())
}

enum OverviewAction {
    Quit,
    Attach(usize),
    Kill(usize),
    Resize(u16, u16),
}

fn handle_overview_event(
    ev: Event,
    n_agents: usize,
    selected: &mut usize,
) -> Option<OverviewAction> {
    match ev {
        Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
            KeyCode::Char('q') => Some(OverviewAction::Quit),
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(OverviewAction::Quit)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if n_agents > 0 {
                    *selected = if *selected == 0 {
                        n_agents - 1
                    } else {
                        *selected - 1
                    };
                }
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if n_agents > 0 {
                    *selected = (*selected + 1) % n_agents;
                }
                None
            }
            KeyCode::Enter => {
                if n_agents > 0 {
                    Some(OverviewAction::Attach(*selected))
                } else {
                    None
                }
            }
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let n = (c as u8 - b'1') as usize;
                if n < n_agents {
                    Some(OverviewAction::Attach(n))
                } else {
                    None
                }
            }
            KeyCode::Char('K') => {
                if n_agents > 0 {
                    Some(OverviewAction::Kill(*selected))
                } else {
                    None
                }
            }
            _ => None,
        },
        Event::Resize(cols, rows) => Some(OverviewAction::Resize(cols, rows)),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn attach(
    stdout_handle: &mut std::io::Stdout,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    agents: &mut [Agent],
    idx: usize,
    prefix: u8,
    detach_byte: u8,
    input_rx: &mut Option<Receiver<InputEvt>>,
    input_thread: &mut Option<std::thread::JoinHandle<()>>,
) -> Result<()> {
    // Leave the ratatui alt-screen so the agent's native TUI gets the real terminal.
    execute!(stdout_handle, LeaveAlternateScreen, Show)?;
    execute!(stdout_handle, Clear(ClearType::All), MoveTo(0, 0))?;

    // Repaint the agent's current screen state so the user isn't staring at a blank terminal.
    if let Some(a) = agents.get(idx) {
        let snapshot = a.current_screen_bytes();
        stdout_handle.write_all(&snapshot).ok();
        stdout_handle.flush().ok();
    }

    // Spin up a stdin reader thread that drives the detach state machine.
    let (tx, rx) = bounded::<InputEvt>(256);
    let tx2 = tx.clone();
    let handle = std::thread::Builder::new()
        .name("agentdeck-stdin".into())
        .spawn(move || stdin_reader(tx2, prefix, detach_byte))
        .context("spawn stdin reader")?;
    *input_rx = Some(rx);
    *input_thread = Some(handle);

    // Tell ratatui its buffer is no longer valid (we're not in alt screen now).
    terminal.clear().ok();
    Ok(())
}

fn detach(
    stdout_handle: &mut std::io::Stdout,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    input_rx: &mut Option<Receiver<InputEvt>>,
    input_thread: &mut Option<std::thread::JoinHandle<()>>,
) -> Result<()> {
    *input_rx = None;
    // The input thread exits on its own after sending Detach/Closed; just drop the handle.
    let _ = input_thread.take();

    execute!(stdout_handle, EnterAlternateScreen, Hide)?;
    terminal.clear().ok();
    Ok(())
}

fn stdin_reader(tx: Sender<InputEvt>, prefix: u8, detach_byte: u8) {
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    let mut buf = [0u8; 1024];
    let mut armed = false;

    loop {
        match handle.read(&mut buf) {
            Ok(0) => {
                let _ = tx.send(InputEvt::Closed);
                return;
            }
            Ok(n) => {
                let mut out: Vec<u8> = Vec::with_capacity(n);
                for &b in &buf[..n] {
                    if armed {
                        armed = false;
                        if b == detach_byte {
                            if !out.is_empty() {
                                let _ = tx.send(InputEvt::Bytes(out));
                            }
                            let _ = tx.send(InputEvt::Detach);
                            return;
                        } else if b == prefix {
                            // Ctrl-A Ctrl-A: send literal prefix byte through.
                            out.push(prefix);
                        } else {
                            out.push(prefix);
                            out.push(b);
                        }
                    } else if b == prefix {
                        armed = true;
                    } else {
                        out.push(b);
                    }
                }
                if !out.is_empty() && tx.send(InputEvt::Bytes(out)).is_err() {
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(?e, "stdin read error in attached mode");
                let _ = tx.send(InputEvt::Closed);
                return;
            }
        }
    }
}

fn char_for_ctrl(byte: u8) -> char {
    // 0x01 -> 'A', 0x02 -> 'B', ...
    if byte < 0x20 {
        (b'A' + byte - 1) as char
    } else {
        '?'
    }
}
