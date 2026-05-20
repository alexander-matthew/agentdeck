use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::agent::{Agent, Status};
use crate::config::Provider;
use crate::state::{self, LiveState};
use crate::usage::UsageState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Deck,
    Agent,
}

/// Layout of the right-hand pane area: a single full-size agent (the
/// historical default) or a grid of agent cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Single,
    Grid,
}

/// One displayable row in the sidebar.
pub enum Row {
    Header(Provider, usize),
    Agent(usize),
}

pub struct RowModel {
    pub rows: Vec<Row>,
    /// Indices into `rows` that point at `Row::Agent` entries. Cursor
    /// navigation and digit-key bindings operate against this list.
    pub selectable: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    Provider,
    Status,
    Created,
}

pub fn build_rows(agents: &[Agent], sort: SortMode) -> RowModel {
    let mut rows: Vec<Row> = Vec::new();
    let mut selectable: Vec<usize> = Vec::new();

    match sort {
        SortMode::Provider => {
            let order = [
                Provider::Claude,
                Provider::Codex,
                Provider::Gemini,
                Provider::Aider,
                Provider::Shell,
                Provider::Other,
            ];
            for p in order {
                let group: Vec<usize> = agents
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| a.provider == p)
                    .map(|(i, _)| i)
                    .collect();
                if group.is_empty() {
                    continue;
                }
                rows.push(Row::Header(p, group.len()));
                for ai in group {
                    selectable.push(rows.len());
                    rows.push(Row::Agent(ai));
                }
            }
        }
        SortMode::Status => {
            // Sort agents by status, but keep original indices for Row::Agent
            let mut indices: Vec<usize> = (0..agents.len()).collect();
            indices.sort_by_key(|&i| match agents[i].status {
                Status::Running => 0,
                Status::Exited(_) => 1,
                Status::SpawnFailed => 2,
            });
            for ai in indices {
                selectable.push(rows.len());
                rows.push(Row::Agent(ai));
            }
        }
        SortMode::Created => {
            let mut indices: Vec<usize> = (0..agents.len()).collect();
            indices.sort_by_key(|&i| agents[i].spawned_at);
            for ai in indices {
                selectable.push(rows.len());
                rows.push(Row::Agent(ai));
            }
        }
    }
    RowModel { rows, selectable }
}

pub struct AddModalState<'a> {
    pub providers: &'a [Provider],
    pub selected_provider: usize,
    pub cwd: &'a str,
    pub cursor: usize,
}

pub struct RenameModalState<'a> {
    pub name: &'a str,
    pub cursor: usize,
}

#[allow(clippy::too_many_arguments)]
pub fn draw_main(
    f: &mut Frame,
    agents: &[Agent],
    model: &RowModel,
    selected_in_selectable: usize,
    focus: Focus,
    view_mode: ViewMode,
    grid_dims: (u16, u16),
    visible_agents: &[usize],
    showing_usage: bool,
    usage_state: &UsageState,
    footer: &str,
    modal: Option<AddModalState>,
    rename_modal: Option<RenameModalState>,
    tk_label: &str,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    // Top status bar.
    f.render_widget(header_widget(agents.len(), focus, tk_label), chunks[0]);

    // Sidebar + main pane.
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(36), Constraint::Min(0)])
        .split(chunks[1]);

    render_sidebar(f, agents, model, selected_in_selectable, focus, body[0]);

    let focused_agent_idx = model
        .selectable
        .get(selected_in_selectable)
        .and_then(|&row_idx| match model.rows.get(row_idx) {
            Some(Row::Agent(ai)) => Some(*ai),
            _ => None,
        });

    if showing_usage {
        render_usage_pane(f, usage_state, body[1]);
    } else {
        match view_mode {
            ViewMode::Single => {
                let focused_agent = focused_agent_idx.and_then(|i| agents.get(i));
                render_agent_pane(f, focused_agent, focus, body[1]);
            }
            ViewMode::Grid => {
                render_agent_grid(
                    f,
                    agents,
                    visible_agents,
                    focused_agent_idx,
                    focus,
                    grid_dims,
                    body[1],
                );
            }
        }
    }

    // Bottom hint bar.
    f.render_widget(
        Paragraph::new(Span::styled(
            footer.to_string(),
            Style::default().fg(Color::DarkGray),
        )),
        chunks[2],
    );

    if let Some(m) = modal {
        render_add_modal(f, m);
    }
    if let Some(m) = rename_modal {
        render_rename_modal(f, m);
    }
}

