use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Sparkline};
use ratatui::Frame;

use crate::app::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let wide = area.width >= 100;

    if wide {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(8)])
            .split(area);

        let top = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(chunks[0]);

        draw_status_card(f, app, top[0]);
        draw_sparkline(f, app, top[1]);
        draw_cred_card(f, app, chunks[1]);
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(10),
                Constraint::Length(6),
                Constraint::Min(0),
            ])
            .split(area);

        draw_status_card(f, app, chunks[0]);
        draw_cred_card(f, app, chunks[1]);
        draw_sparkline(f, app, chunks[2]);
    }
}

fn draw_status_card(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Daemon Status ")
        .borders(Borders::ALL);

    match &app.daemon_status {
        Some(s) => {
            let storage_style = if s.storage_ok {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red)
            };
            let nats_style = if s.nats_ok {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Yellow)
            };

            let uptime_str = format_uptime(s.uptime_secs);

            let lines = vec![
                Line::from(vec![
                    Span::styled("  Version:  ", Style::default().fg(Color::DarkGray)),
                    Span::raw(&s.version),
                ]),
                Line::from(vec![
                    Span::styled("  Endpoint: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(&s.storage_endpoint),
                ]),
                Line::from(vec![
                    Span::styled("  Storage:  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(if s.storage_ok { "OK" } else { "FAIL" }, storage_style),
                ]),
                Line::from(vec![
                    Span::styled("  NATS:     ", Style::default().fg(Color::DarkGray)),
                    Span::styled(if s.nats_ok { "OK" } else { "N/A" }, nats_style),
                ]),
                Line::from(vec![
                    Span::styled("  Mounts:   ", Style::default().fg(Color::DarkGray)),
                    Span::raw(format!("{}", s.active_mounts)),
                ]),
                Line::from(vec![
                    Span::styled("  Uptime:   ", Style::default().fg(Color::DarkGray)),
                    Span::raw(uptime_str),
                ]),
            ];

            f.render_widget(Paragraph::new(lines).block(block), area);
        }
        None => {
            let msg = if let Some(err) = &app.error {
                format!("  Disconnected: {err}")
            } else {
                "  Connecting...".to_string()
            };
            let lines = vec![Line::from(Span::styled(
                msg,
                Style::default().fg(Color::Red),
            ))];
            f.render_widget(Paragraph::new(lines).block(block), area);
        }
    }
}

fn draw_sparkline(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Uptime (ticks) ")
        .borders(Borders::ALL);

    let data: Vec<u64> = app.uptime_history.iter().copied().collect();
    let sparkline = Sparkline::default()
        .block(block)
        .data(&data)
        .style(Style::default().fg(Color::Cyan));

    f.render_widget(sparkline, area);
}

fn draw_cred_card(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Credentials ")
        .borders(Borders::ALL);

    match &app.cred_status {
        Some(c) => {
            let loaded_style = if c.loaded {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red)
            };
            let reload_style = if c.needs_reload {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let lines = vec![
                Line::from(vec![
                    Span::styled("  Loaded: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(if c.loaded { "Yes" } else { "No" }, loaded_style),
                ]),
                Line::from(vec![
                    Span::styled("  Source: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(&c.source),
                ]),
                Line::from(vec![
                    Span::styled("  Reload: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(if c.needs_reload { "NEEDED" } else { "No" }, reload_style),
                ]),
            ];
            f.render_widget(Paragraph::new(lines).block(block), area);
        }
        None => {
            let lines = vec![Line::from(Span::styled(
                "  No credential data",
                Style::default().fg(Color::DarkGray),
            ))];
            f.render_widget(Paragraph::new(lines).block(block), area);
        }
    }
}

fn format_uptime(secs: i64) -> String {
    let secs = secs.unsigned_abs();
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m {s}s")
    } else if hours > 0 {
        format!("{hours}h {mins}m {s}s")
    } else if mins > 0 {
        format!("{mins}m {s}s")
    } else {
        format!("{s}s")
    }
}
