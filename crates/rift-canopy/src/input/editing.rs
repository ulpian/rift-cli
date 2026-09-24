//! Editing mode: the slash palette and the typed add-project path share one
//! buffer and one reducer.

use super::{Action, CanopyKey, Mode};
use crate::App;

pub(super) fn on_key_editing(app: &mut App, key: CanopyKey) -> Action {
    match key {
        CanopyKey::Char(c) => {
            app.command_line.push(c);
            app.command_error = None; // an edit clears the inline error
            app.suggest_highlight = 0; // the list changed under the cursor
            Action::None
        }
        CanopyKey::Backspace => {
            app.command_line.pop();
            app.command_error = None;
            app.suggest_highlight = 0;
            Action::None
        }
        // The suggest panel's keys: arrows highlight; Tab INSERTS the
        // highlight and never executes — completion is always safe to press.
        CanopyKey::Down if app.mode == Mode::Command => {
            let len = crate::current_suggestions(app).len();
            if len > 0 {
                app.suggest_highlight = (app.suggest_highlight + 1).min(len - 1);
            }
            Action::None
        }
        CanopyKey::Up if app.mode == Mode::Command => {
            app.suggest_highlight = app.suggest_highlight.saturating_sub(1);
            Action::None
        }
        CanopyKey::Tab if app.mode == Mode::Command => {
            insert_suggestion(app);
            Action::None
        }
        CanopyKey::Esc | CanopyKey::Quit => {
            app.mode = Mode::Normal;
            app.command_line.clear();
            app.command_error = None;
            Action::None
        }
        CanopyKey::Enter => match app.mode {
            Mode::AddProject => {
                let buf = std::mem::take(&mut app.command_line);
                app.mode = Mode::Normal;
                let trimmed = buf.trim();
                if trimmed.is_empty() {
                    Action::None
                } else {
                    Action::AddProject(trimmed.to_string())
                }
            }
            // The reducer parses (span errors render inline and keep the
            // buffer); on success it clears the buffer + mode itself.
            Mode::Command => Action::RunLine(app.command_line.clone()),
            // on_key_editing is only reached in Command/AddProject; the others
            // are inert here (PanelEditor/ProfileMenu/ColourPicker/Explorer
            // route to their own reducers).
            Mode::Normal
            | Mode::PanelEditor
            | Mode::ProfileMenu
            | Mode::ColourPicker
            | Mode::Explorer
            | Mode::ProjectSettings => Action::None,
        },
        // Navigation/focus keys are inert while editing the command/path buffer.
        _ => Action::None,
    }
}

/// Insert the highlighted suggestion into the buffer (Tab and the mouse
/// click share this — insert-only, never execution).
pub(crate) fn insert_suggestion(app: &mut App) {
    let suggestions = crate::current_suggestions(app);
    if let Some(sug) = suggestions.get(
        app.suggest_highlight
            .min(suggestions.len().saturating_sub(1)),
    ) {
        app.command_line.clone_from(&sug.insert);
        app.command_error = None;
        app.suggest_highlight = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::ready;
    use super::super::on_key;
    use super::*;

    fn feed(app: &mut App, keys: &[CanopyKey]) -> Action {
        let mut last = Action::None;
        for &k in keys {
            last = on_key(app, k);
        }
        last
    }

    /// Reach the typed add-project flow the browse-first way: `a` raises the
    /// destination explorer, `i` falls back to typing a path (the destination explorer —
    /// the old `a`-then-type reflex is now `a`, `i`, type).
    fn type_a_path(app: &mut App) {
        assert!(matches!(
            on_key(app, CanopyKey::Char('a')),
            Action::ExplorerList(_)
        ));
        assert_eq!(app.mode, Mode::Explorer, "`a` browses first");
        assert_eq!(on_key(app, CanopyKey::Char('i')), Action::None);
        assert_eq!(app.mode, Mode::AddProject);
        assert!(app.command_line.is_empty(), "the buffer starts empty");
    }

    #[test]
    fn add_project_flow_yields_path() {
        let mut app = ready(vec![]);
        type_a_path(&mut app);
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
        type_a_path(&mut app);
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
        // Parsing lives in the reducer: Enter submits the raw line;
        // the reducer's `/quit` handling produces Flow::Quit (covered in the
        // state tests).
        assert_eq!(
            on_key(&mut app, CanopyKey::Enter),
            Action::RunLine("quit".into())
        );
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
            Action::RunLine(buf) => assert_eq!(buf, "status"),
            other => panic!("expected RunLine, got {other:?}"),
        }
    }

    #[test]
    fn esc_cancels_editing() {
        let mut app = ready(vec![]);
        type_a_path(&mut app);
        feed(&mut app, &[CanopyKey::Char('x'), CanopyKey::Char('y')]);
        assert_eq!(on_key(&mut app, CanopyKey::Esc), Action::None);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.command_line.is_empty());
    }
}
