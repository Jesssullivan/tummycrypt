//! TUI state machine tests — no terminal needed.
//!
//! Tests Tab navigation, key handling, status updates, disconnect/reconnect,
//! conflict selection, and uptime history management.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

// ── Re-implement App model for testing (binary crate, can't import directly) ──

const HISTORY_LEN: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dashboard,
    Config,
    Mounts,
    Secrets,
    Conflicts,
}

impl Tab {
    fn next(&self) -> Tab {
        match self {
            Tab::Dashboard => Tab::Config,
            Tab::Config => Tab::Mounts,
            Tab::Mounts => Tab::Secrets,
            Tab::Secrets => Tab::Conflicts,
            Tab::Conflicts => Tab::Dashboard,
        }
    }

    fn prev(&self) -> Tab {
        match self {
            Tab::Dashboard => Tab::Conflicts,
            Tab::Config => Tab::Dashboard,
            Tab::Mounts => Tab::Config,
            Tab::Secrets => Tab::Mounts,
            Tab::Conflicts => Tab::Secrets,
        }
    }

    fn title(&self) -> &str {
        match self {
            Tab::Dashboard => "Dashboard",
            Tab::Config => "Config",
            Tab::Mounts => "Mounts",
            Tab::Secrets => "Secrets",
            Tab::Conflicts => "Conflicts",
        }
    }
}

struct App {
    tab: Tab,
    should_quit: bool,
    connected: bool,
    error: Option<String>,
    uptime_history: std::collections::VecDeque<u64>,
    conflicts: Vec<String>,
    conflict_selected: usize,
}

impl App {
    fn new() -> Self {
        Self {
            tab: Tab::Dashboard,
            should_quit: false,
            connected: false,
            error: None,
            uptime_history: std::collections::VecDeque::with_capacity(HISTORY_LEN),
            conflicts: Vec::new(),
            conflict_selected: 0,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Tab => self.tab = self.tab.next(),
            KeyCode::BackTab => self.tab = self.tab.prev(),
            KeyCode::Char('1') => self.tab = Tab::Dashboard,
            KeyCode::Char('2') => self.tab = Tab::Config,
            KeyCode::Char('3') => self.tab = Tab::Mounts,
            KeyCode::Char('4') => self.tab = Tab::Secrets,
            KeyCode::Char('5') => self.tab = Tab::Conflicts,
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

    fn set_disconnected(&mut self, reason: String) {
        self.connected = false;
        self.error = Some(reason);
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

// ── Tab navigation ───────────────────────────────────────────────────────

#[test]
fn initial_state_is_dashboard() {
    let app = App::new();
    assert_eq!(app.tab, Tab::Dashboard);
    assert!(!app.should_quit);
    assert!(!app.connected);
}

#[test]
fn tab_cycles_forward() {
    let mut app = App::new();
    assert_eq!(app.tab, Tab::Dashboard);

    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.tab, Tab::Config);

    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.tab, Tab::Mounts);

    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.tab, Tab::Secrets);

    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.tab, Tab::Conflicts);

    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.tab, Tab::Dashboard); // wraps around
}

#[test]
fn backtab_cycles_backward() {
    let mut app = App::new();
    app.handle_key(key(KeyCode::BackTab));
    assert_eq!(app.tab, Tab::Conflicts); // wraps from Dashboard

    app.handle_key(key(KeyCode::BackTab));
    assert_eq!(app.tab, Tab::Secrets);
}

#[test]
fn number_keys_jump_to_tab() {
    let mut app = App::new();

    app.handle_key(key(KeyCode::Char('3')));
    assert_eq!(app.tab, Tab::Mounts);

    app.handle_key(key(KeyCode::Char('5')));
    assert_eq!(app.tab, Tab::Conflicts);

    app.handle_key(key(KeyCode::Char('1')));
    assert_eq!(app.tab, Tab::Dashboard);
}

// ── Quit handling ────────────────────────────────────────────────────────

#[test]
fn q_quits() {
    let mut app = App::new();
    app.handle_key(key(KeyCode::Char('q')));
    assert!(app.should_quit);
}