fn header_widget(n_agents: usize, focus: Focus, tk_label: &str) -> Paragraph<'static> {
    let focus_chip = match focus {
        Focus::Deck => Span::styled(
            " focus: deck ",
            Style::default()
                .bg(Color::Yellow)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Focus::Agent => Span::styled(
            " focus: agent ",
            Style::default()
                .bg(Color::Green)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
    };
    Paragraph::new(Line::from(vec![
        Span::styled(
            " agentdeck ",
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        focus_chip,
        Span::raw("  "),
        Span::styled(
            format!("[{n_agents} agents]"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(format!("   {} to toggle focus ", tk_label)),
    ]))
}

fn render_sidebar(
    f: &mut Frame,
    agents: &[Agent],
    model: &RowModel,
    selected_in_selectable: usize,
    focus: Focus,
    area: Rect,
) {
    let selected_row = model.selectable.get(selected_in_selectable).copied();

    let mut key_for_agent_index: std::collections::HashMap<usize, char> =
        std::collections::HashMap::new();
    for (n, &row_idx) in model.selectable.iter().enumerate().take(9) {
        if let Some(Row::Agent(ai)) = model.rows.get(row_idx) {
            key_for_agent_index.insert(*ai, char::from(b'1' + n as u8));
        }
    }

    let items: Vec<ListItem> = model
        .rows
        .iter()
        .map(|row| match row {
            Row::Header(p, count) => ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", p.tag()),
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("({count})"), Style::default().fg(Color::DarkGray)),
            ])),
            Row::Agent(ai) => {
                let a = &agents[*ai];
                let live = state::detect(a);
                let (dot, dot_color) = status_dot(a, live);
                let key = key_for_agent_index
                    .get(ai)
                    .map(|c| format!("  {c} "))
                    .unwrap_or_else(|| "    ".to_string());
                ListItem::new(Line::from(vec![
                    Span::styled(key, Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{dot} "), Style::default().fg(dot_color)),
                    Span::styled(
                        format!("{:<14}", truncate(&a.name, 14)),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {:>8}", live.short()), state_color(live)),
                ]))
            }
        })
        .collect();

    let border_style = if focus == Focus::Deck {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(" agents "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶");

    let mut s = ListState::default();
    s.select(selected_row);
    f.render_stateful_widget(list, area, &mut s);
}

fn status_dot(a: &Agent, live: LiveState) -> (&'static str, Color) {
    match a.status {
        Status::Running => match live {
            LiveState::Waiting => ("●", Color::Green),
            LiveState::Working | LiveState::Thinking => ("●", Color::Yellow),
            LiveState::Stuck => ("●", Color::Red),
            LiveState::Starting => ("●", Color::Cyan),
            LiveState::Idle => ("●", Color::Gray),
            LiveState::Exited(0) => ("○", Color::DarkGray),
            LiveState::Exited(_) => ("●", Color::Red),
        },
        Status::Exited(0) => ("○", Color::DarkGray),
        Status::Exited(_) => ("●", Color::Red),
        Status::SpawnFailed => ("✕", Color::Red),
    }
}

fn state_color(live: LiveState) -> Style {
    match live {
        LiveState::Starting => Style::default().fg(Color::Cyan),
        LiveState::Working => Style::default().fg(Color::Yellow),
        LiveState::Thinking => Style::default().fg(Color::Magenta),
        LiveState::Idle => Style::default().fg(Color::Gray),
        LiveState::Waiting => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        LiveState::Stuck => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        LiveState::Exited(0) => Style::default().fg(Color::DarkGray),
        LiveState::Exited(_) => Style::default().fg(Color::Red),
    }
}

fn render_agent_pane(f: &mut Frame, agent: Option<&Agent>, focus: Focus, area: Rect) {
    let is_input_target = focus == Focus::Agent;
    render_agent_cell(f, agent, is_input_target, area);
}

/// Render a single agent into `area`. `is_input_target` controls border color
/// (green vs dark) and whether the terminal cursor gets parked in this cell.
fn render_agent_cell(f: &mut Frame, agent: Option<&Agent>, is_input_target: bool, area: Rect) {
    let border_style = if is_input_target {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title = match agent {
        Some(a) => {
            let live = state::detect(a);
            let mut label = format!(" {} · {} · {} ", a.name, a.status.label(), live.short());
            if a.scroll_offset > 0 {
                let screen = a.parser.screen();
                let (grid_rows, _) = screen.size();
                let m = screen.scrollback() + grid_rows as usize;
                if m > 0 {
                    label.push_str(&format!(
                        " [scrolled {}/{}] (End to jump to live) ",
                        a.scroll_offset, m
                    ));
                } else {
                    label.push_str(&format!(
                        " [scrolled {}] (End to jump to live) ",
                        a.scroll_offset
                    ));
                }
            }
            label
        }
        None => " no agent ".into(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(agent) = agent else { return };
    let screen = agent.parser.screen();
    let (grid_rows, grid_cols) = screen.size();

    let view_rows = inner.height.min(grid_rows);
    let view_cols = inner.width.min(grid_cols);

    let scrollback_rows = screen.scrollback() as i32;
    let effective_offset = agent.scroll_offset.min(scrollback_rows as u16);

    let total_rows = scrollback_rows + (grid_rows as i32);
    let end_row = total_rows - (effective_offset as i32);
    let start_row = (end_row - (view_rows as i32)).max(0);

    let mut lines: Vec<Line> = Vec::with_capacity(view_rows as usize);
    for r in start_row..end_row {
        lines.push(row_to_line(screen, r, view_cols));
    }
    f.render_widget(Paragraph::new(lines), inner);

    if is_input_target && agent.scroll_offset == 0 {
        let (cur_row, cur_col) = screen.cursor_position();
        if cur_row < view_rows && cur_col < view_cols {
            let x = inner.x + cur_col;
            let y = inner.y + cur_row;
            f.set_cursor_position(Position { x, y });
        }
    }
}

/// Render up to `grid_dims.0 * grid_dims.1` agents into a grid of cells.
/// `focused_agent_idx` is the input target — that cell gets the highlighted
/// border and the terminal cursor.
fn render_agent_grid(
    f: &mut Frame,
    agents: &[Agent],
    visible: &[usize],
    focused_agent_idx: Option<usize>,
    focus: Focus,
    grid_dims: (u16, u16),
    area: Rect,
) {
    let (rows, cols) = (grid_dims.0.max(1), grid_dims.1.max(1));

    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Ratio(1, rows as u32))
        .collect();
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    let col_constraints: Vec<Constraint> = (0..cols)
        .map(|_| Constraint::Ratio(1, cols as u32))
        .collect();

    for r in 0..rows {
        let cells = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints.clone())
            .split(row_areas[r as usize]);
        for c in 0..cols {
            let cell_idx = (r as usize) * (cols as usize) + c as usize;
            let cell_area = cells[c as usize];
            let agent = visible.get(cell_idx).and_then(|&ai| agents.get(ai));
            let is_target = match (focused_agent_idx, visible.get(cell_idx)) {
                (Some(fi), Some(&ai)) => fi == ai && focus == Focus::Agent,
                _ => false,
            };
            render_agent_cell(f, agent, is_target, cell_area);
        }
    }
}

/// Render the centralized usage dashboard as a stack of provider cards.
fn render_usage_pane(f: &mut Frame, usage: &UsageState, area: Rect) {
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " usage · subscriptions ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    if usage.is_empty() {
        let hint = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  No usage commands configured.",
                Style::default().fg(Color::Gray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Add entries under [usage_commands] in your config, e.g.:",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "      claude = \"npx -y ccusage@latest --json\"",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "      codex  = \"my-codex-usage-script\"",
                Style::default().fg(Color::DarkGray),
            )),
        ]);
        f.render_widget(hint, inner);
        return;
    }

    let n = usage.entries.len() as u32;
    let constraints: Vec<Constraint> = (0..n).map(|_| Constraint::Ratio(1, n.max(1))).collect();
    let card_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    for (i, (_, entry)) in usage.entries.iter().enumerate() {
        let title = if entry.refreshing {
            format!(" {} · refreshing… ", entry.provider)
        } else {
            match entry.last_run_at {
                Some(t) => format!(" {} · {} ago ", entry.provider, ago(t.elapsed())),
                None => format!(" {} · never run ", entry.provider),
            }
        };
        let border = if entry.last_error.is_some() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(Span::styled(
                title,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        let card_inner = block.inner(card_areas[i]);
        f.render_widget(block, card_areas[i]);

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            format!("  $ {}", entry.command),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
        if let Some(err) = &entry.last_error {
            lines.push(Line::from(Span::styled(
                format!("  ! {}", err),
                Style::default().fg(Color::Red),
            )));
            lines.push(Line::from(""));
        }
        if let Some(out) = &entry.last_output {
            for raw in out.lines() {
                lines.push(Line::from(Span::raw(format!("  {raw}"))));
            }
        } else if entry.last_error.is_none() {
            lines.push(Line::from(Span::styled(
                "  (waiting for first run)",
                Style::default().fg(Color::Gray),
            )));
        }
        f.render_widget(Paragraph::new(lines), card_inner);
    }
}

fn ago(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn row_to_line(screen: &vt100::Screen, row: i32, cols: u16) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run_style: Option<Style> = None;
    let mut run_text = String::new();

    let flush = |spans: &mut Vec<Span<'static>>, style: Option<Style>, text: &mut String| {
        if !text.is_empty() {
            spans.push(Span::styled(
                std::mem::take(text),
                style.unwrap_or_default(),
            ));
        }
    };

    for c in 0..cols {
        let Some(cell) = screen.cell(row as u16, c) else {
            flush(&mut spans, run_style, &mut run_text);
            run_style = None;
            continue;
        };
        let contents = cell.contents();
        let style = cell_to_style(cell);
        if run_style != Some(style) {
            flush(&mut spans, run_style, &mut run_text);
            run_style = Some(style);
        }
        if contents.is_empty() {
            run_text.push(' ');
        } else {
            run_text.push_str(&contents);
        }
    }
    flush(&mut spans, run_style, &mut run_text);
    Line::from(spans)
}

fn cell_to_style(cell: &vt100::Cell) -> Style {
    let mut s = Style::default();
    let fg = vt_color_to_ratatui(cell.fgcolor());
    let bg = vt_color_to_ratatui(cell.bgcolor());
    if let Some(c) = fg {
        s = s.fg(c);
    }
    if let Some(c) = bg {
        s = s.bg(c);
    }
    if cell.bold() {
        s = s.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        s = s.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        s = s.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        s = s.add_modifier(Modifier::REVERSED);
    }
    s
}

fn vt_color_to_ratatui(c: vt100::Color) -> Option<Color> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(match i {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            7 => Color::Gray,
            8 => Color::DarkGray,
            9 => Color::LightRed,
            10 => Color::LightGreen,
            11 => Color::LightYellow,
            12 => Color::LightBlue,
            13 => Color::LightMagenta,
            14 => Color::LightCyan,
            15 => Color::White,
            n => Color::Indexed(n),
        }),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

fn render_add_modal(f: &mut Frame, m: AddModalState) {
    let area = f.area();
    let width = area.width.clamp(40, 72);
    let height = 11u16;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let rect = Rect {
        x,
        y,
        width,
        height,
    };

    f.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " add agent ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1), // Provider selector
            Constraint::Length(1),
            Constraint::Length(1), // CWD label
            Constraint::Length(1), // CWD field
            Constraint::Length(1),
            Constraint::Length(1), // Hints
            Constraint::Min(0),
        ])
        .split(inner);

    // Provider selection
    let mut provider_spans = vec![Span::raw(" variety: ")];
    for (i, p) in m.providers.iter().enumerate() {
        let style = if i == m.selected_provider {
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        provider_spans.push(Span::styled(format!(" {} ", p.tag()), style));
        provider_spans.push(Span::raw(" "));
    }
    f.render_widget(Paragraph::new(Line::from(provider_spans)), layout[1]);

    f.render_widget(
        Paragraph::new(Span::raw(" Working directory for the new agent:")),
        layout[3],
    );

    let split = m.cursor.min(m.cwd.chars().count());
    let left: String = m.cwd.chars().take(split).collect();
    let right: String = m.cwd.chars().skip(split).collect();
    let field = Line::from(vec![
        Span::raw(" cwd: "),
        Span::raw(left),
        Span::styled(
            "│",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::SLOW_BLINK),
        ),
        Span::raw(right),
    ]);
    f.render_widget(Paragraph::new(field), layout[4]);

    f.render_widget(
        Paragraph::new(Span::styled(
            " ←/→: variety   Tab: switch field   Enter: spawn   Esc: cancel ",
            Style::default().fg(Color::DarkGray),
        )),
        layout[6],
    );
}

