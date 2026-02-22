//! Conflicts tab widget — shows pending sync conflicts and resolution controls.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::vertical([Constraint::Min(10), Constraint::Length(8)]).split(area);

    // Conflict list
    if app.conflicts.is_empty() {
        let block = Block::default()
            .title(" Pending Conflicts ")
            .borders(Borders::ALL);
        let text = Paragraph::new("No conflicts detected. All files are in sync.")
            .block(block)
            .style(Style::default().fg(Color::Green));
        frame.render_widget(text, chunks[0]);
    } else {
        let items: Vec<ListItem> = app
            .conflicts
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let style = if i == app.conflict_selected {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let line = Line::from(vec![
                    Span::styled(
                        format!("{} ", if i == app.conflict_selected { ">" } else { " " }),
                        style,
                    ),
                    Span::styled(&c.rel_path, style),
                    Span::raw("  "),
                    Span::styled(
                        format!("{} vs {}", &c.local_device, &c.remote_device),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]);
                ListItem::new(line)
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .title(format!(" Pending Conflicts ({}) ", app.conflicts.len()))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red)),
        );
        frame.render_widget(list, chunks[0]);
    }

    // Detail panel (shows selected conflict details)
    let detail = if let Some(conflict) = app.conflicts.get(app.conflict_selected) {
        vec![
            Line::from(vec![
                Span::styled("Path:    ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(&conflict.rel_path),
            ]),
            Line::from(vec![
                Span::styled("Local:   ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!(
                    "{} ({})",
                    &conflict.local_device,
                    &conflict.local_hash[..16.min(conflict.local_hash.len())]
                )),
            ]),
            Line::from(vec![
                Span::styled("Remote:  ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!(
                    "{} ({})",
                    &conflict.remote_device,
                    &conflict.remote_hash[..16.min(conflict.remote_hash.len())]
                )),
            ]),
            Line::raw(""),
            Line::styled(
                "Keys: [l] keep local  [r] keep remote  [b] keep both  [j/k] navigate",
                Style::default().fg(Color::DarkGray),
            ),
        ]
    } else {
        vec![Line::styled(
            "Select a conflict to see details",
            Style::default().fg(Color::DarkGray),
        )]
    };

    let detail_widget =
        Paragraph::new(detail).block(Block::default().title(" Details ").borders(Borders::ALL));
    frame.render_widget(detail_widget, chunks[1]);
}
