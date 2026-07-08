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
use std::time::Duration;

use crate::col2::{plan_flip, DetailView, Feature, FeatureAction, FeatureActionKind};
use crate::command::Verb;
use crate::dispatch::{DispatchOutcome, DispatchRequest, Overlay};
use crate::hit::{hit_test, Target};
use crate::input::{on_key, Action, CanopyKey, MouseButton, MouseInput, MouseKind};
use crate::render::frame::Size;
use crate::{App, Mode, Panel, ProjectStatus};

/// Which panel currently holds keyboard focus (the spec's `Focus`; identical to
/// the existing [`Panel`]).
pub type Focus = Panel;

/// An event entering the reducer. Canopy-native (the loop translates terminal
/// events into these), so the state layer never names an engine type.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A normalized key press.
    Key(CanopyKey),
    /// A normalized mouse event (click / scroll) at a cell coordinate.
    Mouse(MouseInput),
    /// The terminal was resized.
    Resize(Size),
    /// A fixed-cadence animation/sampling tick.
    Tick,
    /// A per-project update from the data bus (P8): live status for one project.
    ProjectData(ProjectData),
    /// A column-3 feed update from the data bus (ADR-0009's Tokens/Nic/Recall
    /// trio): full section state for one project, applied when it is the
    /// selected one.
    Feed(FeedUpdate),
    /// A delegated dispatch completed (the loop ran it synchronously); the
    /// reducer renders the outcome into the results overlay.
    DispatchDone {
        /// The request that ran.
        request: DispatchRequest,
        /// What it produced.
        outcome: DispatchOutcome,
    },
    /// A data-plane health report from the bus worker (M55-04, ADR-0024 §1).
    /// Emitted on change only; the header dot renders the folded state.
    Health(HealthEvent),
}

/// What the bus worker observed about its own data plane in one read cycle.
/// The header dot folds these through [`fold_health`]: a degraded state holds
/// until a healthy cycle restores it (never a timer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthEvent {
    /// Reads completed with the worker infrastructure and canopy store sound.
    Healthy,
    /// The worker hit an internal failure (e.g. its async runtime).
    WorkerError,
    /// The machine-global canopy store could not be opened.
    StoreUnreachable,
}

/// Fold one health event into the shown [`Health`]: the latest report wins,
/// and only a healthy cycle restores green (the recover-on-healthy-poll rule).
#[must_use]
pub fn fold_health(ev: HealthEvent) -> crate::theme::Health {
    match ev {
        HealthEvent::Healthy => crate::theme::Health::Ok,
        HealthEvent::WorkerError => crate::theme::Health::Warn,
        HealthEvent::StoreUnreachable => crate::theme::Health::Down,
    }
}

/// One feed's full section state for one project. Each update carries the
/// **complete** recomputed view (never a delta), so dropped or duplicated
/// delivery converges on the fresh-read truth by construction (user story 14
/// and the PRD's delivery-robustness property).
#[derive(Clone, Debug, PartialEq)]
pub struct FeedUpdate {
    /// The project (canonical path) this feed update is for.
    pub project: String,
    /// Which sections it fills.
    pub payload: FeedPayload,
}

/// The feed kinds (ADR-0009), each patching its dashboard surface.
#[derive(Clone, Debug, PartialEq)]
pub enum FeedPayload {
    /// Token spend + the input|read|output breakdown.
    Tokens {
        /// The token-usage section.
        tokens: crate::col3::TokensView,
        /// The breakdown section.
        breakdown: crate::col3::BreakdownView,
    },
    /// The nic-gauge section.
    Nic(crate::col3::NicView),
    /// The recall sections + the threads count.
    Recall {
        /// Recall savings.
        savings: crate::col3::RecallView<crate::col3::SavingsData>,
        /// Recall MCP calls.
        calls: crate::col3::RecallView<crate::col3::CallsData>,
        /// Distinct active sessions.
        threads: crate::col3::ThreadsView,
    },
    /// The activity card's trailing-week grouped daily bars (M55-06). Patches
    /// the column-2 activity card, not column 3.
    Activity(crate::col3::ActivityView),
    /// The system panel's four domain metrics (M55-07). Patches `Col3.metrics`.
    Metrics(crate::col3::PanelMetrics),
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
    /// Delegated CLI requests for the injected dispatcher (ADR-0021), each
    /// with its resolved target repository (the selected project's path at
    /// emit time — never process cwd).
    pub dispatches: Vec<(DispatchRequest, String)>,
    /// Feature install/remove actions for the (injected) feature manager (P5 S2).
    /// Canopy plans these but never performs an install itself.
    pub feature_actions: Vec<FeatureAction>,
    /// A resolved project path to open in `$EDITOR` (M55-10). The **only**
    /// sanctioned process spawn from the TUI, performed by the injected
    /// [`crate::dispatch::EditorLauncher`] — structurally outside the palette
    /// grammar (ADR-0025).
    pub editor_launch: Option<String>,
}

