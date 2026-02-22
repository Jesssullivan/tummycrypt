use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Configuration ")
        .borders(Borders::ALL);

    let c = &app.config;
    let mut lines = Vec::new();

    section_header(&mut lines, "Daemon");
    kv(&mut lines, "Socket", &c.daemon.socket.display().to_string());
    kv(
        &mut lines,
        "Metrics",
        c.daemon.metrics_addr.as_deref().unwrap_or("disabled"),
    );
    kv(&mut lines, "Log Level", &c.daemon.log_level);
    kv(&mut lines, "Log Format", &c.daemon.log_format);

    section_header(&mut lines, "Storage");
    kv(&mut lines, "Endpoint", &c.storage.endpoint);
    kv(&mut lines, "Region", &c.storage.region);
    kv(&mut lines, "Bucket", &c.storage.bucket);
    kv(
        &mut lines,
        "TLS Enforced",
        if c.storage.enforce_tls { "Yes" } else { "No" },
    );

    section_header(&mut lines, "Secrets");
    kv(
        &mut lines,
        "Age Identity",
        &c.secrets
            .age_identity
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not set".into()),
    );
    kv(
        &mut lines,
        "KDBX Path",
        &c.secrets
            .kdbx_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not set".into()),
    );
    kv(
        &mut lines,
        "SOPS Dir",
        &c.secrets
            .sops_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not set".into()),
    );

    section_header(&mut lines, "Sync");
    kv(&mut lines, "NATS URL", &c.sync.nats_url);
    kv(
        &mut lines,
        "NATS TLS",
        if c.sync.nats_tls { "Yes" } else { "No" },
    );
    kv(&mut lines, "State DB", &c.sync.state_db.display().to_string());
    kv(&mut lines, "Workers", &c.sync.workers.to_string());

    section_header(&mut lines, "FUSE");
    kv(&mut lines, "Cache Dir", &c.fuse.cache_dir.display().to_string());
    kv(
        &mut lines,
        "Cache Max MB",
        &c.fuse.cache_max_mb.to_string(),
    );

    section_header(&mut lines, "Crypto");
    kv(
        &mut lines,
        "Enabled",
        if c.crypto.enabled { "Yes" } else { "No" },
    );
    if c.crypto.enabled {
        kv(
            &mut lines,
            "Argon2 Memory",
            &format!("{} KiB", c.crypto.argon2_mem_cost_kib),
        );
        kv(
            &mut lines,
            "Argon2 Time",
            &c.crypto.argon2_time_cost.to_string(),
        );
        kv(
            &mut lines,
            "Argon2 Parallelism",
            &c.crypto.argon2_parallelism.to_string(),
        );
    }

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn section_header(lines: &mut Vec<Line<'static>>, name: &str) {
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("  [{name}]"),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(ratatui::style::Modifier::BOLD),
    )));
}

fn kv(lines: &mut Vec<Line<'static>>, key: &str, value: &str) {
    lines.push(Line::from(vec![
        Span::styled(
            format!("    {key:<18}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(value.to_string()),
    ]));
}
