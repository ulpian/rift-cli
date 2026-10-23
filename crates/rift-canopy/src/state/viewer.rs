//! The read-only Recall Viewer's paths through the reducer: entering and
//! leaving the surface, its key map, its scroll clamps, and the
//! generation-tagged worker responses it accepts.
//!
//! Separate because the viewer is a second surface over the same `App`: its
//! panes, filter editor and worker generations have no dashboard twin, and a
//! stale response must be droppable without touching dashboard state.

use crate::command::{Verb, MAX_INPUT_LEN};
use crate::input::{on_key, Action, CanopyKey};
use crate::recall_viewer::{
    geometry, timeline_controls, timeline_item_at, timeline_line_count, Surface, ViewerErrorArea,
    ViewerEvent, ViewerFilterProgress, ViewerLoadState, ViewerPane, ViewerRequest,
    CHECKPOINT_ROW_HEIGHT,
};
use crate::render::frame::Size;
use crate::{App, Mode, Panel};

use super::verbs::run_line;
use super::{AppState, DataRequest, Effects, Flow};

impl AppState {
    /// Enter the selected repository's read-only Recall Viewer and return the
    /// sole metadata-first worker request. No persistence or ordinary dashboard
    /// refresh is emitted.
    pub fn open_recall_viewer(&mut self, target: Option<String>) -> Effects {
        let Some(project) = self
            .app
            .selected_project()
            .map(|project| project.path.clone())
        else {
            self.open_alert(crate::dispatch::Message::warning(
                "Recall Viewer",
                vec!["select a readable Git repository first".to_owned()],
            ));
            return Effects::default();
        };
        self.app.viewer.dashboard_focus = self.app.focus;
        self.app.dashboard_tab = crate::dashboard::DashboardTab::Recall;
        self.app.surface = Surface::RecallViewer;
        self.app.viewer.pane = ViewerPane::Checkpoints;
        self.app.viewer.pane_before_command = ViewerPane::Checkpoints;
        self.app.focus = Panel::Detail;
        self.app.mode = Mode::Normal;
        self.app.command_line.clear();
        self.app.command_error = None;
        let generation = self.app.viewer.start_project(project.clone());
        self.app.viewer.pane = ViewerPane::Checkpoints;
        self.dirty = true;
        Effects {
            data_requests: vec![DataRequest::Viewer(ViewerRequest::Catalogue {
                project,
                generation,
                target,
                refresh: false,
            })],
            ..Effects::default()
        }
    }

    /// Return to the dashboard state that was underneath the viewer.
    pub fn close_recall_viewer(&mut self) -> Effects {
        self.app.dashboard_tab = crate::dashboard::DashboardTab::Home;
        self.app.surface = Surface::Dashboard;
        self.app.projects_open = false;
        self.app.focus = Panel::Detail;
        self.app.mode = Mode::Normal;
        self.app.command_line.clear();
        self.app.command_error = None;
        self.app.viewer.filter_editing = false;
        self.dirty = true;
        let selected = self.app.selected_project().map(|p| p.path.clone());
        Effects {
            data_requests: std::iter::once(DataRequest::SetActive(selected.clone()))
                .chain(selected.map(DataRequest::Refresh))
                .collect(),
            ..Effects::default()
        }
    }
}

/// A key on the viewer surface: the palette keeps the closed parser and its
/// suggestions, the Filter chip is its own session-only editor, and every
/// other key navigates.
pub(super) fn on_viewer_key(s: &mut AppState, key: CanopyKey) -> Flow {
    if s.app.mode == Mode::Command {
        return viewer_command_key(s, key);
    }
    if s.app.viewer.filter_editing {
        return viewer_filter_key(s, key);
    }
    viewer_navigation_key(s, key)
}

/// Fold a generation-tagged viewer response. Project and generation must both
/// match; otherwise the response is stale and is dropped without dirtying.
pub(super) fn apply_viewer_event(s: &mut AppState, event: ViewerEvent) -> Effects {
    match event {
        ViewerEvent::Catalogue {
            project,
            generation,
            rows,
            selected,
            notice,
            complete,
        } if is_open_project(&s.app, &project)
            && s.app.viewer.catalogue_generation == generation =>
        {
            s.app
                .viewer
                .apply_catalogue(rows, selected.as_deref(), notice, complete);
            s.dirty = true;
            viewer_selection_effects(s)
        }
        ViewerEvent::Detail {
            project,
            generation,
            detail,
            complete,
        } if is_open_project(&s.app, &project) && s.app.viewer.detail_generation == generation => {
            s.app.viewer.apply_detail(detail, complete);
            s.dirty = true;
            Effects::default()
        }
        ViewerEvent::FilterProgress {
            project,
            generation,
            matching_ids,
            progress,
        } if is_open_project(&s.app, &project) && s.app.viewer.filter_generation == generation => {
            apply_filter_progress(s, matching_ids, progress)
        }
        ViewerEvent::Error {
            project,
            generation,
            area,
            message,
        } if is_open_project(&s.app, &project) => apply_viewer_error(s, generation, area, message),
        _ => Effects::default(),
    }
}

