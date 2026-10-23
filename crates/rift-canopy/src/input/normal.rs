//! Normal mode: the dashboard's own keys — the acts the rail and the graphs
//! column raise, focus and vertical navigation, and the mode openers.

use super::{enter_explorer, enter_profile_menu, selection_is_unadopted, Action, CanopyKey, Mode};
use crate::{App, Panel};

/// Normal mode's key table, read top to bottom in three groups: the acts
/// (the explorer, Project Settings, the Recall Viewer, the graphs column's
/// keys, adopt and the palette), then [`on_key_normal_motion`], then
/// [`on_key_normal_modes`].
/// Every pattern in the three tables is distinct, so a guard that refuses here
/// falls through the later groups to the same inert `Action::None` it always
/// reached.
pub(super) fn on_key_normal(app: &mut App, key: CanopyKey) -> Action {
    match key {
        // `a` raises the destination explorer — browsing is the
        // default way in; typing a path is one step further (`a` then `i`).
        CanopyKey::Char('a') => enter_explorer(app),
        CanopyKey::Char('s') => Action::OpenProjectSettings,
        CanopyKey::Char('r') => Action::OpenRecallViewer,
        // `w` opens the graphs column's "by worktree" axis (the family view)
        // — the keyboard twin of clicking the token panel's `Σ` total. Bound
        // only where it is hinted (`keymap::GRAPHS`), so no other column
        // advertises it and no other column swallows a `w`.
        CanopyKey::Char('w') if app.focus == Panel::Graphs => Action::OpenWorktreeAxis,
        // `S` opens the Recall card's savings breakdown — the keyboard twin of
        // clicking its headline, which was once the only way in.
        // SHIFT-s, not `s`: the rail's `s` is Project Settings and a second
        // meaning on one key would make that hint a lie. Bound only where it
        // is hinted (`keymap::GRAPHS`), like `w` and the period digits.
        CanopyKey::Char('S') if app.focus == Panel::Graphs => Action::OpenSavingsBreakdown,
        // `1`–`4` select the Recall card's period — the keyboard twins of its
        // four `[24h][7d][30d][all]` buttons, one key per button so neither
        // direction of the parity contract rests on pressing a cycling key
        // three times. Bound only where they are hinted (`keymap::GRAPHS`),
        // like `w`: no other column advertises them and no other column
        // swallows a digit.
        CanopyKey::Char(digit @ '1'..='4') if app.focus == Panel::Graphs => {
            recall_window_for_digit(digit).map_or(Action::None, Action::SetRecallWindow)
        }
        // `A` adopts the selected member (the family view) — the keyboard twin
        // of clicking the Repository card's status row. SHIFT-a, not `a`: the
        // rail's `a` is already "add project" and a second meaning on one key
        // would make the rail's hint a lie. Bound only while the selection IS
        // an unadopted member, so it can never be advertised where it is dead;
        // `keymap::HintContext::DetailNotAdopted` is derived from the same
        // premise.
        CanopyKey::Char('A') if selection_is_unadopted(app) => Action::AdoptWorktree,
        CanopyKey::Char('/') => {
            app.set_focus(Panel::Command);
            app.mode = Mode::Command;
            app.command_line.clear();
            Action::None
        }
        _ => on_key_normal_motion(app, key),
    }
}

/// Focus and vertical navigation, and the rail's remove / reorder / open
/// keys that act on the selected project.
fn on_key_normal_motion(app: &mut App, key: CanopyKey) -> Action {
    match key {
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
        // Enter on the rail opens the selection: selection already follows the
        // cursor, so "open" is focusing the detail column.
        CanopyKey::Enter if app.focus == Panel::Projects => {
            app.set_focus(Panel::Detail);
            Action::None
        }
        _ => on_key_normal_modes(app, key),
    }
}

/// The mode openers, help and quit, reachable from any focus.
fn on_key_normal_modes(app: &mut App, key: CanopyKey) -> Action {
    match key {
        // `?` opens the palette help — the header's `? Help` item, which
        // teaches this key beside the click. Normal mode only: the palette's
        // input and every modal stage keep `?` as typed text or inert.
        CanopyKey::Char('?') => Action::OpenHelp,
        // `p` opens the profile menu — same act as clicking the
        // header identity button; inert until the profile plane is wired.
        CanopyKey::Char('p') => {
            enter_profile_menu(app);
            Action::None
        }
        CanopyKey::Char('q') | CanopyKey::Quit => {
            app.quit = true;
            Action::Quit
        }
        // Other keys (incl. reorder/remove without a focused selection) are inert.
        _ => Action::None,
    }
}

/// The period a `1`–`4` press selects, in the same narrowest-first order the
/// buttons are drawn — so the row reads left to right as `1 2 3 4`.
fn recall_window_for_digit(digit: char) -> Option<crate::col3::RecallWindow> {
    let index = digit.to_digit(10)?.checked_sub(1)?;
    crate::col3::RecallWindow::ALL
        .get(usize::try_from(index).ok()?)
        .copied()
}

/// Route a vertical nav key to the focused panel: the projects list moves its
/// selection; the detail and graphs panels scroll their viewports. `page`
/// selects a larger jump. Pure — no I/O.
fn nav_vertical(app: &mut App, down: bool, page: bool) {
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
        Panel::Detail | Panel::Graphs => app.scroll_focused(down, page),
        Panel::Command => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{project, ready};
    use super::super::on_key;
    use super::*;

    #[test]
    fn selection_is_bounded() {
        let mut app = ready(vec![project("a"), project("b")]);
        app.projects_open = true;
        app.set_focus(Panel::Projects);
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
        app.projects_open = true;
        app.set_focus(Panel::Projects);
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
        assert_eq!(app.focus, Panel::Detail);
        for expected in [Panel::Graphs, Panel::Command, Panel::Detail] {
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
    fn arrows_scroll_detail_when_detail_focused() {
        let mut app = ready(vec![project("a")]);
        app.set_focus(Panel::Detail);
        on_key(&mut app, CanopyKey::Down);
        assert_eq!(app.scroll.detail, 1, "Down scrolls the detail viewport");
        on_key(&mut app, CanopyKey::Up);
        assert_eq!(app.scroll.detail, 0);
    }

    #[test]
    fn enter_on_the_rail_focuses_detail() {
        // Selection and inspection are one motion: Enter on the rail
        // focuses the detail column; the selection is untouched.
        let mut app = ready(vec![project("a"), project("b")]);
        app.projects_open = true;
        app.set_focus(Panel::Projects);
        assert_eq!(app.focus, Panel::Projects);
        on_key(&mut app, CanopyKey::Down);
        assert_eq!(on_key(&mut app, CanopyKey::Enter), Action::None);
        assert_eq!(app.focus, Panel::Detail);
        assert_eq!(app.selected, 1, "the selection stays put");
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
}
