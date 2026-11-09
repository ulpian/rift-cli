//! Navigation uses the existing project-selection, settings, and viewer services.

use super::{AppState, Effects, Flow};
use crate::dashboard::DashboardTab;
use crate::{CanopyKey, Mode, Panel};

pub(super) fn select_tab(s: &mut AppState, tab: DashboardTab) -> Flow {
    if s.app.project_settings.running {
        return Flow::Continue(Effects::default());
    }
    let mut effects = if s.app.surface == crate::recall_viewer::Surface::RecallViewer {
        s.close_recall_viewer()
    } else {
        Effects::default()
    };
    s.app.mode = Mode::Normal;
    s.app.projects_open = false;
    s.app.dashboard_tab = tab;
    s.app.scroll.detail = 0;
    s.app.scroll.graphs = 0;
    s.app.set_focus(if tab == DashboardTab::Tokens {
        Panel::Graphs
    } else {
        Panel::Detail
    });
    s.dirty = true;
    match tab {
        DashboardTab::Settings => {
            if let Flow::Continue(opened) = super::openers::open_project_settings(s) {
                effects.data_requests.extend(opened.data_requests);
            }
        }
        DashboardTab::Recall => effects
            .data_requests
            .extend(s.open_recall_viewer(None).data_requests),
        _ => {}
    }
    Flow::Continue(effects)
}