/// The dashboard focus that mirrors the viewer's pane, so the shared column
/// chrome highlights the pane the viewer is in.
pub(super) fn sync_viewer_focus(app: &mut App) {
    app.focus = match app.viewer.pane {
        ViewerPane::Projects => Panel::Projects,
        ViewerPane::Checkpoints => Panel::Detail,
        ViewerPane::Timeline => Panel::Graphs,
        ViewerPane::Command => Panel::Command,
    };
}

/// Move the cursor in the focused pane, a page at a time when `page`: the
/// rail moves the selection and opens the new project's catalogue; the
/// checkpoint list re-reads the detail under its cursor; the timeline only
/// scrolls.
pub(super) fn move_viewer(s: &mut AppState, down: bool, page: bool) -> Effects {
    match s.app.viewer.pane {
        ViewerPane::Projects => {
            let before = s.app.selected;
            let steps = if page { 8 } else { 1 };
            for _ in 0..steps {
                if down {
                    s.app.select_next();
                } else {
                    s.app.select_prev();
                }
            }
            if s.app.selected == before {
                return Effects::default();
            }
            s.arm_select_pulse();
            select_viewer_project(s)
        }
        ViewerPane::Checkpoints => {
            if s.app.viewer.move_checkpoint(down, page) {
                viewer_selection_effects(s)
            } else {
                Effects::default()
            }
        }
        ViewerPane::Timeline => {
            let len = timeline_line_count(&s.app.viewer);
            let step = if page { 10 } else { 1 };
            s.app.viewer.timeline_cursor = if down {
                s.app
                    .viewer
                    .timeline_cursor
                    .saturating_add(step)
                    .min(len.saturating_sub(1))
            } else {
                s.app.viewer.timeline_cursor.saturating_sub(step)
            };
            clamp_viewer_timeline(&mut s.app, s.size);
            Effects::default()
        }
        ViewerPane::Command => Effects::default(),
    }
}

/// Open the selected project's catalogue in the viewer under a fresh
/// generation, with the rail pane focused; inert with nothing selected. The
/// one tail behind a rail key, a rail click and `/select`, so a response to
/// the previous project is stale by construction on every door.
pub(super) fn select_viewer_project(s: &mut AppState) -> Effects {
    let Some(project) = s.app.selected_project().map(|project| project.path.clone()) else {
        return Effects::default();
    };
    let generation = s.app.viewer.start_project(project.clone());
    s.app.viewer.pane = if s.app.projects_open {
        ViewerPane::Projects
    } else {
        ViewerPane::Checkpoints
    };
    Effects {
        data_requests: vec![DataRequest::Viewer(ViewerRequest::Catalogue {
            project,
            generation,
            target: None,
            refresh: false,
        })],
        ..Effects::default()
    }
}

/// Re-read the open project's catalogue under a fresh generation, so a
/// response to the previous read is dropped as stale; `None` with no project.
pub(super) fn refresh_catalogue(s: &mut AppState) -> Option<DataRequest> {
    let project = s.app.viewer.project.clone()?;
    s.app.viewer.catalogue_generation = s.app.viewer.catalogue_generation.wrapping_add(1).max(1);
    Some(DataRequest::Viewer(ViewerRequest::Catalogue {
        project,
        generation: s.app.viewer.catalogue_generation,
        target: None,
        refresh: true,
    }))
}

/// Flip the timeline between the checkpoint's own items and the full session.
pub(super) fn toggle_full_session(s: &mut AppState) {
    s.app.viewer.full_session = !s.app.viewer.full_session;
    clamp_viewer_timeline(&mut s.app, s.size);
}

/// Jump to the newest checkpoint, re-reading its detail if the cursor moved.
pub(super) fn select_latest_checkpoint(s: &mut AppState) -> Effects {
    if s.app.viewer.select_latest() {
        viewer_selection_effects(s)
    } else {
        Effects::default()
    }
}

