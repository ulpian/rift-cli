//! Project Settings mode: the scrolling modal's cursor, its activate and
//! retry acts, and the raw-confirm step back.

use super::{Action, CanopyKey, Mode};
use crate::App;

pub(super) fn on_key_project_settings(app: &mut App, key: CanopyKey) -> Action {
    if app.project_settings.running {
        return Action::None;
    }
    match key {
        CanopyKey::Quit => {
            app.quit = true;
            Action::Quit
        }
        CanopyKey::Esc if app.project_settings.confirm_raw => {
            app.project_settings.confirm_raw = false;
            app.project_settings.result = None;
            Action::None
        }
        CanopyKey::Esc | CanopyKey::Char('x' | 'X') => {
            app.dashboard_tab = crate::dashboard::DashboardTab::Home;
            app.mode = Mode::Normal;
            app.project_settings.close();
            Action::None
        }
        CanopyKey::Char('j') | CanopyKey::Down => {
            app.project_settings.move_cursor(true, false);
            Action::None
        }
        CanopyKey::Char('k') | CanopyKey::Up => {
            app.project_settings.move_cursor(false, false);
            Action::None
        }
        CanopyKey::PageDown => {
            app.project_settings.move_cursor(true, true);
            Action::None
        }
        CanopyKey::PageUp => {
            app.project_settings.move_cursor(false, true);
            Action::None
        }
        CanopyKey::Enter | CanopyKey::Char(' ') => app
            .project_settings
            .activate()
            .map_or(Action::None, Action::ApplyProjectSettings),
        CanopyKey::Char('r') => app
            .project_settings
            .retry()
            .map_or(Action::None, Action::ApplyProjectSettings),
        _ => Action::None,
    }
}
