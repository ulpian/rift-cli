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

pub(super) fn on_navigation_key(s: &mut AppState, key: CanopyKey) -> Option<Flow> {
    let navigation_allowed = s.app.overlay.is_none()
        && !s.app.project_settings.running
        && !s.app.viewer.filter_editing
        && matches!(s.app.mode, Mode::Normal | Mode::ProjectSettings);
    if !navigation_allowed {
        return None;
    }
    match key {
        CanopyKey::Tab | CanopyKey::BackTab if s.app.projects_open => Some(toggle_drawer(s)),
        CanopyKey::Char('[' | ']') => Some(select_tab(
            s,
            s.app.dashboard_tab.next(key == CanopyKey::Char(']')),
        )),
        CanopyKey::Left if !s.app.projects_open => Some(toggle_drawer(s)),
        CanopyKey::Esc | CanopyKey::Right | CanopyKey::Enter if s.app.projects_open => {
            Some(toggle_drawer(s))
        }
        CanopyKey::Up | CanopyKey::Down | CanopyKey::Char('j' | 'k') if s.app.projects_open => {
            let before = s.app.selected_project().map(|p| p.path.clone());
            let down = matches!(key, CanopyKey::Down | CanopyKey::Char('j'));
            s.app.selected = if down {
                s.app
                    .selected
                    .saturating_add(1)
                    .min(s.app.projects.len().saturating_sub(1))
            } else {
                s.app.selected.saturating_sub(1)
            };
            let mut effects = Effects::default();
            super::reduce::selection_change_effects(s, &before, &mut effects);
            s.dirty = true;
            Some(Flow::Continue(effects))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall_viewer::{ViewerPane, ViewerRequest};
    use crate::state::fixtures::{effects_of, project, project_settings_state, ready};
    use crate::state::{update, DataRequest, Event, ProjectData};

    #[test]
    fn drawer_selection_retargets_viewer_and_closes_to_a_visible_pane() {
        let mut s = ready(vec![project("a"), project("b")]);
        s.open_recall_viewer(None);
        toggle_drawer(&mut s);
        let effects = effects_of(update(&mut s, Event::Key(CanopyKey::Down)));
        assert_eq!(s.app.viewer.project.as_deref(), Some("/r/b"));
        assert!(effects.data_requests.iter().any(|request| matches!(request,
            DataRequest::Viewer(ViewerRequest::Catalogue { project, .. }) if project == "/r/b")));
        update(&mut s, Event::Key(CanopyKey::Enter));
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
    fn closed_drawer_is_excluded_from_normal_focus_cycle() {
        let mut s = ready(vec![project("a")]);
        select_tab(&mut s, DashboardTab::Models);
        for _ in 0..8 {
            update(&mut s, Event::Key(CanopyKey::Tab));
            assert_ne!(s.app.focus, Panel::Projects);
            assert_ne!(s.app.focus, Panel::Graphs);
        }
    }
}
