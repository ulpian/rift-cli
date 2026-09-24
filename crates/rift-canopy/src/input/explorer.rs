//! Explorer mode: the destination explorer's door, its traversal acts and
//! its key grammar.

use super::{Action, CanopyKey, Mode};
use crate::App;

/// Raise the destination explorer — **the** one door all three
/// add-project affordances arrive through (`a`, the rail `+` chip, and both
/// twins of the wizard primary), so entry can never drift between them. Drops
/// any open modal (the wizard's routing, the message-routing rule), seeds the browsed directory
/// from home → the launch cwd → `/`, and returns the initial listing request —
/// the reducer never reads the filesystem or the environment.
pub fn enter_explorer(app: &mut App) -> Action {
    app.overlay = None;
    app.overlay_focus = 0;
    app.mode = Mode::Explorer;
    app.explorer.reset();
    app.command_line.clear();
    // A real ladder: each rung is skipped when it is absent OR blank, so an
    // empty `HOME` falls through to the launch cwd rather than straight to `/`.
    let rung = |s: &Option<String>| s.clone().filter(|v| !v.is_empty());
    let start = rung(&app.home_dir)
        .or_else(|| rung(&app.launch_cwd))
        .unwrap_or_else(|| "/".to_owned());
    app.explorer.cwd = std::path::PathBuf::from(start);
    Action::ExplorerList(app.explorer.cwd.clone())
}

/// Descend into the directory under the cursor (`Enter` / a row click). A file
/// row or an empty list is inert — files are context, never choices.
pub(crate) fn explorer_descend(app: &App) -> Action {
    let Some(name) = app
        .explorer
        .cursor_entry()
        .filter(|e| e.is_dir)
        .map(|e| e.name.clone())
    else {
        return Action::None;
    };
    Action::ExplorerList(app.explorer.cwd.join(name))
}

/// Step one level up (the `⬑ ..` row / Backspace / Left). Inert at the
/// filesystem root — the edge is calm, never an error.
pub(crate) fn explorer_up(app: &App) -> Action {
    app.explorer
        .cwd
        .parent()
        .map_or(Action::None, |p| Action::ExplorerList(p.to_path_buf()))
}

/// Register the folder currently **open** (the destination explorer) — the one the title
/// names, never the highlighted row. The mode returns to `Normal` *before* the
/// persist so the success notice lands on the dashboard exactly like the typed
/// flow's, and the action is the typed flow's own, so canonicalization,
/// validation, idempotency, and the refresh choreography are inherited whole.
pub(crate) fn explorer_select(app: &mut App) -> Action {
    let path = app.explorer.display_cwd();
    app.mode = Mode::Normal;
    app.explorer.reset();
    if path.is_empty() {
        Action::None
    } else {
        Action::AddProject(path)
    }
}

/// Flip the dot-entry filter (`[.dotfiles]` / `.`). The filter is applied at
/// read time, so nothing is re-listed — only the cursor needs re-clamping onto
/// the new visible list.
pub(crate) fn explorer_toggle_hidden(app: &mut App) {
    app.explorer.show_hidden = !app.explorer.show_hidden;
    let last = app.explorer.visible_len().saturating_sub(1);
    app.explorer.cursor = app.explorer.cursor.min(last);
}

/// Fall back to today's typed flow (`[ input as path ]` / `i`): today's flow
/// **exactly**, empty buffer and all (prefilling the browsed path is
/// deliberately out of scope for v1).
pub(crate) fn explorer_type_path(app: &mut App) {
    app.explorer.reset();
    app.start_add_project();
}