/// The reads a checkpoint cursor move owes: the detail of the row under the
/// cursor when it is not already the one shown, and the excerpts of the rows
/// around it that have none yet.
pub(super) fn viewer_selection_effects(s: &mut AppState) -> Effects {
    clamp_viewer_checkpoint(&mut s.app, s.size);
    let Some(project) = s.app.viewer.project.clone() else {
        return Effects::default();
    };
    let Some(checkpoint_id) = s
        .app
        .viewer
        .selected_row()
        .map(|row| row.checkpoint_id.clone())
    else {
        return Effects::default();
    };
    let mut requests = Vec::new();
    if s.app.viewer.detail_target.as_deref() != Some(checkpoint_id.as_str()) {
        let generation = s.app.viewer.start_detail(checkpoint_id.clone());
        requests.push(DataRequest::Viewer(ViewerRequest::Detail {
            project: project.clone(),
            generation,
            checkpoint_id,
        }));
    }
    requests.extend(visible_excerpt_request(&s.app, project));
    Effects {
        data_requests: requests,
        ..Effects::default()
    }
}

/// Clamp both viewer viewports to their content at `size`.
pub(super) fn clamp_viewer_scrolls(app: &mut App, size: Size) {
    clamp_viewer_checkpoint(app, size);
    clamp_viewer_timeline(app, size);
}

/// Keep the timeline cursor in range and its viewport following the cursor.
pub(super) fn clamp_viewer_timeline(app: &mut App, size: Size) {
    let rect = geometry(size, app.viewer.pane).timeline;
    let visible = usize::from(timeline_controls(rect).content.h).max(1);
    let len = timeline_line_count(&app.viewer);
    app.viewer.timeline_cursor = app.viewer.timeline_cursor.min(len.saturating_sub(1));
    app.viewer.timeline_scroll = app.viewer.timeline_scroll.min(len.saturating_sub(visible));
    if app.viewer.timeline_cursor < app.viewer.timeline_scroll {
        app.viewer.timeline_scroll = app.viewer.timeline_cursor;
    } else if app.viewer.timeline_cursor >= app.viewer.timeline_scroll.saturating_add(visible) {
        app.viewer.timeline_scroll = app.viewer.timeline_cursor + 1 - visible;
    }
}

/// The verbs the viewer's palette accepts: read-only ones, and the
/// navigation verbs that act on the viewer itself.
pub(super) fn viewer_command_allowed(verb: &Verb) -> bool {
    matches!(
        verb,
        Verb::Viewer(_)
            | Verb::Select(_)
            | Verb::Focus(_)
            | Verb::Help
            | Verb::Quit
            | Verb::Status
            | Verb::Checkpoint(_)
            | Verb::Session(_)
            | Verb::Explain(_)
            | Verb::Doctor
            | Verb::GcDryRun
            | Verb::ProjectsList
    )
}

/// Command editing keeps the existing closed parser/suggestion behavior, but
/// its completion is folded through the viewer-aware allowlist in `run_line`.
fn viewer_command_key(s: &mut AppState, key: CanopyKey) -> Flow {
    if matches!(key, CanopyKey::Esc | CanopyKey::Quit) {
        s.app.mode = Mode::Normal;
        s.app.command_line.clear();
        s.app.command_error = None;
        s.app.viewer.pane = s.app.viewer.pane_before_command;
        sync_viewer_focus(&mut s.app);
        s.dirty = true;
        return Flow::Continue(Effects::default());
    }
    let action = on_key(&mut s.app, key);
    s.dirty = true;
    match action {
        Action::RunLine(line) => run_line(s, &line),
        Action::Quit => Flow::Quit,
        _ => Flow::Continue(Effects::default()),
    }
}

/// The Filter chip is a separate session-only editor. Every edit advances
/// the generation, immediately recomputes metadata matches, and asks the
/// worker to scan transcript excerpts; older scans become stale/cancelled.
fn viewer_filter_key(s: &mut AppState, key: CanopyKey) -> Flow {
    let changed = match key {
        CanopyKey::Char(character) if s.app.viewer.filter.chars().count() < MAX_INPUT_LEN => {
            s.app.viewer.filter.push(character);
            true
        }
        CanopyKey::Backspace => s.app.viewer.filter.pop().is_some(),
        CanopyKey::Enter | CanopyKey::Esc => {
            s.app.viewer.filter_editing = false;
            false
        }
        CanopyKey::Quit => {
            s.app.quit = true;
            return Flow::Quit;
        }
        _ => false,
    };
    s.dirty = true;
    if !changed {
        return Flow::Continue(Effects::default());
    }
    let generation = s.app.viewer.changed_filter();
    let mut effects = viewer_selection_effects(s);
    if let Some(project) = s.app.viewer.project.clone() {
        effects
            .data_requests
            .push(DataRequest::Viewer(ViewerRequest::FilterScan {
                project,
                generation,
                query: s.app.viewer.filter.clone(),
            }));
    }
    Flow::Continue(effects)
}

