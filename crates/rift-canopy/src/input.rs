//! The pure interaction reducer: keys → state transitions + side-effect actions.
//!
//! All of Canopy's interactivity lives here as a terminal-free function over
//! [`App`], so the dashboard's behavior (selection, the add-project prompt, the
//! slash palette, quitting) is unit- and property-testable without a real
//! terminal. The xre backend ([`crate::xre`]) only translates its event stream
//! into [`CanopyKey`] and renders the resulting state — it holds no logic of its
//! own.

use crate::{parse_command, App, Command};

/// Which editing/interaction mode the dashboard is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Navigating: arrow/`j`/`k` select, `a` adds a project, `/` opens the palette.
    Normal,
    /// Typing a slash command into the bottom palette.
    Command,
    /// Typing a path to register a new project.
    AddProject,
}

/// A terminal-agnostic key, translated from the backend's event stream so the
/// reducer stays pure and testable without a real terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanopyKey {
    /// A printable character.
    Char(char),
    /// Confirm the current line / selection.
    Enter,
    /// Cancel the current mode.
    Esc,
    /// Delete the last character of the edit buffer.
    Backspace,
    /// Move the selection up.
    Up,
    /// Move the selection down.
    Down,
    /// A normalized quit request (e.g. Ctrl-C).
    Quit,
}

/// What the event loop should do after a key is handled. The state mutation has
/// already been applied to [`App`]; this is only the externally-visible effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing beyond the state mutation already applied.
    None,
    /// Exit the dashboard.
    Quit,
    /// Register a project at the given (user-entered) path.
    AddProject(String),
    /// Run a parsed slash command.
    RunCommand(Command),
}

/// Apply one key to the dashboard, returning the effect the event loop must run.
///
/// While the splash is still playing, any key dismisses it (and is otherwise
/// swallowed). Otherwise the key is dispatched by the current [`Mode`].
pub fn on_key(app: &mut App, key: CanopyKey) -> Action {
    if !app.splash.done() {
        app.splash.skip();
        return Action::None;
    }
    match app.mode {
        Mode::Normal => on_key_normal(app, key),
        Mode::Command | Mode::AddProject => on_key_editing(app, key),
    }
}

fn on_key_normal(app: &mut App, key: CanopyKey) -> Action {
    match key {
        CanopyKey::Char('a') => {
            app.mode = Mode::AddProject;
            app.command_line.clear();
            app.status = None;
            Action::None
        }
        CanopyKey::Char('/') => {
            app.mode = Mode::Command;
            app.command_line.clear();
            Action::None
        }
        CanopyKey::Char('j') | CanopyKey::Down => {
            app.select_next();
            Action::None
        }
        CanopyKey::Char('k') | CanopyKey::Up => {
            app.select_prev();
            Action::None
        }
        CanopyKey::Char('q') | CanopyKey::Quit => {
            app.quit = true;
            Action::Quit
        }
        _ => Action::None,
    }
}

