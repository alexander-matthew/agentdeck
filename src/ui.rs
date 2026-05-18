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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Deck,
    Agent,
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

pub fn build_rows(agents: &[Agent]) -> RowModel {
    let order = [
        Provider::Claude,
        Provider::Codex,
        Provider::Gemini,
        Provider::Aider,
        Provider::Other,
    ];
    let mut rows: Vec<Row> = Vec::new();
    let mut selectable: Vec<usize> = Vec::new();
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
    RowModel { rows, selectable }
}

pub struct AddModalState<'a> {
    pub provider: Provider,
    pub cwd: &'a str,
    pub cursor: usize,
}

pub fn draw_main(
    f: &mut Frame,
    agents: &[Agent],
    model: &RowModel,
    selected_in_selectable: usize,
    focus: Focus,
    footer: &str,
    modal: Option<AddModalState>,
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
    f.render_widget(header_widget(agents.len(), focus), chunks[0]);

    // Sidebar + agent pane.
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(36), Constraint::Min(0)])
        .split(chunks[1]);

    render_sidebar(f, agents, model, selected_in_selectable, focus, body[0]);

    let focused_agent =
        model
            .selectable
            .get(selected_in_selectable)
            .and_then(|&row_idx| match model.rows.get(row_idx) {
                Some(Row::Agent(ai)) => agents.get(*ai),
                _ => None,
            });
    render_agent_pane(f, focused_agent, focus, body[1]);

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
}

fn header_widget(n_agents: usize, focus: Focus) -> Paragraph<'static> {
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
        Span::raw("   F1 to toggle focus "),
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
    let border_style = if focus == Focus::Agent {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let title = match agent {
        Some(a) => {
            let live = state::detect(a);
            format!(" {} · {} · {} ", a.name, a.status.label(), live.short())
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
    let start_row = grid_rows.saturating_sub(view_rows);

    let mut lines: Vec<Line> = Vec::with_capacity(view_rows as usize);
    for r in start_row..(start_row + view_rows) {
        lines.push(row_to_line(screen, r, view_cols));
    }
    f.render_widget(Paragraph::new(lines), inner);

    // Position the visible cursor at the agent's cursor (subject to view clipping).
    if focus == Focus::Agent {
        let (cur_row, cur_col) = screen.cursor_position();
        if cur_row >= start_row && cur_row < start_row + view_rows && cur_col < view_cols {
            let x = inner.x + cur_col;
            let y = inner.y + (cur_row - start_row);
            f.set_cursor_position(Position { x, y });
        }
    }
}

fn row_to_line(screen: &vt100::Screen, row: u16, cols: u16) -> Line<'static> {
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
        let Some(cell) = screen.cell(row, c) else {
            flush(&mut spans, run_style, &mut run_text);
            run_style = None;
            continue;
        };
        let contents = cell.contents();
        if contents.is_empty() {
            run_text.push(' ');
            continue;
        }
        let style = cell_to_style(cell);
        if run_style != Some(style) {
            flush(&mut spans, run_style, &mut run_text);
            run_style = Some(style);
        }
        run_text.push_str(&contents);
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
        format!(" add {} agent ", m.provider.tag()),
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
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(Span::raw("Working directory for the new agent:")),
        layout[0],
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
    f.render_widget(Paragraph::new(field), layout[2]);

    f.render_widget(
        Paragraph::new(Span::styled(
            " Enter: spawn   Esc: cancel ",
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