/// Navigation on the viewer surface: Esc and `r` leave it, `q` quits, and
/// every other key moves a pane or a cursor, then re-clamps and redraws.
fn viewer_navigation_key(s: &mut AppState, key: CanopyKey) -> Flow {
    let effects = match key {
        CanopyKey::Esc | CanopyKey::Char('r') => {
            return Flow::Continue(s.close_recall_viewer());
        }
        CanopyKey::Char('q') | CanopyKey::Quit => {
            s.app.quit = true;
            return Flow::Quit;
        }
        CanopyKey::Char('/') => {
            open_viewer_command(s);
            Effects::default()
        }
        CanopyKey::Char('f') => {
            s.app.viewer.begin_filter_editing();
            Effects::default()
        }
        CanopyKey::Char('F') => {
            toggle_full_session(s);
            Effects::default()
        }
        CanopyKey::Char('L') => select_latest_checkpoint(s),
        CanopyKey::Char('R') => reload_catalogue(s),
        CanopyKey::Tab | CanopyKey::BackTab | CanopyKey::Left | CanopyKey::Right => {
            move_viewer_pane(s, key);
            Effects::default()
        }
        CanopyKey::Char('j') | CanopyKey::Down => move_viewer(s, true, false),
        CanopyKey::Char('k') | CanopyKey::Up => move_viewer(s, false, false),
        CanopyKey::PageDown => move_viewer(s, true, true),
        CanopyKey::PageUp => move_viewer(s, false, true),
        CanopyKey::Home => viewer_home(s),
        CanopyKey::End => viewer_end(s),
        CanopyKey::Enter | CanopyKey::Char(' ') => viewer_activate(s),
        _ => Effects::default(),
    };
    debug_assert_eq!(s.app.surface, Surface::RecallViewer);
    clamp_viewer_scrolls(&mut s.app, s.size);
    s.dirty = true;
    Flow::Continue(effects)
}

/// Enter the palette from the viewer, remembering the pane to return to.
fn open_viewer_command(s: &mut AppState) {
    if s.app.viewer.pane != ViewerPane::Command {
        s.app.viewer.pane_before_command = s.app.viewer.pane;
    }
    s.app.viewer.pane = ViewerPane::Command;
    s.app.focus = Panel::Command;
    s.app.mode = Mode::Command;
    s.app.command_line.clear();
    s.app.command_error = None;
    s.arm_suggest_fade();
}

/// `R`: re-read the catalogue and show it loading meanwhile.
fn reload_catalogue(s: &mut AppState) -> Effects {
    let Some(request) = refresh_catalogue(s) else {
        return Effects::default();
    };
    s.app.viewer.catalogue_state = ViewerLoadState::Loading;
    Effects {
        data_requests: vec![request],
        ..Effects::default()
    }
}

/// Tab and Shift-Tab cycle the panes; Left and Right step between the rail,
/// the checkpoints and the timeline.
fn move_viewer_pane(s: &mut AppState, key: CanopyKey) {
    let pane = s.app.viewer.pane;
    s.app.viewer.pane = match key {
        CanopyKey::Tab | CanopyKey::BackTab => pane.cycle(key == CanopyKey::Tab),
        CanopyKey::Left => match pane {
            ViewerPane::Projects | ViewerPane::Checkpoints => ViewerPane::Projects,
            ViewerPane::Timeline | ViewerPane::Command => ViewerPane::Checkpoints,
        },
        CanopyKey::Right => match pane {
            ViewerPane::Projects => ViewerPane::Checkpoints,
            ViewerPane::Checkpoints | ViewerPane::Timeline | ViewerPane::Command => {
                ViewerPane::Timeline
            }
        },
        _ => pane,
    };
    sync_viewer_focus(&mut s.app);
}

/// Home: the first checkpoint (re-reading its detail), or the top of the
/// timeline.
fn viewer_home(s: &mut AppState) -> Effects {
    match s.app.viewer.pane {
        ViewerPane::Checkpoints => {
            if s.app.viewer.checkpoint_edge(false) {
                viewer_selection_effects(s)
            } else {
                Effects::default()
            }
        }
        ViewerPane::Timeline => {
            s.app.viewer.timeline_cursor = 0;
            s.app.viewer.timeline_scroll = 0;
            Effects::default()
        }
        ViewerPane::Projects | ViewerPane::Command => Effects::default(),
    }
}

