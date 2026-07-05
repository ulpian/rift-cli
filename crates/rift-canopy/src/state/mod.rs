//! Unidirectional state: a pure reducer + an immutable render snapshot.
//!
//! The [event loop](crate::app) feeds [`Event`]s into the **pure** [`update`]
//! reducer, which mutates [`AppState`] and returns the side effects to perform
//! ([`Effects`]) — never doing I/O itself. The renderer never reads `AppState`;
//! the loop derives an immutable [`ViewModel`] snapshot per dirty frame, so
//! [`crate::view::draw`] is a pure function of state (the foundation every
//! headless/snapshot test relies on).
//!
//! The dashboard model and the key handling are reused wholesale: [`AppState`]
//! wraps the existing [`App`] and [`update`] folds the existing pure
//! [`crate::input::on_key`] reducer through [`fold_action`].

use std::sync::Arc;

use crate::col2::{plan_flip, DetailView, Feature, FeatureAction, FeatureActionKind};
use crate::input::{on_key, Action, CanopyKey};
use crate::render::frame::Size;
use crate::{App, Command, Mode, Panel, ProjectStatus};

/// Which panel currently holds keyboard focus (the spec's `Focus`; identical to
/// the existing [`Panel`]).
pub type Focus = Panel;

/// An event entering the reducer. Canopy-native (the loop translates terminal
/// events into these), so the state layer never names an engine type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A normalized key press.
    Key(CanopyKey),
    /// The terminal was resized.
    Resize(Size),
    /// A fixed-cadence animation/sampling tick.
    Tick,
    /// A fresh projection snapshot from the data bus (P8): graph-ish series values.
    Data(DataUpdate),
    /// A per-project update from the data bus (P8): live status for one project.
    ProjectData(ProjectData),
}

/// A per-project update from the live data bus (P8). Carries already-scoped,
/// read-only values for one project, keyed by its canonical path. The bus
/// computes the status off-thread from the projection; the reducer just stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectData {
    /// The project key (its canonical path) this update is for.
    pub project: String,
    /// The freshly-derived live status.
    pub status: ProjectStatus,
    /// The rich column-2 detail — `Some` only for the **active** project (the bus
    /// computes it just for the foreground selection); `None` for status-only
    /// updates of the other projects.
    pub detail: Option<DetailView>,
}

/// A validated projection update from the live bus. Carries only already-redacted,
/// scope-checked values; the reducer re-clamps defensively. (P8 fills this in;
/// today it is fed by tests and the bin's static demo seed.)
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataUpdate {
    /// Context-window fullness `0..=100` (drives the nic-gauge).
    pub context_pct: Option<u8>,
    /// Recall savings `0..=100`.
    pub savings_pct: Option<u8>,
    /// A token-usage sample to append to the column-3 series.
    pub token_sample: Option<u64>,
}

/// The outcome of one reduction.
#[derive(Debug)]
pub enum Flow {
    /// Stay in the loop; perform these side effects.
    Continue(Effects),
    /// Exit the dashboard.
    Quit,
}

/// Side effects the loop performs *after* the (pure) reducer returns. The reducer
/// never does I/O; all of it is expressed here.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// Fresh-row requests for the data bus (P8).
    pub data_requests: Vec<DataRequest>,
    /// Append-only persistence (projects/toggles).
    pub persist: Vec<PersistOp>,
    /// Parsed slash commands to dispatch.
    pub commands: Vec<CommandAction>,
    /// Feature install/remove actions for the (injected) feature manager (P5 S2).
    /// Canopy plans these but never performs an install itself.
    pub feature_actions: Vec<FeatureAction>,
}

impl Effects {
    /// `true` if there is nothing to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data_requests.is_empty()
            && self.persist.is_empty()
            && self.commands.is_empty()
            && self.feature_actions.is_empty()
    }
}

/// A scoped read-request the reducer emits for the live data bus (P8). The loop
/// forwards these to the bus, which reads the projection off-thread and feeds the
/// result back as an [`Event::ProjectData`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataRequest {
    /// Make this project the active (foreground) one; the bus prioritizes it and
    /// observes it for changes. `None` clears the active project.
    SetActive(Option<String>),
    /// Re-read this project's data now (by project key — the canonical path).
    Refresh(String),
}