/// The destination explorer's reducer: `j`/`k` and the arrows move
/// the cursor over the visible rows, PageUp/PageDown jump ten, `Enter` descends
/// into a directory, Backspace/Left steps up, `s` registers the open folder,
/// `i` falls back to typing a path, `.` flips the dot-entries, and `Esc` closes.
/// Pure over `App.explorer`; scroll follow-up is `App::clamp_scrolls`'.
pub(super) fn on_key_explorer(app: &mut App, key: CanopyKey) -> Action {
    // Ctrl-C still quits the whole app, even mid-browse.
    if matches!(key, CanopyKey::Quit) {
        app.quit = true;
        return Action::Quit;
    }
    let last = app.explorer.visible_len().saturating_sub(1);
    match key {
        CanopyKey::Char('j') | CanopyKey::Down => {
            app.explorer.cursor = (app.explorer.cursor + 1).min(last);
            Action::None
        }
        CanopyKey::Char('k') | CanopyKey::Up => {
            app.explorer.cursor = app.explorer.cursor.saturating_sub(1);
            Action::None
        }
        CanopyKey::PageDown => {
            app.explorer.cursor = app.explorer.cursor.saturating_add(10).min(last);
            Action::None
        }
        CanopyKey::PageUp => {
            app.explorer.cursor = app.explorer.cursor.saturating_sub(10);
            Action::None
        }
        CanopyKey::Enter => explorer_descend(app),
        CanopyKey::Backspace | CanopyKey::Left => explorer_up(app),
        CanopyKey::Char('s') => explorer_select(app),
        CanopyKey::Char('i') => {
            explorer_type_path(app);
            Action::None
        }
        CanopyKey::Char('.') => {
            explorer_toggle_hidden(app);
            Action::None
        }
        CanopyKey::Esc => {
            app.mode = Mode::Normal;
            app.explorer.reset();
            Action::None
        }
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{project, ready};
    use super::super::on_key;
    use super::*;

    fn dir(name: &str) -> crate::explorer::ExplorerEntry {
        crate::explorer::ExplorerEntry {
            name: name.to_owned(),
            is_dir: true,
            tag: None,
        }
    }

    fn file(name: &str) -> crate::explorer::ExplorerEntry {
        crate::explorer::ExplorerEntry {
            name: name.to_owned(),
            is_dir: false,
            tag: None,
        }
    }

    /// An app browsing `/home/dev` over a hand-built listing (no filesystem —
    /// every listing in these tests crosses the seam as data).
    fn browsing() -> App {
        let mut app = ready(vec![project("a")]);
        app.home_dir = Some("/home/dev".into());
        assert!(matches!(
            on_key(&mut app, CanopyKey::Char('a')),
            Action::ExplorerList(_)
        ));
        app.explorer.apply(crate::explorer::DirListing {
            path: std::path::PathBuf::from("/home/dev"),
            entries: vec![dir(".config"), dir("alpha"), dir("beta"), file("notes.md")],
            omitted: 0,
            error: None,
        });
        app
    }

    #[test]
    fn entering_the_explorer_seeds_home_then_the_launch_cwd_then_root() {
        let mut app = ready(vec![]);
        app.home_dir = Some("/home/dev".into());
        app.launch_cwd = Some("/r/work".into());
        assert_eq!(
            enter_explorer(&mut app),
            Action::ExplorerList(std::path::PathBuf::from("/home/dev")),
            "home wins"
        );
        assert_eq!(app.mode, Mode::Explorer);
        assert_eq!(app.explorer.cwd, std::path::PathBuf::from("/home/dev"));

        let mut app = ready(vec![]);
        app.launch_cwd = Some("/r/work".into());
        assert_eq!(
            enter_explorer(&mut app),
            Action::ExplorerList(std::path::PathBuf::from("/r/work")),
            "no home: the launch cwd"
        );

        let mut app = ready(vec![]);
        assert_eq!(
            enter_explorer(&mut app),
            Action::ExplorerList(std::path::PathBuf::from("/")),
            "neither: the root"
        );
    }

    #[test]
    fn enter_descends_into_a_directory_and_is_inert_on_a_file() {
        let mut app = browsing();
        // Dot-entries are hidden, so the cursor starts on `alpha`.
        assert_eq!(
            on_key(&mut app, CanopyKey::Enter),
            Action::ExplorerList(std::path::PathBuf::from("/home/dev/alpha"))
        );
        // …and a file row is context, never a choice.
        let mut app = browsing();
        on_key(&mut app, CanopyKey::Char('j'));
        on_key(&mut app, CanopyKey::Char('j')); // → notes.md
        assert_eq!(
            app.explorer.cursor_entry().map(|e| e.name.as_str()),
            Some("notes.md")
        );
        assert_eq!(on_key(&mut app, CanopyKey::Enter), Action::None);
    }

    #[test]
    fn backspace_steps_up_and_is_inert_at_the_root() {
        let mut app = browsing();
        assert_eq!(
            on_key(&mut app, CanopyKey::Backspace),
            Action::ExplorerList(std::path::PathBuf::from("/home"))
        );
        assert_eq!(
            on_key(&mut app, CanopyKey::Left),
            Action::ExplorerList(std::path::PathBuf::from("/home")),
            "Left is Backspace's twin"
        );
        // At the filesystem root the edge is calm, not an error.
        app.explorer.cwd = std::path::PathBuf::from("/");
        assert_eq!(on_key(&mut app, CanopyKey::Backspace), Action::None);
        assert_eq!(app.mode, Mode::Explorer, "and it never ejects the operator");
    }

    #[test]
    fn select_registers_the_open_folder_and_returns_to_normal_first() {
        let mut app = browsing();
        app.explorer.cursor = 1; // beta — deliberately NOT what select uses
        assert_eq!(
            on_key(&mut app, CanopyKey::Char('s')),
            Action::AddProject("/home/dev".to_owned()),
            "select registers the folder that is OPEN, not the highlighted row"
        );
        assert_eq!(
            app.mode,
            Mode::Normal,
            "Normal FIRST, so the notice lands on the dashboard"
        );
        assert_eq!(app.explorer, crate::explorer::ExplorerState::default());
    }

    #[test]
    fn select_works_on_a_seeded_but_never_listed_directory() {
        // The seeded cwd is the truth even before the first listing lands (an
        // unreadable home still lets the operator register it).
        let mut app = ready(vec![]);
        app.home_dir = Some("/home/dev".into());
        enter_explorer(&mut app);
        app.explorer.error = Some("permission denied".into());
        assert_eq!(
            on_key(&mut app, CanopyKey::Char('s')),
            Action::AddProject("/home/dev".to_owned())
        );
    }

    #[test]
    fn i_falls_back_to_todays_typed_flow_with_an_empty_buffer() {
        let mut app = browsing();
        app.command_line = "stale".into();
        assert_eq!(on_key(&mut app, CanopyKey::Char('i')), Action::None);
        assert_eq!(app.mode, Mode::AddProject);
        assert!(app.command_line.is_empty(), "today's flow exactly");
        assert_eq!(app.explorer, crate::explorer::ExplorerState::default());
    }

    #[test]
    fn the_dot_toggle_reveals_hidden_entries_and_reclamps_the_cursor() {
        let mut app = browsing();
        assert_eq!(app.explorer.visible_len(), 3, "dot-entries start hidden");
        on_key(&mut app, CanopyKey::Char('.'));
        assert!(app.explorer.show_hidden);
        assert_eq!(app.explorer.visible_len(), 4);
        assert_eq!(
            app.explorer.cursor_entry().map(|e| e.name.as_str()),
            Some(".config"),
            "the same index now names the revealed row"
        );
        // Hiding again re-clamps a cursor that would fall off the end.
        app.explorer.cursor = 3;
        on_key(&mut app, CanopyKey::Char('.'));
        assert!(!app.explorer.show_hidden);
        assert_eq!(app.explorer.cursor, 2, "clamped onto the shorter list");
    }

    #[test]
    fn the_cursor_clamps_at_both_ends_and_pages_by_ten() {
        let mut app = browsing();
        on_key(&mut app, CanopyKey::Char('k'));
        assert_eq!(app.explorer.cursor, 0, "already at the top");
        for _ in 0..9 {
            on_key(&mut app, CanopyKey::Char('j'));
        }
        assert_eq!(app.explorer.cursor, 2, "saturates at the last visible row");
        on_key(&mut app, CanopyKey::PageUp);
        assert_eq!(app.explorer.cursor, 0);
        on_key(&mut app, CanopyKey::PageDown);
        assert_eq!(app.explorer.cursor, 2, "a page jump clamps too");

        // A listing long enough to SEE the page size — a short one cannot tell
        // "ten" from "clamped to the last row".
        app.explorer.entries = (0..30).map(|i| dir(&format!("d{i:02}"))).collect();
        app.explorer.cursor = 0;
        on_key(&mut app, CanopyKey::PageDown);
        assert_eq!(app.explorer.cursor, 10, "a page is ten rows");
        on_key(&mut app, CanopyKey::PageDown);
        assert_eq!(app.explorer.cursor, 20);
        on_key(&mut app, CanopyKey::PageUp);
        assert_eq!(app.explorer.cursor, 10, "…in both directions");
    }

    #[test]
    fn esc_closes_the_explorer_and_ctrl_c_still_quits() {
        let mut app = browsing();
        assert_eq!(on_key(&mut app, CanopyKey::Esc), Action::None);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.explorer, crate::explorer::ExplorerState::default());
        let mut app = browsing();
        assert_eq!(on_key(&mut app, CanopyKey::Quit), Action::Quit);
        assert!(app.quit, "the escape hatch is never trapped");
    }

    #[test]
    fn a_failed_listing_keeps_the_view_and_the_next_good_one_clears_it() {
        let mut app = browsing();
        let before = app.explorer.clone();
        app.explorer.apply(crate::explorer::DirListing {
            path: std::path::PathBuf::from("/root"),
            entries: vec![],
            omitted: 0,
            error: Some("permission denied".into()),
        });
        assert_eq!(app.explorer.cwd, before.cwd, "the operator stays put");
        assert_eq!(app.explorer.entries, before.entries);
        assert_eq!(app.explorer.error.as_deref(), Some("permission denied"));
        // …and every exit is still available from the error state.
        assert!(matches!(
            on_key(&mut app, CanopyKey::Backspace),
            Action::ExplorerList(_)
        ));
    }
}