/// End: the last checkpoint (re-reading its detail), or the bottom of the
/// timeline.
fn viewer_end(s: &mut AppState) -> Effects {
    match s.app.viewer.pane {
        ViewerPane::Checkpoints => {
            if s.app.viewer.checkpoint_edge(true) {
                viewer_selection_effects(s)
            } else {
                Effects::default()
            }
        }
        ViewerPane::Timeline => {
            let end = timeline_line_count(&s.app.viewer).saturating_sub(1);
            s.app.viewer.timeline_cursor = end;
            clamp_viewer_timeline(&mut s.app, s.size);
            Effects::default()
        }
        ViewerPane::Projects | ViewerPane::Command => Effects::default(),
    }
}

/// Enter or Space: step into the next pane from the rail or the checkpoints,
/// fold or unfold the timeline item under the cursor, or open the palette
/// from the command pane.
fn viewer_activate(s: &mut AppState) -> Effects {
    match s.app.viewer.pane {
        ViewerPane::Projects => {
            s.app.viewer.pane = ViewerPane::Checkpoints;
            sync_viewer_focus(&mut s.app);
        }
        ViewerPane::Checkpoints => {
            s.app.viewer.pane = ViewerPane::Timeline;
            sync_viewer_focus(&mut s.app);
        }
        ViewerPane::Timeline => {
            if let Some(id) = timeline_item_at(&s.app.viewer, s.app.viewer.timeline_cursor) {
                s.app.viewer.toggle_expanded(&id);
                clamp_viewer_timeline(&mut s.app, s.size);
            }
        }
        ViewerPane::Command => {
            s.app.viewer.pane_before_command = ViewerPane::Checkpoints;
            s.app.mode = Mode::Command;
            s.app.focus = Panel::Command;
            s.arm_suggest_fade();
        }
    }
    Effects::default()
}

/// Whether `project` is the one the viewer has open — the first half of
/// every response's staleness check.
fn is_open_project(app: &App, project: &str) -> bool {
    app.viewer.project.as_deref() == Some(project)
}

/// A filter scan's progress narrows the visible rows; if that moved the
/// selection, the new row's detail is read like any cursor move.
fn apply_filter_progress(
    s: &mut AppState,
    matching_ids: Vec<String>,
    progress: ViewerFilterProgress,
) -> Effects {
    let before = s
        .app
        .viewer
        .selected_row()
        .map(|row| row.checkpoint_id.clone());
    s.app.viewer.apply_filter_progress(matching_ids, progress);
    s.dirty = true;
    let after = s
        .app
        .viewer
        .selected_row()
        .map(|row| row.checkpoint_id.clone());
    if before == after {
        return Effects::default();
    }
    viewer_selection_effects(s)
}

/// A worker error lands on the area it names, and only if it answers that
/// area's live generation; a stale error is dropped without dirtying.
fn apply_viewer_error(
    s: &mut AppState,
    generation: u64,
    area: ViewerErrorArea,
    message: String,
) -> Effects {
    let live = match area {
        ViewerErrorArea::Catalogue => s.app.viewer.catalogue_generation,
        ViewerErrorArea::Detail => s.app.viewer.detail_generation,
        ViewerErrorArea::Filter => s.app.viewer.filter_generation,
    };
    if live != generation {
        return Effects::default();
    }
    match area {
        ViewerErrorArea::Catalogue => {
            s.app.viewer.catalogue_state = ViewerLoadState::Error(message);
        }
        ViewerErrorArea::Detail => {
            s.app.viewer.detail_state = ViewerLoadState::Error(message);
        }
        ViewerErrorArea::Filter => {
            s.app.viewer.filter_progress = None;
            s.app.viewer.notice = Some(message);
        }
    }
    s.dirty = true;
    Effects::default()
}

