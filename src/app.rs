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
        if apply_nav_action(
            action,
            &mut self.selected,
            &mut self.focus,
            &mut self.should_quit,
            model,
        ) {
            return Ok(());
        }
        match action {
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
                if let Some(ai) = self.agent_idx_at_selected(model)
                    && let Some(a) = self.agents.get_mut(ai)
                {
                    a.kill();
                    self.agents.remove(ai);
                }
            }
            // Nav actions (Quit/MoveUp/MoveDown/FocusAgent/FocusIndex/ToggleFocus/None)
            // are already handled above via `apply_nav_action`.
            _ => {}
        }
        Ok(())
    }

    fn agent_idx_at_selected(&self, model: &ui::RowModel) -> Option<usize> {
        agent_idx_at(self.selected, model)
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
        if let Some(ai) = self.agent_idx_at_selected(model)
            && let Some(a) = self.agents.get_mut(ai)
            && let Some(bytes) = keymap::key_event_to_bytes(&k)
        {
            let _ = a.write(&bytes);
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

/// Apply nav-only actions (no agent list mutation) to the deck state.
/// Returns true if the action was handled here; false if it needs the full
/// `App` context (currently `AddAgent` / `RemoveAgent`).
fn apply_nav_action(
    action: Action,
    selected: &mut usize,
    focus: &mut Focus,
    should_quit: &mut bool,
    model: &ui::RowModel,
) -> bool {
    let n = model.selectable.len();
    match action {
        Action::Quit => *should_quit = true,
        Action::MoveUp => {
            if n > 0 {
                *selected = if *selected == 0 { n - 1 } else { *selected - 1 };
            }
        }
        Action::MoveDown => {
            if n > 0 {
                *selected = (*selected + 1) % n;
            }
        }
        Action::FocusAgent => {
            if agent_idx_at(*selected, model).is_some() {
                *focus = Focus::Agent;
            }
        }
        Action::FocusIndex(i) => {
            if i < n {
                *selected = i;
                *focus = Focus::Agent;
            }
        }
        Action::ToggleFocus => {
            *focus = match *focus {
                Focus::Deck => Focus::Agent,
                Focus::Agent => Focus::Deck,
            };
        }
        Action::None => {}
        Action::AddAgent | Action::RemoveAgent => return false,
    }
    true
}

fn agent_idx_at(selected: usize, model: &ui::RowModel) -> Option<usize> {
    model.selectable.get(selected).and_then(|&row_idx| {
        if let Some(ui::Row::Agent(ai)) = model.rows.get(row_idx) {
            Some(*ai)
        } else {
            None
        }
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::map_deck_key;
    use crate::ui::Row;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// Build a synthetic `RowModel` with `n` agent rows. Bypasses `Agent` entirely
    /// so the deck state machine can be exercised without spawning PTYs.
    fn fake_model(n: usize) -> ui::RowModel {
        let rows: Vec<Row> = (0..n).map(Row::Agent).collect();
        let selectable: Vec<usize> = (0..n).collect();
        ui::RowModel { rows, selectable }
    }

    fn apply(
        code: KeyCode,
        mods: KeyModifiers,
        st: &mut (usize, Focus, bool),
        model: &ui::RowModel,
    ) {
        let ev = KeyEvent::new(code, mods);
        apply_nav_action(map_deck_key(ev), &mut st.0, &mut st.1, &mut st.2, model);
    }

    #[test]
    fn pressing_q_sets_should_quit() {
        let model = fake_model(1);
        let mut st = (0, Focus::Deck, false);
        apply(KeyCode::Char('q'), KeyModifiers::NONE, &mut st, &model);
        assert!(st.2);
        // Ctrl-C is the alternate quit binding.
        let mut st2 = (0, Focus::Deck, false);
        apply(KeyCode::Char('c'), KeyModifiers::CONTROL, &mut st2, &model);
        assert!(st2.2);
    }

    #[test]
    fn pressing_digit_focuses_agent_at_index() {
        let model = fake_model(3);
        let mut st = (0, Focus::Deck, false);
        apply(KeyCode::Char('2'), KeyModifiers::NONE, &mut st, &model);
        assert_eq!(st.0, 1, "'2' selects the second selectable row");
        assert_eq!(st.1, Focus::Agent, "digit jumps focus to the agent pane");
    }

    #[test]
    fn arrow_down_advances_then_wraps() {
        let model = fake_model(2);
        let mut st = (0, Focus::Deck, false);
        apply(KeyCode::Down, KeyModifiers::NONE, &mut st, &model);
        assert_eq!(st.0, 1);
        apply(KeyCode::Down, KeyModifiers::NONE, &mut st, &model);
        assert_eq!(st.0, 0, "down past last entry wraps to first");
    }

    #[test]
    fn arrow_up_at_zero_wraps_to_last() {
        let model = fake_model(2);
        let mut st = (0, Focus::Deck, false);
        apply(KeyCode::Up, KeyModifiers::NONE, &mut st, &model);
        assert_eq!(st.0, 1);
    }

    #[test]
    fn f1_toggles_focus() {
        let model = fake_model(1);
        let mut st = (0, Focus::Deck, false);
        apply(KeyCode::F(1), KeyModifiers::NONE, &mut st, &model);
        assert_eq!(st.1, Focus::Agent);
        apply(KeyCode::F(1), KeyModifiers::NONE, &mut st, &model);
        assert_eq!(st.1, Focus::Deck);
    }
}
