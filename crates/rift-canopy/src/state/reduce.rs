//! The per-family reducers [`update`](super::update) fans out to, and the
//! effects a selection move or a keyboard activation owes.
//!
//! Separate from the facade so each event family reads as one function, and
//! from the pointer and verb paths so what a key, a click and a palette verb
//! share — the selection-change effects, the layout persist — has one home.

use std::sync::Arc;

use crate::dispatch::{DispatchOutcome, DispatchRequest, Overlay, OverlayControl};
use crate::hit::Target;
use crate::input::{Action, CanopyKey};
use crate::recall_viewer::Surface;
use crate::render::frame::Size;
use crate::{App, Mode, ProjectStatus};

use super::actions::{profile_or_fold_action, remove_project_op, reorder_project_op};
use super::anim::refresh_every_ticks;
use super::click::open_header_help;
use super::effects::persist_flow;
use super::events::fold_health;
use super::openers::{
    open_adopt_confirm, open_project_settings, open_savings_breakdown, open_worktree_axis,
    select_recall_window,
};
use super::verbs::{confirm_dispatch, run_line};
use super::viewer::{clamp_viewer_scrolls, on_viewer_key, refresh_catalogue};
use super::{
    AppState, DataRequest, Effects, FeedPayload, FeedUpdate, Flow, HealthEvent, PersistOp,
    ProjectData, WatchMode,
};

