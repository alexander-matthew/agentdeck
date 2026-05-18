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

use crate::agent::{Agent, AgentEvent, RuntimeId, Status};
use crate::config::{AgentConfig, Config, Provider};
use crate::ui::{self, AddModalState, draw_overview};

#[derive(Debug)]
enum InputEvt {
    Bytes(Vec<u8>),
    Detach,
    Closed,
}

enum Mode {
    Overview,
    Attached {
        rid: RuntimeId,
    },
    Adding {
        provider: Provider,
        cwd: String,
        cursor: usize,
    },
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
    let mut next_rid: RuntimeId = 1;
    let mut agents: Vec<Agent> = Vec::with_capacity(cfg.agents.len());
    for ac in cfg.agents.iter() {
        if ac.manual {
            tracing::info!(id = %ac.id, "skipping manual agent");
            continue;
        }
        let rid = next_rid;
        next_rid += 1;
        match Agent::spawn(ac, rid, initial_size, agent_tx.clone()) {
            Ok(a) => agents.push(a),
            Err(e) => {
                tracing::error!(id = %ac.id, error = ?e, "failed to spawn agent");
            }
        }
    }

    if agents.is_empty() {
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
    let mut last_size_check = Instant::now();
    let prefix = cfg.settings.prefix_byte;
    let detach_byte = cfg.settings.detach_key as u8;

    let mut input_rx: Option<Receiver<InputEvt>> = None;
    let mut input_thread: Option<std::thread::JoinHandle<()>> = None;

    while !should_quit {
        // Drain PTY output. In attached mode, mirror bytes for the focused agent
        // straight to the host terminal.
        while let Ok(ev) = agent_rx.try_recv() {
            match ev {
                AgentEvent::Output { rid, bytes } => {
                    if let Some(a) = agents.iter_mut().find(|a| a.rid == rid) {
                        a.feed(&bytes);
                    }
                    if let Mode::Attached { rid: focused } = mode
                        && focused == rid
                    {
                        let _ = stdout_handle.write_all(&bytes);
                        let _ = stdout_handle.flush();
                    }
                }
                AgentEvent::ReaderClosed { rid } => {
                    if let Some(a) = agents.iter_mut().find(|a| a.rid == rid) {
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
                let model = ui::build_rows(&agents);
                if !model.selectable.is_empty() {
                    selected = selected.min(model.selectable.len() - 1);
                }
                let footer = " ↑/↓ select   1-9 attach   a add   x remove   q quit ".to_string();
                terminal.draw(|f| draw_overview(f, &agents, &model, selected, &footer, None))?;
                if event::poll(Duration::from_millis(50))? {
                    let ev = event::read()?;
                    let action = handle_overview_event(ev, &model, &mut selected);
                    if let Some(action) = action {
                        match action {
                            OverviewAction::Quit => should_quit = true,
                            OverviewAction::Attach(ai) => {
                                let rid = agents[ai].rid;
                                attach(
                                    stdout_handle,
                                    &mut terminal,
                                    &mut agents,
                                    rid,
                                    prefix,
                                    detach_byte,
                                    &mut input_rx,
                                    &mut input_thread,
                                )?;
                                mode = Mode::Attached { rid };
                            }
                            OverviewAction::Add(ai) => {
                                let a = &agents[ai];
                                let cwd = a.cwd_label.clone().unwrap_or_else(|| "~".into());
                                let cursor = cwd.chars().count();
                                mode = Mode::Adding {
                                    provider: a.provider,
                                    cwd,
                                    cursor,
                                };
                            }
                            OverviewAction::Remove(ai) => {
                                if let Some(a) = agents.get_mut(ai) {
                                    a.kill();
                                }
                                if ai < agents.len() {
                                    agents.remove(ai);
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
            Mode::Adding {
                ref mut cwd,
                ref mut cursor,
                provider,
            } => {
                let model = ui::build_rows(&agents);
                let footer = " Enter: spawn   Esc: cancel ".to_string();
                let modal = AddModalState {
                    provider,
                    cwd: cwd.as_str(),
                    cursor: *cursor,
                };
                terminal
                    .draw(|f| draw_overview(f, &agents, &model, selected, &footer, Some(modal)))?;
                if event::poll(Duration::from_millis(50))? {
                    let ev = event::read()?;
                    match handle_adding_event(ev, cwd, cursor) {
                        AddingAction::Spawn => {
                            let new_cfg = derive_child_config(&agents, provider, cwd, next_rid);
                            let rid = next_rid;
                            next_rid += 1;
                            let pty_size = PtySize {
                                rows: current_term_size.1.saturating_sub(3).max(10),
                                cols: current_term_size.0.max(40),
                                pixel_width: 0,
                                pixel_height: 0,
                            };
                            match Agent::spawn(&new_cfg, rid, pty_size, agent_tx.clone()) {
                                Ok(a) => {
                                    tracing::info!(id = %new_cfg.id, "spawned runtime agent");
                                    agents.push(a);
                                }
                                Err(e) => {
                                    tracing::error!(error = ?e, "failed to spawn runtime agent");
                                }
                            }
                            mode = Mode::Overview;
                        }
                        AddingAction::Cancel => {
                            mode = Mode::Overview;
                        }
                        AddingAction::None => {}
                    }
                }
            }
            Mode::Attached { rid } => {
                if let Some(rx) = input_rx.as_ref() {
                    while let Ok(evt) = rx.try_recv() {
                        match evt {
                            InputEvt::Bytes(b) => {
                                if let Some(a) = agents.iter_mut().find(|a| a.rid == rid) {
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
                                break;
                            }
                        }
                    }
                }

                // Forward host-terminal resizes to the PTY by polling size.
                if last_size_check.elapsed() > Duration::from_millis(200) {
                    if let Ok(sz) = crossterm::terminal::size()
                        && sz != current_term_size
                    {
                        current_term_size = sz;
                        if let Some(a) = agents.iter_mut().find(|a| a.rid == rid) {
                            a.resize(sz.1, sz.0);
                        }
                    }
                    last_size_check = Instant::now();
                }

                std::thread::sleep(Duration::from_millis(8));
            }
        }
    }

    for a in agents.iter_mut() {
        a.kill();
    }
    Ok(())
}

enum OverviewAction {
    Quit,
    Attach(usize),
    Add(usize),
    Remove(usize),
    Resize(u16, u16),
}

fn handle_overview_event(
    ev: Event,
    model: &ui::RowModel,
    selected: &mut usize,
) -> Option<OverviewAction> {
    let n = model.selectable.len();
    let agent_idx_at_selected = || {
        model.selectable.get(*selected).and_then(|&row_idx| {
            if let Some(ui::Row::Agent(ai)) = model.rows.get(row_idx) {
                Some(*ai)
            } else {
                None
            }
        })
    };

    match ev {
        Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
            KeyCode::Char('q') => Some(OverviewAction::Quit),
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(OverviewAction::Quit)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if n > 0 {
                    *selected = if *selected == 0 { n - 1 } else { *selected - 1 };
                }
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if n > 0 {
                    *selected = (*selected + 1) % n;
                }
                None
            }
            KeyCode::Enter => agent_idx_at_selected().map(OverviewAction::Attach),
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let i = (c as u8 - b'1') as usize;
                model.selectable.get(i).and_then(|&row_idx| {
                    if let Some(ui::Row::Agent(ai)) = model.rows.get(row_idx) {
                        Some(OverviewAction::Attach(*ai))
                    } else {
                        None
                    }
                })
            }
            KeyCode::Char('a') | KeyCode::Char('+') => {
                agent_idx_at_selected().map(OverviewAction::Add)
            }
            KeyCode::Char('x') => agent_idx_at_selected().map(OverviewAction::Remove),
            _ => None,
        },
        Event::Resize(cols, rows) => Some(OverviewAction::Resize(cols, rows)),
        _ => None,
    }
}

enum AddingAction {
    None,
    Spawn,
    Cancel,
}

fn handle_adding_event(ev: Event, cwd: &mut String, cursor: &mut usize) -> AddingAction {
    if let Event::Key(k) = ev
        && k.kind == KeyEventKind::Press
    {
        match k.code {
            KeyCode::Esc => return AddingAction::Cancel,
            KeyCode::Enter => return AddingAction::Spawn,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                return AddingAction::Cancel;
            }
            KeyCode::Char(c) => {
                let idx = byte_index_for_char_cursor(cwd, *cursor);
                cwd.insert(idx, c);
                *cursor += 1;
            }
            KeyCode::Backspace => {
                if *cursor > 0 {
                    let end = byte_index_for_char_cursor(cwd, *cursor);
                    let start = byte_index_for_char_cursor(cwd, *cursor - 1);
                    cwd.drain(start..end);
                    *cursor -= 1;
                }
            }
            KeyCode::Left => {
                if *cursor > 0 {
                    *cursor -= 1;
                }
            }
            KeyCode::Right => {
                if *cursor < cwd.chars().count() {
                    *cursor += 1;
                }
            }
            KeyCode::Home => *cursor = 0,
            KeyCode::End => *cursor = cwd.chars().count(),
            _ => {}
        }
    }
    AddingAction::None
}

fn byte_index_for_char_cursor(s: &str, char_cursor: usize) -> usize {
    s.char_indices()
        .nth(char_cursor)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Build a config for a runtime-spawned agent. Inherits command / args / env from
/// the most recent existing agent of the same provider (if any) and overrides
/// the cwd.
fn derive_child_config(
    agents: &[Agent],
    provider: Provider,
    cwd: &str,
    rid: RuntimeId,
) -> AgentConfig {
    let template = agents
        .iter()
        .rev()
        .find(|a| a.provider == provider)
        .map(|a| a.template.clone());

    let mut cfg = template.unwrap_or_else(|| AgentConfig {
        id: format!("{}-{rid}", provider.tag()),
        name: None,
        provider,
        command: provider.tag().to_string(),
        args: vec![],
        cwd: None,
        env: Default::default(),
        manual: false,
    });

    let trimmed = cwd.trim();
    cfg.cwd = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    };
    cfg.id = format!("{}-{rid}", provider.tag());
    cfg.name = Some(format!(
        "{} · {}",
        cfg.name
            .clone()
            .unwrap_or_else(|| provider_display(provider).to_string()),
        short_cwd(trimmed)
    ));
    cfg
}

fn provider_display(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "Claude",
        Provider::Codex => "Codex",
        Provider::Gemini => "Gemini",
        Provider::Other => "agent",
    }
}

fn short_cwd(cwd: &str) -> String {
    let home = std::env::var("HOME").ok();
    let collapsed = match home {
        Some(h) if cwd.starts_with(&h) => format!("~{}", &cwd[h.len()..]),
        _ => cwd.to_string(),
    };
    let parts: Vec<&str> = collapsed.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return collapsed;
    }
    parts[parts.len() - 1].to_string()
}

#[allow(clippy::too_many_arguments)]
fn attach(
    stdout_handle: &mut std::io::Stdout,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    agents: &mut [Agent],
    rid: RuntimeId,
    prefix: u8,
    detach_byte: u8,
    input_rx: &mut Option<Receiver<InputEvt>>,
    input_thread: &mut Option<std::thread::JoinHandle<()>>,
) -> Result<()> {
    execute!(stdout_handle, LeaveAlternateScreen, Show)?;
    execute!(stdout_handle, Clear(ClearType::All), MoveTo(0, 0))?;

    if let Some(a) = agents.iter().find(|a| a.rid == rid) {
        let snapshot = a.current_screen_bytes();
        stdout_handle.write_all(&snapshot).ok();
        stdout_handle.flush().ok();
    }

    let (tx, rx) = bounded::<InputEvt>(256);
    let handle = std::thread::Builder::new()
        .name("agentdeck-stdin".into())
        .spawn(move || stdin_reader(tx, prefix, detach_byte))
        .context("spawn stdin reader")?;
    *input_rx = Some(rx);
    *input_thread = Some(handle);

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
