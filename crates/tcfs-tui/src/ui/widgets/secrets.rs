use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(0)])
        .split(area);

    draw_cred_detail(f, app, chunks[0]);
    draw_sops_info(f, app, chunks[1]);
}

fn draw_cred_detail(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Credential Status ")
        .borders(Borders::ALL);

    match &app.cred_status {
        Some(c) => {
            let loaded_style = if c.loaded {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            };

            let lines = vec![
                Line::from(vec![
                    Span::styled("  Loaded:      ", Style::default().fg(Color::DarkGray)),
                    Span::styled(if c.loaded { "Yes" } else { "No" }, loaded_style),
                ]),
                Line::from(vec![
                    Span::styled("  Source:      ", Style::default().fg(Color::DarkGray)),
                    Span::raw(&c.source),
                ]),
                Line::from(vec![
                    Span::styled("  Loaded At:   ", Style::default().fg(Color::DarkGray)),
                    Span::raw(if c.loaded_at > 0 {
                        format!("epoch {}", c.loaded_at)
                    } else {
                        "N/A".to_string()
                    }),
                ]),
                Line::from(vec![
                    Span::styled("  Needs Reload:", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        if c.needs_reload { " Yes" } else { " No" },
                        if c.needs_reload {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        },
                    ),
                ]),
            ];
            f.render_widget(Paragraph::new(lines).block(block), area);
        }
        None => {
            let msg = if app.connected {
                "  Loading..."
            } else {
                "  Daemon not connected"
            };
            let lines = vec![Line::from(Span::styled(
                msg,
                Style::default().fg(Color::DarkGray),
            ))];
            f.render_widget(Paragraph::new(lines).block(block), area);
        }
    }
}

fn draw_sops_info(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Secrets Configuration ")
        .borders(Borders::ALL);

    let c = &app.config.secrets;
    let mut lines = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("  Age Identity:  ", Style::default().fg(Color::DarkGray)),
        Span::raw(
            c.age_identity
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not configured".into()),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::styled("  KDBX Path:     ", Style::default().fg(Color::DarkGray)),
        Span::raw(
            c.kdbx_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not configured".into()),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::styled("  SOPS Dir:      ", Style::default().fg(Color::DarkGray)),
        Span::raw(
            c.sops_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not configured".into()),
        ),
    ]));

    // Show RemoteJuggler identity if set
    if let Ok(identity) = std::env::var("REMOTE_JUGGLER_IDENTITY") {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("  RemoteJuggler: ", Style::default().fg(Color::Cyan)),
            Span::raw(identity),
        ]));
    }

    f.render_widget(Paragraph::new(lines).block(block), area);
}
