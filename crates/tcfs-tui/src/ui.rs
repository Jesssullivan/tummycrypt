use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Tabs};
use ratatui::Frame;

use crate::app::{App, Tab};

mod widgets;

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header + tabs
            Constraint::Min(0),    // body
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    draw_header(f, app, chunks[0]);

    match app.tab {
        Tab::Dashboard => widgets::dashboard::draw(f, app, chunks[1]),
        Tab::Config => widgets::config::draw(f, app, chunks[1]),
        Tab::Mounts => widgets::mounts::draw(f, app, chunks[1]),
        Tab::Secrets => widgets::secrets::draw(f, app, chunks[1]),
    }

    draw_footer(f, app, chunks[2]);
}

fn draw_header(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| {
            let style = if *t == app.tab {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Line::from(Span::styled(t.title(), style))
        })
        .collect();

    let selected = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);

    let status_indicator = if app.connected {
        Span::styled(" CONNECTED ", Style::default().fg(Color::Green))
    } else {
        Span::styled(" DISCONNECTED ", Style::default().fg(Color::Red))
    };

    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::BOTTOM).title(vec![
                    Span::styled(
                        " tcfs-tui ",
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    status_indicator,
                ]))
        .select(selected)
        .highlight_style(Style::default().fg(Color::Cyan));

    f.render_widget(tabs, area);
}

fn draw_footer(f: &mut Frame, _app: &App, area: ratatui::layout::Rect) {
    let hints = Line::from(vec![
        Span::styled("[q]", Style::default().fg(Color::Yellow)),
        Span::raw(" Quit  "),
        Span::styled("[Tab]", Style::default().fg(Color::Yellow)),
        Span::raw(" Switch  "),
        Span::styled("[1-4]", Style::default().fg(Color::Yellow)),
        Span::raw(" Jump  "),
    ]);
    f.render_widget(ratatui::widgets::Paragraph::new(hints), area);
}