/// An **insert-only** persistence operation, keyed by the project's canonical
/// `path` (Canopy's stable natural key — the same key status matching uses). There
/// is deliberately no `Update`/`Delete` variant: a removal, relabel, reorder, or
/// toggle-off is a new, superseding insert into the append-only log, which upholds
/// the immutable model (04 §4.3). The bin resolves `path` → the stored
/// `project_id` when it writes the event, so ids never leak into Canopy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistOp {
    /// Register a project at the given path.
    AddProject {
        /// The (user-entered) project path to register.
        path: String,
    },
    /// Hide a project (a superseding `removed` event; the log/history is kept).
    RemoveProject {
        /// The path of the project to hide.
        path: String,
    },
    /// Change a project's display label (a superseding `relabeled` event).
    RelabelProject {
        /// The path of the project to relabel.
        path: String,
        /// The new label.
        label: String,
    },
    /// Move a project up (`true`) or down (`false`) in the list (a superseding
    /// `reordered` event; the bin computes the new order keys).
    ReorderProject {
        /// The path of the project to move.
        path: String,
        /// Move up when `true`, down when `false`.
        up: bool,
    },
    /// Record a feature toggle's desired state (a superseding `toggle` event).
    SetToggle {
        /// The path of the project the toggle belongs to.
        path: String,
        /// The feature key (Canopy's column-2 toggle spelling).
        feature: String,
        /// The desired on/off intent.
        desired: bool,
    },
}

/// A parsed slash command to dispatch (wraps the existing [`Command`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandAction {
    /// The parsed command.
    pub command: Command,
}

/// How many idle ticks between background re-stats of the selected project. At the
/// 100ms poll cadence this re-reads the foreground project roughly every 1.5s, so a
/// `Running` → `Idle` transition (activity aging past the window) surfaces even
/// though no cursor change fires a bus poll. Time-based only; never dirties.
const REFRESH_EVERY_TICKS: u32 = 15;

/// The whole dashboard state: the existing [`App`] model plus loop metadata.
#[derive(Clone, Debug)]
pub struct AppState {
    /// The dashboard model (projects, selection, mode, command buffer, …).
    pub app: App,
    /// The current terminal size.
    pub size: Size,
    /// Set when something observable changed; gates redraws.
    pub dirty: bool,
    /// Idle-tick counter driving the background re-stat cadence (never dirties).
    tick: u32,
    /// The bus-computed rich detail for the active project (keyed by its path);
    /// the snapshot shares it when it matches the selection, else a thin fallback.
    detail: Option<Arc<DetailView>>,
    /// The last snapshot's project rows, shared with every [`ViewModel`] until the
    /// underlying projects change (checked by cheap comparison in [`Self::snapshot`],
    /// so a stale row can never be served).
    rows_cache: Arc<Vec<ProjectRow>>,
    /// The last snapshot's token series, shared likewise.
    series_cache: Arc<Vec<u64>>,
}

impl AppState {
    /// A fresh state for the given projects and size.
    #[must_use]
    pub fn new(projects: Vec<crate::Project>, size: Size) -> Self {
        Self {
            app: App::new(projects),
            size,
            tick: 0,
            dirty: true,
            detail: None,
            rows_cache: Arc::new(Vec::new()),
            series_cache: Arc::new(Vec::new()),
        }
    }

    /// The focused panel.
    #[must_use]
    pub fn focus(&self) -> Focus {
        self.app.focus
    }

    /// Apply a per-project update from the bus: set the matching project's live
    /// status (matched by canonical path) and store the rich detail when the bus
    /// computed it (only for the active project). Returns `true` if anything
    /// changed, so the loop can gate the redraw (a no-op update must not redraw).
    fn apply_project_data(&mut self, pd: &ProjectData) -> bool {
        let mut changed = false;
        for p in &mut self.app.projects {
            if p.path == pd.project && p.status != pd.status {
                p.status = pd.status;
                changed = true;
            }
        }
        if let Some(detail) = &pd.detail {
            if self.detail.as_deref() != Some(detail) {
                self.detail = Some(Arc::new(detail.clone()));
                changed = true;
            }
        }
        changed
    }

