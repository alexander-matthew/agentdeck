//! Main event loop and top-level `App` state.
//!
//! Owns the terminal, the set of running [`Agent`]s, and the channels they
//! emit on. The loop fans `AgentEvent`s and `UsageEvent`s in from background
//! reader threads while polling crossterm for input, then dispatches each
//! key to one of four handlers depending on focus and modal state: deck
//! navigation, the focused agent's PTY, the add-agent modal, or the usage
//! dashboard. Focus toggles between deck and agent via the keymap; when no
//! mapping fires, the configured `toggle_key` is the fallback.
//!
//! The PTY-size invariant lives in [`agent_pane_size`] (see lines 35-54):
//! every time the view mode or grid shape changes, each agent's parser must
//! be resized to match the cell it will be drawn into, otherwise rendering
//! desyncs. Pure rendering belongs in [`crate::ui`]; this module is the only
//! place that mutates `App`.

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, unbounded};
use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use portable_pty::PtySize;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::collections::{BTreeMap, HashMap};
use std::io::stdout;
use std::time::{Duration, Instant};

use crate::agent::{Agent, AgentEvent, RuntimeId, SpawnFailure, Status};
use crate::config::{AgentConfig, Config, Provider};
use crate::keymap::{self, Action};
use crate::ui::{self, AddModalState, Focus, ViewMode, draw_main};
use crate::usage::{self, UsageEvent, UsageState};

const SIDEBAR_WIDTH: u16 = 36;

#[derive(Debug, Clone, Copy)]
struct PaneSize {
    rows: u16,
    cols: u16,
}

/// Size each agent's PTY should be in the *current* view mode. Used to drive
/// PTY resizes whenever the layout changes — agents only render correctly when
/// their parser dimensions match the cell they'll be drawn into.
fn agent_pane_size(view: ViewMode, grid: (u16, u16), term_cols: u16, term_rows: u16) -> PaneSize {
    let avail_cols = term_cols.saturating_sub(SIDEBAR_WIDTH);
    let avail_rows = term_rows.saturating_sub(2); // header + footer

    match view {
        ViewMode::Single => PaneSize {
            cols: avail_cols.saturating_sub(2).max(20),
            rows: avail_rows.saturating_sub(2).max(8),
        },
        ViewMode::Grid => {
            let (gr, gc) = (grid.0.max(1), grid.1.max(1));
            let cell_w = (avail_cols / gc).saturating_sub(2).max(20);
            let cell_h = (avail_rows / gr).saturating_sub(2).max(5);
            PaneSize {
                cols: cell_w,
                rows: cell_h,
            }
        }
    }
}