fn on_key_editing(app: &mut App, key: CanopyKey) -> Action {
    match key {
        CanopyKey::Char(c) => {
            app.command_line.push(c);
            Action::None
        }
        CanopyKey::Backspace => {
            app.command_line.pop();
            Action::None
        }
        CanopyKey::Esc | CanopyKey::Quit => {
            app.mode = Mode::Normal;
            app.command_line.clear();
            Action::None
        }
        CanopyKey::Enter => {
            let buf = std::mem::take(&mut app.command_line);
            let mode = app.mode;
            app.mode = Mode::Normal;
            match mode {
                Mode::AddProject => {
                    let trimmed = buf.trim();
                    if trimmed.is_empty() {
                        Action::None
                    } else {
                        Action::AddProject(trimmed.to_string())
                    }
                }
                Mode::Command => match parse_command(&buf) {
                    Some(cmd) if cmd.name == "quit" => {
                        app.quit = true;
                        Action::Quit
                    }
                    Some(cmd) => Action::RunCommand(cmd),
                    None => Action::None,
                },
                // Editing handlers are only reached from Command/AddProject.
                Mode::Normal => Action::None,
            }
        }
        CanopyKey::Up | CanopyKey::Down => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProjStatus, Project};

    fn project(name: &str) -> Project {
        Project {
            name: name.into(),
            path: format!("/r/{name}"),
            status: ProjStatus::Idle,
        }
    }

    /// An app past the splash, ready for Normal-mode interaction.
    fn ready(projects: Vec<Project>) -> App {
        let mut app = App::new(projects);
        app.splash.skip();
        app
    }

    fn feed(app: &mut App, keys: &[CanopyKey]) -> Action {
        let mut last = Action::None;
        for &k in keys {
            last = on_key(app, k);
        }
        last
    }

    #[test]
    fn first_key_only_dismisses_splash() {
        let mut app = App::new(vec![]); // fresh splash, not done
        assert!(!app.splash.done());
        let action = on_key(&mut app, CanopyKey::Char('a'));
        assert_eq!(action, Action::None);
        assert!(app.splash.done());
        assert_eq!(
            app.mode,
            Mode::Normal,
            "splash key is swallowed, not acted on"
        );
    }

    #[test]
    fn add_project_flow_yields_path() {
        let mut app = ready(vec![]);
        assert_eq!(on_key(&mut app, CanopyKey::Char('a')), Action::None);
        assert_eq!(app.mode, Mode::AddProject);
        let keys = [
            CanopyKey::Char('/'),
            CanopyKey::Char('t'),
            CanopyKey::Char('m'),
            CanopyKey::Char('p'),
        ];
        feed(&mut app, &keys);
        let action = on_key(&mut app, CanopyKey::Enter);
        assert_eq!(action, Action::AddProject("/tmp".to_string()));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.command_line.is_empty());
    }

    #[test]
    fn empty_add_project_is_noop() {
        let mut app = ready(vec![]);
        on_key(&mut app, CanopyKey::Char('a'));
        assert_eq!(on_key(&mut app, CanopyKey::Enter), Action::None);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn slash_quit_command_quits() {
        let mut app = ready(vec![]);
        on_key(&mut app, CanopyKey::Char('/'));
        feed(
            &mut app,
            &[
                CanopyKey::Char('q'),
                CanopyKey::Char('u'),
                CanopyKey::Char('i'),
                CanopyKey::Char('t'),
            ],
        );
        assert_eq!(on_key(&mut app, CanopyKey::Enter), Action::Quit);
        assert!(app.quit);
    }

    #[test]
    fn slash_other_command_runs() {
        let mut app = ready(vec![]);
        on_key(&mut app, CanopyKey::Char('/'));
        feed(
            &mut app,
            &[
                CanopyKey::Char('s'),
                CanopyKey::Char('t'),
                CanopyKey::Char('a'),
                CanopyKey::Char('t'),
                CanopyKey::Char('u'),
                CanopyKey::Char('s'),
            ],
        );
        match on_key(&mut app, CanopyKey::Enter) {
            Action::RunCommand(cmd) => assert_eq!(cmd.name, "status"),
            other => panic!("expected RunCommand, got {other:?}"),
        }
    }

    #[test]
    fn esc_cancels_editing() {
        let mut app = ready(vec![]);
        on_key(&mut app, CanopyKey::Char('a'));
        feed(&mut app, &[CanopyKey::Char('x'), CanopyKey::Char('y')]);
        assert_eq!(on_key(&mut app, CanopyKey::Esc), Action::None);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.command_line.is_empty());
    }

    #[test]
    fn selection_is_bounded() {
        let mut app = ready(vec![project("a"), project("b")]);
        assert_eq!(on_key(&mut app, CanopyKey::Char('k')), Action::None); // already top
        assert_eq!(app.selected, 0);
        on_key(&mut app, CanopyKey::Down);
        assert_eq!(app.selected, 1);
        on_key(&mut app, CanopyKey::Char('j')); // saturates
        assert_eq!(app.selected, 1);
        on_key(&mut app, CanopyKey::Up);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn q_quits_in_normal_mode() {
        let mut app = ready(vec![]);
        assert_eq!(on_key(&mut app, CanopyKey::Char('q')), Action::Quit);
        assert!(app.quit);
    }

    proptest::proptest! {
        /// Any key sequence is panic-free and never drives the selection out of
        /// bounds (the reducer never mutates the project list itself).
        #[test]
        fn arbitrary_keys_keep_selection_in_bounds(
            keys in proptest::collection::vec(key_strategy(), 0..60)
        ) {
            let mut app = ready(vec![project("a"), project("b")]);
            for k in keys {
                let _ = on_key(&mut app, k);
            }
            proptest::prop_assert!(app.selected < app.projects.len());
        }
    }

    fn key_strategy() -> impl proptest::strategy::Strategy<Value = CanopyKey> {
        use proptest::prelude::*;
        prop_oneof![
            any::<char>().prop_map(CanopyKey::Char),
            Just(CanopyKey::Enter),
            Just(CanopyKey::Esc),
            Just(CanopyKey::Backspace),
            Just(CanopyKey::Up),
            Just(CanopyKey::Down),
            Just(CanopyKey::Quit),
        ]
    }
}
