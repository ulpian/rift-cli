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
    /// Move the selection / scroll up.
    Up,
    /// Move the selection / scroll down.
    Down,
    /// Cycle focus to the next panel.
    Tab,
    /// Cycle focus to the previous panel (Shift-Tab).
    BackTab,
    /// Move focus one column left.
    Left,
    /// Move focus one column right.
    Right,
    /// Scroll the focused panel up by a page.
    PageUp,
    /// Scroll the focused panel down by a page.
    PageDown,
    /// Reorder the selected item up (Shift-Up).
    ShiftUp,
    /// Reorder the selected item down (Shift-Down).
    ShiftDown,
    /// A normalized quit request (e.g. Ctrl-C).
    Quit,
}

/// A mouse button — a terminal-agnostic mirror of the backend's, translated in
/// the live driver so the reducer and hit-testing stay engine-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    /// The primary (left) button.
    Left,
    /// The secondary (right) button.
    Right,
    /// The middle button / wheel click.
    Middle,
}

/// A mouse action at a cell coordinate. Only presses and the scroll wheel drive
/// interaction today; moves/drags/releases are modeled so the source can forward
/// them, but the reducer ignores them (no per-move work).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    /// A button was pressed.
    Down(MouseButton),
    /// A button was released.
    Up(MouseButton),
    /// The pointer moved with a button held.
    Drag(MouseButton),
    /// The pointer moved with no button held.
    Moved,
    /// The wheel scrolled up (away from the user).
    ScrollUp,
    /// The wheel scrolled down (toward the user).
    ScrollDown,
}

/// A mouse event in terminal cells from the top-left — the engine-free form the
/// reducer consumes (via [`crate::state::Event::Mouse`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseInput {
    /// What happened.
    pub kind: MouseKind,
    /// Column (cell x), 0-based from the left.
    pub col: u16,
    /// Row (cell y), 0-based from the top.
    pub row: u16,
}

/// What the event loop should do after a key is handled. The state mutation has
/// already been applied to [`App`]; this is only the externally-visible effect.
///
/// `RemoveProject`/`ReorderProject` carry no target: they act on the **currently
/// selected** project, which the reducer resolves to a path when it builds the
/// append-only [`crate::PersistOp`] (keeping the key handler free of ids/paths).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing beyond the state mutation already applied.
    None,
    /// Exit the dashboard.
    Quit,
    /// Register a project at the given (user-entered) path.
    AddProject(String),
    /// Remove (hide) the currently selected project.
    RemoveProject,
    /// Reorder the currently selected project up (`true`) or down (`false`).
    ReorderProject(bool),
    /// Flip the feature toggle under the column-2 cursor.
    ToggleFeature,
    /// Retry (reinstall) the feature toggle under the column-2 cursor.
    RetryFeature,
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
    use crate::Panel;
    match key {
        CanopyKey::Char('a') => {
            app.mode = Mode::AddProject;
            app.command_line.clear();
            app.status = None;
            Action::None
        }
        CanopyKey::Char('/') => {
            app.set_focus(Panel::Command);
            app.mode = Mode::Command;
            app.command_line.clear();
            Action::None
        }
        CanopyKey::Tab => {
            app.cycle_focus(true);
            Action::None
        }
        CanopyKey::BackTab => {
            app.cycle_focus(false);
            Action::None
        }
        CanopyKey::Left => {
            app.move_focus(true);
            Action::None
        }
        CanopyKey::Right => {
            app.move_focus(false);
            Action::None
        }
        CanopyKey::Char('j') | CanopyKey::Down => {
            nav_vertical(app, true, false);
            Action::None
        }
        CanopyKey::Char('k') | CanopyKey::Up => {
            nav_vertical(app, false, false);
            Action::None
        }
        CanopyKey::PageDown => {
            nav_vertical(app, true, true);
            Action::None
        }
        CanopyKey::PageUp => {
            nav_vertical(app, false, true);
            Action::None
        }
        // Remove / reorder act on the selected project — only when the projects
        // column is focused and a selection exists (else inert).
        CanopyKey::Char('d' | 'x')
            if app.focus == Panel::Projects && app.selected_project().is_some() =>
        {
            Action::RemoveProject
        }
        CanopyKey::ShiftUp if app.focus == Panel::Projects && app.selected_project().is_some() => {
            Action::ReorderProject(true)
        }
        CanopyKey::ShiftDown
            if app.focus == Panel::Projects && app.selected_project().is_some() =>
        {
            Action::ReorderProject(false)
        }
        // Column-2 feature toggles: Space / Enter / `t` flip the cursored toggle,
        // `r` retries a failed one. Only when the detail panel is focused.
        CanopyKey::Char(' ' | 't') | CanopyKey::Enter if app.focus == Panel::Detail => {
            Action::ToggleFeature
        }
        CanopyKey::Char('r') if app.focus == Panel::Detail => Action::RetryFeature,
        CanopyKey::Char('q') | CanopyKey::Quit => {
            app.quit = true;
            Action::Quit
        }
        // Other keys (incl. reorder/remove without a focused selection) are inert.
        _ => Action::None,
    }
}