impl Effects {
    /// `true` if there is nothing to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data_requests.is_empty()
            && self.persist.is_empty()
            && self.dispatches.is_empty()
            && self.feature_actions.is_empty()
            && self.editor_launch.is_none()
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

/// How many idle ticks between background re-stats of the selected project. At the
/// 100ms poll cadence this re-reads the foreground project roughly every 1.5s, so a
/// `Running` → `Idle` transition (activity aging past the window) surfaces even
/// though no cursor change fires a bus poll. Time-based only; never dirties.
const REFRESH_EVERY_TICKS: u32 = 15;

/// How long the one-shot startup sweep runs as the panels first fill after the
/// splash (M55-14, ADR-0022) — a single settle window, then it never re-fires.
const STARTUP_SWEEP: Duration = Duration::from_millis(450);
/// How long a selection/hover pulse takes to decay back to rest (M55-14).
const PULSE_DECAY: Duration = Duration::from_millis(300);

/// How long the suggest panel's text fades in (ADR-0022's shared contract).
const SUGGEST_FADE: Duration = Duration::from_millis(180);

/// How long a column-3 data change eases from its old shape to the new one. Kept
/// short so the graph "catches up" without feeling laggy; the loop only spins
/// fast (and only redraws) for this window, then returns to the ~0% CPU idle.
const COL3_SETTLE: Duration = Duration::from_millis(350);

/// An in-flight column-3 tween: where the series eased *from*, and how far in.
/// The *target* is always `AppState::col3` (the truth), so a mid-tween snapshot
/// still reports the true headline numbers.
#[derive(Clone, Debug)]
struct Col3Anim {
    /// The pre-change series to ease away from.
    from: Arc<crate::col3::Col3>,
    /// Elapsed time since the change (drives progress against [`COL3_SETTLE`]).
    elapsed: Duration,
}

/// The raw linear progress of a tween in `[0, 1]` for `elapsed` against the
/// settle window.
fn elapsed_fraction(elapsed: Duration) -> f32 {
    let settle = COL3_SETTLE.as_secs_f32();
    if settle <= 0.0 {
        return 1.0;
    }
    (elapsed.as_secs_f32() / settle).clamp(0.0, 1.0)
}

/// Ease-out cubic: fast start, gentle finish — the motion reads as "settling"
/// rather than linear. Endpoints are exact (`0 → 0`, `1 → 1`).
fn ease_out(t: f32) -> f32 {
    let inv = 1.0 - t.clamp(0.0, 1.0);
    1.0 - inv * inv * inv
}

/// The whole dashboard state: the existing [`App`] model plus loop metadata.
#[derive(Clone, Debug)]
pub struct AppState {
    /// The dashboard model (projects, selection, mode, command buffer, …).
    pub app: App,
    /// The current terminal size.
    pub size: Size,
    /// Set when something observable changed; gates redraws.
    pub dirty: bool,
    /// Honor OS/user reduced-motion: skip col-3 tweens, snap to each new value.
    pub reduced_motion: bool,
    /// The binary's version string, injected at launch (the render crate stays
    /// env-free); empty hides the header's version segment.
    pub version: String,
    /// The data-plane health the header dot shows (folded from bus reports).
    pub health: crate::theme::Health,
    /// Idle-tick counter driving the background re-stat cadence (never dirties).
    tick: u32,
    /// The bus-computed rich detail for the active project (keyed by its path);
    /// the snapshot shares it when it matches the selection, else a thin fallback.
    detail: Option<Arc<DetailView>>,
    /// The last snapshot's project rows, shared with every [`ViewModel`] until the
    /// underlying projects change (checked by cheap comparison in [`Self::snapshot`],
    /// so a stale row can never be served).
    rows_cache: Arc<Vec<ProjectRow>>,
    /// The column-3 sections' view-model, filled by the data plane's feeds
    /// (W3/W4); `Default` is every section's honest placeholder. This is always the
    /// **target** value; the displayed value is [`Self::snapshot`]'s interpolation
    /// when a tween is in flight.
    col3: Arc<crate::col3::Col3>,
    /// The in-flight column-3 tween, if any (drives the loop's fast-poll window).
    col3_anim: Option<Col3Anim>,
    /// The selected project's activity-card data (M55-06), filled by the
    /// Activity feed; `Default` is the honest placeholder. Column-2, so it sits
    /// outside the col3 tween machinery (activity snaps; W6 may add polish).
    activity: Arc<crate::col3::ActivityView>,
    /// The suggest panel's fade-in progress (0..=1, dt-advanced under the
    /// ADR-0022 animator contract; snaps to 1 under reduced motion / ASCII).
    suggest_fade: f32,
    /// The one-shot startup sweep progress (0..=1; M55-14). Rests at 1.0 (done);
    /// armed to 0.0 once per launch by [`Self::arm_startup_sweep`], then advances
    /// to 1.0 and **never re-fires**. Reduced motion / headless stays at 1.0.
    startup_sweep: f32,
    /// A transient selection/hover pulse (1.0 → 0.0 decay; M55-14). Armed on a
    /// selection change; a brief colour lerp acknowledges the cursor.
    select_pulse: f32,
    /// A transient button press-flash (1.0 → 0.0 decay; ADR-0028), keyed by
    /// [`Self::pressed`] so the right button lights. Distinct from
    /// `select_pulse`: this is bound to the activation event, not a selection
    /// change, so re-pressing the same button re-arms it.
    press_pulse: f32,
    /// The button whose press-flash is in flight (keys `press_pulse`).
    pressed: Option<Target>,
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
            reduced_motion: false,
            version: String::new(),
            health: crate::theme::Health::Ok,
            detail: None,
            rows_cache: Arc::new(Vec::new()),
            col3: Arc::new(crate::col3::Col3::default()),
            col3_anim: None,
            activity: Arc::new(crate::col3::ActivityView::default()),
            suggest_fade: 1.0,
            startup_sweep: 1.0,
            select_pulse: 0.0,
            press_pulse: 0.0,
            pressed: None,
        }
    }

    /// Arm the one-shot startup sweep (M55-14): the panels reveal as the
    /// dashboard first fills after the splash. Reduced motion shows the final
    /// frame instantly. Called once at launch; the sweep never re-fires.
    pub fn arm_startup_sweep(&mut self) {
        self.startup_sweep = if self.reduced_motion { 1.0 } else { 0.0 };
    }

    /// The startup-sweep reveal progress (0..=1) for the renderer.
    #[must_use]
    pub fn startup_sweep(&self) -> f32 {
        self.startup_sweep
    }

    /// (Re)arm the suggest panel's fade-in: entering command mode starts the
    /// ~180 ms ramp (ADR-0022); reduced motion shows instantly.
    pub fn arm_suggest_fade(&mut self) {
        self.suggest_fade = if self.reduced_motion { 1.0 } else { 0.0 };
    }

    /// The current suggest fade progress (0..=1).
    #[must_use]
    pub fn suggest_fade(&self) -> f32 {
        self.suggest_fade
    }

    /// Replace the column-3 sections' view-model (the W3/W4 feeds' apply path).
    ///
    /// An identical replay is a no-op (delivery robustness). Otherwise the new
    /// value becomes the target and — unless `reduced_motion` or the change is a
    /// pure placeholder↔data transition — a tween is armed from the prior value,
    /// so the graphs ease to the new shape over [`COL3_SETTLE`]. Headlines and
    /// variants are never tweened (see [`crate::col3::interpolate`]).
    pub fn set_col3(&mut self, col3: crate::col3::Col3) {
        if *self.col3 == col3 {
            return;
        }
        let animate = !self.reduced_motion && crate::col3::should_animate(&self.col3, &col3);
        if animate {
            // Ease from what is on screen *now* — the interpolated frame if a tween
            // is already running — so a re-tween continues smoothly instead of
            // snapping back to the previous target.
            let from = self.displayed_col3();
            self.col3 = Arc::new(col3);
            self.col3_anim = Some(Col3Anim {
                from,
                elapsed: Duration::ZERO,
            });
        } else {
            // A non-animating change (a nic snap, or a placeholder↔data variant
            // flip): adopt the new target but LEAVE any in-flight tween running —
            // `interpolate` reads non-tweened sections from the target, so the fresh
            // values show at once while other sections keep easing (no
            // cross-section cancellation).
            self.col3 = Arc::new(col3);
        }
        self.dirty = true;
    }

    /// Advance any in-flight column-3 tween by `dt`, clearing it once it settles.
    /// Returns `true` while a tween is (or was just) active, so the loop redraws
    /// the interpolated frame; returns `false` at rest (no wasted redraw). Takes
    /// an explicit `dt` so it is deterministic in tests.
    pub fn advance_anim(&mut self, dt: Duration) -> bool {
        let mut active = false;
        if let Some(anim) = self.col3_anim.as_mut() {
            anim.elapsed = anim.elapsed.saturating_add(dt);
            if anim.elapsed >= COL3_SETTLE {
                self.col3_anim = None; // the final (settled) frame renders the target
            }
            active = true;
        }
        // The suggest panel's fade rides the same dt-advance (ADR-0022): the
        // loop's settle-window fast poll carries the frames; at 1.0 it is a
        // no-op and never redraws (the idle property).
        if self.app.mode == Mode::Command && self.suggest_fade < 1.0 {
            let step = dt.as_secs_f32() / SUGGEST_FADE.as_secs_f32();
            self.suggest_fade = (self.suggest_fade + step).min(1.0);
            active = true;
        }
        // The one-shot startup sweep (M55-14): advances to 1.0 then rests
        // forever (it never re-arms, so it can't re-fire).
        if self.startup_sweep < 1.0 {
            let step = dt.as_secs_f32() / STARTUP_SWEEP.as_secs_f32();
            self.startup_sweep = (self.startup_sweep + step).min(1.0);
            active = true;
        }
        // The selection pulse decays to rest (M55-14).
        if self.select_pulse > 0.0 {
            let step = dt.as_secs_f32() / PULSE_DECAY.as_secs_f32();
            self.select_pulse = (self.select_pulse - step).max(0.0);
            active = true;
        }
        // The button press-flash decays to rest (ADR-0028) — same shape as the
        // selection pulse, keyed by `pressed`.
        if self.press_pulse > 0.0 {
            let step = dt.as_secs_f32() / PULSE_DECAY.as_secs_f32();
            self.press_pulse = (self.press_pulse - step).max(0.0);
            if self.press_pulse <= 0.0 {
                self.pressed = None;
            }
            active = true;
        }
        active
    }

    /// Whether any dt-tween is in flight (drives the loop's fast-poll window):
    /// a col-3 tween, the suggest fade, the startup sweep, or a selection pulse.
    #[must_use]
    pub fn anim_active(&self) -> bool {
        self.col3_anim.is_some()
            || (self.app.mode == Mode::Command && self.suggest_fade < 1.0)
            || self.startup_sweep < 1.0
            || self.select_pulse > 0.0
            || self.press_pulse > 0.0
    }

    /// Arm a selection/hover pulse (M55-14): a brief colour lerp acknowledges
    /// the cursor. Reduced motion skips it (rests at 0).
    fn arm_select_pulse(&mut self) {
        if !self.reduced_motion {
            self.select_pulse = 1.0;
        }
    }

    /// Arm a button press-flash (ADR-0028), keyed by `target`: a brief colour
    /// lerp acknowledges the press. Bound to the activation event, so a repeat
    /// press re-arms; reduced motion skips it (rests at 0).
    fn arm_press_pulse(&mut self, target: Target) {
        if !self.reduced_motion {
            self.press_pulse = 1.0;
            self.pressed = Some(target);
        }
    }

    /// The displayed column-3 value: the interpolated (eased) frame while a tween
    /// is in flight, else the shared target `Arc` (unchanged — so a quiescent
    /// snapshot stays pointer-equal, the ~0% CPU invariant).
    fn displayed_col3(&self) -> Arc<crate::col3::Col3> {
        match &self.col3_anim {
            Some(anim) => {
                let progress = ease_out(elapsed_fraction(anim.elapsed));
                Arc::new(crate::col3::interpolate(&anim.from, &self.col3, progress))
            }
            None => Arc::clone(&self.col3),
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
            version: self.version.clone(),
            health: self.health,
            show_wizard: self.app.show_wizard,
            projects: Arc::clone(&self.rows_cache),
            selected: self.app.selected,
            detail,
            col3: self.displayed_col3(),
            activity: Arc::clone(&self.activity),
            detail_scroll: self.app.scroll.detail,
            graphs_scroll: self.app.scroll.graphs,
            toggle_cursor: self.app.toggle_cursor,
            command_line: self.app.command_line.clone(),
            status: self.app.status.clone(),
            overlay: self.app.overlay.clone(),
            overlay_focus: self.app.overlay_focus,
            command_error: self.app.command_error.clone(),
            suggestions: crate::current_suggestions(&self.app),
            suggest_highlight: self.app.suggest_highlight,
            suggest_fade: self.suggest_fade,
            sweep: self.startup_sweep,
            select_pulse: self.select_pulse,
            press_pulse: self.press_pulse,
            pressed: self.pressed.clone(),
            feature_runtime: self.app.feature_runtime.clone(),
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
        Event::ProjectData(pd) => {
            // Only dirty when the status actually changed, so a re-stat that finds
            // nothing new never forces a redraw (the ~0% CPU invariant).
            if s.apply_project_data(&pd) {
                s.dirty = true;
            }
            Flow::Continue(Effects::default())
        }
        Event::Feed(update) => {
            apply_feed(s, update);
            Flow::Continue(Effects::default())
        }
        Event::Key(k) => {
            let before = s.app.selected_project().map(|p| p.path.clone());
            let mode_before = s.app.mode;
            let action = on_key(&mut s.app, k);
            if mode_before != Mode::Command && s.app.mode == Mode::Command {
                // Entering command mode opens the suggest panel: arm its fade
                // (ADR-0022; reduced motion / ASCII snap to the final frame).
                s.arm_suggest_fade();
                s.app.suggest_highlight = 0;
            }
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
                Action::ToggleFeature => {
                    toggle_flow(&s.app, s.detail.as_deref(), cursored_feature(&s.app))
                }
                Action::RetryFeature => retry_flow(&s.app),
                Action::RunLine(buf) => run_line(s, &buf),
                Action::ConfirmDispatch => confirm_dispatch(s),
                Action::OpenEditor => editor_flow(s),
                other => fold_action(other),
            };
            // When the selection moves, ask the bus for the new project's data and
            // reset the toggle cursor to the top of the new project's toggles.
            if let Flow::Continue(eff) = &mut flow {
                eff.data_requests
                    .extend(selection_change_effects(s, &before));
            }
            flow
        }
        Event::Mouse(m) => on_mouse(s, m),
        Event::Health(ev) => {
            // Fold the report; dirty only when the dot actually changes (a
            // steady Healthy stream never forces a redraw).
            let next = fold_health(ev);
            if s.health != next {
                s.health = next;
                s.dirty = true;
            }
            Flow::Continue(Effects::default())
        }
        Event::DispatchDone { request, outcome } => {
            if outcome.code == 0 {
                // Bumped only on successful dispatch (M5-10).
                if let Some(word) = request.verb_line().split_whitespace().nth(1) {
                    s.app.frecency.bump(word);
                }
            }
            s.app.open_overlay(Overlay::results(&request, &outcome));
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
    }
}

