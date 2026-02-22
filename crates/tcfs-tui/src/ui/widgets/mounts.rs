use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Active Mounts ")
        .borders(Borders::ALL);

    let mount_count = app
        .daemon_status
        .as_ref()
        .map(|s| s.active_mounts)
        .unwrap_or(0);

    if !app.connected {
        let lines = vec![Line::from(Span::styled(
            "  Daemon not connected",
            Style::default().fg(Color::Red),
        ))];
        f.render_widget(Paragraph::new(lines).block(block), area);
        return;
    }

    if mount_count == 0 {
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "  No active FUSE mounts",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Use `tcfs mount <remote> <mountpoint>` to create one",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        f.render_widget(Paragraph::new(lines).block(block), area);
        return;
    }

    // When Mount RPC is implemented, this will show real data
    let header = Row::new(vec!["Mountpoint", "Remote", "Status"]).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = vec![Row::new(vec![
        "(mount data pending)",
        "",
        "",
    ])];

    let widths = [
        ratatui::layout::Constraint::Percentage(40),
        ratatui::layout::Constraint::Percentage(40),
        ratatui::layout::Constraint::Percentage(20),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(block);

    f.render_widget(table, area);
}