    /// Apply a validated projection update, clamping out-of-range values.
    fn apply_data(&mut self, u: &DataUpdate) {
        if let Some(c) = u.context_pct {
            self.app.context_pct = c.min(100);
        }
        if let Some(s) = u.savings_pct {
            self.app.savings_pct = s.min(100);
        }
        if let Some(t) = u.token_sample {
            self.app.token_series.push(t);
            // Cap the ring so a flood can't grow memory without bound.
            const CAP: usize = 256;
            if self.app.token_series.len() > CAP {
                let drop = self.app.token_series.len() - CAP;
                self.app.token_series.drain(0..drop);
            }
        }
    }

    /// Derive the immutable render snapshot the renderer consumes.
    ///
    /// The project rows, token series, and rich detail are **shared** (`Arc`)
    /// rather than deep-cloned per frame: each cache is compared against the
    /// live state and rebuilt only when it actually differs — so a snapshot is
    /// O(compare) on quiet data, and a mutation is always visible in the next
    /// snapshot (the comparison, not an invalidation hook, is the truth).
    #[must_use]
    pub fn snapshot(&mut self) -> ViewModel {
        let rows_match = self.rows_cache.len() == self.app.projects.len()
            && self
                .rows_cache
                .iter()
                .zip(&self.app.projects)
                .all(|(r, p)| r.status == p.status && r.kind == p.kind && r.name == p.name);
        if !rows_match {
            self.rows_cache = Arc::new(
                self.app
                    .projects
                    .iter()
                    .map(|p| ProjectRow {
                        name: p.name.clone(),
                        status: p.status,
                        kind: p.kind,
                    })
                    .collect(),
            );
        }
        if *self.series_cache != self.app.token_series {
            self.series_cache = Arc::new(self.app.token_series.clone());
        }
        // Prefer the bus-computed rich detail when it is for the selected project;
        // otherwise a thin fallback keeps the panel populated until it arrives.
        let detail = self.app.selected_project().map(|p| match &self.detail {
            Some(d) if d.path == p.path => Arc::clone(d),
            _ => Arc::new(DetailView::thin(p.path.clone(), p.name.clone(), p.status)),
        });
        ViewModel {
            size: self.size,
            focus: self.app.focus,
            mode: self.app.mode,
            show_wizard: self.app.show_wizard,
            projects: Arc::clone(&self.rows_cache),
            selected: self.app.selected,
            detail,
            context_pct: self.app.context_pct,
            savings_pct: self.app.savings_pct,
            token_series: Arc::clone(&self.series_cache),
            detail_scroll: self.app.scroll.detail,
            graphs_scroll: self.app.scroll.graphs,
            toggle_cursor: self.app.toggle_cursor,
            command_line: self.app.command_line.clone(),
            status: self.app.status.clone(),
        }
    }
}