/// Keep the checkpoint cursor visible: the list's header grows a row while a
/// filter scan or a new-checkpoint count is shown.
fn clamp_viewer_checkpoint(app: &mut App, size: Size) {
    let rect = geometry(size, app.viewer.pane).checkpoints;
    let header = if app.viewer.filter_progress.is_some() || app.viewer.new_count > 0 {
        4
    } else {
        3
    };
    let rows = usize::from(rect.h.saturating_sub(header)) / usize::from(CHECKPOINT_ROW_HEIGHT);
    let rows = rows.max(1);
    let max = app.viewer.visible.len().saturating_sub(rows);
    app.viewer.checkpoint_scroll = app.viewer.checkpoint_scroll.min(max);
    if app.viewer.selected < app.viewer.checkpoint_scroll {
        app.viewer.checkpoint_scroll = app.viewer.selected;
    } else if app.viewer.selected >= app.viewer.checkpoint_scroll.saturating_add(rows) {
        app.viewer.checkpoint_scroll = app.viewer.selected + 1 - rows;
    }
}

/// The excerpt read for the rows around the selection that have none yet —
/// a few above and a screen below, so scrolling stays ahead of the worker.
fn visible_excerpt_request(app: &App, project: String) -> Option<DataRequest> {
    let start = app.viewer.selected.saturating_sub(4);
    let end = (app.viewer.selected + 12).min(app.viewer.visible.len());
    let checkpoint_ids = app
        .viewer
        .visible
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .filter_map(|index| app.viewer.catalogue.get(*index))
        .filter(|row| row.excerpt.is_none())
        .map(|row| row.checkpoint_id.clone())
        .collect::<Vec<_>>();
    if checkpoint_ids.is_empty() {
        return None;
    }
    Some(DataRequest::Viewer(ViewerRequest::VisibleExcerpts {
        project,
        generation: app.viewer.catalogue_generation,
        checkpoint_ids,
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{Overlay, Severity};
    use crate::hit::Target;
    use crate::state::click::apply_click;
    use crate::state::fixtures::*;
    use crate::state::{update, Event};

    /// Every key the viewer binds, plus one it does not.
    fn viewer_keys() -> [CanopyKey; 24] {
        [
            CanopyKey::Esc,
            CanopyKey::Char('r'),
            CanopyKey::Char('q'),
            CanopyKey::Quit,
            CanopyKey::Char('/'),
            CanopyKey::Char('f'),
            CanopyKey::Char('F'),
            CanopyKey::Char('L'),
            CanopyKey::Char('R'),
            CanopyKey::Tab,
            CanopyKey::BackTab,
            CanopyKey::Left,
            CanopyKey::Right,
            CanopyKey::Char('j'),
            CanopyKey::Down,
            CanopyKey::Char('k'),
            CanopyKey::Up,
            CanopyKey::PageDown,
            CanopyKey::PageUp,
            CanopyKey::Home,
            CanopyKey::End,
            CanopyKey::Enter,
            CanopyKey::Char(' '),
            CanopyKey::Char('x'),
        ]
    }

    #[test]
    fn viewer_keys_persist_nothing_in_any_pane() {
        let base = populated_viewer_for_wheel(120, 40);
        for pane in [
            crate::recall_viewer::ViewerPane::Projects,
            crate::recall_viewer::ViewerPane::Checkpoints,
            crate::recall_viewer::ViewerPane::Timeline,
            crate::recall_viewer::ViewerPane::Command,
        ] {
            for key in viewer_keys() {
                let mut state = base.clone();
                state.app.viewer.pane = pane;
                state.app.viewer.selected = 10;
                state.app.viewer.checkpoint_scroll = 5;
                state.app.viewer.timeline_cursor = 10;
                state.app.viewer.timeline_scroll = 5;
                match update(&mut state, Event::Key(key)) {
                    Flow::Continue(effects) => assert!(
                        effects.persist.is_empty(),
                        "viewer key {key:?} in {pane:?} persisted data"
                    ),
                    Flow::Quit => assert!(state.app.quit),
                }
            }
        }
    }

    #[test]
    fn viewer_filter_edits_persist_nothing() {
        let base = populated_viewer_for_wheel(120, 40);
        for key in [
            CanopyKey::Char('x'),
            CanopyKey::Backspace,
            CanopyKey::Enter,
            CanopyKey::Esc,
            CanopyKey::Quit,
            CanopyKey::Down,
        ] {
            let mut state = base.clone();
            state.app.viewer.begin_filter_editing();
            state.app.viewer.filter = "seed".to_owned();
            let flow = update(&mut state, Event::Key(key));
            if let Flow::Continue(effects) = flow {
                assert!(effects.persist.is_empty());
            }
        }
        let mut bounded = base.clone();
        bounded.app.viewer.begin_filter_editing();
        bounded.app.viewer.filter = "x".repeat(crate::command::MAX_INPUT_LEN);
        assert!(
            effects_of(update(&mut bounded, Event::Key(CanopyKey::Char('y'))))
                .persist
                .is_empty()
        );
        let mut no_project = base.clone();
        no_project.app.viewer.project = None;
        no_project.app.viewer.begin_filter_editing();
        assert!(
            effects_of(update(&mut no_project, Event::Key(CanopyKey::Char('x'))))
                .persist
                .is_empty()
        );
    }

    #[test]
    fn viewer_command_lines_persist_nothing() {
        let base = populated_viewer_for_wheel(120, 40);
        for (line, key) in [
            ("/status", CanopyKey::Enter),
            ("/verify", CanopyKey::Enter),
            ("", CanopyKey::Esc),
            ("", CanopyKey::Quit),
            ("", CanopyKey::Char('x')),
        ] {
            let mut state = base.clone();
            state.app.mode = Mode::Command;
            state.app.viewer.pane = crate::recall_viewer::ViewerPane::Command;
            state.app.viewer.pane_before_command = crate::recall_viewer::ViewerPane::Timeline;
            state.app.command_line = line.to_owned();
            if let Flow::Continue(effects) = update(&mut state, Event::Key(key)) {
                assert!(effects.persist.is_empty());
            }
        }
    }

    /// The populated viewer and the project it has open.
    fn viewer_base() -> (AppState, String) {
        let base = populated_viewer_for_wheel(120, 40);
        let project = base.app.viewer.project.clone().expect("viewer project");
        (base, project)
    }

    #[test]
    fn viewer_catalogue_and_detail_responses_of_the_live_generation_apply() {
        let (base, project) = viewer_base();
        let rows = (*base.app.viewer.catalogue).clone();
        let detail = (*base.app.viewer.detail).clone();
        for complete in [false, true] {
            let mut state = base.clone();
            let effects = effects_of(update(
                &mut state,
                Event::Viewer(ViewerEvent::Catalogue {
                    project: project.clone(),
                    generation: base.app.viewer.catalogue_generation,
                    rows: rows.clone(),
                    selected: Some("cp-010".to_owned()),
                    notice: Some("catalogue notice".to_owned()),
                    complete,
                }),
            ));
            assert!(effects.persist.is_empty());
            assert_eq!(
                state.app.viewer.catalogue_state,
                if complete {
                    ViewerLoadState::Ready
                } else {
                    ViewerLoadState::Partial
                }
            );

            let mut state = base.clone();
            assert!(effects_of(update(
                &mut state,
                Event::Viewer(ViewerEvent::Detail {
                    project: project.clone(),
                    generation: base.app.viewer.detail_generation,
                    detail: detail.clone(),
                    complete,
                }),
            ))
            .persist
            .is_empty());
        }
    }

    #[test]
    fn viewer_filter_progress_applies_only_for_the_live_generation() {
        let (base, project) = viewer_base();
        let mut changed = base.clone();
        changed.app.viewer.begin_filter_editing();
        changed.app.viewer.filter = "transcript-only".to_owned();
        let generation = changed.app.viewer.changed_filter();
        assert!(effects_of(update(
            &mut changed,
            Event::Viewer(ViewerEvent::FilterProgress {
                project: project.clone(),
                generation,
                matching_ids: vec!["cp-079".to_owned()],
                progress: ViewerFilterProgress {
                    scanned: 80,
                    total: 80,
                    complete: true,
                },
            }),
        ))
        .persist
        .is_empty());

        let mut unchanged = base.clone();
        assert!(effects_of(update(
            &mut unchanged,
            Event::Viewer(ViewerEvent::FilterProgress {
                project,
                generation: base.app.viewer.filter_generation,
                matching_ids: Vec::new(),
                progress: ViewerFilterProgress {
                    scanned: 1,
                    total: 80,
                    complete: false,
                },
            }),
        ))
        .persist
        .is_empty());
    }

    #[test]
    fn viewer_errors_land_on_their_area_and_stale_ones_are_dropped() {
        let (base, project) = viewer_base();
        for (area, generation) in [
            (
                ViewerErrorArea::Catalogue,
                base.app.viewer.catalogue_generation,
            ),
            (ViewerErrorArea::Detail, base.app.viewer.detail_generation),
            (ViewerErrorArea::Filter, base.app.viewer.filter_generation),
        ] {
            let mut state = base.clone();
            assert!(effects_of(update(
                &mut state,
                Event::Viewer(ViewerEvent::Error {
                    project: project.clone(),
                    generation,
                    area,
                    message: "safe error".to_owned(),
                }),
            ))
            .persist
            .is_empty());

            let mut stale_response_state = base.clone();
            stale_response_state.dirty = false;
            assert!(effects_of(update(
                &mut stale_response_state,
                Event::Viewer(ViewerEvent::Error {
                    project: project.clone(),
                    generation: generation.saturating_add(100),
                    area,
                    message: "stale".to_owned(),
                }),
            ))
            .is_empty());
            assert!(!stale_response_state.dirty);
        }
    }

    #[test]
    fn a_viewer_response_for_another_project_is_dropped() {
        let (mut foreign, _project) = viewer_base();
        let rows = (*foreign.app.viewer.catalogue).clone();
        let generation = foreign.app.viewer.catalogue_generation;
        foreign.dirty = false;
        assert!(effects_of(update(
            &mut foreign,
            Event::Viewer(ViewerEvent::Catalogue {
                project: "/other".to_owned(),
                generation,
                rows,
                selected: None,
                notice: None,
                complete: true,
            }),
        ))
        .is_empty());
        assert!(!foreign.dirty);
    }

    #[test]
    fn recall_viewer_entry_navigation_and_exit_emit_no_persistence() {
        let mut s = ready(vec![project("a")]);
        let opened = effects_of(update(&mut s, Event::Key(CanopyKey::Char('r'))));
        assert_eq!(s.app.surface, crate::recall_viewer::Surface::RecallViewer);
        assert!(opened.persist.is_empty());
        assert!(matches!(
            opened.data_requests.as_slice(),
            [DataRequest::Viewer(
                crate::recall_viewer::ViewerRequest::Catalogue {
                    project,
                    target: None,
                    refresh: false,
                    ..
                }
            )] if project == "/r/a"
        ));

        for key in [
            CanopyKey::Tab,
            CanopyKey::Down,
            CanopyKey::Char('f'),
            CanopyKey::Esc,
        ] {
            let effects = effects_of(update(&mut s, Event::Key(key)));
            assert!(effects.persist.is_empty());
        }
        // Esc first closes filter editing; the dashboard chip exercises the
        // explicit surface-close path independently.
        let _ = apply_click(&mut s, Target::ViewerDashboard);
        assert_eq!(s.app.surface, crate::recall_viewer::Surface::Dashboard);
    }

    #[test]
    fn recall_viewer_drops_stale_worker_generations() {
        let mut s = ready(vec![project("a")]);
        let _ = s.open_recall_viewer(None);
        let generation = s.app.viewer.catalogue_generation;
        let flow = update(
            &mut s,
            Event::Viewer(crate::recall_viewer::ViewerEvent::Catalogue {
                project: "/r/a".to_owned(),
                generation: generation.saturating_add(1),
                rows: Vec::new(),
                selected: None,
                notice: Some("stale".to_owned()),
                complete: true,
            }),
        );
        assert!(effects_of(flow).is_empty());
        assert!(s.app.viewer.notice.is_none());
        assert_eq!(
            s.app.viewer.catalogue_state,
            crate::recall_viewer::ViewerLoadState::Loading
        );
    }

    #[test]
    fn recall_viewer_command_allowlist_refuses_verify_and_allows_status() {
        let mut refused = ready(vec![project("a")]);
        let _ = refused.open_recall_viewer(None);
        let effects = effects_of(run_line(&mut refused, "verify"));
        assert!(effects.is_empty());
        assert!(matches!(
            &refused.app.overlay,
            Some(Overlay::Alert(message)) if message.severity == Severity::Warning
        ));

        let mut allowed = ready(vec![project("a")]);
        let _ = allowed.open_recall_viewer(None);
        let effects = effects_of(run_line(&mut allowed, "status"));
        assert!(effects.persist.is_empty());
        assert!(matches!(
            effects.dispatches.as_slice(),
            [(crate::dispatch::DispatchRequest::Status, repo)] if repo == "/r/a"
        ));
    }

    #[test]
    fn palette_viewer_repo_is_session_only_and_reducer_pure() {
        let mut s = ready(vec![project("a")]);
        let effects = effects_of(run_line(&mut s, "viewer --repo relative/unregistered"));
        assert!(effects.persist.is_empty());
        assert_eq!(s.app.projects.len(), 2);
        assert_eq!(s.app.projects[1].path, "relative/unregistered");
        assert_eq!(s.app.surface, crate::recall_viewer::Surface::RecallViewer);

        let before = s.app.viewer.full_session;
        let click = effects_of(apply_click(&mut s, Target::ViewerFullSession));
        assert!(click.persist.is_empty());
        assert_ne!(s.app.viewer.full_session, before);
    }
}
