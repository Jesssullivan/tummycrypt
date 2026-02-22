//! tcfs-tui: TummyCrypt terminal user interface
//!
//! Dashboard showing daemon status, config, mounts, and credentials.

mod app;
mod daemon;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use app::App;
use daemon::DaemonUpdate;

fn parse_args() -> (PathBuf, PathBuf) {
    let args: Vec<String> = std::env::args().collect();
    let mut config_path = PathBuf::from("/etc/tcfs/config.toml");
    let mut socket_path: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" | "-c" => {
                if i + 1 < args.len() {
                    config_path = PathBuf::from(&args[i + 1]);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--socket" | "-s" => {
                if i + 1 < args.len() {
                    socket_path = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                i += 1;
            }
        }
    }

    let config: tcfs_core::config::TcfsConfig = if config_path.exists() {
        let contents = std::fs::read_to_string(&config_path).unwrap_or_default();
        toml::from_str(&contents).unwrap_or_default()
    } else {
        tcfs_core::config::TcfsConfig::default()
    };

    let sock = socket_path.unwrap_or_else(|| config.daemon.socket.clone());
    (config_path, sock)
}

#[tokio::main]
async fn main() -> Result<()> {
    let (config_path, socket_path) = parse_args();

    let config: tcfs_core::config::TcfsConfig = if config_path.exists() {
        let contents = std::fs::read_to_string(&config_path).context("reading config")?;
        toml::from_str(&contents).unwrap_or_default()
    } else {
        tcfs_core::config::TcfsConfig::default()
    };

    // Set up terminal
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    stdout
        .execute(EnterAlternateScreen)
        .context("enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    // Panic hook: restore terminal on panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        original_hook(info);
    }));

    let mut app = App::new(config);

    // Spawn daemon poller
    let (tx, mut rx) = mpsc::channel::<DaemonUpdate>(16);
    tokio::spawn(daemon::poll_daemon(socket_path, tx));

    // Main event loop
    loop {
        terminal.draw(|f| ui::draw(f, &app))?;

        // Drain all pending daemon updates
        while let Ok(update) = rx.try_recv() {
            match update {
                DaemonUpdate::Status(s) => app.update_status(s),
                DaemonUpdate::Creds(c) => app.update_cred_status(c),
                DaemonUpdate::Disconnected(reason) => app.set_disconnected(reason),
            }
        }

        // Poll for keyboard events with 500ms timeout
        if event::poll(Duration::from_millis(500)).context("event poll")? {
            if let Event::Key(key) = event::read().context("event read")? {
                app.handle_key(key);
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode().context("disable raw mode")?;
    io::stdout()
        .execute(LeaveAlternateScreen)
        .context("leave alternate screen")?;

    Ok(())
}