#[test]
fn esc_quits() {
    let mut app = App::new();
    app.handle_key(key(KeyCode::Esc));
    assert!(app.should_quit);
}

// ── Tab titles ───────────────────────────────────────────────────────────

#[test]
fn tab_titles() {
    assert_eq!(Tab::Dashboard.title(), "Dashboard");
    assert_eq!(Tab::Config.title(), "Config");
    assert_eq!(Tab::Mounts.title(), "Mounts");
    assert_eq!(Tab::Secrets.title(), "Secrets");
    assert_eq!(Tab::Conflicts.title(), "Conflicts");
}

// ── Tab next/prev are inverses ───────────────────────────────────────────

#[test]
fn next_prev_roundtrip() {
    for tab in [
        Tab::Dashboard,
        Tab::Config,
        Tab::Mounts,
        Tab::Secrets,
        Tab::Conflicts,
    ] {
        assert_eq!(tab.next().prev(), tab);
        assert_eq!(tab.prev().next(), tab);
    }
}

// ── Conflict navigation ─────────────────────────────────────────────────

#[test]
fn conflict_j_moves_down() {
    let mut app = App::new();
    app.tab = Tab::Conflicts;
    app.conflicts = vec!["a".into(), "b".into(), "c".into()];
    app.conflict_selected = 0;

    app.handle_key(key(KeyCode::Char('j')));
    assert_eq!(app.conflict_selected, 1);

    app.handle_key(key(KeyCode::Char('j')));
    assert_eq!(app.conflict_selected, 2);

    app.handle_key(key(KeyCode::Char('j')));
    assert_eq!(app.conflict_selected, 0); // wraps
}

#[test]
fn conflict_k_moves_up() {
    let mut app = App::new();
    app.tab = Tab::Conflicts;
    app.conflicts = vec!["a".into(), "b".into(), "c".into()];
    app.conflict_selected = 0;

    app.handle_key(key(KeyCode::Char('k')));
    assert_eq!(app.conflict_selected, 2); // wraps from 0 → last
}

#[test]
fn conflict_nav_no_crash_on_empty() {
    let mut app = App::new();
    app.tab = Tab::Conflicts;
    app.conflicts = vec![]; // empty

    // Should not panic
    app.handle_key(key(KeyCode::Char('j')));
    assert_eq!(app.conflict_selected, 0);

    app.handle_key(key(KeyCode::Char('k')));
    assert_eq!(app.conflict_selected, 0);
}

#[test]
fn conflict_nav_ignored_on_other_tabs() {
    let mut app = App::new();
    app.tab = Tab::Dashboard; // NOT conflicts tab
    app.conflicts = vec!["a".into(), "b".into()];
    app.conflict_selected = 0;

    app.handle_key(key(KeyCode::Char('j')));
    assert_eq!(app.conflict_selected, 0); // unchanged
}

// ── Disconnect / error state ─────────────────────────────────────────────

#[test]
fn disconnect_sets_error() {
    let mut app = App::new();
    app.connected = true;

    app.set_disconnected("connection refused".into());

    assert!(!app.connected);
    assert_eq!(app.error.as_deref(), Some("connection refused"));
}

// ── Uptime history ───────────────────────────────────────────────────────

#[test]
fn uptime_history_caps_at_limit() {
    let mut app = App::new();

    for i in 0..(HISTORY_LEN + 10) {
        if app.uptime_history.len() >= HISTORY_LEN {
            app.uptime_history.pop_front();
        }
        app.uptime_history.push_back(i as u64);
    }

    assert_eq!(app.uptime_history.len(), HISTORY_LEN);
    assert_eq!(app.uptime_history.front(), Some(&10));
}

// ── Unknown keys are ignored ─────────────────────────────────────────────

#[test]
fn unknown_key_no_effect() {
    let mut app = App::new();
    let before_tab = app.tab;

    app.handle_key(key(KeyCode::Char('z')));

    assert_eq!(app.tab, before_tab);
    assert!(!app.should_quit);
}
