use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::agent::{Agent, Status};

pub fn draw_overview(f: &mut Frame, agents: &[Agent], selected: usize, footer: &str) {
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
        .constraints([Constraint::Length(36), Constraint::Min(0)])
        .split(chunks[1]);

    render_agent_list(f, agents, selected, body[0]);
    render_preview(f, agents.get(selected), body[1]);

    let foot = Paragraph::new(Span::styled(
        footer.to_string(),
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(foot, chunks[2]);
}

fn render_agent_list(f: &mut Frame, agents: &[Agent], selected: usize, area: Rect) {
    let items: Vec<ListItem> = agents
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let key = if i < 9 {
                format!("{}", i + 1)
            } else {
                "·".into()
            };
            let (dot, dot_color) = match a.status {
                Status::Running => ("●", Color::Green),
                Status::Exited(0) => ("○", Color::DarkGray),
                Status::Exited(_) => ("●", Color::Red),
                Status::SpawnFailed => ("✕", Color::Red),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {key} "), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{dot} "), Style::default().fg(dot_color)),
                Span::styled(
                    format!("{:<14}", truncate(&a.name, 14)),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}", a.provider.tag()),
                    Style::default().fg(Color::Blue),
                ),
            ]))
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

    let mut state = ListState::default();
    state.select(Some(selected));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_preview(f: &mut Frame, agent: Option<&Agent>, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(match agent {
        Some(a) => format!(" preview · {} · {} ", a.name, a.status.label()),
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