/// A resize re-clamps the open surface's scroll offsets and redraws.
pub(super) fn on_resize(s: &mut AppState, size: Size) -> Flow {
    s.size = size;
    clamp_surface_scrolls(s);
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// The idle cadence never dirties (the ~0% CPU invariant). Every
/// `refresh_every_ticks`, the *selected* project is re-requested so a
/// time-based status change (Running → Idle) surfaces without a cursor
/// change; the bus re-derives and the `ProjectData` arm dirties only if the
/// status actually moved. The viewer re-reads its catalogue on the same
/// cadence, and an open Project Settings page polls every tenth tick so a
/// step that finished off-thread shows without waiting a minute.
pub(super) fn on_tick(s: &mut AppState) -> Flow {
    s.tick = s.tick.wrapping_add(1);
    let due = s.tick.is_multiple_of(refresh_every_ticks(s.watch_mode));
    let settings_due = s.app.mode == Mode::ProjectSettings && s.tick.is_multiple_of(10);
    let request = match s.app.surface {
        Surface::RecallViewer if due => refresh_catalogue(s),
        Surface::Dashboard if due || settings_due => selected_refresh(&s.app),
        Surface::RecallViewer | Surface::Dashboard => None,
    };
    Flow::Continue(Effects {
        data_requests: request.into_iter().collect(),
        ..Effects::default()
    })
}

/// Store a project's fresh status and detail, then let the add flow's
/// deferred decision and the open Project Settings page read the same delivery.
pub(super) fn on_project_data(s: &mut AppState, pd: &ProjectData) -> Flow {
    // Only dirty when the status actually changed, so a re-stat that finds
    // nothing new never forces a redraw (the ~0% CPU invariant).
    if s.apply_project_data(pd) {
        s.dirty = true;
    }
    if pending_setup_prompt(s, pd) {
        return open_project_settings(s);
    }
    if s.app.mode == Mode::ProjectSettings
        && s.app
            .selected_project()
            .is_some_and(|p| p.path == pd.project)
    {
        if let Some(detail) = pd.detail.as_ref() {
            s.app
                .project_settings
                .poll((*detail.project_settings).clone());
            s.dirty = true;
        }
    }
    Flow::Continue(Effects::default())
}

/// Apply one feed update to the selected project's surfaces. A feed for any
/// other project is stale routing and is dropped (the bus recomputes on the
/// next `SetActive`). The activity card (column 2) patches directly and dirties
/// on real change; the column-3 feeds flow through `set_col3` (which dirties
/// and arms a tween only on a real change).
pub(super) fn apply_feed(s: &mut AppState, update: FeedUpdate) {
    let selected = s.app.selected_project().map(|p| p.path.clone());
    if selected.as_deref() != Some(update.project.as_str()) {
        return;
    }
    match update.payload {
        FeedPayload::Activity(activity) => {
            if *s.activity != activity {
                s.activity = Arc::new(activity);
                s.dirty = true;
            }
        }
        FeedPayload::Tokens { tokens, breakdown } => {
            let mut col3 = (*s.col3).clone();
            col3.tokens = tokens;
            col3.breakdown = breakdown;
            s.set_col3(col3);
        }
        FeedPayload::System {
            context,
            metrics,
            active_sessions,
        } => {
            let mut col3 = (*s.col3).clone();
            col3.nic = context;
            col3.metrics = metrics;
            col3.active_sessions = active_sessions;
            s.set_col3(col3);
        }
        FeedPayload::Recall { savings, calls } => {
            let mut col3 = (*s.col3).clone();
            col3.savings = savings;
            col3.calls = calls;
            s.set_col3(col3);
        }
        FeedPayload::Dispatch(dispatch) => {
            let mut col3 = (*s.col3).clone();
            col3.dispatch = dispatch;
            s.set_col3(col3);
        }
        FeedPayload::Recap(recap) => {
            let mut col3 = (*s.col3).clone();
            col3.recap = recap;
            s.set_col3(col3);
        }
    }
}

/// A key press: the viewer owns it on its own surface; otherwise the pure
/// [`crate::input::on_key`] reducer runs and its action folds into a flow,
/// with the selection-change and leave-the-editor effects appended.
pub(super) fn on_key(s: &mut AppState, key: CanopyKey) -> Flow {
    if let Some(flow) = super::dashboard::on_navigation_key(s, key) {
        return flow;
    }
    if viewer_owns_the_key(&s.app) {
        return on_viewer_key(s, key);
    }
    let before = s.app.selected_project().map(|p| p.path.clone());
    let mode_before = s.app.mode;
    // Keyboard-activation parity (the press-flash rule): Enter/Space on a focused
    // modal button flashes it exactly like a click. Captured *before*
    // `on_key`, which may dismiss the modal.
    let modal_flash = modal_activation_flash(&s.app, key);
    let action = crate::input::on_key(&mut s.app, key);
    if let Some(target) = modal_flash {
        s.arm_press_pulse(target);
    }
    if mode_before != Mode::Command && s.app.mode == Mode::Command {
        // Entering command mode opens the suggest panel: arm its fade
        // (the animator contract; reduced motion / ASCII snap to the final frame).
        s.arm_suggest_fade();
        s.app.suggest_highlight = 0;
    }
    s.dirty = true; // any key may change selection/mode/buffer
    clamp_surface_scrolls(s); // a scroll key can never pass the end
    let mut flow = key_action_flow(s, action);
    if let Flow::Continue(eff) = &mut flow {
        // When the selection moves, ask the bus for the new project's data.
        if s.app.surface != Surface::RecallViewer {
            selection_change_effects(s, &before, eff);
        }
        // Leaving the panel editor persists the arrangement.
        if mode_before == Mode::PanelEditor && s.app.mode != Mode::PanelEditor {
            eff.persist.extend(layout_persist_op(&s.app));
        }
    }
    flow
}

/// Fold the report; dirty only when the dot actually changes (a steady
/// Healthy stream never forces a redraw).
pub(super) fn on_health(s: &mut AppState, ev: HealthEvent) -> Flow {
    let next = fold_health(ev);
    if s.health != next {
        s.health = next;
        s.dirty = true;
    }
    Flow::Continue(Effects::default())
}

/// Adopt the reported wake mode; dirty exactly once per CHANGE (a repeated
/// report never forces a redraw). The cadence reads it on the next tick; the
/// System panel row rides the col-3 model.
pub(super) fn on_watch_mode(s: &mut AppState, mode: WatchMode) -> Flow {
    if s.watch_mode == mode {
        return Flow::Continue(Effects::default());
    }
    s.watch_mode = mode;
    let mut col3 = (*s.col3).clone();
    col3.watch = mode;
    s.set_col3(col3); // dirties; the System panel row rides the model
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// A delegated dispatch finished: bump its verb's frecency only on success,
/// and render the outcome into the results overlay either way.
pub(super) fn on_dispatch_done(
    s: &mut AppState,
    request: &DispatchRequest,
    outcome: &DispatchOutcome,
) -> Flow {
    if outcome.code == 0 {
        if let Some(word) = request.verb_line().split_whitespace().nth(1) {
            s.app.frecency.bump(word);
        }
    }
    s.app.open_overlay(Overlay::results(request, outcome));
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// After a possible selection move, reset the per-project sections, ask the bus
/// for the newly-selected project's data, and set
/// `eff.load_layout` so the loop restores the new project's saved arrangement
/// (per-project layout persistence). A no-op when the selection did not change. Shared by the keyboard
/// and mouse paths so a click and an arrow key are behaviorally identical.
pub(super) fn selection_change_effects(
    s: &mut AppState,
    before: &Option<String>,
    eff: &mut Effects,
) {
    let after = s.app.selected_project().map(|p| p.path.clone());
    if *before == after {
        return;
    }
    s.detail = None;
    s.app.scroll = crate::PanelScroll::default();
    s.arm_select_pulse(); // acknowledge the cursor move
                          // A different project's sections must never linger: reset to placeholders
                          // (and cancel any in-flight tween) until its own feeds arrive.
    s.set_col3(crate::col3::Col3::default());
    if !matches!(*s.activity, crate::col3::ActivityView::NoProjection) {
        s.activity = Arc::new(crate::col3::ActivityView::default());
        s.dirty = true;
    }
    eff.data_requests
        .push(DataRequest::SetActive(after.clone()));
    if s.app.surface == Surface::RecallViewer {
        eff.data_requests
            .extend(super::viewer::select_viewer_project(s).data_requests);
    } else if s.app.mode == Mode::ProjectSettings {
        if let Flow::Continue(settings) = open_project_settings(s) {
            eff.data_requests.extend(settings.data_requests);
        }
    }
    if let Some(path) = after {
        eff.data_requests.push(DataRequest::Refresh(path.clone()));
        eff.load_layout = Some(path);
    }
}

/// The `SetLayout` persist op for the selected project's current arrangement
/// (scoped to the active profile) — `None` layout when
/// it equals the default (a reset). `None` when no project is selected.
pub(super) fn layout_persist_op(app: &App) -> Option<PersistOp> {
    let path = app.selected_project().map(|p| p.path.clone())?;
    let layout = if app.layout == crate::layout_model::LayoutModel::default() {
        None
    } else {
        Some(app.layout.encode())
    };
    Some(PersistOp::SetLayout {
        profile: app.active_profile.clone(),
        path,
        layout,
    })
}

/// The modal button a keyboard activation (Enter/Space) would flash,
/// mirroring `on_key_overlay`'s activation rule — Space never fires the mutating
/// Confirm, and a focused Body/none flashes nothing.
pub(super) fn modal_activation_flash(app: &App, key: CanopyKey) -> Option<Target> {
    let is_enter = matches!(key, CanopyKey::Enter);
    let is_space = matches!(key, CanopyKey::Char(' '));
    if !(is_enter || is_space) {
        return None;
    }
    match app.focused_overlay_control()? {
        OverlayControl::Confirm if is_enter => Some(Target::OverlayConfirm),
        OverlayControl::Cancel => Some(Target::OverlayCancel),
        OverlayControl::Close => Some(Target::OverlayClose),
        OverlayControl::Primary => Some(Target::WizardAdd),
        _ => None,
    }
}

/// A refresh of the selected project, or `None` with nothing selected.
fn selected_refresh(app: &App) -> Option<DataRequest> {
    app.selected_project()
        .map(|p| DataRequest::Refresh(p.path.clone()))
}

/// The add flow's deferred decision: the first read of a project just
/// registered sets it up — `rift init` with the plan the setup page would
/// open with, shown step by step in the progress dialog — when there is a
/// setup to perform: never initialized here, configured by the repository
/// but not set up on this machine, or set up only half way (a projection
/// whose hook install is not on record, or a setup plan left pending). It
/// stays quiet for a project that is already set up or is not a repository
/// at all. Adding a project IS initializing it; Project Settings stays the
/// surface for changing the choices afterwards, not a confirm step in the
/// way of the first one. A read for another project leaves the decision
/// pending.
fn pending_setup_prompt(s: &mut AppState, pd: &ProjectData) -> bool {
    if s.pending_setup_prompt.as_deref() != Some(pd.project.as_str()) {
        return false;
    }
    s.pending_setup_prompt = None;
    let unfinished =
        pd.status.repository_card_only() || pd.status == ProjectStatus::SetupIncomplete;
    let offered =
        unfinished && pd.status != ProjectStatus::NotRepository && s.app.mode == Mode::Normal;
    if !offered {
        return false;
    }
    // A notice still up from the add (`registered …`) yields to the setup's
    // own frame, so the dialog is not drawn over it.
    if matches!(s.app.overlay, Some(Overlay::Notice(_))) {
        s.app.overlay = None;
        s.app.overlay_focus = 0;
        s.notice_settle = 0.0;
    }
    s.app.select_path(&pd.project);
    s.dirty = true;
    true
}

/// Whether the viewer, not the dashboard, answers a key: on its surface with
/// no overlay up, in normal or command mode or while its filter is edited.
fn viewer_owns_the_key(app: &App) -> bool {
    app.surface == Surface::RecallViewer
        && app.overlay.is_none()
        && (app.mode == Mode::Normal || app.mode == Mode::Command || app.viewer.filter_editing)
}

/// Clamp the open surface's scroll offsets to its content at the current size.
fn clamp_surface_scrolls(s: &mut AppState) {
    match s.app.surface {
        Surface::RecallViewer => clamp_viewer_scrolls(&mut s.app, s.size),
        Surface::Dashboard => s.app.clamp_scrolls(s.size),
    }
}

/// Fold the action a key produced into its flow. Remove/reorder target the
/// *selected* project — resolved here, so the key handler stays free of
/// paths; no selection ⇒ inert. Every opener is the SAME function the
/// pointer's twin calls, so the two doors cannot drift apart, and a period
/// button's key arms the same press flash a click does, so the two paths are
/// indistinguishable on screen as well as in state.
fn key_action_flow(s: &mut AppState, action: Action) -> Flow {
    let selected = s.app.selected_project().map(|p| p.path.clone());
    match action {
        Action::RemoveProject => persist_flow(selected.map(|path| remove_project_op(&s.app, path))),
        Action::ReorderProject(up) => {
            persist_flow(selected.map(|path| reorder_project_op(&s.app, path, up)))
        }
        Action::OpenProjectSettings => open_project_settings(s),
        Action::OpenRecallViewer => Flow::Continue(s.open_recall_viewer(None)),
        Action::OpenHelp => open_header_help(s),
        Action::OpenWorktreeAxis => open_worktree_axis(s),
        Action::OpenSavingsBreakdown => open_savings_breakdown(s),
        Action::SetRecallWindow(window) => {
            s.arm_press_pulse(Target::RecallWindow(window));
            select_recall_window(s, window)
        }
        Action::AdoptWorktree => open_adopt_confirm(s),
        Action::ApplyProjectSettings(action) => Flow::Continue(Effects {
            project_settings_actions: vec![action],
            ..Effects::default()
        }),
        Action::RunLine(buf) => run_line(s, &buf),
        Action::ConfirmDispatch => confirm_dispatch(s),
        other => profile_or_fold_action(s, other),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{DispatchOutcome, DispatchRequest, Overlay};
    use crate::input::{CanopyKey, MouseKind};
    use crate::render::frame::Size;
    use crate::state::anim::{REFRESH_EVERY_TICKS_POLL_ONLY, REFRESH_EVERY_TICKS_WATCHED};
    use crate::state::fixtures::*;
    use crate::state::{update, Event, Flow, HealthEvent, ProjectData, WatchMode};
    use crate::{Mode, ProjectStatus};

    /// The add flow's deferred wizard: a project just registered opens
    /// Project Settings on its first status read when there is a setup to
    /// offer — never initialized, configured by the repository but not set
    /// up on this machine, or set up only half way — and stays quiet when it
    /// is already set up here or is not a repository at all. One read
    /// decides, whatever it says.
    #[test]
    fn a_newly_added_project_opens_setup_only_when_there_is_setup_to_offer() {
        for (status, opens) in [
            (ProjectStatus::NotProvisioned, true),
            (ProjectStatus::NotInitialized, true),
            // A family carrying a projection whose hooks were never
            // installed: there is setup left to do, so the add does it.
            (ProjectStatus::SetupIncomplete, true),
            (ProjectStatus::Idle, false),
            (ProjectStatus::NotRepository, false),
        ] {
            let mut s = ready(vec![project("a"), project("b")]);
            s.pending_setup_prompt = Some("/r/b".into());
            let flow = update(
                &mut s,
                Event::ProjectData(ProjectData {
                    project: "/r/b".into(),
                    status,
                    detail: None,
                }),
            );
            assert!(
                s.pending_setup_prompt.is_none(),
                "{status:?}: the first read decides"
            );
            let effects = effects_of(flow);
            assert!(
                effects.project_settings_actions.is_empty(),
                "{status:?}: setup needs explicit review before any writes"
            );
            assert_eq!(
                s.app.mode,
                if opens {
                    Mode::ProjectSettings
                } else {
                    Mode::Normal
                },
                "{status:?}: open staged setup only when it is needed"
            );
            if opens {
                assert_eq!(
                    s.app.selected_project().map(|p| p.path.as_str()),
                    Some("/r/b"),
                    "the setup runs on the row that was added"
                );
            }
        }

        // A notice still up from the add (`registered …`) is cleared before
        // the setup's own frame opens, so the dialog is not drawn over it.
        let mut s = ready(vec![project("a"), project("b")]);
        s.pending_setup_prompt = Some("/r/b".into());
        s.open_notice(crate::dispatch::Message::success(
            "registry",
            vec!["registered /r/b".into()],
        ));
        update(
            &mut s,
            Event::ProjectData(ProjectData {
                project: "/r/b".into(),
                status: ProjectStatus::NotProvisioned,
                detail: None,
            }),
        );
        assert!(s.app.overlay.is_none(), "the toast yields to the setup");

        // A read for another project leaves the decision pending.
        let mut s = ready(vec![project("a"), project("b")]);
        s.pending_setup_prompt = Some("/r/b".into());
        update(
            &mut s,
            Event::ProjectData(ProjectData {
                project: "/r/a".into(),
                status: ProjectStatus::NotProvisioned,
                detail: None,
            }),
        );
        assert_eq!(s.pending_setup_prompt.as_deref(), Some("/r/b"));
        assert_ne!(s.app.mode, Mode::ProjectSettings);
    }

    #[test]
    fn project_settings_polls_only_while_open_and_wheel_scrolls_independently() {
        let mut s = project_settings_state(crate::ProjectInitialization::Initialized);
        update(&mut s, Event::Resize(Size::new(80, 24)));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        for _ in 0..9 {
            assert!(effects_of(update(&mut s, Event::Tick))
                .data_requests
                .is_empty());
        }
        assert_eq!(
            effects_of(update(&mut s, Event::Tick)).data_requests,
            vec![DataRequest::Refresh("/r/a".into())]
        );

        let modal = crate::dashboard::geometry(s.size).0;
        let body = crate::view::project_settings_body_rect(modal);
        for _ in 0..4 {
            update(&mut s, mouse(MouseKind::ScrollDown, body.x + 1, body.y + 1));
        }
        assert!(s.app.project_settings.scroll > 0);
        assert!(!s.app.project_settings.follow_cursor);
        update(&mut s, Event::Key(CanopyKey::Down));
        assert!(s.app.project_settings.follow_cursor);

        update(&mut s, Event::Key(CanopyKey::Esc));
        let before = s.tick;
        let effects = effects_of(update(&mut s, Event::Tick));
        assert_eq!(s.tick, before.wrapping_add(1));
        assert!(effects.data_requests.is_empty());
    }

    #[test]
    fn resize_sets_size_and_dirty() {
        let mut s = ready(vec![]);
        let flow = update(&mut s, Event::Resize(Size::new(80, 24)));
        assert!(matches!(flow, Flow::Continue(_)));
        assert_eq!(s.size, Size::new(80, 24));
        assert!(s.dirty);
    }

    proptest::proptest! {
        /// After a resize, scroll offsets are content-clamped in range:
        /// never past the end (a huge pre-value collapses to the overflow), and the
        /// clamp is idempotent — re-clamping the same size never moves them.
        #[test]
        fn scroll_offset_always_in_range_after_resize(
            pre in 0usize..10_000, w in 1u16..400, h in 1u16..400,
        ) {
            let mut s = ready(vec![project("a")]);
            s.app.scroll.detail = pre;
            s.app.scroll.graphs = pre;
            update(&mut s, Event::Resize(Size::new(w, h)));
            // Bounded by the fully-expanded content (never the raw pre-value).
            proptest::prop_assert!(
                s.app.scroll.graphs <= usize::from(crate::col3::content_rows(&crate::col3::SectionId::ALL))
            );
            // Idempotent: already in range, so a second clamp is a no-op.
            let (g, d) = (s.app.scroll.graphs, s.app.scroll.detail);
            s.app.clamp_scrolls(Size::new(w, h));
            proptest::prop_assert_eq!(s.app.scroll.graphs, g);
            proptest::prop_assert_eq!(s.app.scroll.detail, d);
        }
    }

    #[test]
    fn tick_does_not_dirty_when_idle() {
        let mut s = ready(vec![]);
        let flow = update(&mut s, Event::Tick);
        assert!(matches!(flow, Flow::Continue(e) if e.is_empty()));
        assert!(!s.dirty, "idle tick must not force a redraw");
    }

    #[test]
    fn periodic_tick_restats_selected_without_dirtying() {
        let mut s = ready(vec![project("a")]);
        // The first REFRESH_EVERY_TICKS_WATCHED-1 ticks are silent…
        for _ in 0..(REFRESH_EVERY_TICKS_WATCHED - 1) {
            match update(&mut s, Event::Tick) {
                Flow::Continue(e) => assert!(e.data_requests.is_empty()),
                Flow::Quit => panic!("expected Continue"),
            }
        }
        // …then the cadence tick re-requests the selected project.
        match update(&mut s, Event::Tick) {
            Flow::Continue(e) => {
                assert_eq!(e.data_requests, vec![DataRequest::Refresh("/r/a".into())]);
            }
            Flow::Quit => panic!("expected Continue"),
        }
        assert!(!s.dirty, "a background re-stat never forces a redraw");
    }

    /// **A poll-only bus re-stats every fifteen ticks**: once the
    /// bus reports `PollOnly`, the heartbeat is the only wake, so the idle
    /// cadence tightens from 600 to 15 — still never dirtying.
    #[test]
    fn a_poll_only_bus_restats_every_fifteen_ticks() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::WatchMode(WatchMode::PollOnly));
        s.dirty = false;
        for _ in 0..(REFRESH_EVERY_TICKS_POLL_ONLY - 1) {
            match update(&mut s, Event::Tick) {
                Flow::Continue(e) => assert!(e.data_requests.is_empty()),
                Flow::Quit => panic!("expected Continue"),
            }
        }
        match update(&mut s, Event::Tick) {
            Flow::Continue(e) => {
                assert_eq!(e.data_requests, vec![DataRequest::Refresh("/r/a".into())]);
            }
            Flow::Quit => panic!("expected Continue"),
        }
        assert!(!s.dirty, "a background re-stat never forces a redraw");
        assert_eq!(REFRESH_EVERY_TICKS_POLL_ONLY, 15);
        assert_eq!(REFRESH_EVERY_TICKS_WATCHED, 600);
    }

    /// **A watch-mode change dirties exactly once**: the first report of a
    /// new mode redraws (the System panel row), a repeat never does.
    #[test]
    fn a_watch_mode_change_dirties_exactly_once() {
        let mut s = ready(vec![]);
        s.dirty = false;
        update(&mut s, Event::WatchMode(WatchMode::Native));
        assert!(!s.dirty, "the default mode re-reported is not a change");
        update(&mut s, Event::WatchMode(WatchMode::PollOnly));
        assert!(s.dirty);
        s.dirty = false;
        update(&mut s, Event::WatchMode(WatchMode::PollOnly));
        assert!(!s.dirty, "a repeated report never forces a redraw");
        update(&mut s, Event::WatchMode(WatchMode::Native));
        assert!(s.dirty);
    }

    #[test]
    fn periodic_tick_with_no_selection_is_inert() {
        let mut s = ready(vec![]);
        for _ in 0..REFRESH_EVERY_TICKS_WATCHED {
            match update(&mut s, Event::Tick) {
                Flow::Continue(e) => assert!(e.is_empty()),
                Flow::Quit => panic!("expected Continue"),
            }
        }
        assert!(!s.dirty);
    }

    #[test]
    fn project_data_updates_status_by_path() {
        let mut s = ready(vec![project("a"), project("b")]);
        assert_eq!(s.app.projects[0].status, ProjectStatus::Idle);
        update(
            &mut s,
            Event::ProjectData(ProjectData {
                project: "/r/a".into(),
                status: ProjectStatus::Running {
                    since_ms: 5,
                    active_sessions: 1,
                },
                detail: None,
            }),
        );
        assert!(matches!(
            s.app.projects[0].status,
            ProjectStatus::Running { .. }
        ));
        assert_eq!(
            s.app.projects[1].status,
            ProjectStatus::Idle,
            "only /r/a changed"
        );
        assert!(s.dirty);
    }

    #[test]
    fn project_data_for_unchanged_status_does_not_dirty() {
        let mut s = ready(vec![project("a")]);
        update(
            &mut s,
            Event::ProjectData(ProjectData {
                project: "/r/a".into(),
                status: ProjectStatus::Idle, // same as current
                detail: None,
            }),
        );
        assert!(!s.dirty, "a no-op re-stat must not force a redraw");
    }

    #[test]
    fn selection_change_requests_new_project_data() {
        let mut s = ready(vec![project("a"), project("b")]);
        open_drawer(&mut s);
        // Move selection down → a data request for the newly selected project.
        let flow = update(&mut s, Event::Key(CanopyKey::Down));
        match flow {
            Flow::Continue(e) => {
                assert!(
                    e.data_requests
                        .contains(&DataRequest::SetActive(Some("/r/b".into()))),
                    "expected SetActive for the new selection, got {:?}",
                    e.data_requests
                );
                assert!(e
                    .data_requests
                    .contains(&DataRequest::Refresh("/r/b".into())));
            }
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn non_selection_key_makes_no_data_request() {
        let mut s = ready(vec![project("a")]);
        // Pressing a key that does not move the selection emits no data request.
        let flow = update(&mut s, Event::Key(CanopyKey::Char('x')));
        match flow {
            Flow::Continue(e) => assert!(e.data_requests.is_empty()),
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn quit_key_yields_flow_quit() {
        let mut s = ready(vec![]);
        assert!(matches!(
            update(&mut s, Event::Key(CanopyKey::Quit)),
            Flow::Quit
        ));
    }

    #[test]
    fn activity_feed_fills_the_selected_project_and_snaps() {
        use crate::col3::{ActivityData, ActivityView, DayBar};
        let mut s = ready(vec![project("a")]);
        let days = ActivityData {
            days: vec![DayBar {
                weekday: "Mon",
                events: 5,
                sessions: 1,
                checkpoints: 2,
            }],
            ..ActivityData::default()
        };
        update(
            &mut s,
            Event::Feed(FeedUpdate {
                project: "/r/a".into(),
                payload: FeedPayload::Activity(ActivityView::Days(days.clone())),
            }),
        );
        assert_eq!(*s.activity, ActivityView::Days(days));
        assert!(s.dirty);
        assert_eq!(s.snapshot().activity.as_ref(), &*s.activity);
    }

    #[test]
    fn feed_for_unselected_project_is_dropped() {
        use crate::col3::ActivityView;
        let mut s = ready(vec![project("a"), project("b")]); // "a" selected
        update(
            &mut s,
            Event::Feed(FeedUpdate {
                project: "/r/b".into(),
                payload: FeedPayload::Activity(ActivityView::Days(
                    crate::col3::ActivityData::default(),
                )),
            }),
        );
        assert_eq!(
            *s.activity,
            ActivityView::NoProjection,
            "a non-selected project's activity is stale routing, dropped"
        );
    }

    #[test]
    fn selection_change_resets_activity_to_placeholder() {
        use crate::col3::{ActivityData, ActivityView};
        let mut s = ready(vec![project("a"), project("b")]);
        open_drawer(&mut s);
        s.activity = Arc::new(ActivityView::Days(ActivityData::default()));
        update(&mut s, Event::Key(CanopyKey::Down)); // select b
        assert_eq!(
            *s.activity,
            ActivityView::NoProjection,
            "b's card must not show a's activity until b's feed arrives"
        );
    }

    #[test]
    fn health_events_fold_and_recover_on_healthy_poll() {
        use crate::theme::Health;
        let mut s = ready(vec![]);
        assert_eq!(s.health, Health::Ok, "optimistic at launch");

        // A worker error downgrades to amber…
        update(&mut s, Event::Health(HealthEvent::WorkerError));
        assert_eq!(s.health, Health::Warn);
        assert!(s.dirty);

        // …a store failure to red…
        s.dirty = false;
        update(&mut s, Event::Health(HealthEvent::StoreUnreachable));
        assert_eq!(s.health, Health::Down);
        assert!(s.dirty);

        // …and only a healthy poll restores green.
        s.dirty = false;
        update(&mut s, Event::Health(HealthEvent::Healthy));
        assert_eq!(s.health, Health::Ok);
        assert!(s.dirty);
    }

    #[test]
    fn unchanged_health_never_dirties() {
        let mut s = ready(vec![]);
        update(&mut s, Event::Health(HealthEvent::Healthy));
        assert!(
            !s.dirty,
            "Ok → Ok is a no-op (a steady stream never redraws)"
        );
        update(&mut s, Event::Health(HealthEvent::WorkerError));
        s.dirty = false;
        update(&mut s, Event::Health(HealthEvent::WorkerError));
        assert!(!s.dirty, "Warn → Warn is a no-op");
    }

    #[test]
    fn keyboard_activation_of_a_modal_button_flashes_it() {
        // Enter on the default-focused Confirm flashes it, exactly like a click.
        let mut s = ready(vec![project("a")]);
        s.app.open_overlay(Overlay::Confirm {
            request: DispatchRequest::Rebuild,
            repo: "/r".into(),
        });
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        assert_eq!(s.pressed, Some(Target::OverlayConfirm));
        // Tab to the close X on a results modal, then Enter flashes OverlayClose.
        let mut s = ready(vec![project("a")]);
        s.app.open_overlay(Overlay::Results {
            title: "t".into(),
            lines: vec!["x".into()],
            scroll: 0,
            footer: None,
            truncated: false,
        });
        update(&mut s, Event::Key(CanopyKey::Tab)); // Body → Close
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        assert_eq!(s.pressed, Some(Target::OverlayClose));
    }

    #[test]
    fn frecency_bumps_only_on_successful_dispatch() {
        let mut s = ready(vec![project("a")]);
        // A failed delegated dispatch never bumps.
        let _ = update(
            &mut s,
            Event::DispatchDone {
                request: DispatchRequest::Verify,
                outcome: DispatchOutcome {
                    code: 65,
                    human: "broken".into(),
                    json: None,
                },
            },
        );
        assert_eq!(s.app.frecency.count("verify"), 0);
        // A successful one bumps its verb word.
        let _ = update(
            &mut s,
            Event::DispatchDone {
                request: DispatchRequest::Verify,
                outcome: DispatchOutcome {
                    code: 0,
                    human: "ok".into(),
                    json: None,
                },
            },
        );
        assert_eq!(s.app.frecency.count("verify"), 1);
    }

    #[test]
    fn dispatch_done_renders_the_results_overlay() {
        let mut s = ready(vec![project("a")]);
        let outcome = DispatchOutcome {
            code: 65,
            human: "chain: BROKEN".into(),
            json: None,
        };
        let _ = update(
            &mut s,
            Event::DispatchDone {
                request: DispatchRequest::Verify,
                outcome,
            },
        );
        match &s.app.overlay {
            Some(Overlay::Results { title, footer, .. }) => {
                assert_eq!(title, "rift verify");
                assert_eq!(footer.as_deref(), Some("verify: FAILED — integrity (65)"));
            }
            other => panic!("expected Results, got {other:?}"),
        }
    }

    #[test]
    fn feed_updates_are_full_state_so_delivery_order_converges() {
        // Delivery robustness: every feed update carries FULL
        // section state, so the display equals whichever update applied last —
        // drops and duplicates cannot skew it.
        use crate::col3::{TokensData, TokensView};
        let feed = |total: f64| {
            Event::Feed(FeedUpdate {
                project: "/r/a".into(),
                payload: FeedPayload::Tokens {
                    tokens: TokensView::Data(TokensData {
                        rate: vec![total],
                        total,
                        by_worktree: crate::col3::WorktreeAxis::default(),
                    }),
                    breakdown: crate::col3::BreakdownView::NoActivity,
                },
            })
        };
        let total_of = |s: &mut AppState| match &s.snapshot().col3.tokens {
            TokensView::Data(d) => d.total,
            TokensView::NoActivity => -1.0,
        };

        let mut s = ready(vec![project("a")]);
        update(&mut s, feed(100.0));
        update(&mut s, feed(700.0));
        assert!((total_of(&mut s) - 700.0).abs() < f64::EPSILON);
        // A duplicated (replayed) update changes nothing.
        s.dirty = false;
        update(&mut s, feed(700.0));
        assert!(!s.dirty, "an identical feed replay must not redraw");
        // A dropped intermediate is invisible: last-applied wins regardless.
        let mut s2 = ready(vec![project("a")]);
        update(&mut s2, feed(700.0));
        assert!((total_of(&mut s2) - 700.0).abs() < f64::EPSILON);
    }

    #[test]
    fn feed_for_a_different_project_is_dropped_and_selection_change_resets() {
        use crate::col3::{TokensData, TokensView};
        let feed = |proj: &str| {
            Event::Feed(FeedUpdate {
                project: proj.into(),
                payload: FeedPayload::Tokens {
                    tokens: TokensView::Data(TokensData {
                        rate: vec![1.0],
                        total: 1.0,
                        by_worktree: crate::col3::WorktreeAxis::default(),
                    }),
                    breakdown: crate::col3::BreakdownView::NoActivity,
                },
            })
        };
        let mut s = ready(vec![project("a"), project("b")]);
        open_drawer(&mut s);
        // A feed for the unselected project must not paint a's column 3.
        update(&mut s, feed("/r/b"));
        assert_eq!(s.snapshot().col3.tokens, TokensView::NoActivity);
        // Apply a's feed, then move the selection: sections reset to
        // placeholders (no stale cross-project data).
        update(&mut s, feed("/r/a"));
        assert!(matches!(s.snapshot().col3.tokens, TokensView::Data(_)));
        update(&mut s, Event::Key(CanopyKey::Down));
        assert_eq!(
            s.snapshot().col3.tokens,
            TokensView::NoActivity,
            "the previous project's sections must not linger"
        );
    }

    #[test]
    fn reducer_is_pure_repeatable() {
        // Same (state, event) ⇒ same next state + same effects, run twice.
        let base = ready(vec![project("a"), project("b")]);
        let mut a = base.clone();
        let mut b = base.clone();
        let fa = update(&mut a, Event::Key(CanopyKey::Down));
        let fb = update(&mut b, Event::Key(CanopyKey::Down));
        assert_eq!(a.app.selected, b.app.selected);
        assert!(matches!((fa, fb), (Flow::Continue(x), Flow::Continue(y)) if x == y));
    }
}