/// Apply one feed update to the selected project's surfaces. A feed for any
/// other project is stale routing and is dropped (the bus recomputes on the
/// next `SetActive`). The activity card (column 2) patches directly and dirties
/// on real change; the column-3 feeds flow through `set_col3` (which dirties
/// and arms a tween only on a real change).
fn apply_feed(s: &mut AppState, update: FeedUpdate) {
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
        FeedPayload::Nic(nic) => {
            let mut col3 = (*s.col3).clone();
            col3.nic = nic;
            s.set_col3(col3);
        }
        FeedPayload::Recall {
            savings,
            calls,
            threads,
        } => {
            let mut col3 = (*s.col3).clone();
            col3.savings = savings;
            col3.calls = calls;
            col3.threads = threads;
            s.set_col3(col3);
        }
        FeedPayload::Metrics(metrics) => {
            let mut col3 = (*s.col3).clone();
            col3.metrics = metrics;
            s.set_col3(col3);
        }
    }
}

/// After a possible selection move, reset the per-project cursor and column-3
/// sections and ask the bus for the newly-selected project's data. Returns the
/// data requests to add (empty when the selection did not change). Shared by the
/// keyboard and mouse paths so a click and an arrow key are behaviorally
/// identical.
fn selection_change_effects(s: &mut AppState, before: &Option<String>) -> Vec<DataRequest> {
    let after = s.app.selected_project().map(|p| p.path.clone());
    if *before == after {
        return Vec::new();
    }
    s.app.toggle_cursor = 0;
    s.arm_select_pulse(); // acknowledge the cursor move (M55-14)
                          // A different project's sections must never linger: reset to placeholders
                          // (and cancel any in-flight tween) until its own feeds arrive.
    s.set_col3(crate::col3::Col3::default());
    if !matches!(*s.activity, crate::col3::ActivityView::NoProjection) {
        s.activity = Arc::new(crate::col3::ActivityView::default());
        s.dirty = true;
    }
    let mut reqs = vec![DataRequest::SetActive(after.clone())];
    if let Some(path) = after {
        reqs.push(DataRequest::Refresh(path));
    }
    reqs
}

/// Handle a mouse event by hit-testing it against the current view and mapping
/// the target onto the **same** state changes the keyboard produces. Only left
/// presses and the scroll wheel act; moves/drags/releases are ignored (no
/// per-move work, keeping the ~0% CPU idle). While the splash is up, any click
/// dismisses it and is otherwise swallowed (mirrors the key path).
fn on_mouse(s: &mut AppState, m: MouseInput) -> Flow {
    if !s.app.splash.done() {
        s.app.splash.skip();
        return Flow::Continue(Effects::default());
    }
    let vm = s.snapshot();
    match m.kind {
        MouseKind::Down(MouseButton::Left) => match hit_test(&vm, m.col, m.row) {
            Some(target) => apply_click(s, target),
            None => Flow::Continue(Effects::default()),
        },
        MouseKind::ScrollUp | MouseKind::ScrollDown => {
            let down = matches!(m.kind, MouseKind::ScrollDown);
            apply_scroll(s, &vm, m.col, m.row, down)
        }
        // Right/middle presses, drags, moves and releases don't drive the UI.
        _ => Flow::Continue(Effects::default()),
    }
}

