use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, unbounded};
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyEvent, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use portable_pty::PtySize;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::stdout;
use std::time::Duration;

use crate::agent::{Agent, AgentEvent, RuntimeId, Status};
use crate::config::{AgentConfig, Config, Provider};
use crate::keymap::{self, Action};
use crate::ui::{self, AddModalState, Focus, draw_main};

const SIDEBAR_WIDTH: u16 = 36;

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

    let mut app = App::new(cfg)?;
    let result = app.run_loop();

    execute!(stdout_handle, LeaveAlternateScreen, Show).ok();
    disable_raw_mode().ok();

    result
}

struct AddingState {
    provider: Provider,
    cwd: String,
    cursor: usize,
}

struct App {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
    agents: Vec<Agent>,
    selected: usize,
    focus: Focus,
    adding: Option<AddingState>,
    should_quit: bool,
    last_pane_dims: (u16, u16),
    next_rid: RuntimeId,

    agent_tx: Sender<AgentEvent>,
    agent_rx: Receiver<AgentEvent>,
}

impl App {
    fn new(cfg: Config) -> Result<Self> {
        let backend = CrosstermBackend::new(stdout());
        let terminal = Terminal::new(backend).context("new terminal")?;

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
            return Err(anyhow::anyhow!("no agents could be spawned"));
        }

