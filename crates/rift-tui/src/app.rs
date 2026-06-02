//! TUI application state and the key-handling state machine.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::widgets::ListState;

use crate::metrics::MetricsSnapshot;

/// Which region currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Options,
    Command,
}

/// Command-bar editing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
}

/// An entry in the left-hand options list.
#[derive(Debug, Clone)]
pub struct ProjectItem {
    pub name: String,
    pub path: PathBuf,
}

/// An action the event loop must perform that the state machine can't do alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Quit the application.
    Quit,
    /// Suspend the TUI and run `entire <args>` inheriting the terminal.
    RunExternal(Vec<OsString>),
}

/// The full application state.
pub struct AppState {
    pub repo: PathBuf,
    pub focus: Focus,
    pub mode: Mode,
    pub options: Vec<ProjectItem>,
    pub list_state: ListState,
    pub metrics: MetricsSnapshot,
    pub command_input: String,
    pub status: String,
    pub should_quit: bool,
}

impl AppState {
    /// Build state for `repo`, seeding the options list and a metrics snapshot.
    #[must_use]
    pub fn new(repo: &Path, options: Vec<ProjectItem>, metrics: MetricsSnapshot) -> Self {
        let mut list_state = ListState::default();
        if !options.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            repo: repo.to_path_buf(),
            focus: Focus::Options,
            mode: Mode::Normal,
            options,
            list_state,
            metrics,
            command_input: String::new(),
            status: "press `:` to enter a command · `q` to quit · ↑/↓ to navigate".to_string(),
            should_quit: false,
        }
    }

    /// Handle a key event, possibly returning an [`Action`] for the loop.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self.mode {
            Mode::Normal => self.on_key_normal(key),
            Mode::Insert => self.on_key_insert(key),
        }
    }

    fn on_key_normal(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                Some(Action::Quit)
            }
            KeyCode::Char(':') => {
                self.focus = Focus::Command;
                self.mode = Mode::Insert;
                self.command_input.clear();
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev();
                None
            }
            _ => None,
        }
    }

    fn on_key_insert(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.leave_command_mode();
                None
            }
            KeyCode::Enter => self.submit_command(),
            KeyCode::Backspace => {
                self.command_input.pop();
                None
            }
            KeyCode::Char(c) => {
                self.command_input.push(c);
                None
            }
            _ => None,
        }
    }

    fn leave_command_mode(&mut self) {
        self.mode = Mode::Normal;
        self.focus = Focus::Options;
        self.command_input.clear();
    }

    /// Interpret the current command-bar buffer.
    ///
    /// Internal verbs start with `:` (`:q`, `:help`); anything else is treated
    /// as a `rift`/`entire` command and handed back as [`Action::RunExternal`].
    fn submit_command(&mut self) -> Option<Action> {
        let raw = self.command_input.trim().to_string();
        self.leave_command_mode();
        if raw.is_empty() {
            return None;
        }

        if let Some(verb) = raw.strip_prefix(':') {
            return self.run_internal(verb);
        }

        // Treat as an entire command; strip a leading `rift`/`entire` if typed.
        let mut tokens: Vec<&str> = raw.split_whitespace().collect();
        if matches!(tokens.first(), Some(&"rift" | &"entire")) {
            tokens.remove(0);
        }
        if tokens.is_empty() {
            return None;
        }
        self.status = format!("running: entire {}", tokens.join(" "));
        Some(Action::RunExternal(
            tokens.into_iter().map(OsString::from).collect(),
        ))
    }

    fn run_internal(&mut self, verb: &str) -> Option<Action> {
        match verb.trim() {
            "q" | "quit" => {
                self.should_quit = true;
                Some(Action::Quit)
            }
            "help" => {
                self.status = "commands: `:q` quit · type `status`/`checkpoint list` to run entire"
                    .to_string();
                None
            }
            other => {
                self.status = format!("unknown command: :{other}");
                None
            }
        }
    }

    /// The currently selected project, if any.
    #[must_use]
    pub fn selected(&self) -> Option<&ProjectItem> {
        self.list_state.selected().and_then(|i| self.options.get(i))
    }

    fn select_next(&mut self) {
        if self.options.is_empty() {
            return;
        }
        let next = match self.list_state.selected() {
            Some(i) => (i + 1) % self.options.len(),
            None => 0,
        };
        self.list_state.select(Some(next));
    }

    fn select_prev(&mut self) {
        if self.options.is_empty() {
            return;
        }
        let prev = match self.list_state.selected() {
            Some(0) | None => self.options.len() - 1,
            Some(i) => i - 1,
        };
        self.list_state.select(Some(prev));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEvent;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn app() -> AppState {
        let options = vec![
            ProjectItem {
                name: "a".into(),
                path: "/a".into(),
            },
            ProjectItem {
                name: "b".into(),
                path: "/b".into(),
            },
        ];
        AppState::new(Path::new("/repo"), options, MetricsSnapshot::placeholder())
    }

    #[test]
    fn q_quits() {
        let mut a = app();
        assert_eq!(a.on_key(key(KeyCode::Char('q'))), Some(Action::Quit));
        assert!(a.should_quit);
    }

    #[test]
    fn navigation_wraps() {
        let mut a = app();
        assert_eq!(a.list_state.selected(), Some(0));
        a.on_key(key(KeyCode::Up));
        assert_eq!(a.list_state.selected(), Some(1));
        a.on_key(key(KeyCode::Down));
        assert_eq!(a.list_state.selected(), Some(0));
    }

    #[test]
    fn colon_enters_command_mode_and_typing_builds_buffer() {
        let mut a = app();
        a.on_key(key(KeyCode::Char(':')));
        assert_eq!(a.mode, Mode::Insert);
        a.on_key(key(KeyCode::Char('h')));
        a.on_key(key(KeyCode::Char('i')));
        assert_eq!(a.command_input, "hi");
    }

    #[test]
    fn entire_command_becomes_run_external() {
        let mut a = app();
        a.mode = Mode::Insert;
        a.command_input = "rift status".into(); // leading `rift` is stripped
        let action = a.on_key(key(KeyCode::Enter));
        assert_eq!(
            action,
            Some(Action::RunExternal(vec![OsString::from("status")]))
        );
        assert_eq!(a.mode, Mode::Normal); // submitting leaves command mode
    }

    #[test]
    fn colon_q_quits() {
        let mut a = app();
        a.mode = Mode::Insert;
        a.command_input = ":q".into();
        let action = a.on_key(key(KeyCode::Enter));
        assert_eq!(action, Some(Action::Quit));
    }
}