/// The **pure** reducer: events in, next state + [`Effects`] out. No I/O — every
/// side effect is returned, never performed here (the loop performs them).
pub fn update(s: &mut AppState, ev: Event) -> Flow {
    match ev {
        Event::Resize(size) => {
            s.size = size;
            s.app.clamp_scrolls(size);
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Event::Tick => {
            // Idle cadence: never dirties (the ~0% CPU invariant). Every
            // REFRESH_EVERY_TICKS, re-request the *selected* project so a
            // time-based status change (Running → Idle) surfaces without a cursor
            // change; the bus re-derives and the ProjectData arm dirties only if
            // the status actually moved.
            s.tick = s.tick.wrapping_add(1);
            if s.tick.is_multiple_of(REFRESH_EVERY_TICKS) {
                if let Some(path) = s.app.selected_project().map(|p| p.path.clone()) {
                    return Flow::Continue(Effects {
                        data_requests: vec![DataRequest::Refresh(path)],
                        ..Effects::default()
                    });
                }
            }
            Flow::Continue(Effects::default())
        }
        Event::Data(u) => {
            s.apply_data(&u);
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Event::ProjectData(pd) => {
            // Only dirty when the status actually changed, so a re-stat that finds
            // nothing new never forces a redraw (the ~0% CPU invariant).
            if s.apply_project_data(&pd) {
                s.dirty = true;
            }
            Flow::Continue(Effects::default())
        }
        Event::Key(k) => {
            let before = s.app.selected_project().map(|p| p.path.clone());
            let action = on_key(&mut s.app, k);
            s.dirty = true; // any key may change selection/mode/buffer
                            // Remove/reorder target the *selected* project — resolve it here (the
                            // key handler stays free of paths). No selection ⇒ inert.
            let sel = s.app.selected_project().map(|p| p.path.clone());
            let mut flow = match action {
                Action::RemoveProject => {
                    persist_flow(sel.map(|path| PersistOp::RemoveProject { path }))
                }
                Action::ReorderProject(up) => {
                    persist_flow(sel.map(|path| PersistOp::ReorderProject { path, up }))
                }
                // Flipping/retrying a feature reads the selected project's current
                // toggle states (from the stored detail) to plan the change.
                Action::ToggleFeature => toggle_flow(&s.app, s.detail.as_deref()),
                Action::RetryFeature => retry_flow(&s.app),
                other => fold_action(other),
            };
            // When the selection moves, ask the bus for the new project's data and
            // reset the toggle cursor to the top of the new project's toggles.
            let after = s.app.selected_project().map(|p| p.path.clone());
            if before != after {
                s.app.toggle_cursor = 0;
                if let Flow::Continue(eff) = &mut flow {
                    eff.data_requests
                        .push(DataRequest::SetActive(after.clone()));
                    if let Some(path) = after {
                        eff.data_requests.push(DataRequest::Refresh(path));
                    }
                }
            }
            flow
        }
    }
}

/// Fold the existing [`Action`] (from [`on_key`]) into a [`Flow`]/[`Effects`].
/// `RemoveProject`/`ReorderProject`/`ToggleFeature`/`RetryFeature` are handled in
/// [`update`] (they need the selected project + toggle state), so they never reach
/// here.
fn fold_action(action: Action) -> Flow {
    match action {
        Action::Quit => Flow::Quit,
        Action::None
        | Action::RemoveProject
        | Action::ReorderProject(_)
        | Action::ToggleFeature
        | Action::RetryFeature => Flow::Continue(Effects::default()),
        Action::AddProject(path) => Flow::Continue(Effects {
            persist: vec![PersistOp::AddProject { path }],
            ..Effects::default()
        }),
        Action::RunCommand(command) => Flow::Continue(Effects {
            commands: vec![CommandAction { command }],
            ..Effects::default()
        }),
    }
}

/// Wrap an optional persist op into a `Flow::Continue` (no op ⇒ no effect).
fn persist_flow(op: Option<PersistOp>) -> Flow {
    Flow::Continue(Effects {
        persist: op.into_iter().collect(),
        ..Effects::default()
    })
}

/// The feature under the toggle cursor (clamped into range).
fn cursored_feature(app: &App) -> Feature {
    Feature::ALL
        .get(app.toggle_cursor.min(Feature::ALL.len() - 1))
        .copied()
        .unwrap_or(Feature::Fff)
}

/// Plan a flip of the cursored feature into append-only `SetToggle` writes plus the
/// matching install/remove [`FeatureAction`]s (base-first / cascade order). No
/// selection ⇒ inert.
fn toggle_flow(app: &App, detail: Option<&DetailView>) -> Flow {
    let Some(path) = app.selected_project().map(|p| p.path.clone()) else {
        return Flow::Continue(Effects::default());
    };
    let feature = cursored_feature(app);
    // The current on-states come from the selected project's detail (if present).
    let view = detail
        .filter(|d| d.path == path)
        .map(|d| d.toggles.clone())
        .unwrap_or_default();
    let is_on = |f: Feature| view.is_on(f);

    let mut eff = Effects::default();
    for change in plan_flip(&is_on, feature) {
        eff.persist.push(PersistOp::SetToggle {
            path: path.clone(),
            feature: change.feature.key().to_string(),
            desired: change.desired,
        });
        eff.feature_actions.push(FeatureAction::from_change(change));
    }
    Flow::Continue(eff)
}

/// Retry (reinstall) the cursored feature — no desired-state change, just a fresh
/// install action for the manager (the one-key retry after a `Failed`).
fn retry_flow(app: &App) -> Flow {
    Flow::Continue(Effects {
        feature_actions: vec![FeatureAction {
            feature: cursored_feature(app),
            kind: FeatureActionKind::Reinstall,
        }],
        ..Effects::default()
    })
}

/// One project row in the render snapshot. Selection is not a row property —
/// the renderer derives it from [`ViewModel::selected`] — so the shared row
/// list never rebuilds on a selection move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRow {
    /// Display name.
    pub name: String,
    /// Live status.
    pub status: ProjectStatus,
    /// Repository root or linked worktree (drives the child-row indent).
    pub kind: crate::EntryKind,
}

/// The immutable per-frame render input. Cheap to clone (Arc-shared vectors).
#[derive(Clone, Debug)]
pub struct ViewModel {
    /// Terminal size.
    pub size: Size,
    /// Focused panel.
    pub focus: Focus,
    /// Interaction mode.
    pub mode: Mode,
    /// Whether to show the first-run wizard.
    pub show_wizard: bool,
    /// Column-1 project rows (shared, rebuilt only when the rows change).
    pub projects: Arc<Vec<ProjectRow>>,
    /// The selected project's index (selection highlight + scroll-to-selection).
    pub selected: usize,
    /// Column-2 selected-project detail (shared with the state's cached copy).
    pub detail: Option<Arc<DetailView>>,
    /// Context-window % (nic-gauge).
    pub context_pct: u8,
    /// Recall savings %.
    pub savings_pct: u8,
    /// Column-3 token series.
    pub token_series: Arc<Vec<u64>>,
    /// Column-2 detail viewport top (scroll offset).
    pub detail_scroll: usize,
    /// Column-3 graphs viewport top (scroll offset).
    pub graphs_scroll: usize,
    /// The cursor position within the column-2 feature toggles (`0..6`).
    pub toggle_cursor: usize,
    /// The command/add-project edit buffer.
    pub command_line: String,
    /// A transient status/toast line.
    pub status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntryKind, Project, ProjectStatus};

    fn project(name: &str) -> Project {
        Project {
            name: name.into(),
            path: format!("/r/{name}"),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }
    }

    fn ready(projects: Vec<Project>) -> AppState {
        let mut s = AppState::new(projects, Size::new(120, 40));
        s.app.splash.skip(); // past the splash
        s.dirty = false;
        s
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
        /// After a resize, scroll offsets are clamped in range (never grow without
        /// bound when the terminal shrinks).
        #[test]
        fn scroll_offset_always_in_range_after_resize(
            pre in 0usize..10_000, w in 1u16..400, h in 1u16..400,
        ) {
            let mut s = ready(vec![project("a")]);
            s.app.scroll.detail = pre;
            s.app.scroll.graphs = pre;
            update(&mut s, Event::Resize(Size::new(w, h)));
            proptest::prop_assert!(s.app.scroll.detail <= usize::from(h));
            proptest::prop_assert!(s.app.scroll.graphs <= usize::from(h));
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
        // The first REFRESH_EVERY_TICKS-1 ticks are silent…
        for _ in 0..(REFRESH_EVERY_TICKS - 1) {
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

    #[test]
    fn periodic_tick_with_no_selection_is_inert() {
        let mut s = ready(vec![]);
        for _ in 0..REFRESH_EVERY_TICKS {
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
    fn data_update_clamps_out_of_range() {
        let mut s = ready(vec![]);
        update(
            &mut s,
            Event::Data(DataUpdate {
                context_pct: Some(250),
                savings_pct: Some(180),
                token_sample: Some(42),
            }),
        );
        assert_eq!(s.app.context_pct, 100, "context clamped to 100");
        assert_eq!(s.app.savings_pct, 100, "savings clamped to 100");
        assert_eq!(s.app.token_series, vec![42]);
        assert!(s.dirty);
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
    fn add_project_action_becomes_persist_effect() {
        let mut s = ready(vec![]);
        // enter add-project mode and type a path, then Enter
        update(&mut s, Event::Key(CanopyKey::Char('a')));
        for c in "/tmp/x".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let flow = update(&mut s, Event::Key(CanopyKey::Enter));
        match flow {
            Flow::Continue(e) => {
                assert_eq!(
                    e.persist,
                    vec![PersistOp::AddProject {
                        path: "/tmp/x".into()
                    }]
                );
            }
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn remove_key_emits_remove_persist_for_selected() {
        let mut s = ready(vec![project("a"), project("b")]);
        update(&mut s, Event::Key(CanopyKey::Down)); // select /r/b
        let flow = update(&mut s, Event::Key(CanopyKey::Char('d')));
        match flow {
            Flow::Continue(e) => assert_eq!(
                e.persist,
                vec![PersistOp::RemoveProject {
                    path: "/r/b".into()
                }]
            ),
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn reorder_key_emits_reorder_persist_for_selected() {
        let mut s = ready(vec![project("a"), project("b")]);
        let flow = update(&mut s, Event::Key(CanopyKey::ShiftDown));
        match flow {
            Flow::Continue(e) => assert_eq!(
                e.persist,
                vec![PersistOp::ReorderProject {
                    path: "/r/a".into(),
                    up: false
                }]
            ),
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn remove_with_no_projects_is_inert() {
        let mut s = ready(vec![]);
        let flow = update(&mut s, Event::Key(CanopyKey::Char('d')));
        match flow {
            Flow::Continue(e) => assert!(e.persist.is_empty()),
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn flip_feature_emits_settoggle_and_install_action() {
        let mut s = ready(vec![project("a")]);
        s.app.set_focus(Panel::Detail); // cursor at 0 = Fff, currently off
        let flow = update(&mut s, Event::Key(CanopyKey::Char(' ')));
        match flow {
            Flow::Continue(e) => {
                assert_eq!(
                    e.persist,
                    vec![PersistOp::SetToggle {
                        path: "/r/a".into(),
                        feature: "fff".into(),
                        desired: true
                    }]
                );
                assert_eq!(
                    e.feature_actions,
                    vec![FeatureAction {
                        feature: Feature::Fff,
                        kind: FeatureActionKind::Install
                    }]
                );
            }
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn enabling_variant_flip_enqueues_base_first() {
        let mut s = ready(vec![project("a")]);
        s.app.set_focus(Panel::Detail);
        s.app.toggle_cursor = 2; // CodegraphTs — base off ⇒ base first
        let flow = update(&mut s, Event::Key(CanopyKey::Enter));
        match flow {
            Flow::Continue(e) => {
                assert_eq!(
                    e.persist,
                    vec![
                        PersistOp::SetToggle {
                            path: "/r/a".into(),
                            feature: "codegraph".into(),
                            desired: true
                        },
                        PersistOp::SetToggle {
                            path: "/r/a".into(),
                            feature: "codegraph-typescript".into(),
                            desired: true
                        },
                    ]
                );
                assert_eq!(e.feature_actions.len(), 2, "base install then variant");
            }
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn retry_feature_emits_reinstall_without_persist() {
        let mut s = ready(vec![project("a")]);
        s.app.set_focus(Panel::Detail);
        let flow = update(&mut s, Event::Key(CanopyKey::Char('r')));
        match flow {
            Flow::Continue(e) => {
                assert!(e.persist.is_empty(), "retry never changes desired state");
                assert_eq!(
                    e.feature_actions,
                    vec![FeatureAction {
                        feature: Feature::Fff,
                        kind: FeatureActionKind::Reinstall
                    }]
                );
            }
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn flip_with_no_selection_is_inert() {
        let mut s = ready(vec![]);
        s.app.set_focus(Panel::Detail);
        let flow = update(&mut s, Event::Key(CanopyKey::Char(' ')));
        match flow {
            Flow::Continue(e) => assert!(e.is_empty()),
            Flow::Quit => panic!("expected Continue"),
        }
    }

    /// Structural: Canopy only ever *emits* feature actions — it never installs.
    /// `Effects` carries `FeatureAction`s for the injected manager; there is no
    /// install/exec surface in the effect set.
    #[test]
    fn canopy_never_installs_directly() {
        let mut s = ready(vec![project("a")]);
        s.app.set_focus(Panel::Detail);
        let Flow::Continue(e) = update(&mut s, Event::Key(CanopyKey::Char('t'))) else {
            panic!("expected Continue");
        };
        // The only feature surface is the declarative action list.
        assert!(!e.feature_actions.is_empty());
        for a in &e.feature_actions {
            assert!(matches!(
                a.kind,
                FeatureActionKind::Install
                    | FeatureActionKind::Remove
                    | FeatureActionKind::Reinstall
            ));
        }
    }

    #[test]
    fn run_command_action_becomes_command_effect() {
        let mut s = ready(vec![]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "status".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        match update(&mut s, Event::Key(CanopyKey::Enter)) {
            Flow::Continue(e) => assert_eq!(e.commands.len(), 1),
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn persist_ops_are_insert_only() {
        // Exhaustive match: every PersistOp is an insert (add/remove/relabel/
        // reorder/toggle are all superseding appends). If anyone adds an
        // Update/Delete variant, this stops compiling — the immutability guard.
        let ops = [
            PersistOp::AddProject {
                path: String::new(),
            },
            PersistOp::RemoveProject {
                path: String::new(),
            },
            PersistOp::RelabelProject {
                path: String::new(),
                label: String::new(),
            },
            PersistOp::ReorderProject {
                path: String::new(),
                up: true,
            },
            PersistOp::SetToggle {
                path: String::new(),
                feature: String::new(),
                desired: false,
            },
        ];
        for op in ops {
            match op {
                PersistOp::AddProject { .. }
                | PersistOp::RemoveProject { .. }
                | PersistOp::RelabelProject { .. }
                | PersistOp::ReorderProject { .. }
                | PersistOp::SetToggle { .. } => {}
            }
        }
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

    #[test]
    fn snapshot_is_deterministic_for_unchanged_state() {
        let mut s = ready(vec![project("a"), project("b")]);
        let v1 = s.snapshot();
        let v2 = s.snapshot();
        assert_eq!(*v1.projects, *v2.projects);
        assert_eq!(v1.size, v2.size);
        assert_eq!(v1.command_line, v2.command_line);
        // The unchanged rows/series are *shared*, not re-allocated per frame.
        assert!(Arc::ptr_eq(&v1.projects, &v2.projects));
        assert!(Arc::ptr_eq(&v1.token_series, &v2.token_series));
    }

    #[test]
    fn mutation_is_visible_in_next_snapshot() {
        // The Arc sharing must never serve stale state: a held old snapshot and a
        // post-mutation snapshot disagree exactly where the mutation happened.
        let mut s = ready(vec![project("a"), project("b")]);
        let before = s.snapshot();
        update(
            &mut s,
            Event::ProjectData(ProjectData {
                project: "/r/a".into(),
                status: ProjectStatus::Running {
                    since_ms: 7,
                    active_sessions: 1,
                },
                detail: None,
            }),
        );
        let after = s.snapshot();
        assert_eq!(before.projects[0].status, ProjectStatus::Idle);
        assert!(matches!(
            after.projects[0].status,
            ProjectStatus::Running { .. }
        ));
        // A direct (non-reducer) mutation is caught by the comparison too.
        s.app.projects[1].name = "renamed".into();
        let direct = s.snapshot();
        assert_eq!(direct.projects[1].name, "renamed");
        assert_eq!(
            after.projects[1].name, "b",
            "the held snapshot is immutable"
        );
    }

    #[test]
    fn snapshot_marks_selection() {
        let mut s = ready(vec![project("a"), project("b")]);
        update(&mut s, Event::Key(CanopyKey::Down));
        let v = s.snapshot();
        assert_eq!(v.selected, 1);
        assert_eq!(v.detail.unwrap().path, "/r/b");
    }

    #[test]
    fn data_series_is_capped() {
        let mut s = ready(vec![]);
        for i in 0..300u64 {
            update(
                &mut s,
                Event::Data(DataUpdate {
                    token_sample: Some(i),
                    ..Default::default()
                }),
            );
        }
        assert!(s.app.token_series.len() <= 256);
    }
}