/// Apply a left-click target: focus panels, select a project, flip a toggle, or
/// open the command bar — each the same mutation as the equivalent keypress.
fn apply_click(s: &mut AppState, target: Target) -> Flow {
    // Acknowledge a button press with the flash (ADR-0028) — keyed by target so
    // the right button lights; a repeat press re-arms. Selection/focus/body
    // targets don't flash (the rail row has its own selection pulse).
    if target.is_button() {
        s.arm_press_pulse(target.clone());
    }
    match target {
        Target::PanelFocus(panel) => {
            s.app.set_focus(panel);
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::CommandBar => {
            s.app.set_focus(Panel::Command);
            s.app.mode = Mode::Command;
            s.app.command_line.clear();
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::ProjectRow(i) => {
            s.app.set_focus(Panel::Projects);
            let before = s.app.selected_project().map(|p| p.path.clone());
            if i < s.app.projects.len() {
                s.app.selected = i;
            }
            s.dirty = true;
            Flow::Continue(Effects {
                data_requests: selection_change_effects(s, &before),
                ..Effects::default()
            })
        }
        // The rail action bar (M56-05): the same typed effects the `a` / `d` /
        // ⇧-arrow keys raise — no new capability, only a new trigger. The press
        // flash is armed above (is_button).
        Target::RailAdd | Target::RailRemove | Target::RailReorderUp | Target::RailReorderDown => {
            apply_rail_action(s, &target)
        }
        Target::ToggleRow(i) => {
            s.app.set_focus(Panel::Detail);
            let last = Feature::ALL.len().saturating_sub(1);
            s.app.toggle_cursor = i.min(last);
            s.dirty = true;
            // Flip it — identical to Space/Enter/`t` on the keyboard.
            toggle_flow(&s.app, s.detail.as_deref(), cursored_feature(&s.app))
        }
        Target::Section(_) => {
            s.app.set_focus(Panel::Graphs);
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        // The overlay's mouse surface (M5-09): Confirm is the mouse Enter,
        // Cancel the mouse Esc; a body click is inert (wheel scrolls).
        Target::OverlayConfirm => {
            s.dirty = true;
            confirm_dispatch(s)
        }
        Target::OverlayCancel => {
            if matches!(s.app.overlay, Some(Overlay::Confirm { .. })) {
                s.app.overlay = None;
            }
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        // The close X (ADR-0027) — the click/Tab equivalent of Esc; dismisses
        // any modal that carries a Close control.
        Target::OverlayClose => {
            s.app.overlay = None;
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::OverlayBody => Flow::Continue(Effects::default()),
        // A suggestion click INSERTS only — Tab's mouse equivalent, never
        // execution (M5-11; PRD user story 32).
        Target::SuggestRow(i) => {
            s.app.suggest_highlight = i;
            crate::input::insert_suggestion(&mut s.app);
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        // The header actions (M55-04) map onto the exact same effects their
        // keys produce — `?` help, `/` palette, `q` quit.
        Target::HeaderHelp => {
            s.dirty = true;
            dashboard_verb_flow(s, Verb::Help)
        }
        Target::HeaderPalette => {
            s.app.set_focus(Panel::Command);
            s.app.mode = Mode::Command;
            s.app.command_line.clear();
            s.arm_suggest_fade();
            s.app.suggest_highlight = 0;
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::HeaderQuit => Flow::Quit,
        // The repository card's button — the same launch the `e` key produces.
        Target::EditorButton => {
            s.app.set_focus(Panel::Detail);
            s.dirty = true;
            editor_flow(s)
        }
    }
}

/// Open the selected project in `$EDITOR` (M55-10): resolve the selected
/// project's path and emit the single sanctioned launch effect. No selection ⇒
/// inert. The path is the project's own — never process cwd (ADR-0025).
fn editor_flow(s: &AppState) -> Flow {
    let Some(path) = s.app.selected_project().map(|p| p.path.clone()) else {
        return Flow::Continue(Effects::default());
    };
    Flow::Continue(Effects {
        editor_launch: Some(path),
        ..Effects::default()
    })
}

/// Apply a scroll-wheel event over the panel under the cursor, using the same
/// per-panel vertical semantics as the keyboard: projects move the selection,
/// detail moves the toggle cursor, graphs scroll their viewport.
fn apply_scroll(s: &mut AppState, vm: &ViewModel, col: u16, row: u16, down: bool) -> Flow {
    let panel = match hit_test(vm, col, row) {
        Some(Target::ProjectRow(_)) => Panel::Projects,
        Some(Target::ToggleRow(_)) => Panel::Detail,
        Some(Target::Section(_)) => Panel::Graphs,
        Some(Target::PanelFocus(p)) => p,
        // The wheel moves the suggest highlight (never inserts, never runs).
        Some(Target::SuggestRow(_)) => {
            let len = crate::current_suggestions(&s.app).len();
            if len > 0 {
                s.app.suggest_highlight = if down {
                    (s.app.suggest_highlight + 1).min(len - 1)
                } else {
                    s.app.suggest_highlight.saturating_sub(1)
                };
                s.dirty = true;
            }
            return Flow::Continue(Effects::default());
        }
        // The wheel scrolls an open Results overlay like `j`/`k`.
        Some(
            Target::OverlayBody
            | Target::OverlayConfirm
            | Target::OverlayCancel
            | Target::OverlayClose,
        ) => {
            if let Some(Overlay::Results { scroll, lines, .. }) = s.app.overlay.as_mut() {
                *scroll = if down {
                    scroll.saturating_add(1).min(lines.len().saturating_sub(1))
                } else {
                    scroll.saturating_sub(1)
                };
                s.dirty = true;
            }
            return Flow::Continue(Effects::default());
        }
        Some(
            Target::CommandBar
            | Target::HeaderHelp
            | Target::HeaderPalette
            | Target::HeaderQuit
            | Target::EditorButton
            | Target::RailAdd
            | Target::RailReorderUp
            | Target::RailReorderDown
            | Target::RailRemove,
        )
        | None => return Flow::Continue(Effects::default()),
    };
    s.dirty = true;
    match panel {
        Panel::Projects => {
            let before = s.app.selected_project().map(|p| p.path.clone());
            if down {
                s.app.select_next();
            } else {
                s.app.select_prev();
            }
            Flow::Continue(Effects {
                data_requests: selection_change_effects(s, &before),
                ..Effects::default()
            })
        }
        Panel::Detail => {
            s.app.move_toggle_cursor(down);
            Flow::Continue(Effects::default())
        }
        Panel::Graphs => {
            s.app.scroll.graphs = if down {
                s.app.scroll.graphs.saturating_add(1)
            } else {
                s.app.scroll.graphs.saturating_sub(1)
            };
            Flow::Continue(Effects::default())
        }
        Panel::Command => Flow::Continue(Effects::default()),
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
        | Action::RetryFeature
        | Action::RunLine(_)
        | Action::ConfirmDispatch
        | Action::OpenEditor => Flow::Continue(Effects::default()),
        Action::AddProject(path) => Flow::Continue(Effects {
            persist: vec![PersistOp::AddProject { path }],
            ..Effects::default()
        }),
    }
}

/// Submit one palette buffer (M5-07/09): parse; a span error renders inline
/// (buffer + mode kept so the underline points at the typed text); a verb
/// routes by tier — dashboard verbs to typed panel effects, read-only
/// delegation straight to the dispatcher behind a Running frame, mutating
/// delegation to the confirm overlay.
fn run_line(s: &mut AppState, buf: &str) -> Flow {
    match crate::command::parse(buf) {
        Err(e) => {
            // An incomplete line accepts the highlighted suggestion first
            // (PRD user story: Enter runs, accepting the highlight when the
            // line is incomplete); only if THAT parses does it run.
            let suggestions = crate::current_suggestions(&s.app);
            if let Some(sug) = suggestions.get(
                s.app
                    .suggest_highlight
                    .min(suggestions.len().saturating_sub(1)),
            ) {
                if crate::command::parse(&sug.insert).is_ok() {
                    let line = sug.insert.clone();
                    s.app.command_line.clone_from(&line);
                    return run_line(s, &line);
                }
            }
            s.app.command_error = Some(e);
            Flow::Continue(Effects::default())
        }
        Ok(verb) => {
            s.app.command_error = None;
            s.app.command_line.clear();
            s.app.mode = Mode::Normal;
            // Frecency: a dashboard verb executes right here (bump now); a
            // delegated verb bumps only when its dispatch succeeds.
            if crate::dispatch::request_for(&verb).is_none() {
                s.app.frecency.bump(verb_word_of(&verb));
            }
            match crate::dispatch::request_for(&verb) {
                Some(req) => {
                    let repo = s
                        .app
                        .selected_project()
                        .map(|p| p.path.clone())
                        .unwrap_or_default();
                    if req.is_mutating() {
                        s.app.open_overlay(Overlay::Confirm { request: req, repo });
                        Flow::Continue(Effects::default())
                    } else {
                        s.app.open_overlay(Overlay::Running {
                            title: req.verb_line(),
                        });
                        Flow::Continue(Effects {
                            dispatches: vec![(req, repo)],
                            ..Effects::default()
                        })
                    }
                }
                None => dashboard_verb_flow(s, verb),
            }
        }
    }
}

/// The confirm overlay's Enter/Confirm: swap Confirm → Running and emit the
/// pending dispatch with the repo resolved at confirm time.
fn confirm_dispatch(s: &mut AppState) -> Flow {
    let Some(Overlay::Confirm { request, repo }) = s.app.overlay.take() else {
        return Flow::Continue(Effects::default());
    };
    s.app.open_overlay(Overlay::Running {
        title: request.verb_line(),
    });
    Flow::Continue(Effects {
        dispatches: vec![(request, repo)],
        ..Effects::default()
    })
}

/// Resolve a grammar project reference against the current rows.
fn resolve_project_ref(app: &App, r: &crate::command::ProjectRef) -> Option<String> {
    use crate::command::ProjectRef;
    match r {
        ProjectRef::Selected => app.selected_project().map(|p| p.path.clone()),
        ProjectRef::Index(n) => app
            .projects
            .get(n.saturating_sub(1))
            .map(|p| p.path.clone()),
        ProjectRef::Name(name) => app
            .projects
            .iter()
            .find(|p| p.name == *name || p.path == *name)
            .map(|p| p.path.clone()),
    }
}

/// A dashboard-tier verb dispatches to the SAME typed effects the panels
/// emit — the palette can never exceed a panel's privilege (ADR-0021).
fn dashboard_verb_flow(s: &mut AppState, verb: Verb) -> Flow {
    use crate::command::PanelName;
    match verb {
        Verb::Quit => Flow::Quit,
        Verb::Help => {
            let lines: Vec<String> = crate::command::help_lines();
            s.app.open_overlay(Overlay::Results {
                title: "palette".into(),
                lines,
                scroll: 0,
                footer: None,
                truncated: false,
            });
            Flow::Continue(Effects::default())
        }
        Verb::Focus(p) => {
            s.app.set_focus(match p {
                PanelName::Projects => Panel::Projects,
                PanelName::Detail => Panel::Detail,
                PanelName::Graphs => Panel::Graphs,
                PanelName::Command => Panel::Command,
            });
            Flow::Continue(Effects::default())
        }
        Verb::Select(r) => {
            let before = s.app.selected_project().map(|p| p.path.clone());
            let Some(path) = resolve_project_ref(&s.app, &r) else {
                return no_match_status(s);
            };
            s.app.select_path(&path);
            Flow::Continue(Effects {
                data_requests: selection_change_effects(s, &before),
                ..Effects::default()
            })
        }
        Verb::Add => {
            let Some(path) = s.app.launch_cwd.clone() else {
                s.app.status = Some("launch directory unknown — run `rift init` there".into());
                return Flow::Continue(Effects::default());
            };
            Flow::Continue(Effects {
                persist: vec![PersistOp::AddProject { path }],
                ..Effects::default()
            })
        }
        Verb::Remove(r) => {
            let Some(path) = resolve_project_ref(&s.app, &r) else {
                return no_match_status(s);
            };
            persist_flow(Some(PersistOp::RemoveProject { path }))
        }
        Verb::Relabel(r, label) => {
            let Some(path) = resolve_project_ref(&s.app, &r) else {
                return no_match_status(s);
            };
            persist_flow(Some(PersistOp::RelabelProject { path, label }))
        }
        Verb::Toggle(key) => {
            let Some(feature) = Feature::ALL.iter().find(|f| f.key() == key) else {
                return unknown_feature_status(s, &key);
            };
            toggle_flow(&s.app, s.detail.as_deref(), *feature)
        }
        Verb::Reinstall(key) => {
            let Some(feature) = Feature::ALL.iter().find(|f| f.key() == key) else {
                return unknown_feature_status(s, &key);
            };
            let Some(path) = s.app.selected_project().map(|p| p.path.clone()) else {
                return no_match_status(s);
            };
            Flow::Continue(Effects {
                feature_actions: vec![FeatureAction {
                    feature: *feature,
                    kind: FeatureActionKind::Reinstall,
                    path,
                }],
                ..Effects::default()
            })
        }
        // Delegable verbs never reach here (request_for handled them).
        _ => Flow::Continue(Effects::default()),
    }
}

/// The catalog word a dashboard verb bumps in the frecency log.
fn verb_word_of(verb: &Verb) -> &'static str {
    match verb {
        Verb::Select(_) => "select",
        Verb::Add => "add",
        Verb::Remove(_) => "remove",
        Verb::Relabel(..) => "relabel",
        Verb::Toggle(_) => "toggle",
        Verb::Reinstall(_) => "reinstall",
        Verb::Focus(_) => "focus",
        Verb::Help => "help",
        Verb::Quit => "quit",
        // Delegated verbs bump on DispatchDone, not here.
        _ => "",
    }
}

/// The "no matching project" toast (shared by the project-ref verbs).
fn no_match_status(s: &mut AppState) -> Flow {
    s.app.status = Some("no matching project".into());
    Flow::Continue(Effects::default())
}

/// The "unknown feature" toast (shared by toggle/reinstall).
fn unknown_feature_status(s: &mut AppState, key: &str) -> Flow {
    s.app.status = Some(format!("unknown feature `{key}` — try /help"));
    Flow::Continue(Effects::default())
}

/// The rail action bar's clicks (M56-05) — the add/remove/reorder buttons raise
/// the same typed effects the `a` / `d` / ⇧-arrow keys do. Add enters
/// AddProject mode; remove/reorder act on the selected project (no selection ⇒
/// inert). Split out of `apply_click` to keep that reducer under the line limit.
fn apply_rail_action(s: &mut AppState, target: &Target) -> Flow {
    s.app.set_focus(Panel::Projects);
    s.dirty = true;
    match target {
        Target::RailAdd => {
            s.app.mode = Mode::AddProject;
            s.app.command_line.clear();
            s.app.status = None;
            Flow::Continue(Effects::default())
        }
        Target::RailRemove => {
            let sel = s.app.selected_project().map(|p| p.path.clone());
            persist_flow(sel.map(|path| PersistOp::RemoveProject { path }))
        }
        Target::RailReorderUp | Target::RailReorderDown => {
            let up = matches!(target, Target::RailReorderUp);
            let sel = s.app.selected_project().map(|p| p.path.clone());
            persist_flow(sel.map(|path| PersistOp::ReorderProject { path, up }))
        }
        _ => Flow::Continue(Effects::default()),
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

/// Plan a flip of `feature` into append-only `SetToggle` writes plus the
/// matching install/remove [`FeatureAction`]s (base-first / cascade order). No
/// selection ⇒ inert. The keyboard/mouse paths pass the cursored feature; the
/// palette's `/toggle <key>` passes the named one.
fn toggle_flow(app: &App, detail: Option<&DetailView>, feature: Feature) -> Flow {
    let Some(path) = app.selected_project().map(|p| p.path.clone()) else {
        return Flow::Continue(Effects::default());
    };
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
        eff.feature_actions
            .push(FeatureAction::from_change(change, path.clone()));
    }
    Flow::Continue(eff)
}

/// Retry (reinstall) the cursored feature — no desired-state change, just a fresh
/// install action for the manager (the one-key retry after a `Failed`). No
/// selection ⇒ inert.
fn retry_flow(app: &App) -> Flow {
    let Some(path) = app.selected_project().map(|p| p.path.clone()) else {
        return Flow::Continue(Effects::default());
    };
    Flow::Continue(Effects {
        feature_actions: vec![FeatureAction {
            feature: cursored_feature(app),
            kind: FeatureActionKind::Reinstall,
            path,
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
    /// The binary's version (header segment; empty hides it).
    pub version: String,
    /// The data-plane health the header dot shows.
    pub health: crate::theme::Health,
    /// Whether to show the first-run wizard.
    pub show_wizard: bool,
    /// Column-1 project rows (shared, rebuilt only when the rows change).
    pub projects: Arc<Vec<ProjectRow>>,
    /// The selected project's index (selection highlight + scroll-to-selection).
    pub selected: usize,
    /// Column-2 selected-project detail (shared with the state's cached copy).
    pub detail: Option<Arc<DetailView>>,
    /// The column-3 sections (M4-10/W3/W4): the six live-graph view-models.
    pub col3: Arc<crate::col3::Col3>,
    /// The selected project's activity-card data (M55-06); `Default` placeholder.
    pub activity: Arc<crate::col3::ActivityView>,
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
    /// The modal dispatch surface (results / confirm / running), if open.
    pub overlay: Option<Overlay>,
    /// The modal focus ring's cursor — an index into `overlay.controls()`
    /// (ADR-0027); the renderer highlights the focused control.
    pub overlay_focus: usize,
    /// The inline parse error under the command bar, if any.
    pub command_error: Option<crate::command::ParseError>,
    /// The suggest panel's ranked rows (empty unless command mode; M5-10/11).
    pub suggestions: Vec<crate::suggest::Suggestion>,
    /// The highlighted suggestion row.
    pub suggest_highlight: usize,
    /// The panel's fade-in progress (0..=1; ADR-0022).
    pub suggest_fade: f32,
    /// The one-shot startup-sweep reveal progress (0..=1; M55-14). 1.0 = fully
    /// revealed (reduced motion / headless rest here).
    pub sweep: f32,
    /// The transient selection pulse (1.0 → 0.0; M55-14).
    pub select_pulse: f32,
    /// The transient button press-flash (1.0 → 0.0; ADR-0028), keyed by
    /// [`Self::pressed`].
    pub press_pulse: f32,
    /// The button whose press-flash is in flight (keys `press_pulse`).
    pub pressed: Option<crate::hit::Target>,
    /// Feature-row runtime states (M5-12): action results overlaid on the
    /// persisted desired state.
    pub feature_runtime: std::collections::BTreeMap<String, crate::FeatureRuntime>,
}

impl ViewModel {
    /// The press-flash fraction (0.0 at rest) for the button drawn as `target`
    /// (ADR-0028) — the shared way every button draw site asks "am I flashing?".
    #[must_use]
    pub fn button_flash(&self, target: &crate::hit::Target) -> f32 {
        if self.pressed.as_ref() == Some(target) {
            self.press_pulse
        } else {
            0.0
        }
    }

    /// The modal control the focus ring rests on (ADR-0027) — the render side's
    /// view of the focused control, so the focused button draws highlighted.
    #[must_use]
    pub fn focused_overlay_control(&self) -> Option<crate::dispatch::OverlayControl> {
        let controls = self.overlay.as_ref()?.controls();
        controls
            .get(self.overlay_focus.min(controls.len().checked_sub(1)?))
            .copied()
    }
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
    fn quit_key_yields_flow_quit() {
        let mut s = ready(vec![]);
        assert!(matches!(
            update(&mut s, Event::Key(CanopyKey::Quit)),
            Flow::Quit
        ));
    }

    // --- M55-14: the startup sweep + the selection pulse ----------------------

    #[test]
    fn startup_sweep_fires_once_then_settles_and_never_re_fires() {
        let mut s = ready(vec![project("a")]);
        assert!(
            (s.startup_sweep - 1.0).abs() < f32::EPSILON,
            "rests done by default"
        );
        s.arm_startup_sweep();
        assert_eq!(s.startup_sweep, 0.0, "armed to the start");
        assert!(s.anim_active(), "a sweeping frame fast-polls");

        // A chosen dt advances the reveal deterministically.
        assert!(s.advance_anim(STARTUP_SWEEP / 2));
        assert!(
            (s.startup_sweep - 0.5).abs() < 0.02,
            "half-way after half the window"
        );
        assert!(s.advance_anim(STARTUP_SWEEP)); // overshoots → clamps to 1.0
        assert_eq!(s.startup_sweep, 1.0, "settles at the final frame");

        // At rest it is idle and a further advance never re-arms it.
        assert!(!s.anim_active());
        assert!(!s.advance_anim(STARTUP_SWEEP));
        assert_eq!(s.startup_sweep, 1.0, "the one-shot sweep never re-fires");
    }

    #[test]
    fn reduced_motion_shows_the_final_frame_instantly() {
        let mut s = ready(vec![project("a")]);
        s.reduced_motion = true;
        s.arm_startup_sweep();
        assert_eq!(
            s.startup_sweep, 1.0,
            "reduced motion snaps to the settled frame"
        );
        assert!(!s.anim_active());
        // The pulse is likewise skipped.
        s.arm_select_pulse();
        assert_eq!(s.select_pulse, 0.0);
    }

    #[test]
    fn selection_change_arms_a_decaying_pulse() {
        let mut s = ready(vec![project("a"), project("b")]);
        update(&mut s, Event::Key(CanopyKey::Down)); // select b
        assert_eq!(s.select_pulse, 1.0, "a cursor move arms the pulse");
        assert!(s.anim_active());
        assert!(s.advance_anim(PULSE_DECAY / 2));
        assert!(s.select_pulse < 1.0 && s.select_pulse > 0.0, "it decays");
        assert!(s.advance_anim(PULSE_DECAY)); // overshoots → clamps to 0
        assert_eq!(s.select_pulse, 0.0, "decays to rest");
        assert!(!s.anim_active());
    }

    #[test]
    fn a_button_press_flashes_and_re_presses_re_arm() {
        let mut s = ready(vec![project("a")]);
        // A button press arms the flash keyed by its target (ADR-0028).
        let _ = apply_click(&mut s, Target::HeaderHelp);
        assert_eq!(s.press_pulse, 1.0, "a button press arms the flash");
        assert_eq!(s.pressed, Some(Target::HeaderHelp));
        assert!(s.anim_active());
        assert_eq!(s.select_pulse, 0.0, "the selection pulse is untouched");
        // It decays…
        assert!(s.advance_anim(PULSE_DECAY / 2));
        assert!(s.press_pulse < 1.0 && s.press_pulse > 0.0, "it decays");
        // …and a fresh press re-arms it to full (bound to the event, not a change).
        let _ = apply_click(&mut s, Target::HeaderHelp);
        assert_eq!(s.press_pulse, 1.0, "re-pressing the same button re-flashes");
        // Overshoot → rest, and `pressed` clears.
        assert!(s.advance_anim(PULSE_DECAY));
        assert_eq!(s.press_pulse, 0.0, "decays to rest");
        assert_eq!(s.pressed, None);
        assert!(!s.anim_active());
    }

    #[test]
    fn reduced_motion_snaps_the_press_flash() {
        let mut s = ready(vec![project("a")]);
        s.reduced_motion = true;
        let _ = apply_click(&mut s, Target::HeaderHelp);
        assert_eq!(s.press_pulse, 0.0, "reduced motion never flashes");
        assert_eq!(s.pressed, None);
    }

    #[test]
    fn a_row_select_uses_the_selection_pulse_not_the_press_flash() {
        let mut s = ready(vec![project("a"), project("b")]);
        let _ = apply_click(&mut s, Target::ProjectRow(1));
        assert_eq!(s.press_pulse, 0.0, "a row select is not a button press");
        assert!(s.pressed.is_none());
        assert_eq!(s.select_pulse, 1.0, "it arms the selection pulse instead");
    }

    #[test]
    fn sweep_reveal_snapshots_the_full_frame_at_rest() {
        // The settled snapshot (sweep = 1.0) is the full dashboard — every
        // headless/golden render sees the final frame, never a mid-wipe one.
        use crate::render::frame::Frame;
        let mut s = ready(vec![project("a")]);
        let vm = s.snapshot();
        assert!((vm.sweep - 1.0).abs() < f32::EPSILON);
        // Mid-sweep, the lower rows blank (a wipe reveal).
        s.arm_startup_sweep();
        s.advance_anim(STARTUP_SWEEP / 4); // ~25% revealed
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        crate::view::draw(&vm, &mut b, &mut f);
        // A row near the bottom of the body is blank during the wipe.
        let bottom = vm.size.h.saturating_sub(2);
        let row: String = (0..vm.size.w)
            .map(|x| f.get(x, bottom).map_or(' ', |c| c.ch))
            .collect();
        assert_eq!(
            row.trim(),
            "",
            "rows below the reveal line are blank mid-sweep"
        );
    }

    // --- M55-06/07: the activity + metrics feeds ------------------------------

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
    fn metrics_feed_patches_col3_metrics_without_a_tween() {
        use crate::col3::PanelMetrics;
        let mut s = ready(vec![project("a")]);
        let metrics = PanelMetrics {
            capture_per_min: Some(1.5),
            tokens_per_min: Some(600.0),
            projection_bytes: Some(4096),
            recall_per_hour: None,
        };
        update(
            &mut s,
            Event::Feed(FeedUpdate {
                project: "/r/a".into(),
                payload: FeedPayload::Metrics(metrics.clone()),
            }),
        );
        assert_eq!(s.snapshot().col3.metrics, metrics);
        assert!(
            s.col3_anim.is_none(),
            "a metrics-only change never arms a tween"
        );
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
        s.activity = Arc::new(ActivityView::Days(ActivityData::default()));
        update(&mut s, Event::Key(CanopyKey::Down)); // select b
        assert_eq!(
            *s.activity,
            ActivityView::NoProjection,
            "b's card must not show a's activity until b's feed arrives"
        );
    }

    // --- M55-04: the data-plane health dot ------------------------------------

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
    fn snapshot_carries_health_and_version() {
        let mut s = ready(vec![]);
        s.version = "1.2.3".into();
        update(&mut s, Event::Health(HealthEvent::StoreUnreachable));
        let vm = s.snapshot();
        assert_eq!(vm.health, crate::theme::Health::Down);
        assert_eq!(vm.version, "1.2.3");
    }

    // --- M55-10: the editor launch (the one sanctioned spawn) -----------------

    #[test]
    fn e_key_and_button_emit_the_editor_launch_for_the_selected_project() {
        // The `e` key (detail focused) and the repository card's button both
        // resolve to the SELECTED project's path — never process cwd.
        let mut s = ready(vec![project("a"), project("b")]);
        update(&mut s, Event::Key(CanopyKey::Down)); // select b
        s.app.set_focus(Panel::Detail);
        let flow = update(&mut s, Event::Key(CanopyKey::Char('e')));
        match flow {
            Flow::Continue(e) => assert_eq!(e.editor_launch, Some("/r/b".to_string())),
            Flow::Quit => panic!("expected Continue"),
        }
        // The button click (mouse) is the same launch.
        let flow = apply_click(&mut s, Target::EditorButton);
        match flow {
            Flow::Continue(e) => assert_eq!(e.editor_launch, Some("/r/b".to_string())),
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn e_key_only_launches_from_the_detail_column() {
        // Projects-focused `e` is inert; command-mode `e` types into the buffer.
        let mut s = ready(vec![project("a")]);
        assert!(matches!(
            update(&mut s, Event::Key(CanopyKey::Char('e'))),
            Flow::Continue(e) if e.editor_launch.is_none()
        ));
        update(&mut s, Event::Key(CanopyKey::Char('/'))); // command mode
        update(&mut s, Event::Key(CanopyKey::Char('e')));
        assert_eq!(
            s.app.command_line, "e",
            "e types into the palette, never launches"
        );
    }

    #[test]
    fn no_parseable_command_ever_yields_an_editor_launch() {
        // The launcher is structurally OUTSIDE the grammar (ADR-0025): no verb
        // maps to it. Running every catalog verb never emits editor_launch.
        let mut s = ready(vec![project("a")]);
        for word in crate::command::VERB_WORDS {
            if let Ok(_verb) = crate::command::parse(word) {
                let flow = run_line(&mut s, word);
                if let Flow::Continue(e) = flow {
                    assert!(
                        e.editor_launch.is_none(),
                        "the `{word}` verb must never reach the editor launcher"
                    );
                }
                s.app.overlay = None; // clear any overlay the verb opened
            }
        }
    }

    #[test]
    fn header_click_targets_map_to_their_key_equivalents() {
        // HeaderQuit quits; HeaderPalette opens command mode; HeaderHelp opens
        // the help overlay — each the same effect as its key.
        let mut s = ready(vec![project("a")]);
        assert!(matches!(
            apply_click(&mut s, Target::HeaderQuit),
            Flow::Quit
        ));

        let mut s = ready(vec![project("a")]);
        let _ = apply_click(&mut s, Target::HeaderPalette);
        assert_eq!(s.app.mode, Mode::Command);
        assert_eq!(s.app.focus, Panel::Command);

        let mut s = ready(vec![project("a")]);
        let _ = apply_click(&mut s, Target::HeaderHelp);
        assert!(
            matches!(s.app.overlay, Some(Overlay::Results { .. })),
            "the help overlay opened"
        );
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
                        kind: FeatureActionKind::Install,
                        path: "/r/a".into(),
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
                        kind: FeatureActionKind::Reinstall,
                        path: "/r/a".into(),
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
    fn read_only_verb_dispatches_behind_a_running_frame() {
        // `/status` (read-only tier) emits one dispatch with the selected
        // project's repo and opens the Running overlay — no confirm.
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "status".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        match update(&mut s, Event::Key(CanopyKey::Enter)) {
            Flow::Continue(e) => {
                assert_eq!(e.dispatches.len(), 1);
                let (req, repo) = &e.dispatches[0];
                assert_eq!(*req, DispatchRequest::Status);
                assert_eq!(repo, "/r/a");
            }
            Flow::Quit => panic!("expected Continue"),
        }
        assert!(
            matches!(s.app.overlay, Some(Overlay::Running { .. })),
            "the running line shows while the dispatch holds the loop"
        );
    }

    #[test]
    fn mutating_verb_confirms_first_and_stray_keys_are_inert() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "rebuild".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        match update(&mut s, Event::Key(CanopyKey::Enter)) {
            Flow::Continue(e) => assert!(e.dispatches.is_empty(), "confirm first"),
            Flow::Quit => panic!("expected Continue"),
        }
        assert!(matches!(s.app.overlay, Some(Overlay::Confirm { .. })));
        // Stray keys never confirm a mutating modal — a buffered keystroke is
        // harmless, and Space (unlike Enter) never activates the Confirm control
        // (ADR-0027). These keep focus on the default Confirm control.
        for k in [CanopyKey::Char('y'), CanopyKey::Char(' '), CanopyKey::Down] {
            match update(&mut s, Event::Key(k)) {
                Flow::Continue(e) => assert!(e.dispatches.is_empty(), "{k:?} must be inert"),
                Flow::Quit => panic!("{k:?} must not quit"),
            }
            assert!(matches!(s.app.overlay, Some(Overlay::Confirm { .. })));
        }
        // Enter confirms the default-focused Confirm control: Confirm → Running
        // + the dispatch effect.
        match update(&mut s, Event::Key(CanopyKey::Enter)) {
            Flow::Continue(e) => {
                assert_eq!(e.dispatches.len(), 1);
                assert_eq!(e.dispatches[0].0, DispatchRequest::Rebuild);
            }
            Flow::Quit => panic!("expected Continue"),
        }
        assert!(matches!(s.app.overlay, Some(Overlay::Running { .. })));
    }

    #[test]
    fn tab_cycles_the_modal_focus_ring_and_enter_activates_the_focused_control() {
        // Open a mutating confirm (default focus = Confirm).
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "rebuild".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        assert!(matches!(s.app.overlay, Some(Overlay::Confirm { .. })));
        assert_eq!(
            s.app.focused_overlay_control(),
            Some(crate::dispatch::OverlayControl::Confirm),
            "a confirm modal defaults focus to Confirm"
        );
        // Tab → Cancel; Enter now cancels (dismiss, no dispatch).
        update(&mut s, Event::Key(CanopyKey::Tab));
        assert_eq!(
            s.app.focused_overlay_control(),
            Some(crate::dispatch::OverlayControl::Cancel)
        );
        match update(&mut s, Event::Key(CanopyKey::Enter)) {
            Flow::Continue(e) => assert!(e.dispatches.is_empty(), "Cancel dispatches nothing"),
            Flow::Quit => panic!("expected Continue"),
        }
        assert!(
            s.app.overlay.is_none(),
            "activating Cancel dismisses the modal"
        );
    }

    #[test]
    fn shift_tab_wraps_the_ring_to_the_close_control() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "rebuild".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        // Shift-Tab from the default (Confirm) wraps to the last control (Close).
        update(&mut s, Event::Key(CanopyKey::BackTab));
        assert_eq!(
            s.app.focused_overlay_control(),
            Some(crate::dispatch::OverlayControl::Close)
        );
        // Enter on Close dismisses.
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        assert!(s.app.overlay.is_none());
    }

    #[test]
    fn clicking_the_close_x_dismisses_the_modal_and_flashes() {
        let mut s = ready(vec![project("a")]);
        s.app.open_overlay(Overlay::Results {
            title: "t".into(),
            lines: vec!["ok".into()],
            scroll: 0,
            footer: None,
            truncated: false,
        });
        let _ = apply_click(&mut s, Target::OverlayClose);
        assert!(s.app.overlay.is_none(), "the X dismisses the modal");
        assert_eq!(
            s.press_pulse, 1.0,
            "the close X press-flashes like any button"
        );
        assert_eq!(s.pressed, Some(Target::OverlayClose));
    }

    #[test]
    fn rail_buttons_raise_the_same_effects_as_keys_and_flash() {
        // Add → enters AddProject mode (like the `a` key) and press-flashes.
        let mut s = ready(vec![project("a"), project("b")]);
        let _ = apply_click(&mut s, Target::RailAdd);
        assert_eq!(s.app.mode, Mode::AddProject);
        assert_eq!(s.pressed, Some(Target::RailAdd));
        // Remove selected → the same PersistOp the `d` key raises.
        let mut s = ready(vec![project("a"), project("b")]);
        match apply_click(&mut s, Target::RailRemove) {
            Flow::Continue(e) => assert!(matches!(
                e.persist.as_slice(),
                [PersistOp::RemoveProject { .. }]
            )),
            Flow::Quit => panic!("remove is not quit"),
        }
        // Reorder up → ReorderProject { up: true } on the selected path.
        let mut s = ready(vec![project("a"), project("b")]);
        match apply_click(&mut s, Target::RailReorderUp) {
            Flow::Continue(e) => assert!(matches!(
                e.persist.as_slice(),
                [PersistOp::ReorderProject { up: true, .. }]
            )),
            Flow::Quit => panic!("reorder is not quit"),
        }
    }

    #[test]
    fn esc_cancels_a_confirm_dispatching_nothing() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "gc".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        assert!(matches!(s.app.overlay, Some(Overlay::Confirm { .. })));
        match update(&mut s, Event::Key(CanopyKey::Esc)) {
            Flow::Continue(e) => assert!(e.dispatches.is_empty()),
            Flow::Quit => panic!("expected Continue"),
        }
        assert!(s.app.overlay.is_none(), "cancel dismisses");
    }

    #[test]
    fn parse_error_renders_inline_and_keeps_the_buffer() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "statsu".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        let err = s.app.command_error.as_ref().expect("inline error");
        assert_eq!(err.hint, Some("status"), "nearest-verb hint");
        assert_eq!(
            s.app.command_line, "statsu",
            "buffer kept for the underline"
        );
        assert_eq!(s.app.mode, Mode::Command, "still editing");
        // The next edit clears the error.
        update(&mut s, Event::Key(CanopyKey::Backspace));
        assert!(s.app.command_error.is_none());
    }

    #[test]
    fn suggest_fade_is_deterministic_by_dt_and_idles_after_settle() {
        // ADR-0022: progress is a pure function of accumulated dt; after the
        // settle the animator is inert (the fast-poll-only-while-animating
        // idle property extends to the panel).
        let mut a = ready(vec![project("a")]);
        let mut b = ready(vec![project("a")]);
        for s in [&mut a, &mut b] {
            update(s, Event::Key(CanopyKey::Char('/')));
            assert!(s.anim_active(), "entering command mode arms the fade");
        }
        // Two identical dt sequences produce identical progress.
        for s in [&mut a, &mut b] {
            assert!(s.advance_anim(Duration::from_millis(60)));
            assert!(s.advance_anim(Duration::from_millis(60)));
        }
        assert!((a.suggest_fade() - b.suggest_fade()).abs() < f32::EPSILON);
        assert!(a.suggest_fade() < 1.0);
        // Settle: past 180ms the fade is done and the loop idles again.
        assert!(a.advance_anim(Duration::from_millis(120)));
        assert!((a.suggest_fade() - 1.0).abs() < f32::EPSILON);
        assert!(!a.anim_active(), "idle after settle");
        assert!(
            !a.advance_anim(Duration::from_millis(16)),
            "no wasted redraw at rest"
        );
    }

    #[test]
    fn reduced_motion_snaps_the_suggest_fade() {
        let mut s = ready(vec![project("a")]);
        s.reduced_motion = true;
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        assert!(
            (s.suggest_fade() - 1.0).abs() < f32::EPSILON,
            "instant final frame"
        );
        assert!(!s.anim_active());
    }

    #[test]
    fn suggestion_click_inserts_and_never_executes() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "stat".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        // Click the first suggestion row (geometry-independent: drive the
        // reducer's target arm directly through a real hit on the panel).
        let vm = s.snapshot();
        assert!(!vm.suggestions.is_empty());
        let rect = crate::view::suggest_panel_rect(vm.size, vm.suggestions.len());
        assert!(rect.h > 0, "panel present at 80x24? size={:?}", vm.size);
        let flow = update(
            &mut s,
            Event::Mouse(MouseInput {
                kind: MouseKind::Down(MouseButton::Left),
                col: rect.x + 2,
                row: rect.y + 1,
            }),
        );
        match flow {
            Flow::Continue(e) => {
                assert!(e.dispatches.is_empty(), "insert-only: no dispatch");
                assert!(e.persist.is_empty());
            }
            Flow::Quit => panic!("a suggestion click must never quit"),
        }
        assert_eq!(s.app.command_line, "status", "the click inserted the row");
        assert_eq!(s.app.mode, Mode::Command, "still editing");
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
    fn feed_updates_are_full_state_so_delivery_order_converges() {
        // Delivery robustness (user story 14): every feed update carries FULL
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
                    }),
                    breakdown: crate::col3::BreakdownView::NoActivity,
                },
            })
        };
        let mut s = ready(vec![project("a"), project("b")]);
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

    /// Build a Tokens feed for `/r/a` with the given series/total.
    fn tokens_feed(rate: Vec<f64>, total: f64) -> Event {
        use crate::col3::{TokensData, TokensView};
        Event::Feed(FeedUpdate {
            project: "/r/a".into(),
            payload: FeedPayload::Tokens {
                tokens: TokensView::Data(TokensData { rate, total }),
                breakdown: crate::col3::BreakdownView::NoActivity,
            },
        })
    }

    fn tokens_of(v: &ViewModel) -> (Vec<f64>, f64) {
        match &v.col3.tokens {
            crate::col3::TokensView::Data(d) => (d.rate.clone(), d.total),
            crate::col3::TokensView::NoActivity => (vec![], -1.0),
        }
    }

    #[test]
    fn data_to_data_feed_arms_a_tween_that_settles() {
        let mut s = ready(vec![project("a")]);
        // First feed: placeholder→Data → snaps, no tween armed.
        update(&mut s, tokens_feed(vec![1.0, 2.0], 3.0));
        assert!(!s.anim_active(), "the first population snaps");
        // Second feed: Data→Data with a changed series → a tween is armed.
        update(&mut s, tokens_feed(vec![5.0, 6.0], 11.0));
        assert!(s.anim_active(), "a data change arms a tween");
        // At t=0 the series eases from the OLD shape, but the headline total is
        // already the target (headlines never tween — gate-m4 / delivery tests).
        s.advance_anim(Duration::ZERO);
        let (rate0, total0) = tokens_of(&s.snapshot());
        assert_eq!(rate0, vec![1.0, 2.0], "series starts at the old shape");
        assert!(
            (total0 - 11.0).abs() < f64::EPSILON,
            "headline is the target"
        );
        // Advancing past the settle window ends the tween; the final frame is the
        // exact target series.
        assert!(s.advance_anim(Duration::from_millis(400)));
        assert!(!s.anim_active(), "the tween settles and clears");
        let (rate_settled, _) = tokens_of(&s.snapshot());
        assert_eq!(
            rate_settled,
            vec![5.0, 6.0],
            "settled frame equals the target"
        );
    }

    #[test]
    fn reduced_motion_snaps_without_a_tween() {
        let mut s = ready(vec![project("a")]);
        s.reduced_motion = true;
        update(&mut s, tokens_feed(vec![1.0], 1.0));
        update(&mut s, tokens_feed(vec![9.0], 9.0));
        assert!(!s.anim_active(), "reduced_motion never tweens");
        assert_eq!(
            tokens_of(&s.snapshot()).0,
            vec![9.0],
            "shows the target at once"
        );
    }

    #[test]
    fn a_non_animating_feed_keeps_an_in_flight_tween() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, tokens_feed(vec![1.0, 2.0], 3.0)); // placeholder→Data: snaps
        update(&mut s, tokens_feed(vec![5.0, 6.0], 11.0)); // Data→Data: arms a tween
        assert!(s.anim_active());
        // A nic feed snaps (nic is never tweened); it must NOT cancel the tokens
        // tween, and its own value must show immediately.
        let nic = Event::Feed(FeedUpdate {
            project: "/r/a".into(),
            payload: FeedPayload::Nic(crate::col3::NicView::Reading(crate::col3::NicReading {
                pct: 50,
                band: "Thinking",
                model: "m".into(),
                threads: 1,
            })),
        });
        update(&mut s, nic);
        assert!(
            s.anim_active(),
            "an unrelated (snapping) nic update must not cancel the tokens tween"
        );
        match &s.snapshot().col3.nic {
            crate::col3::NicView::Reading(r) => assert_eq!(r.pct, 50, "nic shows at once"),
            other => panic!("expected a reading, got {other:?}"),
        }
    }

    #[test]
    fn advance_anim_is_a_noop_at_rest() {
        let mut s = ready(vec![project("a")]);
        assert!(
            !s.advance_anim(Duration::from_millis(16)),
            "with no tween in flight, advancing is a no-op (no redraw)"
        );
    }

    #[test]
    fn identical_feed_replay_neither_dirties_nor_tweens() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, tokens_feed(vec![3.0], 3.0)); // placeholder→Data, snaps
        s.dirty = false;
        update(&mut s, tokens_feed(vec![3.0], 3.0)); // identical replay
        assert!(!s.dirty, "an identical feed replay must not redraw");
        assert!(!s.anim_active(), "and must not arm a tween");
    }

    fn click(col: u16, row: u16) -> Event {
        Event::Mouse(MouseInput {
            kind: MouseKind::Down(MouseButton::Left),
            col,
            row,
        })
    }

    #[test]
    fn clicking_a_project_row_matches_the_keyboard_selection() {
        // A click on the second project row must produce the same selection and
        // the same bus effects as pressing Down from the top.
        let mut sk = ready(vec![project("a"), project("b"), project("c")]);
        let fk = update(&mut sk, Event::Key(CanopyKey::Down));
        let mut sm = ready(vec![project("a"), project("b"), project("c")]);
        // col1 inner content starts at (1,2); project rows at y=2,3,4.
        let fm = update(&mut sm, click(3, 3));
        assert_eq!(sk.app.selected, 1);
        assert_eq!(sm.app.selected, 1, "the click selected the clicked row");
        match (fk, fm) {
            (Flow::Continue(ek), Flow::Continue(em)) => {
                assert_eq!(ek.data_requests, em.data_requests, "same bus effects");
                assert!(em
                    .data_requests
                    .contains(&DataRequest::SetActive(Some("/r/b".into()))));
            }
            _ => panic!("expected Continue"),
        }
    }

    #[test]
    fn clicking_a_column_focuses_that_panel() {
        let mut s = ready(vec![project("a")]);
        assert_eq!(s.app.focus, Panel::Projects);
        update(&mut s, click(40, 10)); // deep in the detail column
        assert_eq!(s.app.focus, Panel::Detail);
        update(&mut s, click(100, 6)); // deep in the graphs column
        assert_eq!(s.app.focus, Panel::Graphs);
    }

    #[test]
    fn clicking_the_command_bar_opens_command_mode() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, click(10, 39)); // the bottom command row
        assert_eq!(s.app.focus, Panel::Command);
        assert_eq!(s.app.mode, Mode::Command);
    }

    #[test]
    fn a_click_during_the_splash_only_dismisses_it() {
        let mut s = AppState::new(vec![project("a")], Size::new(120, 40)); // fresh splash
        assert!(!s.app.splash.done());
        let f = update(&mut s, click(3, 2));
        assert!(s.app.splash.done(), "the click dismissed the splash");
        assert_eq!(s.app.focus, Panel::Projects, "and was otherwise swallowed");
        assert!(matches!(f, Flow::Continue(e) if e.is_empty()));
    }

    #[test]
    fn scrolling_over_the_graphs_column_scrolls_its_viewport() {
        let mut s = ready(vec![project("a")]);
        let wheel = |kind| {
            Event::Mouse(MouseInput {
                kind,
                col: 100,
                row: 6,
            })
        };
        update(&mut s, wheel(MouseKind::ScrollDown));
        assert_eq!(s.app.scroll.graphs, 1);
        update(&mut s, wheel(MouseKind::ScrollUp));
        assert_eq!(s.app.scroll.graphs, 0);
    }

    #[test]
    fn a_right_click_and_a_move_are_inert() {
        let mut s = ready(vec![project("a"), project("b")]);
        for kind in [
            MouseKind::Down(MouseButton::Right),
            MouseKind::Moved,
            MouseKind::Up(MouseButton::Left),
        ] {
            let f = update(
                &mut s,
                Event::Mouse(MouseInput {
                    kind,
                    col: 3,
                    row: 3,
                }),
            );
            assert!(matches!(f, Flow::Continue(e) if e.is_empty()));
        }
        assert_eq!(s.app.selected, 0, "no inert event moved the selection");
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
        // The unchanged rows are *shared*, not re-allocated per frame.
        assert!(Arc::ptr_eq(&v1.projects, &v2.projects));
        assert!(Arc::ptr_eq(&v1.col3, &v2.col3));
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
}