/// Route a vertical nav key to the focused panel: the projects list moves its
/// selection; the detail panel moves the feature-toggle cursor; the graphs panel
/// scrolls its viewport. `page` selects a larger jump. Pure — no I/O.
fn nav_vertical(app: &mut App, down: bool, page: bool) {
    use crate::Panel;
    match app.focus {
        Panel::Projects => {
            let steps = if page { 10 } else { 1 };
            for _ in 0..steps {
                if down {
                    app.select_next();
                } else {
                    app.select_prev();
                }
            }
        }
        Panel::Detail => app.move_toggle_cursor(down),
        Panel::Graphs => app.scroll_focused(down, page),
        Panel::Command => {}
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
        // Navigation/focus keys are inert while editing the command/path buffer.
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntryKind, Panel, Project, ProjectStatus};

    fn project(name: &str) -> Project {
        Project {
            name: name.into(),
            path: format!("/r/{name}"),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
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

    #[test]
    fn remove_and_reorder_keys_act_on_focused_projects() {
        let mut app = ready(vec![project("a"), project("b")]);
        assert_eq!(app.focus, Panel::Projects);
        assert_eq!(
            on_key(&mut app, CanopyKey::Char('d')),
            Action::RemoveProject
        );
        assert_eq!(
            on_key(&mut app, CanopyKey::Char('x')),
            Action::RemoveProject
        );
        assert_eq!(
            on_key(&mut app, CanopyKey::ShiftUp),
            Action::ReorderProject(true)
        );
        assert_eq!(
            on_key(&mut app, CanopyKey::ShiftDown),
            Action::ReorderProject(false)
        );
    }

    #[test]
    fn remove_and_reorder_are_inert_when_projects_unfocused() {
        let mut app = ready(vec![project("a")]);
        app.set_focus(Panel::Detail);
        assert_eq!(on_key(&mut app, CanopyKey::Char('d')), Action::None);
        assert_eq!(on_key(&mut app, CanopyKey::ShiftUp), Action::None);
    }

    #[test]
    fn remove_is_inert_with_no_projects() {
        let mut app = ready(vec![]);
        assert_eq!(on_key(&mut app, CanopyKey::Char('d')), Action::None);
    }

    #[test]
    fn tab_navigation_order_through_on_key() {
        let mut app = ready(vec![project("a")]);
        assert_eq!(app.focus, Panel::Projects);
        for expected in [
            Panel::Detail,
            Panel::Graphs,
            Panel::Command,
            Panel::Projects,
        ] {
            assert_eq!(on_key(&mut app, CanopyKey::Tab), Action::None);
            assert_eq!(app.focus, expected);
        }
        // Shift-Tab reverses.
        assert_eq!(on_key(&mut app, CanopyKey::BackTab), Action::None);
        assert_eq!(app.focus, Panel::Command);
    }

    #[test]
    fn arrows_scroll_focused_graphs_panel() {
        let mut app = ready(vec![project("a"), project("b")]);
        app.set_focus(Panel::Graphs);
        on_key(&mut app, CanopyKey::Down);
        assert_eq!(
            app.scroll.graphs, 1,
            "Down scrolls the focused graphs panel"
        );
        assert_eq!(
            app.selected, 0,
            "selection is unchanged when Projects isn't focused"
        );
        on_key(&mut app, CanopyKey::Up);
        assert_eq!(app.scroll.graphs, 0);
    }

    #[test]
    fn arrows_move_toggle_cursor_when_detail_focused() {
        let mut app = ready(vec![project("a")]);
        app.set_focus(Panel::Detail);
        assert_eq!(app.toggle_cursor, 0);
        on_key(&mut app, CanopyKey::Down);
        assert_eq!(app.toggle_cursor, 1, "Down moves the toggle cursor");
        on_key(&mut app, CanopyKey::Up);
        assert_eq!(app.toggle_cursor, 0);
        assert_eq!(app.scroll.detail, 0, "detail no longer scrolls via arrows");
    }

    #[test]
    fn detail_focus_keys_flip_and_retry_toggles() {
        let mut app = ready(vec![project("a")]);
        app.set_focus(Panel::Detail);
        assert_eq!(
            on_key(&mut app, CanopyKey::Char(' ')),
            Action::ToggleFeature
        );
        assert_eq!(
            on_key(&mut app, CanopyKey::Char('t')),
            Action::ToggleFeature
        );
        assert_eq!(on_key(&mut app, CanopyKey::Enter), Action::ToggleFeature);
        assert_eq!(on_key(&mut app, CanopyKey::Char('r')), Action::RetryFeature);
        // Inert when the detail panel is not focused.
        app.set_focus(Panel::Projects);
        assert_eq!(on_key(&mut app, CanopyKey::Char('t')), Action::None);
    }

    #[test]
    fn focus_returns_to_previous_on_command_cancel() {
        let mut app = ready(vec![project("a")]);
        app.set_focus(Panel::Graphs);
        on_key(&mut app, CanopyKey::Char('/')); // focuses Command, enters Command mode
        assert_eq!(app.focus, Panel::Command);
        assert_eq!(app.mode, Mode::Command);
        assert_eq!(app.focus_prev, Panel::Graphs);
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
            Just(CanopyKey::Tab),
            Just(CanopyKey::BackTab),
            Just(CanopyKey::Left),
            Just(CanopyKey::Right),
            Just(CanopyKey::PageUp),
            Just(CanopyKey::PageDown),
            Just(CanopyKey::ShiftUp),
            Just(CanopyKey::ShiftDown),
            Just(CanopyKey::Quit),
        ]
    }
}