pub(super) fn toggle_drawer(s: &mut AppState) -> Flow {
    if s.app.project_settings.running {
        return Flow::Continue(Effects::default());
    }
    s.app.projects_open = !s.app.projects_open;
    if s.app.projects_open {
        s.app.project_cursor = s.app.selected;
        align_picker_cursor(s);
        if s.app.mode == Mode::Command {
            s.app.mode = Mode::Normal;
        }
        s.app.viewer.filter_editing = false;
    }
    if s.app.surface == crate::recall_viewer::Surface::RecallViewer {
        s.app.viewer.pane = if s.app.projects_open {
            crate::recall_viewer::ViewerPane::Projects
        } else {
            crate::recall_viewer::ViewerPane::Checkpoints
        };
    }
    s.app.set_focus(if s.app.projects_open {
        Panel::Projects
    } else if s.app.dashboard_tab == DashboardTab::Tokens {
        Panel::Graphs
    } else {
        Panel::Detail
    });
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// Keys the dashboard's navigation leaves to the surface underneath: text
/// being typed, and Left while the viewer has an unfolded timeline item to
/// fold — the one case where Left is not the project drawer's.
fn navigation_stands_aside(app: &crate::state::App, key: CanopyKey) -> bool {
    app.mode == Mode::Command
        || app.viewer.filter_editing
        || (key == CanopyKey::Left
            && !app.projects_open
            && super::viewer::left_folds_a_timeline_item(app))
}

pub(super) fn on_navigation_key(s: &mut AppState, key: CanopyKey) -> Option<Flow> {
    let navigation_allowed = s.app.overlay.is_none()
        && !s.app.project_settings.running
        && matches!(
            s.app.mode,
            Mode::Normal | Mode::ProjectSettings | Mode::Command
        );
    if !navigation_allowed {
        return None;
    }
    if matches!(key, CanopyKey::Tab | CanopyKey::BackTab) {
        if s.app.projects_open {
            let filters = crate::project_picker::FILTERS;
            let index = filters
                .iter()
                .position(|filter| *filter == s.app.project_filter)
                .unwrap_or(0);
            let next = (index + if key == CanopyKey::Tab { 1 } else { 2 }) % filters.len();
            return Some(select_picker_filter(
                s,
                filters.get(next).copied().flatten(),
            ));
        }
        return Some(select_tab(
            s,
            s.app.dashboard_tab.next(key == CanopyKey::Tab),
        ));
    }
    if navigation_stands_aside(&s.app, key) {
        return None;
    }
    match key {
        CanopyKey::Char('[' | ']') => Some(select_tab(
            s,
            s.app.dashboard_tab.next(key == CanopyKey::Char(']')),
        )),
        CanopyKey::Left if !s.app.projects_open => Some(toggle_drawer(s)),
        CanopyKey::Esc | CanopyKey::Left if s.app.projects_open => Some(toggle_drawer(s)),
        CanopyKey::Enter | CanopyKey::Right if s.app.projects_open => {
            let visible = picker_indices(s);
            Some(if visible.contains(&s.app.project_cursor) {
                super::click::click_project_row(s, s.app.project_cursor)
            } else {
                Flow::Continue(Effects::default())
            })
        }
        CanopyKey::Up
        | CanopyKey::Down
        | CanopyKey::Char('j' | 'k')
        | CanopyKey::PageUp
        | CanopyKey::PageDown
            if s.app.projects_open =>
        {
            let down = matches!(key, CanopyKey::Down | CanopyKey::Char('j'));
            let page = matches!(key, CanopyKey::PageUp | CanopyKey::PageDown);
            Some(move_picker(
                s,
                down || key == CanopyKey::PageDown,
                if page { 10 } else { 1 },
            ))
        }
        CanopyKey::Char('d' | 'x') | CanopyKey::ShiftUp | CanopyKey::ShiftDown
            if s.app.projects_open =>
        {
            Some(manage_picker_project(s, key))
        }
        CanopyKey::Char('a' | '/') if s.app.projects_open => {
            s.app.projects_open = false;
            s.app.mode = Mode::Normal;
            None
        }
        CanopyKey::Char('?') if s.app.projects_open => Some(super::click::open_header_help(s)),
        CanopyKey::Quit | CanopyKey::Char('q') if s.app.projects_open => {
            s.app.quit = true;
            Some(Flow::Quit)
        }
        _ if s.app.projects_open => Some(Flow::Continue(Effects::default())),
        _ => None,
    }
}

/// Project management keeps the highlighted row separate from active project data.
fn manage_picker_project(s: &mut AppState, key: CanopyKey) -> Flow {
    let target = match key {
        CanopyKey::ShiftUp => crate::hit::Target::RailReorderUp,
        CanopyKey::ShiftDown => crate::hit::Target::RailReorderDown,
        _ => crate::hit::Target::RailRemove,
    };
    super::click::apply_click(s, target)
}

fn picker_indices(s: &AppState) -> Vec<usize> {
    s.app
        .projects
        .iter()
        .enumerate()
        .filter(|(_, p)| crate::project_picker::accepts(s.app.project_filter, p.kind))
        .map(|(i, _)| i)
        .collect()
}

fn align_picker_cursor(s: &mut AppState) {
    let visible = picker_indices(s);
    if !visible.contains(&s.app.project_cursor) {
        s.app.project_cursor = visible.first().copied().unwrap_or(0);
    }
}

pub(super) fn select_picker_filter(s: &mut AppState, filter: Option<crate::EntryKind>) -> Flow {
    s.app.project_filter = filter;
    align_picker_cursor(s);
    s.dirty = true;
    Flow::Continue(Effects::default())
}

pub(super) fn move_picker(s: &mut AppState, down: bool, step: usize) -> Flow {
    let visible = picker_indices(s);
    let position = visible
        .iter()
        .position(|i| *i == s.app.project_cursor)
        .unwrap_or(0);
    let next = if down {
        position
            .saturating_add(step)
            .min(visible.len().saturating_sub(1))
    } else {
        position.saturating_sub(step)
    };
    s.app.project_cursor = visible.get(next).copied().unwrap_or(0);
    s.dirty = true;
    Flow::Continue(Effects::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall_viewer::{ViewerPane, ViewerRequest};
    use crate::state::fixtures::{effects_of, project, project_settings_state, ready};
    use crate::state::{update, DataRequest, Event, ProjectData};

    #[test]
    fn picker_help_and_quit_work_when_opened_from_settings() {
        let mut s = ready(vec![project("a")]);
        select_tab(&mut s, DashboardTab::Settings);
        toggle_drawer(&mut s);
        update(&mut s, Event::Key(CanopyKey::Char('?')));
        assert!(
            s.app.overlay.is_some(),
            "the picker Help key is live over Settings"
        );
        update(&mut s, Event::Key(CanopyKey::Esc));
        assert!(matches!(
            update(&mut s, Event::Key(CanopyKey::Char('q'))),
            Flow::Quit
        ));
    }

    #[test]
    fn picker_cursor_follows_reorder_and_filter_after_removal() {
        let a = project("a");
        let mut b = project("b");
        b.kind = crate::EntryKind::Worktree;
        let mut c = project("c");
        c.kind = crate::EntryKind::Worktree;
        let mut s = ready(vec![a.clone(), b.clone(), c.clone()]);
        toggle_drawer(&mut s);
        select_picker_filter(&mut s, Some(crate::EntryKind::Worktree));
        assert_eq!(s.app.project_cursor, 1);
        s.app.set_projects(vec![a.clone(), c.clone(), b]);
        assert_eq!(
            s.app.project_cursor, 2,
            "the cursor follows b after reorder"
        );
        s.app.set_projects(vec![a, c]);
        assert_eq!(
            s.app.project_cursor, 1,
            "removing b leaves the cursor on a worktree"
        );
        assert_eq!(
            s.app.selected, 0,
            "management preserves the active repository"
        );
    }

    #[test]
    fn drawer_selection_retargets_viewer_and_closes_to_a_visible_pane() {
        let mut s = ready(vec![project("a"), project("b")]);
        s.open_recall_viewer(None);
        toggle_drawer(&mut s);
        let effects = effects_of(update(&mut s, Event::Key(CanopyKey::Down)));
        assert_eq!(s.app.viewer.project.as_deref(), Some("/r/a"));
        assert!(
            effects.data_requests.is_empty(),
            "browsing does not retarget the data bus"
        );
        let effects = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(s.app.viewer.project.as_deref(), Some("/r/b"));
        assert!(effects.data_requests.iter().any(|request| matches!(request,
            DataRequest::Viewer(ViewerRequest::Catalogue { project, .. }) if project == "/r/b")));
        assert!(!s.app.projects_open);
        assert_eq!(s.app.viewer.pane, ViewerPane::Checkpoints);
        assert_eq!(s.app.focus, Panel::Detail);
    }

    #[test]
    fn switching_settings_clears_old_choices_and_rejects_foreign_delivery() {
        let mut s = project_settings_state(crate::ProjectInitialization::Initialized);
        s.app.projects.push(project("b"));
        let old_detail = s.detail.as_deref().unwrap().clone();
        select_tab(&mut s, DashboardTab::Settings);
        toggle_drawer(&mut s);
        update(&mut s, Event::Key(CanopyKey::Down));
        update(&mut s, Event::Key(CanopyKey::Enter));
        let view = s.app.project_settings.view.as_deref().unwrap();
        assert_eq!(view.path, "/r/b");
        assert!(
            s.app.project_settings.items().is_empty(),
            "no setup act before the real view arrives"
        );
        update(
            &mut s,
            Event::ProjectData(ProjectData {
                project: "/r/a".into(),
                status: crate::ProjectStatus::Idle,
                detail: Some(old_detail),
            }),
        );
        assert_eq!(s.app.project_settings.view.as_deref().unwrap().path, "/r/b");
    }

    #[test]
    fn tab_and_shift_tab_switch_top_views_instead_of_panel_focus() {
        let mut s = ready(vec![project("a")]);
        for expected in [
            DashboardTab::Models,
            DashboardTab::Activity,
            DashboardTab::Tokens,
            DashboardTab::Recall,
            DashboardTab::Settings,
            DashboardTab::Home,
        ] {
            update(&mut s, Event::Key(CanopyKey::Tab));
            assert_eq!(s.app.dashboard_tab, expected);
            assert_ne!(s.app.focus, Panel::Projects);
        }
        update(&mut s, Event::Key(CanopyKey::BackTab));
        assert_eq!(s.app.dashboard_tab, DashboardTab::Settings);
    }

    #[test]
    fn cancelling_picker_preserves_project_and_tab() {
        let mut s = ready(vec![project("a"), project("b")]);
        toggle_drawer(&mut s);
        update(&mut s, Event::Key(CanopyKey::Down));
        update(&mut s, Event::Key(CanopyKey::Esc));
        assert_eq!(s.app.selected, 0);
        assert_eq!(s.app.dashboard_tab, DashboardTab::Home);
        assert!(!s.app.projects_open);
    }
    #[test]
    fn picker_tab_filters_and_navigation_never_escape_or_retarget_until_enter() {
        let mut s = ready(vec![project("repo"), project("tree")]);
        s.app.projects.get_mut(1).unwrap().kind = crate::EntryKind::Worktree;
        toggle_drawer(&mut s);
        for expected in [
            Some(crate::EntryKind::Repo),
            Some(crate::EntryKind::Worktree),
            None,
        ] {
            let effects = effects_of(update(&mut s, Event::Key(CanopyKey::Tab)));
            assert_eq!(s.app.project_filter, expected);
            assert!(s.app.projects_open);
            assert_eq!(s.app.dashboard_tab, DashboardTab::Home);
            assert_eq!(s.app.selected, 0);
            assert!(effects.data_requests.is_empty());
        }
        update(&mut s, Event::Key(CanopyKey::BackTab));
        assert_eq!(s.app.project_filter, Some(crate::EntryKind::Worktree));
        assert_eq!(s.app.project_cursor, 1);
        update(&mut s, Event::Key(CanopyKey::Enter));
        assert_eq!(s.app.selected, 1);
        assert!(!s.app.projects_open);
        update(&mut s, Event::Key(CanopyKey::Tab));
        assert_eq!(s.app.dashboard_tab, DashboardTab::Models);
    }

    #[test]
    fn an_empty_project_filter_cannot_open_a_hidden_project() {
        let mut s = ready(vec![project("repo")]);
        toggle_drawer(&mut s);
        select_picker_filter(&mut s, Some(crate::EntryKind::Worktree));
        update(&mut s, Event::Key(CanopyKey::Enter));
        assert!(s.app.projects_open);
        assert_eq!(s.app.selected, 0);
    }
}