fn render_rename_modal(f: &mut Frame, m: RenameModalState) {
    let area = f.area();
    let width = area.width.clamp(40, 60);
    let height = 7u16;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let rect = Rect {
        x,
        y,
        width,
        height,
    };

    f.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " rename session ",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1), // Prompt
            Constraint::Length(1), // Field
            Constraint::Length(1), // Hints
            Constraint::Min(0),
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(Span::raw(" Enter a new name for this session:")),
        layout[1],
    );

    let split = m.cursor.min(m.name.chars().count());
    let left: String = m.name.chars().take(split).collect();
    let right: String = m.name.chars().skip(split).collect();
    let field = Line::from(vec![
        Span::raw(" name: "),
        Span::raw(left),
        Span::styled(
            "│",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::SLOW_BLINK),
        ),
        Span::raw(right),
    ]);
    f.render_widget(Paragraph::new(field), layout[2]);

    f.render_widget(
        Paragraph::new(Span::styled(
            " Enter: save   Esc: cancel ",
            Style::default().fg(Color::DarkGray),
        )),
        layout[3],
    );
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::test_helpers::mock_agent;
    use crate::usage::{UsageEvent, UsageState};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::collections::BTreeMap;
    use std::time::Instant;

    fn buf_lines(term: &Terminal<TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer();
        let (w, h) = (buf.area.width, buf.area.height);
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect()
    }

    #[test]
    fn draw_main_80x24_renders_header_sidebar_and_footer() {
        let agents = vec![
            mock_agent(Provider::Claude, "alpha"),
            mock_agent(Provider::Codex, "bravo"),
        ];
        let model = build_rows(&agents, SortMode::Provider);
        let visible: Vec<usize> = (0..agents.len()).collect();
        let usage = UsageState::default();
        let mut term = Terminal::new(TestBackend::new(80, 24)).expect("backend");
        term.draw(|f| {
            draw_main(
                f,
                &agents,
                &model,
                0,
                Focus::Deck,
                ViewMode::Single,
                (2, 2),
                &visible,
                false,
                &usage,
                " footer ",
                None,
                None,
                "Ctrl-Space",
            )
        })
        .expect("draw");

        let lines = buf_lines(&term);
        assert_eq!(lines.len(), 24);
        assert!(
            lines[0].contains("agentdeck")
                && lines[0].contains("focus: deck")
                && lines[0].contains("[2 agents]"),
            "header row: {:?}",
            lines[0]
        );
        assert!(
            lines[1].contains("agents"),
            "sidebar title row: {:?}",
            lines[1]
        );
        let body = lines[2..23].join("\n");
        for needle in ["claude", "codex", "alpha", "bravo"] {
            assert!(body.contains(needle), "sidebar missing {needle:?}");
        }
        assert!(lines[23].contains("footer"), "footer row: {:?}", lines[23]);
    }

    #[test]
    fn draw_main_grid_renders_all_visible_agent_names_in_tiles() {
        let agents = vec![
            mock_agent(Provider::Claude, "alpha"),
            mock_agent(Provider::Codex, "bravo"),
            mock_agent(Provider::Gemini, "charlie"),
            mock_agent(Provider::Aider, "delta"),
        ];
        let model = build_rows(&agents, SortMode::Provider);
        let visible: Vec<usize> = (0..agents.len()).collect();
        let usage = UsageState::default();
        let mut term = Terminal::new(TestBackend::new(120, 30)).expect("backend");
        term.draw(|f| {
            draw_main(
                f,
                &agents,
                &model,
                0,
                Focus::Deck,
                ViewMode::Grid,
                (2, 2),
                &visible,
                false,
                &usage,
                " footer ",
                None,
                None,
                "Ctrl-Space",
            )
        })
        .expect("draw");

        let lines = buf_lines(&term);
        assert_eq!(lines.len(), 30);
        let body = lines[1..29].join("\n");
        for needle in ["alpha", "bravo", "charlie", "delta"] {
            assert!(body.contains(needle), "grid body missing {needle:?}");
        }
    }

    #[test]
    fn draw_main_usage_dashboard_renders_each_provider_card() {
        let agents = vec![mock_agent(Provider::Claude, "alpha")];
        let model = build_rows(&agents, SortMode::Provider);
        let visible: Vec<usize> = (0..agents.len()).collect();

        let mut cmds = BTreeMap::new();
        cmds.insert("claude".to_string(), "echo hi".to_string());
        cmds.insert("codex".to_string(), "echo bye".to_string());
        let mut usage = UsageState::from_commands(&cmds);
        usage.apply(UsageEvent::Result {
            provider: "claude".to_string(),
            output: "spend: $1.23".to_string(),
            error: None,
            at: Instant::now(),
        });

        let mut term = Terminal::new(TestBackend::new(120, 30)).expect("backend");
        term.draw(|f| {
            draw_main(
                f,
                &agents,
                &model,
                0,
                Focus::Deck,
                ViewMode::Single,
                (1, 1),
                &visible,
                true,
                &usage,
                " footer ",
                None,
                None,
                "Ctrl-Space",
            )
        })
        .expect("draw");

        let lines = buf_lines(&term);
        let all = lines.join("\n");
        for needle in ["claude", "codex", "spend: $1.23"] {
            assert!(all.contains(needle), "usage pane missing {needle:?}");
        }
    }

    #[test]
    fn draw_main_renders_scrolled_position_indicator_and_hint() {
        let mut alpha = mock_agent(Provider::Claude, "alpha");
        // Feed enough lines to push rows into vt100 scrollback (>24 visible rows).
        let mut bytes = String::new();
        for i in 0..40 {
            bytes.push_str(&format!("line {i}\r\n"));
        }
        alpha.feed(bytes.as_bytes());
        alpha.scroll_offset = 3;

        let agents = vec![alpha];
        let model = build_rows(&agents, SortMode::Provider);
        let visible: Vec<usize> = (0..agents.len()).collect();
        let usage = UsageState::default();
        let mut term = Terminal::new(TestBackend::new(120, 24)).expect("backend");
        term.draw(|f| {
            draw_main(
                f,
                &agents,
                &model,
                0,
                Focus::Agent,
                ViewMode::Single,
                (1, 1),
                &visible,
                false,
                &usage,
                " footer ",
                None,
                None,
                "Ctrl-Space",
            )
        })
        .expect("draw");

        let lines = buf_lines(&term);
        let all = lines.join("\n");
        assert!(
            all.contains("[scrolled 3/"),
            "missing scrolled N/M indicator: {all}"
        );
        assert!(
            all.contains("(End to jump to live)"),
            "missing jump-to-live hint: {all}"
        );
    }

    #[test]
    fn draw_main_no_scroll_indicator_when_offset_zero() {
        let agents = vec![mock_agent(Provider::Claude, "alpha")];
        let model = build_rows(&agents, SortMode::Provider);
        let visible: Vec<usize> = (0..agents.len()).collect();
        let usage = UsageState::default();
        let mut term = Terminal::new(TestBackend::new(120, 24)).expect("backend");
        term.draw(|f| {
            draw_main(
                f,
                &agents,
                &model,
                0,
                Focus::Agent,
                ViewMode::Single,
                (1, 1),
                &visible,
                false,
                &usage,
                " footer ",
                None,
                None,
                "Ctrl-Space",
            )
        })
        .expect("draw");

        let all = buf_lines(&term).join("\n");
        assert!(!all.contains("scrolled"), "unexpected scroll text: {all}");
        assert!(
            !all.contains("(End to jump to live)"),
            "unexpected hint: {all}"
        );
    }

    #[test]
    fn draw_main_agent_focus_chip_switches_color_label() {
        let agents = vec![mock_agent(Provider::Claude, "alpha")];
        let model = build_rows(&agents, SortMode::Provider);
        let visible: Vec<usize> = (0..agents.len()).collect();
        let usage = UsageState::default();

        let render = |focus: Focus| -> Vec<String> {
            let mut term = Terminal::new(TestBackend::new(80, 24)).expect("backend");
            term.draw(|f| {
                draw_main(
                    f,
                    &agents,
                    &model,
                    0,
                    focus,
                    ViewMode::Single,
                    (1, 1),
                    &visible,
                    false,
                    &usage,
                    " footer ",
                    None,
                    None,
                    "Ctrl-Space",
                )
            })
            .expect("draw");
            buf_lines(&term)
        };

        let deck_lines = render(Focus::Deck);
        assert!(
            deck_lines[0].contains("focus: deck"),
            "deck header: {:?}",
            deck_lines[0]
        );

        let agent_lines = render(Focus::Agent);
        assert!(
            agent_lines[0].contains("focus: agent"),
            "agent header: {:?}",
            agent_lines[0]
        );
    }
}
