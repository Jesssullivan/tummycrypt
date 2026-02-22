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
}

impl Tab {
    pub const ALL: &[Tab] = &[Tab::Dashboard, Tab::Config, Tab::Mounts, Tab::Secrets];

    pub fn title(&self) -> &str {
        match self {
            Tab::Dashboard => "Dashboard",
            Tab::Config => "Config",
            Tab::Mounts => "Mounts",
            Tab::Secrets => "Secrets",
        }
    }

    pub fn next(&self) -> Tab {
        match self {
            Tab::Dashboard => Tab::Config,
            Tab::Config => Tab::Mounts,
            Tab::Mounts => Tab::Secrets,
            Tab::Secrets => Tab::Dashboard,
        }
    }

    pub fn prev(&self) -> Tab {
        match self {
            Tab::Dashboard => Tab::Secrets,
            Tab::Config => Tab::Dashboard,
            Tab::Mounts => Tab::Config,
            Tab::Secrets => Tab::Mounts,
        }
    }
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
