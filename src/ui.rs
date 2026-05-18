use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::agent::{Agent, Status};
use crate::config::Provider;
use crate::state::{self, LiveState};

/// One displayable row in the overview pane.
///
/// We rebuild this every frame from the current agent list so that adding /
/// removing agents at runtime "just works" — no separate ordering state to keep
/// in sync.
pub enum Row {
    Header(Provider, usize),
    Agent(usize),
}

pub struct RowModel {
    pub rows: Vec<Row>,
    /// Indices into `rows` that point at `Row::Agent` entries. Cursor navigation
    /// and digit-key bindings operate against this list.
    pub selectable: Vec<usize>,
}

pub fn build_rows(agents: &[Agent]) -> RowModel {
    let order = [
        Provider::Claude,
        Provider::Codex,
        Provider::Gemini,
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

pub fn draw_overview(
    f: &mut Frame,
    agents: &[Agent],
    model: &RowModel,
    selected_in_selectable: usize,
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

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " agentdeck ",
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  multi-provider agent control room  "),
        Span::styled(
            format!("[{} agents]", agents.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    f.render_widget(header, chunks[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(42), Constraint::Min(0)])
        .split(chunks[1]);

    render_agent_list(f, agents, model, selected_in_selectable, body[0]);
    let preview_agent =
        model
            .selectable
            .get(selected_in_selectable)
            .and_then(|&row_idx| match model.rows.get(row_idx) {
                Some(Row::Agent(ai)) => agents.get(*ai),
                _ => None,
            });
    render_preview(f, preview_agent, body[1]);

    let foot = Paragraph::new(Span::styled(
        footer.to_string(),
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(foot, chunks[2]);

    if let Some(m) = modal {
        render_add_modal(f, m);
    }
}

fn render_agent_list(
    f: &mut Frame,
    agents: &[Agent],
    model: &RowModel,
    selected_in_selectable: usize,
    area: Rect,
) {
    let selected_row = model.selectable.get(selected_in_selectable).copied();

    // Assign 1-9 number-key hints in selectable order.
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
                let badge_style = state_color(live);
                ListItem::new(Line::from(vec![
                    Span::styled(key, Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{dot} "), Style::default().fg(dot_color)),
                    Span::styled(
                        format!("{:<18}", truncate(&a.name, 18)),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {:>9}", live.short()), badge_style),
                ]))
            }
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" agents "))
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

fn render_preview(f: &mut Frame, agent: Option<&Agent>, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(match agent {
        Some(a) => {
            let live = state::detect(a);
            let cwd = a.cwd_label.clone().unwrap_or_else(|| "—".into());
            format!(
                " preview · {} · {} · {} · cwd {} ",
                a.name,
                a.status.label(),
                live.short(),
                cwd
            )
        }
        None => " preview ".into(),
    });

    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(agent) = agent else { return };
    let screen = agent.parser.screen();
    let (rows, _cols) = screen.size();
    let visible_rows = inner.height as usize;
    let start_row = rows.saturating_sub(visible_rows as u16);

    let mut lines: Vec<Line> = Vec::with_capacity(visible_rows);
    for r in start_row..rows {
        let mut spans: Vec<Span> = Vec::new();
        let row_cols = inner.width;
        for c in 0..row_cols {
            let cell = screen.cell(r, c);
            let ch = match cell {
                Some(cell) => {
                    let s = cell.contents();
                    if s.is_empty() {
                        " ".to_string()
                    } else {
                        s.to_string()
                    }
                }
                None => " ".to_string(),
            };
            spans.push(Span::raw(ch));
        }
        lines.push(Line::from(spans));
    }

    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(p, inner);
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

    // Render cwd field with a visible cursor indicator at `cursor`.
    let (left, right) = m.cwd.split_at(m.cursor.min(m.cwd.len()));
    let field = Line::from(vec![
        Span::raw(" cwd: "),
        Span::raw(left.to_string()),
        Span::styled(
            "│",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::SLOW_BLINK),
        ),
        Span::raw(right.to_string()),
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
