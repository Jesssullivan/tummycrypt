use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent};
use tcfs_core::config::TcfsConfig;
use tcfs_core::proto::{CredentialStatusResponse, StatusResponse};

const HISTORY_LEN: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Config,
    Mounts,
    Secrets,
    Conflicts,
}

impl Tab {
    pub const ALL: &[Tab] = &[
        Tab::Dashboard,
        Tab::Config,
        Tab::Mounts,
        Tab::Secrets,
        Tab::Conflicts,
    ];

    pub fn title(&self) -> &str {
        match self {
            Tab::Dashboard => "Dashboard",
            Tab::Config => "Config",
            Tab::Mounts => "Mounts",
            Tab::Secrets => "Secrets",
            Tab::Conflicts => "Conflicts",
        }
    }

    pub fn next(&self) -> Tab {
        match self {
            Tab::Dashboard => Tab::Config,
            Tab::Config => Tab::Mounts,
            Tab::Mounts => Tab::Secrets,
            Tab::Secrets => Tab::Conflicts,
            Tab::Conflicts => Tab::Dashboard,
        }
    }

    pub fn prev(&self) -> Tab {
        match self {
            Tab::Dashboard => Tab::Conflicts,
            Tab::Config => Tab::Dashboard,
            Tab::Mounts => Tab::Config,
            Tab::Secrets => Tab::Mounts,
            Tab::Conflicts => Tab::Secrets,
        }
    }
}

/// A pending conflict shown in the TUI.
#[derive(Debug, Clone)]
pub struct PendingConflict {
    pub rel_path: String,
    pub local_device: String,
    pub remote_device: String,
    pub local_hash: String,
    pub remote_hash: String,
    pub detected_at: u64,
}

pub struct App {
    pub tab: Tab,
    pub should_quit: bool,
    pub daemon_status: Option<StatusResponse>,
    pub cred_status: Option<CredentialStatusResponse>,
    pub config: TcfsConfig,
    pub connected: bool,
    pub error: Option<String>,
    pub uptime_history: VecDeque<u64>,
    pub conflicts: Vec<PendingConflict>,
    pub conflict_selected: usize,
}

impl App {
    pub fn new(config: TcfsConfig) -> Self {
        Self {
            tab: Tab::Dashboard,
            should_quit: false,
            daemon_status: None,
            cred_status: None,
            config,
            connected: false,
            error: None,
            uptime_history: VecDeque::with_capacity(HISTORY_LEN),
            conflicts: Vec::new(),
            conflict_selected: 0,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Tab => self.tab = self.tab.next(),
            KeyCode::BackTab => self.tab = self.tab.prev(),
            KeyCode::Char('1') => self.tab = Tab::Dashboard,
            KeyCode::Char('2') => self.tab = Tab::Config,
            KeyCode::Char('3') => self.tab = Tab::Mounts,
            KeyCode::Char('4') => self.tab = Tab::Secrets,
            KeyCode::Char('5') => self.tab = Tab::Conflicts,
            // Conflicts tab shortcuts
            KeyCode::Char('j') | KeyCode::Down if self.tab == Tab::Conflicts => {
                if !self.conflicts.is_empty() {
                    self.conflict_selected = (self.conflict_selected + 1) % self.conflicts.len();
                }
            }
            KeyCode::Char('k') | KeyCode::Up if self.tab == Tab::Conflicts => {
                if !self.conflicts.is_empty() {
                    self.conflict_selected = self
                        .conflict_selected
                        .checked_sub(1)
                        .unwrap_or(self.conflicts.len() - 1);
                }
            }
            _ => {}
        }
    }

    pub fn update_status(&mut self, status: StatusResponse) {
        if self.uptime_history.len() >= HISTORY_LEN {
            self.uptime_history.pop_front();
        }
        self.uptime_history.push_back(status.uptime_secs as u64);
        self.daemon_status = Some(status);
        self.connected = true;
        self.error = None;
    }

    pub fn update_cred_status(&mut self, cred: CredentialStatusResponse) {
        self.cred_status = Some(cred);
    }

    pub fn set_disconnected(&mut self, reason: String) {
        self.connected = false;
        self.error = Some(reason);
        self.daemon_status = None;
        self.cred_status = None;
    }
}