        Ok(Self {
            terminal,
            agents,
            selected: 0,
            focus: Focus::Agent,
            adding: None,
            should_quit: false,
            last_pane_dims: (pane.cols, pane.rows),
            next_rid,
            agent_tx,
            agent_rx,
        })
    }

    fn run_loop(&mut self) -> Result<()> {
        while !self.should_quit {
            self.handle_pty_output();
            self.poll_agent_exits();

            let model = ui::build_rows(&self.agents);
            if !model.selectable.is_empty() {
                self.selected = self.selected.min(model.selectable.len() - 1);
            }

            let footer = self.footer_for();
            let modal = self.adding.as_ref().map(|s| AddModalState {
                provider: s.provider,
                cwd: s.cwd.as_str(),
                cursor: s.cursor,
            });
            self.terminal.draw(|f| {
                draw_main(
                    f,
                    &self.agents,
                    &model,
                    self.selected,
                    self.focus,
                    &footer,
                    modal,
                )
            })?;

            if event::poll(Duration::from_millis(50))? {
                let ev = event::read()?;
                self.handle_event(ev, &model)?;
            }
        }

        for a in self.agents.iter_mut() {
            a.kill();
        }
        Ok(())
    }

    fn handle_pty_output(&mut self) {
        while let Ok(ev) = self.agent_rx.try_recv() {
            match ev {
                AgentEvent::Output { rid, bytes } => {
                    if let Some(a) = self.agents.iter_mut().find(|a| a.rid == rid) {
                        a.feed(&bytes);
                    }
                }
                AgentEvent::ReaderClosed { rid } => {
                    if let Some(a) = self.agents.iter_mut().find(|a| a.rid == rid) {
                        a.poll_exit();
                        if matches!(a.status, Status::Running) {
                            a.status = Status::Exited(0);
                        }
                    }
                }
            }
        }
    }

    fn poll_agent_exits(&mut self) {
        for a in self.agents.iter_mut() {
            a.poll_exit();
        }
    }

    fn footer_for(&self) -> String {
        if self.adding.is_some() {
            return " Enter: spawn   Esc: cancel ".into();
        }
        match self.focus {
            Focus::Agent => " typing → focused agent   F1 → deck   Ctrl-C → interrupt agent ".into(),
            Focus::Deck => {
                " ↑/↓ select   1-9 focus   Enter focus agent   a add   x remove   q quit   F1 → agent "
                    .into()
            }
        }
    }

    fn handle_event(&mut self, ev: Event, model: &ui::RowModel) -> Result<()> {
        match ev {
            Event::Resize(cols, rows) => {
                let pane = agent_pane_size(cols, rows);
                if (pane.cols, pane.rows) != self.last_pane_dims {
                    self.last_pane_dims = (pane.cols, pane.rows);
                    for a in self.agents.iter_mut() {
                        a.resize(pane.rows, pane.cols);
                    }
                }
            }
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                if let Some(state) = self.adding.as_mut() {
                    let result = handle_adding_event(k, state);
                    match result {
                        AddingResult::Spawn => {
                            let cwd = state.cwd.trim().to_string();
                            let provider = state.provider;
                            self.adding = None;
                            self.spawn_runtime_agent(provider, &cwd);
                        }
                        AddingResult::Cancel => self.adding = None,
                        AddingResult::None => {}
                    }
                    return Ok(());
                }

                if self.focus == Focus::Deck {
                    let action = keymap::map_deck_key(k);
                    self.handle_action(action, model)?;
                } else {
                    // Check for F1 toggle even in agent focus mode.
                    if k.code == event::KeyCode::F(1) {
                        self.focus = Focus::Deck;
                    } else {
                        self.forward_to_agent(k, model);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_action(&mut self, action: Action, model: &ui::RowModel) -> Result<()> {
        match action {
            Action::Quit => self.should_quit = true,
            Action::MoveUp => {
                let n = model.selectable.len();
                if n > 0 {
                    self.selected = if self.selected == 0 { n - 1 } else { self.selected - 1 };
                }
            }
            Action::MoveDown => {
                let n = model.selectable.len();
                if n > 0 {
                    self.selected = (self.selected + 1) % n;
                }
            }
            Action::FocusAgent => {
                if self.agent_idx_at_selected(model).is_some() {
                    self.focus = Focus::Agent;
                }
            }
            Action::FocusIndex(i) => {
                if i < model.selectable.len() {
                    self.selected = i;
                    self.focus = Focus::Agent;
                }
            }
            Action::AddAgent => {
                if let Some(ai) = self.agent_idx_at_selected(model) {
                    let a = &self.agents[ai];
                    let cwd = a.cwd_label.clone().unwrap_or_else(|| "~".into());
                    let cursor = cwd.chars().count();
                    self.adding = Some(AddingState {
                        provider: a.provider,
                        cwd,
                        cursor,
                    });
                }
            }
            Action::RemoveAgent => {
                if let Some(ai) = self.agent_idx_at_selected(model) {
                    let a = self.agents.get_mut(ai).unwrap();
                    a.kill();
                    self.agents.remove(ai);
                }
            }
            Action::ToggleFocus => {
                self.focus = match self.focus {
                    Focus::Deck => Focus::Agent,
                    Focus::Agent => Focus::Deck,
                };
            }
            Action::None => {}
        }
        Ok(())
    }

    fn agent_idx_at_selected(&self, model: &ui::RowModel) -> Option<usize> {
        model.selectable.get(self.selected).and_then(|&row_idx| {
            if let Some(ui::Row::Agent(ai)) = model.rows.get(row_idx) {
                Some(*ai)
            } else {
                None
            }
        })
    }

    fn spawn_runtime_agent(&mut self, provider: Provider, cwd: &str) {
        let cfg = derive_child_config(&self.agents, provider, cwd, self.next_rid);
        let rid = self.next_rid;
        self.next_rid += 1;
        let size = PtySize {
            rows: self.last_pane_dims.1,
            cols: self.last_pane_dims.0,
            pixel_width: 0,
            pixel_height: 0,
        };
        match Agent::spawn(&cfg, rid, size, self.agent_tx.clone()) {
            Ok(a) => {
                tracing::info!(id = %cfg.id, "spawned runtime agent");
                self.agents.push(a);
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to spawn runtime agent");
            }
        }
    }

    fn forward_to_agent(&mut self, k: KeyEvent, model: &ui::RowModel) {
        if let Some(ai) = self.agent_idx_at_selected(model) {
            if let Some(a) = self.agents.get_mut(ai) {
                if let Some(bytes) = keymap::key_event_to_bytes(&k) {
                    let _ = a.write(&bytes);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddingResult {
    None,
    Spawn,
    Cancel,
}

fn handle_adding_event(k: KeyEvent, state: &mut AddingState) -> AddingResult {
    match k.code {
        event::KeyCode::Esc => AddingResult::Cancel,
        event::KeyCode::Enter => AddingResult::Spawn,
        event::KeyCode::Char('c') if k.modifiers.contains(event::KeyModifiers::CONTROL) => {
            AddingResult::Cancel
        }
        event::KeyCode::Char(c) => {
            let idx = byte_index_for_char_cursor(&state.cwd, state.cursor);
            state.cwd.insert(idx, c);
            state.cursor += 1;
            AddingResult::None
        }
        event::KeyCode::Backspace if state.cursor > 0 => {
            let end = byte_index_for_char_cursor(&state.cwd, state.cursor);
            let start = byte_index_for_char_cursor(&state.cwd, state.cursor - 1);
            state.cwd.drain(start..end);
            state.cursor -= 1;
            AddingResult::None
        }
        event::KeyCode::Left if state.cursor > 0 => {
            state.cursor -= 1;
            AddingResult::None
        }
        event::KeyCode::Right if state.cursor < state.cwd.chars().count() => {
            state.cursor += 1;
            AddingResult::None
        }
        event::KeyCode::Home => {
            state.cursor = 0;
            AddingResult::None
        }
        event::KeyCode::End => {
            state.cursor = state.cwd.chars().count();
            AddingResult::None
        }
        _ => AddingResult::None,
    }
}

fn byte_index_for_char_cursor(s: &str, char_cursor: usize) -> usize {
    s.char_indices()
        .nth(char_cursor)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

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
        Provider::Aider => "Aider",
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
