use anyhow::{Context, Result};
use crossbeam_channel::unbounded;
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use portable_pty::PtySize;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::stdout;
use std::time::Duration;

use crate::agent::{Agent, AgentEvent, RuntimeId, Status};
use crate::config::{AgentConfig, Config, Provider};
use crate::keymap::key_event_to_bytes;
use crate::ui::{self, AddModalState, Focus, draw_main};

const SIDEBAR_WIDTH: u16 = 36;

/// Agent-pane dimensions derived from the terminal size. Used to keep every
/// PTY sized to the area where it will render.
struct PaneSize {
    rows: u16,
    cols: u16,
}

fn agent_pane_size(term_cols: u16, term_rows: u16) -> PaneSize {
    let cols = term_cols
        .saturating_sub(SIDEBAR_WIDTH)
        .saturating_sub(2) // agent pane block borders
        .max(20);
    let rows = term_rows
        .saturating_sub(2) // header + footer rows
        .saturating_sub(2) // agent pane block borders
        .max(8);
    PaneSize { rows, cols }
}

pub fn run(cfg: Config) -> Result<()> {
    let mut stdout_handle = stdout();
    enable_raw_mode().context("enable raw mode")?;
    execute!(stdout_handle, EnterAlternateScreen, Hide).context("enter alt screen")?;

    let result = run_inner(cfg);

    execute!(stdout_handle, LeaveAlternateScreen, Show).ok();
    disable_raw_mode().ok();

    result
}