pub fn run(cfg: Config) -> Result<()> {
    let mut stdout_handle = stdout();
    enable_raw_mode().context("enable raw mode")?;
    execute!(
        stdout_handle,
        EnterAlternateScreen,
        Hide,
        EnableMouseCapture
    )
    .context("enter alt screen")?;

    let mut app = App::new(cfg)?;
    let result = app.run_loop();

    execute!(
        stdout_handle,
        LeaveAlternateScreen,
        Show,
        DisableMouseCapture
    )
    .ok();
    disable_raw_mode().ok();

    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddingField {
    Provider,
    Cwd,
}

struct AddingState {
    providers: Vec<Provider>,
    selected_provider: usize,
    cwd: String,
    cursor: usize,
    field: AddingField,
}

struct RenamingState {
    name: String,
    cursor: usize,
}

struct App {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
    agents: Vec<Agent>,
    selected: usize,
    focus: Focus,
    view_mode: ViewMode,
    grid_dims: (u16, u16),
    sort_mode: ui::SortMode,
    adding: Option<AddingState>,
    renaming: Option<RenamingState>,
    help_visible: bool,
    pending_spawn_failures: Vec<SpawnFailure>,
    should_quit: bool,
    last_pane_dims: (u16, u16),
    next_rid: RuntimeId,
    toggle_key: Option<KeyEvent>,
    last_user_activity: Instant,
    last_known_states: HashMap<RuntimeId, crate::state::LiveState>,

    agent_tx: Sender<AgentEvent>,
    agent_rx: Receiver<AgentEvent>,

    showing_usage: bool,
    usage_state: UsageState,
    usage_tx: Sender<UsageEvent>,
    usage_rx: Receiver<UsageEvent>,
    usage_commands: BTreeMap<String, String>,
    usage_refresh_interval: Duration,
    last_usage_refresh: Option<Instant>,
}

impl App {
    fn new(cfg: Config) -> Result<Self> {
        let toggle_key = keymap::parse_key(&cfg.settings.toggle_key);
        // User-visible signal lives in ~/.local/state/agentdeck/agentdeck.log.
        if toggle_key.is_none() && cfg.settings.toggle_key != "ctrl-space" {
            tracing::warn!(
                value = %cfg.settings.toggle_key,
                "could not parse [settings].toggle_key; falling back to ctrl-space"
            );
        }
        let backend = CrosstermBackend::new(stdout());
        let terminal = Terminal::new(backend).context("new terminal")?;

        let (term_cols, term_rows) = crossterm::terminal::size().context("query terminal size")?;
        let grid_dims = (cfg.settings.grid_rows.max(1), cfg.settings.grid_cols.max(1));
        let pane = agent_pane_size(ViewMode::Single, grid_dims, term_cols, term_rows);
        let initial_size = PtySize {
            rows: pane.rows,
            cols: pane.cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let (agent_tx, agent_rx) = unbounded::<AgentEvent>();
        let (agents, pending_spawn_failures, next_rid) =
            spawn_configured_agents(&cfg.agents, initial_size, &agent_tx, 1);

        if agents.is_empty() {
            return Err(anyhow::anyhow!("no agents could be spawned"));
        }

        let usage_state = UsageState::from_commands(&cfg.usage_commands);
        let (usage_tx, usage_rx) = unbounded::<UsageEvent>();

        Ok(Self {
            terminal,
            agents,
            selected: 0,
            focus: Focus::Agent,
            view_mode: ViewMode::Single,
            grid_dims,
            sort_mode: ui::SortMode::Provider,
            adding: None,
            renaming: None,
            help_visible: false,
            pending_spawn_failures,
            should_quit: false,
            last_pane_dims: (pane.cols, pane.rows),
            next_rid,
            toggle_key,
            last_user_activity: Instant::now(),
            last_known_states: HashMap::new(),
            agent_tx,
            agent_rx,
            showing_usage: false,
            usage_state,
            usage_tx,
            usage_rx,
            usage_commands: cfg.usage_commands,
            usage_refresh_interval: Duration::from_secs(cfg.settings.usage_refresh_secs.max(5)),
            last_usage_refresh: None,
        })
    }

    fn run_loop(&mut self) -> Result<()> {
        // Kick off an initial refresh so the dashboard isn't empty on first open.
        self.refresh_usage_all();

        while !self.should_quit {
            self.handle_pty_output();
            self.poll_agent_exits();
            self.drain_usage_events();
            self.maybe_tick_usage_refresh();

            let model = ui::build_rows(&self.agents, self.sort_mode);
            if !model.selectable.is_empty() {
                self.selected = self.selected.min(model.selectable.len() - 1);
            }

            self.evaluate_smart_focus(&model);

            let visible_agents = self.visible_agents(&model);
            let footer = self.footer_for();
            let modal = self.adding.as_ref().map(|s| AddModalState {
                providers: &s.providers,
                selected_provider: s.selected_provider,
                cwd: &s.cwd,
                cursor: s.cursor,
            });
            let rename_modal = self.renaming.as_ref().map(|s| ui::RenameModalState {
                name: &s.name,
                cursor: s.cursor,
            });
            let tk_label = self.toggle_key_label();
            let view = self.view_mode;
            let grid = self.grid_dims;
            let showing_usage = self.showing_usage;
            let help_visible = self.help_visible;
            let usage_snapshot = self.usage_state.clone();
            let spawn_failures = &self.pending_spawn_failures;
            self.terminal.draw(|f| {
                draw_main(
                    f,
                    &self.agents,
                    &model,
                    self.selected,
                    self.focus,
                    view,
                    grid,
                    &visible_agents,
                    showing_usage,
                    &usage_snapshot,
                    &footer,
                    modal,
                    rename_modal,
                    help_visible,
                    spawn_failures,
                    &tk_label,
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

    fn drain_usage_events(&mut self) {
        while let Ok(ev) = self.usage_rx.try_recv() {
            self.usage_state.apply(ev);
        }
    }

    fn maybe_tick_usage_refresh(&mut self) {
        if self.usage_commands.is_empty() {
            return;
        }
        let due = match self.last_usage_refresh {
            None => true,
            Some(t) => t.elapsed() >= self.usage_refresh_interval,
        };
        if due {
            self.refresh_usage_all();
        }
    }

    fn refresh_usage_all(&mut self) {
        for (provider, command) in self.usage_commands.iter() {
            let cmd = command.trim();
            if cmd.is_empty() {
                continue;
            }
            usage::spawn_refresh(provider.clone(), cmd.to_string(), self.usage_tx.clone());
        }
        self.last_usage_refresh = Some(Instant::now());
    }

    fn evaluate_smart_focus(&mut self, model: &ui::RowModel) {
        use crate::state::{self, LiveState};
        let now = Instant::now();
        let mut target_rid = None;

        for agent in &self.agents {
            let current_state = state::detect(agent);
            let prev_state = self
                .last_known_states
                .get(&agent.rid)
                .copied()
                .unwrap_or(LiveState::Idle);

            // Detect transition to Waiting
            if current_state == LiveState::Waiting && prev_state != LiveState::Waiting {
                let current_agent_idx = self.agent_idx_at_selected(model);
                let is_current = current_agent_idx
                    .map(|i| self.agents[i].rid == agent.rid)
                    .unwrap_or(false);

                if !is_current {
                    let user_idle =
                        now.duration_since(self.last_user_activity) > Duration::from_secs(2);
                    let current_agent_idle = if let Some(idx) = current_agent_idx {
                        let s = state::detect(&self.agents[idx]);
                        matches!(s, LiveState::Idle | LiveState::Exited(_) | LiveState::Stuck)
                    } else {
                        true
                    };

                    if user_idle && current_agent_idle {
                        target_rid = Some(agent.rid);
                    }
                }
            }
            self.last_known_states.insert(agent.rid, current_state);
        }

        if let Some(rid) = target_rid
            && let Some(agent_idx) = self.agents.iter().position(|a| a.rid == rid)
        {
            for (selectable_idx, &row_idx) in model.selectable.iter().enumerate() {
                if let Some(ui::Row::Agent(ai)) = model.rows.get(row_idx)
                    && *ai == agent_idx
                {
                    self.selected = selectable_idx;
                    self.focus = Focus::Agent;
                    break;
                }
            }
        }
    }

    fn footer_for(&self) -> String {
        if self.adding.is_some() {
            return " Enter: spawn   Esc: cancel ".into();
        }
        if self.renaming.is_some() {
            return " Enter: save   Esc: cancel ".into();
        }
        if self.showing_usage {
            return " r: refresh now   u / Esc: close usage   q: quit ".into();
        }
        let tk = self.toggle_key_label();
        let view_chip = match self.view_mode {
            ViewMode::Single => "single",
            ViewMode::Grid => "grid",
        };
        match self.focus {
            Focus::Agent => format!(
                " typing → focused agent   {tk} → deck   Ctrl-C → interrupt   [{view_chip}] "
            ),
            Focus::Deck => format!(
                " ↑/↓ select   1-9 focus   Tab jump   Enter focus   PgUp/PgDn scroll   a add   x remove   r rename   o sort   g grid   u usage   ?:help   q quit   {tk} → agent   [{view_chip}] "
            ),
        }
    }

    fn toggle_key_label(&self) -> String {
        match self.toggle_key {
            Some(k) => {
                let mut s = String::new();
                if k.modifiers.contains(event::KeyModifiers::CONTROL) {
                    s.push_str("Ctrl-");
                }
                if k.modifiers.contains(event::KeyModifiers::ALT) {
                    s.push_str("Alt-");
                }
                if k.modifiers.contains(event::KeyModifiers::SHIFT) {
                    s.push_str("Shift-");
                }
                if k.modifiers.contains(event::KeyModifiers::SUPER) {
                    s.push_str("Cmd-");
                }
                match k.code {
                    event::KeyCode::F(n) => s.push_str(&format!("F{n}")),
                    event::KeyCode::Char(' ') => s.push_str("Space"),
                    event::KeyCode::Char(c) => s.push_str(&c.to_uppercase().to_string()),
                    event::KeyCode::Esc => s.push_str("Esc"),
                    event::KeyCode::Enter => s.push_str("Enter"),
                    _ => s.push_str("Key"),
                }
                s
            }
            None => "Ctrl-Space".into(),
        }
    }

    /// Agent indices that should be rendered in the right pane area right now.
    /// Single mode shows just the selected agent; Grid mode shows the page that
    /// contains the selected agent.
    fn visible_agents(&self, model: &ui::RowModel) -> Vec<usize> {
        match self.view_mode {
            ViewMode::Single => self
                .agent_idx_at_selected(model)
                .map(|i| vec![i])
                .unwrap_or_default(),
            ViewMode::Grid => {
                let page_size = (self.grid_dims.0 as usize) * (self.grid_dims.1 as usize);
                if page_size == 0 || model.selectable.is_empty() {
                    return Vec::new();
                }
                let n = model.selectable.len();
                let page = self.selected / page_size;
                let start = page * page_size;
                let end = (start + page_size).min(n);
                (start..end)
                    .filter_map(|i| self.agent_idx_at_selectable(model, i))
                    .collect()
            }
        }
    }

    fn handle_event(&mut self, ev: Event, model: &ui::RowModel) -> Result<()> {
        match ev {
            Event::Resize(cols, rows) => {
                let pane = agent_pane_size(self.view_mode, self.grid_dims, cols, rows);
                if (pane.cols, pane.rows) != self.last_pane_dims {
                    self.last_pane_dims = (pane.cols, pane.rows);
                    for a in self.agents.iter_mut() {
                        a.resize(pane.rows, pane.cols);
                    }
                }
            }
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                self.last_user_activity = Instant::now();
                if !self.pending_spawn_failures.is_empty() {
                    // Any key dismisses the startup spawn-failure modal; the
                    // failures stay in the tracing log for post-hoc inspection.
                    self.pending_spawn_failures.clear();
                    return Ok(());
                }
                if self.help_visible {
                    // Any key closes the help overlay; do not forward to the PTY.
                    self.help_visible = false;
                    return Ok(());
                }
                if let Some(state) = self.adding.as_mut() {
                    let result = handle_adding_event(k, state);
                    match result {
                        AddingResult::Spawn => {
                            let cwd = state.cwd.trim().to_string();
                            let provider = state.providers[state.selected_provider];
                            self.adding = None;
                            self.spawn_runtime_agent(provider, &cwd);
                        }
                        AddingResult::Cancel => self.adding = None,
                        AddingResult::None => {}
                    }
                    return Ok(());
                }

                if let Some(state) = self.renaming.as_mut() {
                    match k.code {
                        KeyCode::Esc => self.renaming = None,
                        KeyCode::Enter => {
                            let new_name = state.name.trim().to_string();
                            if !new_name.is_empty()
                                && let Some(ai) = self.agent_idx_at_selected(model)
                                && let Some(agent) = self.agents.get_mut(ai)
                            {
                                agent.name = new_name;
                            }
                            self.renaming = None;
                        }
                        KeyCode::Char(c) => {
                            let idx = byte_index_for_char_cursor(&state.name, state.cursor);
                            state.name.insert(idx, c);
                            state.cursor += 1;
                        }
                        KeyCode::Backspace if state.cursor > 0 => {
                            let end = byte_index_for_char_cursor(&state.name, state.cursor);
                            let start = byte_index_for_char_cursor(&state.name, state.cursor - 1);
                            state.name.drain(start..end);
                            state.cursor -= 1;
                        }
                        KeyCode::Left if state.cursor > 0 => state.cursor -= 1,
                        KeyCode::Right if state.cursor < state.name.chars().count() => {
                            state.cursor += 1;
                        }
                        _ => {}
                    }
                    return Ok(());
                }

                if self.showing_usage {
                    self.handle_usage_key(k);
                    return Ok(());
                }

                if self.focus == Focus::Deck {
                    let action = keymap::map_deck_key(k, self.toggle_key);
                    self.handle_action(action, model)?;
                } else {
                    // Check for toggle focus even in agent focus mode. If the
                    // configured key didn't parse, fall back to the default
                    // (Ctrl-Space) so a typo'd config still has *some* escape.
                    let is_toggle = if let Some(tk) = self.toggle_key {
                        k.code == tk.code && k.modifiers == tk.modifiers
                    } else {
                        k.code == event::KeyCode::Char(' ')
                            && k.modifiers.contains(event::KeyModifiers::CONTROL)
                    };

                    if is_toggle {
                        self.focus = Focus::Deck;
                    } else {
                        if let Some(agent) = self.current_agent_mut(model) {
                            agent.scroll_offset = 0;
                        }
                        self.forward_to_agent(k, model);
                    }
                }
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::Down(event::MouseButton::Left)
                    if m.column < SIDEBAR_WIDTH && m.row >= 2 =>
                {
                    let row_idx = (m.row - 2) as usize;
                    if let Some(pos) = model.selectable.iter().position(|&r| r == row_idx) {
                        self.selected = pos;
                        self.focus = Focus::Agent;
                    }
                }
                MouseEventKind::ScrollUp => {
                    if let Some(agent) = self.current_agent_mut(model) {
                        agent.scroll_up(1);
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let Some(agent) = self.current_agent_mut(model) {
                        agent.scroll_down(1);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }

    fn handle_usage_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc | KeyCode::Char('u') => {
                self.showing_usage = false;
            }
            KeyCode::Char('r') => {
                self.refresh_usage_all();
            }
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') if k.modifiers.contains(event::KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            _ => {}
        }
    }

    fn current_agent_mut(&mut self, model: &ui::RowModel) -> Option<&mut Agent> {
        let row_idx = *model.selectable.get(self.selected)?;
        let ui::Row::Agent(agent_idx) = model.rows.get(row_idx)? else {
            return None;
        };
        self.agents.get_mut(*agent_idx)
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
                let providers = vec![
                    Provider::Claude,
                    Provider::Codex,
                    Provider::Gemini,
                    Provider::Aider,
                    Provider::Shell,
                    Provider::Other,
                ];
                let (provider_idx, cwd) = if let Some(ai) = self.agent_idx_at_selected(model) {
                    let a = &self.agents[ai];
                    let idx = providers.iter().position(|&p| p == a.provider).unwrap_or(0);
                    let cwd = a.cwd_label.clone().unwrap_or_else(|| "~".into());
                    (idx, cwd)
                } else {
                    (0, "~".into())
                };
                let cursor = cwd.chars().count();
                self.adding = Some(AddingState {
                    providers,
                    selected_provider: provider_idx,
                    cwd,
                    cursor,
                    field: AddingField::Provider,
                });
            }
            Action::RemoveAgent => {
                if let Some(ai) = self.agent_idx_at_selected(model)
                    && let Some(a) = self.agents.get_mut(ai)
                {
                    a.kill();
                    self.agents.remove(ai);
                }
            }
            Action::RenameAgent => {
                if let Some(ai) = self.agent_idx_at_selected(model) {
                    let a = &self.agents[ai];
                    self.renaming = Some(RenamingState {
                        name: a.name.clone(),
                        cursor: a.name.chars().count(),
                    });
                }
            }
            Action::CycleSort => {
                self.sort_mode = match self.sort_mode {
                    ui::SortMode::Provider => ui::SortMode::Status,
                    ui::SortMode::Status => ui::SortMode::Created,
                    ui::SortMode::Created => ui::SortMode::Provider,
                };
            }
            Action::ToggleView => {
                self.view_mode = match self.view_mode {
                    ViewMode::Single => ViewMode::Grid,
                    ViewMode::Grid => ViewMode::Single,
                };
                self.resync_pty_sizes();
            }
            Action::ToggleUsage => {
                self.showing_usage = !self.showing_usage;
                if self.showing_usage {
                    self.maybe_tick_usage_refresh();
                }
            }
            Action::ToggleHelp => {
                self.help_visible = !self.help_visible;
            }
            Action::ScrollUp | Action::ScrollDown | Action::ScrollTop | Action::ScrollBottom => {
                let pane_rows = self.last_pane_dims.1;
                if let Some(agent) = self.current_agent_mut(model) {
                    apply_scroll_action(agent, action, pane_rows);
                }
            }
            Action::FocusNextWaiting => {
                let n = model.selectable.len();
                if n > 0 {
                    for i in 1..=n {
                        let idx = (self.selected + i) % n;
                        if let Some(agent_idx) = self.agent_idx_at_selectable(model, idx) {
                            let state = crate::state::detect(&self.agents[agent_idx]);
                            if state == crate::state::LiveState::Waiting {
                                self.selected = idx;
                                self.focus = Focus::Agent;
                                break;
                            }
                        }
                    }
                }
            }
            // Nav actions (Quit/MoveUp/MoveDown/FocusAgent/FocusIndex/ToggleFocus/None)
            // are already handled above via `apply_nav_action`.
            _ => {}
        }
        Ok(())
    }

    fn resync_pty_sizes(&mut self) {
        let (term_cols, term_rows) = match crossterm::terminal::size() {
            Ok(s) => s,
            Err(_) => return,
        };
        let pane = agent_pane_size(self.view_mode, self.grid_dims, term_cols, term_rows);
        self.last_pane_dims = (pane.cols, pane.rows);
        for a in self.agents.iter_mut() {
            a.resize(pane.rows, pane.cols);
        }
    }

    fn agent_idx_at_selected(&self, model: &ui::RowModel) -> Option<usize> {
        agent_idx_at(self.selected, model)
    }

    fn agent_idx_at_selectable(
        &self,
        model: &ui::RowModel,
        selectable_idx: usize,
    ) -> Option<usize> {
        agent_idx_at(selectable_idx, model)
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
        event::KeyCode::Tab => {
            state.field = match state.field {
                AddingField::Provider => AddingField::Cwd,
                AddingField::Cwd => AddingField::Provider,
            };
            AddingResult::None
        }
        event::KeyCode::Char('c') if k.modifiers.contains(event::KeyModifiers::CONTROL) => {
            AddingResult::Cancel
        }
        event::KeyCode::Left => {
            match state.field {
                AddingField::Provider => {
                    if state.selected_provider > 0 {
                        state.selected_provider -= 1;
                    } else {
                        state.selected_provider = state.providers.len() - 1;
                    }
                }
                AddingField::Cwd => {
                    if state.cursor > 0 {
                        state.cursor -= 1;
                    }
                }
            }
            AddingResult::None
        }
        event::KeyCode::Right => {
            match state.field {
                AddingField::Provider => {
                    state.selected_provider = (state.selected_provider + 1) % state.providers.len();
                }
                AddingField::Cwd => {
                    if state.cursor < state.cwd.chars().count() {
                        state.cursor += 1;
                    }
                }
            }
            AddingResult::None
        }
        event::KeyCode::Char(c) if state.field == AddingField::Cwd => {
            let idx = byte_index_for_char_cursor(&state.cwd, state.cursor);
            state.cwd.insert(idx, c);
            state.cursor += 1;
            AddingResult::None
        }
        event::KeyCode::Backspace if state.field == AddingField::Cwd && state.cursor > 0 => {
            let end = byte_index_for_char_cursor(&state.cwd, state.cursor);
            let start = byte_index_for_char_cursor(&state.cwd, state.cursor - 1);
            state.cwd.drain(start..end);
            state.cursor -= 1;
            AddingResult::None
        }
        event::KeyCode::Home if state.field == AddingField::Cwd => {
            state.cursor = 0;
            AddingResult::None
        }
        event::KeyCode::End if state.field == AddingField::Cwd => {
            state.cursor = state.cwd.chars().count();
            AddingResult::None
        }
        _ => AddingResult::None,
    }
}

/// Try to spawn every non-manual agent in `agent_configs`. Returns the
/// successfully-spawned agents, a list of failures (id + provider + error
/// string) for surfacing in the UI, and the next free `RuntimeId`. Failures
/// also continue to be emitted via `tracing::error!` for log inspection.
fn spawn_configured_agents(
    agent_configs: &[AgentConfig],
    initial_size: PtySize,
    agent_tx: &Sender<AgentEvent>,
    starting_rid: RuntimeId,
) -> (Vec<Agent>, Vec<SpawnFailure>, RuntimeId) {
    let mut next_rid = starting_rid;
    let mut agents: Vec<Agent> = Vec::with_capacity(agent_configs.len());
    let mut failures: Vec<SpawnFailure> = Vec::new();
    for ac in agent_configs.iter() {
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
                failures.push(SpawnFailure {
                    id: ac.id.clone(),
                    provider: ac.provider,
                    error: format!("{e:#}"),
                });
            }
        }
    }
    (agents, failures, next_rid)
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

    let mut cfg = template.unwrap_or_else(|| {
        let command = match provider {
            Provider::Shell => std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()),
            _ => provider.tag().to_string(),
        };
        AgentConfig {
            id: format!("{}-{rid}", provider.tag()),
            name: None,
            provider,
            command,
            args: vec![],
            cwd: None,
            env: Default::default(),
            manual: false,
        }
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
        Provider::Shell => "Shell",
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
        Action::AddAgent
        | Action::RemoveAgent
        | Action::RenameAgent
        | Action::CycleSort
        | Action::ToggleView
        | Action::ToggleUsage
        | Action::ToggleHelp
        | Action::FocusNextWaiting
        | Action::ScrollUp
        | Action::ScrollDown
        | Action::ScrollTop
        | Action::ScrollBottom => return false,
    }
    true
}

/// Apply a scroll action to a single agent. Page-sized scrolls move by
/// `pane_rows - 2` (minus the top/bottom border) with a floor of 1. Returns
/// `true` if `action` was a scroll variant, `false` otherwise.
fn apply_scroll_action(agent: &mut Agent, action: Action, pane_rows: u16) -> bool {
    let page = pane_rows.saturating_sub(2).max(1);
    match action {
        Action::ScrollUp => agent.scroll_up(page),
        Action::ScrollDown => agent.scroll_down(page),
        Action::ScrollTop => agent.scroll_offset = crate::agent::MAX_SCROLLBACK,
        Action::ScrollBottom => agent.scroll_offset = 0,
        _ => return false,
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
        apply_nav_action(
            map_deck_key(ev, None),
            &mut st.0,
            &mut st.1,
            &mut st.2,
            model,
        );
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
    fn unparseable_toggle_key_returns_none() {
        // Precondition for the warn-log in App::new: parse_key must return None
        // for obviously-bad strings. The log emission itself is not unit-testable
        // without a tracing fixture; the user-visible signal is the log entry at
        // ~/.local/state/agentdeck/agentdeck.log.
        assert!(keymap::parse_key("ctrl-spacebar").is_none());
    }

    #[test]
    fn spawn_configured_agents_collects_failures_alongside_successes() {
        use crate::config::AgentConfig;
        use std::collections::BTreeMap;

        let (tx, _rx) = unbounded::<AgentEvent>();
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let make = |id: &str, provider, command: &str| AgentConfig {
            id: id.into(),
            name: None,
            provider,
            command: command.into(),
            args: vec![],
            cwd: None,
            env: BTreeMap::new(),
            manual: false,
        };
        let configs = vec![
            make("ok", Provider::Shell, "true"),
            make("typo", Provider::Claude, "/nonexistent/path/claud"),
        ];
        let (mut spawned, failures, next_rid) = spawn_configured_agents(&configs, size, &tx, 1);
        assert_eq!(spawned.len(), 1, "the valid `true` agent should spawn");
        assert_eq!(failures.len(), 1, "the invalid path agent should fail");
        assert_eq!(failures[0].id, "typo");
        assert_eq!(failures[0].provider, Provider::Claude);
        assert!(
            !failures[0].error.is_empty(),
            "failure should carry a non-empty error string"
        );
        // Each config consumed a rid even if the spawn failed.
        assert_eq!(next_rid, 3);
        for a in spawned.iter_mut() {
            a.kill();
        }
    }

    #[test]
    fn apply_scroll_action_drives_mock_agent_through_each_variant() {
        use crate::agent::{MAX_SCROLLBACK, test_helpers::mock_agent};

        let mut agent = mock_agent(Provider::Claude, "alpha");
        assert_eq!(agent.scroll_offset, 0);

        // PgUp from live should jump by (pane_rows - 2).
        assert!(apply_scroll_action(
            &mut agent,
            Action::ScrollUp,
            20, /* pane_rows */
        ));
        assert_eq!(agent.scroll_offset, 18);

        // Second PgUp accumulates.
        apply_scroll_action(&mut agent, Action::ScrollUp, 20);
        assert_eq!(agent.scroll_offset, 36);

        // PgDn unwinds by a page.
        apply_scroll_action(&mut agent, Action::ScrollDown, 20);
        assert_eq!(agent.scroll_offset, 18);

        // Home jumps to the scrollback ceiling; subsequent PgUp is a no-op.
        apply_scroll_action(&mut agent, Action::ScrollTop, 20);
        assert_eq!(agent.scroll_offset, MAX_SCROLLBACK);
        apply_scroll_action(&mut agent, Action::ScrollUp, 20);
        assert_eq!(agent.scroll_offset, MAX_SCROLLBACK);

        // End snaps back to live.
        apply_scroll_action(&mut agent, Action::ScrollBottom, 20);
        assert_eq!(agent.scroll_offset, 0);

        // PgDn at live stays at 0 (saturating_sub).
        apply_scroll_action(&mut agent, Action::ScrollDown, 20);
        assert_eq!(agent.scroll_offset, 0);

        // Tiny pane_rows still produces a page floor of 1.
        apply_scroll_action(&mut agent, Action::ScrollUp, 1);
        assert_eq!(agent.scroll_offset, 1);

        // Non-scroll actions return false and leave the agent untouched.
        let before = agent.scroll_offset;
        assert!(!apply_scroll_action(&mut agent, Action::Quit, 20));
        assert_eq!(agent.scroll_offset, before);
    }

    #[test]
    fn ctrl_space_toggles_focus() {
        // With no configured toggle_key, map_deck_key falls back to Ctrl-Space.
        let model = fake_model(1);
        let mut st = (0, Focus::Deck, false);
        apply(KeyCode::Char(' '), KeyModifiers::CONTROL, &mut st, &model);
        assert_eq!(st.1, Focus::Agent);
        apply(KeyCode::Char(' '), KeyModifiers::CONTROL, &mut st, &model);
        assert_eq!(st.1, Focus::Deck);
    }
}