fn run_inner(cfg: Config) -> Result<()> {
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).context("new terminal")?;

    let (term_cols, term_rows) = crossterm::terminal::size().context("query terminal size")?;
    let pane = agent_pane_size(term_cols, term_rows);
    let initial_size = PtySize {
        rows: pane.rows,
        cols: pane.cols,
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
        // Tear down the screen ourselves so the message lands on the real terminal.
        execute!(stdout(), LeaveAlternateScreen, Show).ok();
        disable_raw_mode().ok();
        eprintln!(
            "agentdeck: no agents could be spawned. Check your config (run `agentdeck --print-config`)."
        );
        return Ok(());
    }

    let mut selected: usize = 0;
    let mut focus = Focus::Agent;
    let mut adding: Option<AddingState> = None;
    let mut should_quit = false;
    let mut last_pane_dims = (pane.cols, pane.rows);

    while !should_quit {
        // Drain PTY output -> feed every agent's vt100 parser. Nothing is
        // written directly to stdout — the agent's grid renders through our UI.
        while let Ok(ev) = agent_rx.try_recv() {
            match ev {
                AgentEvent::Output { rid, bytes } => {
                    if let Some(a) = agents.iter_mut().find(|a| a.rid == rid) {
                        a.feed(&bytes);
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

        let model = ui::build_rows(&agents);
        if !model.selectable.is_empty() {
            selected = selected.min(model.selectable.len() - 1);
        }

        let footer = footer_for(focus, adding.is_some());
        let modal = adding.as_ref().map(|s| AddModalState {
            provider: s.provider,
            cwd: s.cwd.as_str(),
            cursor: s.cursor,
        });
        terminal.draw(|f| draw_main(f, &agents, &model, selected, focus, &footer, modal))?;

        if event::poll(Duration::from_millis(50))? {
            let ev = event::read()?;
            handle_event(
                ev,
                &mut agents,
                &model,
                &mut selected,
                &mut focus,
                &mut adding,
                &mut should_quit,
                &mut next_rid,
                &agent_tx,
                &mut last_pane_dims,
            );
        }
    }

    for a in agents.iter_mut() {
        a.kill();
    }
    Ok(())
}

struct AddingState {
    provider: Provider,
    cwd: String,
    cursor: usize,
}

fn footer_for(focus: Focus, adding: bool) -> String {
    if adding {
        return " Enter: spawn   Esc: cancel ".into();
    }
    match focus {
        Focus::Agent => " typing → focused agent   F1 → deck   Ctrl-C → interrupt agent ".into(),
        Focus::Deck => {
            " ↑/↓ select   1-9 attach   Enter focus agent   a add   x remove   q quit   F1 → agent "
                .into()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_event(
    ev: Event,
    agents: &mut Vec<Agent>,
    model: &ui::RowModel,
    selected: &mut usize,
    focus: &mut Focus,
    adding: &mut Option<AddingState>,
    should_quit: &mut bool,
    next_rid: &mut RuntimeId,
    agent_tx: &crossbeam_channel::Sender<AgentEvent>,
    last_pane_dims: &mut (u16, u16),
) {
    match ev {
        Event::Resize(cols, rows) => {
            let pane = agent_pane_size(cols, rows);
            if (pane.cols, pane.rows) != *last_pane_dims {
                *last_pane_dims = (pane.cols, pane.rows);
                for a in agents.iter_mut() {
                    a.resize(pane.rows, pane.cols);
                }
            }
        }
        Event::Key(k) if k.kind == KeyEventKind::Press => {
            // Modal swallows all keys until Enter or Esc.
            if let Some(state) = adding.as_mut() {
                let result = handle_adding_event(&k, state);
                match result {
                    AddingResult::Spawn => {
                        let cwd = state.cwd.trim().to_string();
                        let provider = state.provider;
                        *adding = None;
                        spawn_runtime_agent(
                            agents,
                            provider,
                            &cwd,
                            next_rid,
                            agent_tx,
                            last_pane_dims,
                        );
                    }
                    AddingResult::Cancel => *adding = None,
                    AddingResult::None => {}
                }
                return;
            }

            // F1 is reserved at the agentdeck level — it toggles which pane has
            // focus and is never forwarded to any child. None of the supported
            // agent CLIs bind F1, so this is the safest "always works" key.
            if k.code == KeyCode::F(1) {
                *focus = match *focus {
                    Focus::Deck => Focus::Agent,
                    Focus::Agent => Focus::Deck,
                };
                return;
            }

            match *focus {
                Focus::Deck => {
                    handle_deck_key(k, agents, model, selected, focus, adding, should_quit)
                }
                Focus::Agent => forward_to_agent(k, agents, model, *selected),
            }
        }
        _ => {}
    }
}

fn handle_deck_key(
    k: KeyEvent,
    agents: &mut Vec<Agent>,
    model: &ui::RowModel,
    selected: &mut usize,
    focus: &mut Focus,
    adding: &mut Option<AddingState>,
    should_quit: &mut bool,
) {
    let n = model.selectable.len();
    let agent_idx_at = |sel: usize| -> Option<usize> {
        model.selectable.get(sel).and_then(|&row_idx| {
            if let Some(ui::Row::Agent(ai)) = model.rows.get(row_idx) {
                Some(*ai)
            } else {
                None
            }
        })
    };

    match k.code {
        KeyCode::Char('q') => *should_quit = true,
        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => *should_quit = true,
        KeyCode::Up | KeyCode::Char('k') if n > 0 => {
            *selected = if *selected == 0 { n - 1 } else { *selected - 1 };
        }
        KeyCode::Down | KeyCode::Char('j') if n > 0 => {
            *selected = (*selected + 1) % n;
        }
        KeyCode::Enter => {
            if agent_idx_at(*selected).is_some() {
                *focus = Focus::Agent;
            }
        }
        KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
            let i = (c as u8 - b'1') as usize;
            if i < n {
                *selected = i;
                *focus = Focus::Agent;
            }
        }
        KeyCode::Char('a') | KeyCode::Char('+') => {
            if let Some(ai) = agent_idx_at(*selected) {
                let a = &agents[ai];
                let cwd = a.cwd_label.clone().unwrap_or_else(|| "~".into());
                let cursor = cwd.chars().count();
                *adding = Some(AddingState {
                    provider: a.provider,
                    cwd,
                    cursor,
                });
            }
        }
        KeyCode::Char('x') => {
            if let Some(ai) = agent_idx_at(*selected) {
                if let Some(a) = agents.get_mut(ai) {
                    a.kill();
                }
                if ai < agents.len() {
                    agents.remove(ai);
                }
            }
        }
        _ => {}
    }
}

fn forward_to_agent(k: KeyEvent, agents: &mut [Agent], model: &ui::RowModel, selected: usize) {
    let Some(ai) = model.selectable.get(selected).and_then(|&row_idx| {
        if let Some(ui::Row::Agent(ai)) = model.rows.get(row_idx) {
            Some(*ai)
        } else {
            None
        }
    }) else {
        return;
    };
    let Some(a) = agents.get_mut(ai) else { return };
    if let Some(bytes) = key_event_to_bytes(&k) {
        let _ = a.write(&bytes);
    }
}

enum AddingResult {
    None,
    Spawn,
    Cancel,
}

fn handle_adding_event(k: &KeyEvent, state: &mut AddingState) -> AddingResult {
    match k.code {
        KeyCode::Esc => return AddingResult::Cancel,
        KeyCode::Enter => return AddingResult::Spawn,
        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
            return AddingResult::Cancel;
        }
        KeyCode::Char(c) => {
            let idx = byte_index_for_char_cursor(&state.cwd, state.cursor);
            state.cwd.insert(idx, c);
            state.cursor += 1;
        }
        KeyCode::Backspace if state.cursor > 0 => {
            let end = byte_index_for_char_cursor(&state.cwd, state.cursor);
            let start = byte_index_for_char_cursor(&state.cwd, state.cursor - 1);
            state.cwd.drain(start..end);
            state.cursor -= 1;
        }
        KeyCode::Left if state.cursor > 0 => state.cursor -= 1,
        KeyCode::Right if state.cursor < state.cwd.chars().count() => state.cursor += 1,
        KeyCode::Home => state.cursor = 0,
        KeyCode::End => state.cursor = state.cwd.chars().count(),
        _ => {}
    }
    AddingResult::None
}

fn byte_index_for_char_cursor(s: &str, char_cursor: usize) -> usize {
    s.char_indices()
        .nth(char_cursor)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

fn spawn_runtime_agent(
    agents: &mut Vec<Agent>,
    provider: Provider,
    cwd: &str,
    next_rid: &mut RuntimeId,
    agent_tx: &crossbeam_channel::Sender<AgentEvent>,
    pane_dims: &(u16, u16),
) {
    let cfg = derive_child_config(agents, provider, cwd, *next_rid);
    let rid = *next_rid;
    *next_rid += 1;
    let size = PtySize {
        rows: pane_dims.1,
        cols: pane_dims.0,
        pixel_width: 0,
        pixel_height: 0,
    };
    match Agent::spawn(&cfg, rid, size, agent_tx.clone()) {
        Ok(a) => {
            tracing::info!(id = %cfg.id, "spawned runtime agent");
            agents.push(a);
        }
        Err(e) => {
            tracing::error!(error = ?e, "failed to spawn runtime agent");
        }
    }
}

/// Build a config for a runtime-spawned agent. Inherits command / args / env
/// from the most recent existing agent of the same provider and overrides cwd.
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
        provider_display(provider),
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
