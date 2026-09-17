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
//! [`crate::input::on_key`] reducer through `fold_action`.

use std::sync::Arc;
use std::time::Duration;

use crate::col2::DetailView;
use crate::command::Verb;
use crate::dispatch::{DispatchOutcome, DispatchRequest, Overlay};
use crate::hit::{hit_test, ScrollRegion, Target};
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
    /// A per-project update from the data bus: live status for one project.
    ProjectData(ProjectData),
    /// A column-3 feed update from the data bus (the data-plane split's Tokens/Nic/Recall
    /// trio): full section state for one project, applied when it is the
    /// selected one.
    Feed(FeedUpdate),
    /// Read-only Recall Viewer worker response.
    Viewer(crate::recall_viewer::ViewerEvent),
    /// A delegated dispatch completed (the loop ran it synchronously); the
    /// reducer renders the outcome into the results overlay.
    DispatchDone {
        /// The request that ran.
        request: DispatchRequest,
        /// What it produced.
        outcome: DispatchOutcome,
    },
    /// A data-plane health report from the bus worker (the dashboard information architecture).
    /// Emitted on change only; the header dot renders the folded state.
    Health(HealthEvent),
    /// How the bus worker is woken: sent ONCE, first,
    /// right after the worker attached (or failed to attach) its filesystem
    /// watcher. `PollOnly` is the silent degrade that used to be invisible;
    /// the heartbeat tightens on it and the System panel says so.
    WatchMode(WatchMode),
}

/// How the data bus learns that something changed: a native filesystem
/// watcher, or the timeout poll alone (the watcher failed to attach, or the
/// platform has none). The dashboard adapts its idle re-stat cadence to this
/// (`refresh_every_ticks`) instead of degrading silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WatchMode {
    /// A native watcher wakes the worker promptly; the heartbeat is slow.
    #[default]
    Native,
    /// No watcher — the poll timeout is the only wake; the heartbeat is fast.
    PollOnly,
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

/// Fold one health event into the shown `Health`: the latest report wins,
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
/// delivery converges on the fresh-read truth by construction — the
/// delivery-robustness property this design owes.
#[derive(Clone, Debug, PartialEq)]
pub struct FeedUpdate {
    /// The project (canonical path) this feed update is for.
    pub project: String,
    /// Which sections it fills.
    pub payload: FeedPayload,
}

/// The feed kinds (the data-plane split), each patching its dashboard surface.
#[derive(Clone, Debug, PartialEq)]
pub enum FeedPayload {
    /// Token spend + the input|read|output breakdown.
    Tokens {
        /// The token-usage section.
        tokens: crate::col3::TokensView,
        /// The breakdown section.
        breakdown: crate::col3::BreakdownView,
    },
    /// One atomic System-panel snapshot: context, recent activity/storage, and
    /// the shared active-session count always advance together.
    System {
        /// Main-chain context pressure.
        context: crate::col3::NicView,
        /// Five-minute activity, storage, and explicit scopes.
        metrics: crate::col3::PanelMetrics,
        /// Top-level capture sessions currently live.
        active_sessions: crate::col3::ActiveSessionsView,
    },
    /// The Recall savings and per-channel serve-call chart.
    Recall {
        /// Recall savings.
        savings: crate::col3::RecallView<crate::col3::SavingsData>,
        /// Recall serve calls.
        calls: crate::col3::RecallView<crate::col3::CallsData>,
    },
    /// The activity card's trailing-month heat strips. Patches
    /// the column-2 activity card, not column 3.
    Activity(crate::col3::ActivityView),
    /// `rift dispatch`'s window facts as a card.
    ///
    /// Its own feed, not a field of another one, for the reason the four above
    /// are separate: a feed carries **full section state** and coalesces under
    /// its own key, so a dropped or duplicated delivery converges on the
    /// fresh-read truth for this panel alone.
    Dispatch(crate::col3::DispatchView),
    /// `rift recap`'s window facts as a card.
    Recap(crate::col3::RecapView),
}

/// A per-project update from the live data bus. Carries already-scoped,
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
// Built and consumed once per event, never stored or moved through a channel,
// so the size difference costs nothing; boxing `Effects` would put one
// allocation on the idle `Tick` path that the ~0% CPU invariant keeps free.
#[allow(clippy::large_enum_variant)]
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
    /// Fresh-row requests for the data bus.
    pub data_requests: Vec<DataRequest>,
    /// Append-only persistence (projects, layouts, and profiles).
    pub persist: Vec<PersistOp>,
    /// Delegated CLI requests for the injected dispatcher, each
    /// with its resolved target repository (the selected project's path at
    /// emit time — never process cwd).
    pub dispatches: Vec<(DispatchRequest, String)>,
    /// Repository Project Settings lifecycle actions for the injected manager.
    pub project_settings_actions: Vec<crate::ProjectSettingsAction>,
    /// A project path whose saved layout the loop should load and apply:
    /// set on a selection change and at startup. The reducer emits the request;
    /// the loop performs the read through the injected layout loader.
    pub load_layout: Option<String>,
    /// Reload the profile plane after a profile persist op:
    /// names, active id, membership, selection, and arrangement all reload
    /// through the injected profile loader. The reducer only raises the flag.
    pub reload_profiles: bool,
    /// A profile whose member view + the catalogue the loop should load
    /// through the loader seam (the members editor): set
    /// on entry and after every membership op, so the editor's rows always
    /// re-read the store.
    pub load_membership: Option<String>,
    /// A NON-active profile whose theme overrides the loop should load for
    /// the colour picker's swatches: set when the picker opens
    /// for a profile whose overrides the state can't derive purely.
    pub load_picker_theme: Option<String>,
    /// A directory whose immediate children the loop should list through the
    /// injected lister (the destination explorer) — the reducer only *requests*. The reply
    /// lands on `App.explorer` in the same iteration, before the next frame, so
    /// navigation never shows a loading state.
    pub list_dir: Option<std::path::PathBuf>,
}

impl Effects {
    /// `true` if there is nothing to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data_requests.is_empty()
            && self.persist.is_empty()
            && self.dispatches.is_empty()
            && self.project_settings_actions.is_empty()
            && self.load_layout.is_none()
            && !self.reload_profiles
            && self.load_membership.is_none()
            && self.load_picker_theme.is_none()
            && self.list_dir.is_none()
    }
}

/// A scoped read-request the reducer emits for the live data bus. The loop
/// forwards these to the bus, which reads the projection off-thread and feeds the
/// result back as an [`Event::ProjectData`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataRequest {
    /// Make this project the active (foreground) one; the bus prioritizes it and
    /// observes it for changes. `None` clears the active project.
    SetActive(Option<String>),
    /// Re-read this project's data now (by project key — the canonical path).
    Refresh(String),
    /// Compute the Recall feed over this period from now on, and recompute the
    /// active project's now.
    ///
    /// The window is worker state rather than a parameter on `Refresh` because
    /// every *other* wake — a watermark change, a filesystem doorbell, the idle
    /// re-stat — recomputes the feed too, and a window that only survived until
    /// the next such wake would silently snap back to the default while the
    /// button the viewer pressed stayed lit.
    SetRecallWindow(crate::col3::RecallWindow),
    /// A request routed exclusively through the read-only viewer worker.
    Viewer(crate::recall_viewer::ViewerRequest),
}

/// An **insert-only** persistence operation, keyed by the project's canonical
/// `path` (Canopy's stable natural key — the same key status matching uses). There
/// is deliberately no `Update`/`Delete` variant: a removal, relabel, reorder, or
/// change is a new, superseding insert into an append-only log, which upholds the
/// immutable model. The bin resolves `path` → the stored
/// `project_id` when it writes the event, so ids never leak into Canopy. Profiles
/// are keyed by their stable `profile` id, which Canopy *does* hold —
/// names are mutable display data served through the profile loader seam.
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
    /// Record the project's dashboard arrangement (a superseding `layout` event;
    /// per-project layout persistence, profile-scoped per the profile plane). `Some` is an encoded
    /// [`crate::layout_model::LayoutModel`]; `None` is a reset to the default —
    /// a superseding event, never a delete.
    SetLayout {
        /// The profile the arrangement belongs to (all new writes are
        /// profile-scoped; the legacy per-project table is frozen for writes).
        profile: String,
        /// The path of the project the layout belongs to.
        path: String,
        /// The encoded arrangement, or `None` to reset to the default.
        layout: Option<String>,
    },
    /// Mint a new profile: a `created` event, plus explicit
    /// membership `added` events copying the creating (active) profile's
    /// effective project list at creation time.
    CreateProfile {
        /// The validated display name (trimmed, non-empty, ≤ 24, unique).
        name: String,
    },
    /// Duplicate a profile: a full clone of the source's
    /// effective state at clone time — membership, per-project arrangements,
    /// and the `Some` theme slots — as explicit events under a new name.
    /// Never writes `selected` (cloning is not switching).
    DuplicateProfile {
        /// The stable id of the profile to clone.
        source: String,
        /// The validated new display name (trimmed, non-empty, ≤ 24, unique).
        name: String,
    },
    /// Rename a profile (a superseding `renamed` event).
    RenameProfile {
        /// The stable id of the profile to rename.
        profile: String,
        /// The validated new display name.
        name: String,
    },
    /// Remove a profile (a superseding `removed` event, never a row deletion).
    /// Removing the active or the last live profile is refused in the reducer
    /// and again in the store (the profile plane).
    RemoveProfile {
        /// The stable id of the profile to remove.
        profile: String,
    },
    /// Make a profile the one the dashboard shows (a superseding `selected`
    /// event; the latest fold names the active profile).
    SelectProfile {
        /// The stable id of the profile to activate.
        profile: String,
    },
    /// Include a catalogue project in a profile's membership view (this
    /// profile only; `canopy_project_event` stays the registration source of
    /// truth).
    AddProfileProject {
        /// The stable id of the profile whose view gains the project.
        profile: String,
        /// The catalogue project's canonical path.
        path: String,
    },
    /// Hide a project from a profile's membership view (this profile only —
    /// other profiles and the global catalogue are untouched).
    RemoveProfileProject {
        /// The stable id of the profile whose view loses the project.
        profile: String,
        /// The catalogue project's canonical path.
        path: String,
    },
    /// Move a project up (`true`) or down (`false`) within a profile's view (a
    /// superseding `reordered` event; the bin computes the new order keys).
    ReorderProfileProject {
        /// The stable id of the profile whose view is reordered.
        profile: String,
        /// The catalogue project's canonical path.
        path: String,
        /// Move up when `true`, down when `false`.
        up: bool,
    },
    /// Record one theme-slot override for a profile (a superseding event per
    /// (profile, slot) — the runtime surface theme). `colour` is resolved `#RRGGBB` hex; `None`
    /// is a reset to the Rift-dark default — a superseding event, never a
    /// delete. The slot carries [`crate::theme::ThemeSlot`]'s storage spelling.
    SetProfileTheme {
        /// The stable id of the profile the override belongs to.
        profile: String,
        /// The slot key (`background`/`border`/`button`/`graph`/`text`).
        slot: String,
        /// The resolved hex colour, or `None` to reset the slot.
        colour: Option<String>,
    },
    /// Pin — or with `None` unpin — the startup profile: one
    /// superseding latest-wins event. Launch itself only reads the fold.
    SetStartupProfile {
        /// The profile launch should open on, or `None` to follow
        /// last-selected (today's behaviour).
        profile: Option<String>,
    },
}

/// How many idle ticks between background re-stats of the selected project
/// when a native watcher backs the bus — the **status/aging cadence only**,
/// never the metric driver: panel metrics are anchored to events, so
/// time alone cannot move them, and fs wakes drive their recompute). At the
/// 100ms poll cadence this re-reads the foreground project roughly every 60s
/// — ample for the 30-minute `Running` → `Idle` aging (activity falling out of
/// the window surfaces even though no cursor change fires a bus poll).
/// Time-based only; never dirties.
const REFRESH_EVERY_TICKS_WATCHED: u32 = 600;

/// The same cadence when the bus is poll-only: with
/// no watcher the heartbeat IS the wake, so it tightens to ~1.5s rather than
/// leaving the dashboard a minute stale. Adaptive, never silent.
const REFRESH_EVERY_TICKS_POLL_ONLY: u32 = 15;

/// The idle re-stat cadence for the bus's reported wake mode.
const fn refresh_every_ticks(mode: WatchMode) -> u32 {
    match mode {
        WatchMode::Native => REFRESH_EVERY_TICKS_WATCHED,
        WatchMode::PollOnly => REFRESH_EVERY_TICKS_POLL_ONLY,
    }
}

/// How long the one-shot startup sweep runs as the panels first fill after the
/// splash (the animator contract) — a single settle window, then it never re-fires.
const STARTUP_SWEEP: Duration = Duration::from_millis(450);
/// How long a selection/hover pulse takes to decay back to rest.
const PULSE_DECAY: Duration = Duration::from_millis(300);

/// How long the suggest panel's text fades in (the animator contract's shared contract).
const SUGGEST_FADE: Duration = Duration::from_millis(180);

/// How long a column-3 data change eases from its old shape to the new one. Kept
/// short so the graph "catches up" without feeling laggy; the loop only spins
/// fast (and only redraws) for this window, then returns to the ~0% CPU idle.
const COL3_SETTLE: Duration = Duration::from_millis(350);

/// How long a success/info [`Overlay::Notice`] lingers before its `dt`-timed
/// auto-dismiss under the animator contract — a bounded settle, not a timer or
/// thread, so the
/// idle property holds (the loop spins fast only for this window, then rests).
const NOTICE_SETTLE: Duration = Duration::from_millis(2200);

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
    /// The active profile's surface theme: Rift dark plus the
    /// profile's slot overrides. Swapped (dirtying) on profile change and on a
    /// live picker apply; rides the snapshot so the renderer stays pure.
    theme: crate::theme::Theme,
    /// Idle-tick counter driving the background re-stat cadence (never dirties).
    tick: u32,
    /// How the bus is woken, as it last reported (`Native` until told
    /// otherwise — the pre-report cadence is the watched one, exactly as
    /// before the report existed).
    watch_mode: WatchMode,
    /// The bus-computed rich detail for the active project (keyed by its path);
    /// the snapshot shares it when it matches the selection, else a thin fallback.
    detail: Option<Arc<DetailView>>,
    /// The last snapshot's project rows, shared with every [`ViewModel`] until the
    /// underlying projects change (checked by cheap comparison in [`Self::snapshot`],
    /// so a stale row can never be served).
    rows_cache: Arc<Vec<ProjectRow>>,
    /// The last snapshot's layout model, shared with every
    /// [`ViewModel`] until `app.layout` changes — pointer-equal on quiescent
    /// frames, like `rows_cache`.
    layout_cache: Arc<crate::layout_model::LayoutModel>,
    /// The column-3 sections' view-model, filled by the data plane's feeds
    /// `Default` is every section's honest placeholder. This is always the
    /// **target** value; the displayed value is [`Self::snapshot`]'s interpolation
    /// when a tween is in flight.
    col3: Arc<crate::col3::Col3>,
    /// The in-flight column-3 tween, if any (drives the loop's fast-poll window).
    col3_anim: Option<Col3Anim>,
    /// The selected project's activity-card data, filled by the
    /// Activity feed; `Default` is the honest placeholder. Column-2, so it sits
    /// outside the col3 tween machinery (activity snaps; polish may come later).
    activity: Arc<crate::col3::ActivityView>,
    /// The suggest panel's fade-in progress (0..=1, dt-advanced under the
    /// shared animator contract; snaps to 1 under reduced motion / ASCII).
    suggest_fade: f32,
    /// The one-shot startup sweep progress (0..=1). Rests at 1.0 (done);
    /// armed to 0.0 once per launch by [`Self::arm_startup_sweep`], then advances
    /// to 1.0 and **never re-fires**. Reduced motion / headless stays at 1.0.
    startup_sweep: f32,
    /// A transient selection/hover pulse (1.0 → 0.0 decay). Armed on a
    /// selection change; a brief colour lerp acknowledges the cursor.
    select_pulse: f32,
    /// A transient button press-flash (1.0 → 0.0 decay), keyed by
    /// [`Self::pressed`] so the right button lights. Distinct from
    /// `select_pulse`: this is bound to the activation event, not a selection
    /// change, so re-pressing the same button re-arms it.
    press_pulse: f32,
    /// The button whose press-flash is in flight (keys `press_pulse`).
    pressed: Option<Target>,
    /// The open [`Overlay::Notice`]'s remaining settle (1.0 → 0.0).
    /// Armed to 1.0 when a Notice opens, decayed by `dt` in [`Self::advance_anim`],
    /// which dismisses the Notice when it reaches 0 — a bounded auto-dismiss.
    notice_settle: f32,
    /// A project just registered through the add flow, whose first status
    /// read decides whether Project Settings opens on it. Adding a project is
    /// the moment a user expects Rift to look at the machine and offer the
    /// agents it found — but a repository already set up here needs no
    /// wizard, and the add flow cannot know which case it is until the data
    /// plane reads it, so the decision waits for that read (the
    /// `ProjectData` arm of [`update`]) rather than opening blind.
    pub pending_setup_prompt: Option<String>,
    /// Set at a pinned launch whose resolved profile differs from the store's
    /// last-selected fold: launch writes nothing, so the loop
    /// prepends the synchronizing `SelectProfile` to the session's **first**
    /// persist batch — observation never mutates; the first mutation aligns
    /// the fold with what the session shows.
    pub select_sync_pending: bool,
}

impl AppState {
    /// A fresh state for the given projects and size.
    #[must_use]
    pub fn new(projects: Vec<crate::Project>, size: Size) -> Self {
        Self {
            app: App::new(projects),
            size,
            tick: 0,
            watch_mode: WatchMode::Native,
            dirty: true,
            reduced_motion: false,
            version: String::new(),
            health: crate::theme::Health::Ok,
            theme: crate::theme::Theme::rift_dark(),
            detail: None,
            rows_cache: Arc::new(Vec::new()),
            layout_cache: Arc::new(crate::layout_model::LayoutModel::default()),
            col3: Arc::new(crate::col3::Col3::default()),
            col3_anim: None,
            activity: Arc::new(crate::col3::ActivityView::default()),
            suggest_fade: 1.0,
            startup_sweep: 1.0,
            select_pulse: 0.0,
            press_pulse: 0.0,
            pressed: None,
            notice_settle: 0.0,
            pending_setup_prompt: None,
            select_sync_pending: false,
        }
    }

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
        self.app.surface = crate::recall_viewer::Surface::RecallViewer;
        self.app.viewer.pane = crate::recall_viewer::ViewerPane::Checkpoints;
        self.app.viewer.pane_before_command = crate::recall_viewer::ViewerPane::Checkpoints;
        self.app.focus = Panel::Detail;
        self.app.mode = Mode::Normal;
        self.app.command_line.clear();
        self.app.command_error = None;
        let generation = self.app.viewer.start_project(project.clone());
        self.app.viewer.pane = crate::recall_viewer::ViewerPane::Checkpoints;
        self.dirty = true;
        Effects {
            data_requests: vec![DataRequest::Viewer(
                crate::recall_viewer::ViewerRequest::Catalogue {
                    project,
                    generation,
                    target,
                    refresh: false,
                },
            )],
            ..Effects::default()
        }
    }

    /// Return to the dashboard state that was underneath the viewer.
    pub fn close_recall_viewer(&mut self) {
        self.app.surface = crate::recall_viewer::Surface::Dashboard;
        self.app.focus = self.app.viewer.dashboard_focus;
        self.app.mode = Mode::Normal;
        self.app.command_line.clear();
        self.app.command_error = None;
        self.app.viewer.filter_editing = false;
        self.dirty = true;
    }

    /// Open a blocking [`Overlay::Alert`] (an error / warning message)
    /// — the one door error sites route through. Dirties so the modal presents.
    pub fn open_alert(&mut self, message: crate::dispatch::Message) {
        self.app.open_overlay(Overlay::Alert(message));
        self.notice_settle = 0.0;
        self.dirty = true;
    }

    /// Open a brief [`Overlay::Notice`] (a success/info message) and arm
    /// its `dt`-timed auto-dismiss settle. Dirties so the toast presents; the
    /// loop's fast-poll window carries the settle to its bounded close.
    pub fn open_notice(&mut self, message: crate::dispatch::Message) {
        self.app.open_overlay(Overlay::Notice(message));
        self.notice_settle = 1.0;
        self.dirty = true;
    }

    /// Swap the active surface theme: Rift dark plus a profile's
    /// slot overrides, dirtying only on a real change — the set-theme path the
    /// picker's live apply and the profile switch both ride.
    pub fn set_theme(&mut self, overrides: crate::theme::ThemeOverrides) {
        let next = crate::theme::Theme::with_overrides(overrides);
        if next != self.theme {
            self.theme = next;
            self.dirty = true;
        }
    }

    /// The active surface theme (the snapshot copies it each frame).
    #[must_use]
    pub fn theme(&self) -> crate::theme::Theme {
        self.theme
    }

    /// Arm the one-shot startup sweep: the panels reveal as the
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
    /// ~180 ms ramp; reduced motion shows instantly.
    pub fn arm_suggest_fade(&mut self) {
        self.suggest_fade = if self.reduced_motion { 1.0 } else { 0.0 };
    }

    /// The current suggest fade progress (0..=1).
    #[must_use]
    pub fn suggest_fade(&self) -> f32 {
        self.suggest_fade
    }

    /// Apply a project's saved dashboard arrangement — the loop's path
    /// after loading the layout for the newly-active project. Dirties (so the new
    /// grid presents) only on a real change.
    pub fn set_layout(&mut self, layout: crate::layout_model::LayoutModel) {
        if self.app.layout != layout {
            self.app.layout = layout;
            self.dirty = true;
        }
    }

    /// Replace the column-3 sections' view-model (the graph feeds' apply path).
    ///
    /// An identical replay is a no-op (delivery robustness). Otherwise the new
    /// value becomes the target and — unless `reduced_motion` or the change is a
    /// pure placeholder↔data transition — a tween is armed from the prior value,
    /// so the graphs ease to the new shape over `COL3_SETTLE`. Headlines and
    /// variants are never tweened (see `crate::col3::interpolate`).
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
        // The suggest panel's fade rides the same dt-advance: the
        // loop's settle-window fast poll carries the frames; at 1.0 it is a
        // no-op and never redraws (the idle property).
        if self.app.mode == Mode::Command && self.suggest_fade < 1.0 {
            let step = dt.as_secs_f32() / SUGGEST_FADE.as_secs_f32();
            self.suggest_fade = (self.suggest_fade + step).min(1.0);
            active = true;
        }
        // The one-shot startup sweep: advances to 1.0 then rests
        // forever (it never re-arms, so it can't re-fire).
        if self.startup_sweep < 1.0 {
            let step = dt.as_secs_f32() / STARTUP_SWEEP.as_secs_f32();
            self.startup_sweep = (self.startup_sweep + step).min(1.0);
            active = true;
        }
        // The selection pulse decays to rest.
        if self.select_pulse > 0.0 {
            let step = dt.as_secs_f32() / PULSE_DECAY.as_secs_f32();
            self.select_pulse = (self.select_pulse - step).max(0.0);
            active = true;
        }
        // The button press-flash decays to rest — same shape as the
        // selection pulse, keyed by `pressed`.
        if self.press_pulse > 0.0 {
            let step = dt.as_secs_f32() / PULSE_DECAY.as_secs_f32();
            self.press_pulse = (self.press_pulse - step).max(0.0);
            if self.press_pulse <= 0.0 {
                self.pressed = None;
            }
            active = true;
        }
        // A Notice auto-dismisses by `dt`: its settle decays and, at 0,
        // the Notice closes itself — a bounded settle, never a timer/thread. A
        // Notice dismissed early (X/Esc) drops out here (the overlay is no longer
        // a Notice), so the settle can't keep the loop awake past its close.
        if matches!(self.app.overlay, Some(Overlay::Notice(_))) {
            let step = dt.as_secs_f32() / NOTICE_SETTLE.as_secs_f32();
            self.notice_settle = (self.notice_settle - step).max(0.0);
            if self.notice_settle <= 0.0 {
                self.app.overlay = None;
                self.app.overlay_focus = 0;
                self.app.maybe_raise_wizard(); // an empty registry keeps its wizard
            }
            active = true;
        } else {
            self.notice_settle = 0.0;
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
            || (matches!(self.app.overlay, Some(Overlay::Notice(_))) && self.notice_settle > 0.0)
    }

    /// Arm a selection/hover pulse: a brief colour lerp acknowledges
    /// the cursor. Reduced motion skips it (rests at 0).
    fn arm_select_pulse(&mut self) {
        if !self.reduced_motion {
            self.select_pulse = 1.0;
        }
    }

    /// Arm a button press-flash, keyed by `target`: a brief colour
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
        // Rebuild the shared layout only when it actually changed (like rows).
        if *self.layout_cache != self.app.layout {
            self.layout_cache = Arc::new(self.app.layout.clone());
        }
        ViewModel {
            size: self.size,
            focus: self.app.focus,
            mode: self.app.mode,
            surface: self.app.surface,
            viewer: self.app.viewer.clone(),
            version: self.version.clone(),
            health: self.health,
            projects: Arc::clone(&self.rows_cache),
            selected: self.app.selected,
            detail,
            col3: self.displayed_col3(),
            activity: Arc::clone(&self.activity),
            detail_scroll: self.app.scroll.detail,
            graphs_scroll: self.app.scroll.graphs,
            command_line: self.app.command_line.clone(),
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
            layout: Arc::clone(&self.layout_cache),
            editor: self.app.editor,
            profile_name: self.app.active_profile_name().map(String::from),
            profiles: self.app.profiles.clone(),
            active_profile: self.app.active_profile.clone(),
            startup_profile: self.app.startup_profile.clone(),
            profile_menu: self.app.profile_menu.clone(),
            theme: self.theme,
            colour_picker: self.app.colour_picker.clone(),
            explorer: self.app.explorer.clone(),
            project_settings: self.app.project_settings.clone(),
        }
    }
}

/// The **pure** reducer: events in, next state + [`Effects`] out. No I/O — every
/// side effect is returned, never performed here (the loop performs them).
#[allow(clippy::too_many_lines)] // one arm per event in the central pure reducer
pub fn update(s: &mut AppState, ev: Event) -> Flow {
    match ev {
        Event::Resize(size) => {
            s.size = size;
            if s.app.surface == crate::recall_viewer::Surface::RecallViewer {
                clamp_viewer_scrolls(&mut s.app, size);
            } else {
                s.app.clamp_scrolls(size);
            }
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
            if s.app.surface == crate::recall_viewer::Surface::RecallViewer {
                if s.tick.is_multiple_of(refresh_every_ticks(s.watch_mode)) {
                    if let Some(project) = s.app.viewer.project.clone() {
                        s.app.viewer.catalogue_generation =
                            s.app.viewer.catalogue_generation.wrapping_add(1).max(1);
                        return Flow::Continue(Effects {
                            data_requests: vec![DataRequest::Viewer(
                                crate::recall_viewer::ViewerRequest::Catalogue {
                                    project,
                                    generation: s.app.viewer.catalogue_generation,
                                    target: None,
                                    refresh: true,
                                },
                            )],
                            ..Effects::default()
                        });
                    }
                }
                return Flow::Continue(Effects::default());
            }
            if s.app.mode == Mode::ProjectSettings && s.tick.is_multiple_of(10) {
                if let Some(path) = s.app.selected_project().map(|p| p.path.clone()) {
                    return Flow::Continue(Effects {
                        data_requests: vec![DataRequest::Refresh(path)],
                        ..Effects::default()
                    });
                }
            }
            if s.tick.is_multiple_of(refresh_every_ticks(s.watch_mode)) {
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
            // The add flow's deferred decision: the first read of a project
            // just registered sets it up — `rift init` with the detected
            // default plan, shown step by step in the progress dialog — when
            // there is a setup to perform: never initialized here, or
            // configured by the repository but not set up on this machine.
            // It stays quiet for a project that is already set up or is not
            // a repository at all. Adding a project IS initializing it;
            // Project Settings stays the surface for changing the choices
            // afterwards, not a confirm step in the way of the first one.
            if s.pending_setup_prompt.as_deref() == Some(pd.project.as_str()) {
                s.pending_setup_prompt = None;
                if pd.status.repository_card_only()
                    && pd.status != ProjectStatus::NotRepository
                    && s.app.mode == Mode::Normal
                {
                    if matches!(s.app.overlay, Some(Overlay::Notice(_))) {
                        s.app.overlay = None;
                        s.app.overlay_focus = 0;
                        s.notice_settle = 0.0;
                    }
                    s.app.select_path(&pd.project);
                    s.dirty = true;
                    return Flow::Continue(Effects {
                        project_settings_actions: vec![crate::ProjectSettingsAction {
                            path: pd.project.clone(),
                            kind: crate::ProjectSettingsActionKind::Init,
                        }],
                        ..Effects::default()
                    });
                }
            }
            if s.app.mode == Mode::ProjectSettings {
                if let Some(detail) = pd.detail.as_ref() {
                    s.app
                        .project_settings
                        .poll((*detail.project_settings).clone());
                    s.dirty = true;
                }
            }
            Flow::Continue(Effects::default())
        }
        Event::Feed(update) => {
            apply_feed(s, update);
            Flow::Continue(Effects::default())
        }
        Event::Viewer(event) => Flow::Continue(apply_viewer_event(s, event)),
        Event::Key(k) => {
            if s.app.surface == crate::recall_viewer::Surface::RecallViewer
                && s.app.overlay.is_none()
                && (s.app.mode == Mode::Normal
                    || s.app.mode == Mode::Command
                    || s.app.viewer.filter_editing)
            {
                return on_viewer_key(s, k);
            }
            let before = s.app.selected_project().map(|p| p.path.clone());
            let mode_before = s.app.mode;
            // Keyboard-activation parity (the press-flash rule): Enter/Space on a focused
            // modal button flashes it exactly like a click. Captured *before*
            // `on_key`, which may dismiss the modal.
            let modal_flash = modal_activation_flash(&s.app, k);
            let action = on_key(&mut s.app, k);
            if let Some(t) = modal_flash {
                s.arm_press_pulse(t);
            }
            if mode_before != Mode::Command && s.app.mode == Mode::Command {
                // Entering command mode opens the suggest panel: arm its fade
                // (the animator contract; reduced motion / ASCII snap to the final frame).
                s.arm_suggest_fade();
                s.app.suggest_highlight = 0;
            }
            s.dirty = true; // any key may change selection/mode/buffer
            if s.app.surface == crate::recall_viewer::Surface::RecallViewer {
                clamp_viewer_scrolls(&mut s.app, s.size);
            } else {
                s.app.clamp_scrolls(s.size); // a scroll key can never pass the end
            }
            // Remove/reorder target the *selected* project — resolve it here (the
            // key handler stays free of paths). No selection ⇒ inert.
            let sel = s.app.selected_project().map(|p| p.path.clone());
            let mut flow = match action {
                Action::RemoveProject => {
                    let op = sel.map(|path| remove_project_op(&s.app, path));
                    persist_flow(op)
                }
                Action::ReorderProject(up) => {
                    let op = sel.map(|path| reorder_project_op(&s.app, path, up));
                    persist_flow(op)
                }
                Action::OpenProjectSettings => open_project_settings(s),
                Action::OpenRecallViewer => Flow::Continue(s.open_recall_viewer(None)),
                Action::OpenWorktreeAxis => open_worktree_axis(s),
                // The keyboard twin of either savings chip, through the SAME
                // opener — so the two doors cannot drift apart.
                Action::OpenSavingsBreakdown => open_savings_breakdown(s),
                // The keyboard twin of a period button, through the SAME act —
                // and it arms the same press flash a click does, so the two
                // paths are indistinguishable on screen as well as in state.
                Action::SetRecallWindow(window) => {
                    s.arm_press_pulse(Target::RecallWindow(window));
                    select_recall_window(s, window)
                }
                Action::ToggleRecallUnit => {
                    s.arm_press_pulse(Target::RecallUnitToggle);
                    toggle_recall_unit(s)
                }
                Action::AdoptWorktree => open_adopt_confirm(s),
                Action::ApplyProjectSettings(action) => Flow::Continue(Effects {
                    project_settings_actions: vec![action],
                    ..Effects::default()
                }),
                Action::RunLine(buf) => run_line(s, &buf),
                Action::ConfirmDispatch => confirm_dispatch(s),
                other => profile_or_fold_action(s, other),
            };
            // When the selection moves, ask the bus for the new project's data.
            if let Flow::Continue(eff) = &mut flow {
                if s.app.surface != crate::recall_viewer::Surface::RecallViewer {
                    selection_change_effects(s, &before, eff);
                }
                // Leaving the panel editor persists the arrangement.
                if mode_before == Mode::PanelEditor && s.app.mode != Mode::PanelEditor {
                    if let Some(op) = layout_persist_op(&s.app) {
                        eff.persist.push(op);
                    }
                }
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
        Event::WatchMode(mode) => {
            // Adopt the reported wake mode; dirty exactly once per CHANGE (a
            // repeated report never forces a redraw). The cadence reads it on
            // the next tick; the System panel row rides the col-3 model.
            if s.watch_mode != mode {
                s.watch_mode = mode;
                let mut col3 = (*s.col3).clone();
                col3.watch = mode;
                s.set_col3(col3); // dirties; the System panel row rides the model
                s.dirty = true;
            }
            Flow::Continue(Effects::default())
        }
        Event::DispatchDone { request, outcome } => {
            if outcome.code == 0 {
                // Bumped only on successful dispatch.
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

#[allow(clippy::too_many_lines)] // one arm per viewer key in the central pure reducer
fn on_viewer_key(s: &mut AppState, key: CanopyKey) -> Flow {
    use crate::recall_viewer::{
        timeline_item_at, timeline_line_count, Surface, ViewerPane, ViewerRequest,
    };

    // Command editing keeps the existing closed parser/suggestion behavior,
    // but its completion is folded through the viewer-aware allowlist in
    // `run_line` below.
    if s.app.mode == Mode::Command {
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
        return match action {
            Action::RunLine(line) => run_line(s, &line),
            Action::Quit => Flow::Quit,
            _ => Flow::Continue(Effects::default()),
        };
    }

    // The Filter chip is a separate session-only editor. Every edit advances
    // the generation, immediately recomputes metadata matches, and asks the
    // worker to scan transcript excerpts; older scans become stale/cancelled.
    if s.app.viewer.filter_editing {
        let mut changed = false;
        match key {
            CanopyKey::Char(character)
                if s.app.viewer.filter.chars().count() < crate::command::MAX_INPUT_LEN =>
            {
                s.app.viewer.filter.push(character);
                changed = true;
            }
            CanopyKey::Backspace => {
                changed = s.app.viewer.filter.pop().is_some();
            }
            CanopyKey::Enter | CanopyKey::Esc => {
                s.app.viewer.filter_editing = false;
            }
            CanopyKey::Quit => {
                s.app.quit = true;
                return Flow::Quit;
            }
            _ => {}
        }
        s.dirty = true;
        if changed {
            let generation = s.app.viewer.changed_filter();
            let mut effects = viewer_selection_effects(s, true);
            if let Some(project) = s.app.viewer.project.clone() {
                effects
                    .data_requests
                    .push(DataRequest::Viewer(ViewerRequest::FilterScan {
                        project,
                        generation,
                        query: s.app.viewer.filter.clone(),
                    }));
            }
            return Flow::Continue(effects);
        }
        return Flow::Continue(Effects::default());
    }

    let mut effects = Effects::default();
    match key {
        CanopyKey::Esc | CanopyKey::Char('r') => {
            s.close_recall_viewer();
            return Flow::Continue(effects);
        }
        CanopyKey::Char('q') | CanopyKey::Quit => {
            s.app.quit = true;
            return Flow::Quit;
        }
        CanopyKey::Char('/') => {
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
        CanopyKey::Char('f') => {
            s.app.viewer.begin_filter_editing();
        }
        CanopyKey::Char('F') => {
            s.app.viewer.full_session = !s.app.viewer.full_session;
            clamp_viewer_timeline(&mut s.app, s.size);
        }
        CanopyKey::Char('L') => {
            if s.app.viewer.select_latest() {
                effects = viewer_selection_effects(s, true);
            }
        }
        CanopyKey::Char('R') => {
            if let Some(project) = s.app.viewer.project.clone() {
                s.app.viewer.catalogue_generation =
                    s.app.viewer.catalogue_generation.wrapping_add(1).max(1);
                s.app.viewer.catalogue_state = crate::recall_viewer::ViewerLoadState::Loading;
                effects
                    .data_requests
                    .push(DataRequest::Viewer(ViewerRequest::Catalogue {
                        project,
                        generation: s.app.viewer.catalogue_generation,
                        target: None,
                        refresh: true,
                    }));
            }
        }
        CanopyKey::Tab => {
            s.app.viewer.pane = s.app.viewer.pane.cycle(true);
            sync_viewer_focus(&mut s.app);
        }
        CanopyKey::BackTab => {
            s.app.viewer.pane = s.app.viewer.pane.cycle(false);
            sync_viewer_focus(&mut s.app);
        }
        CanopyKey::Left => {
            s.app.viewer.pane = match s.app.viewer.pane {
                ViewerPane::Projects | ViewerPane::Checkpoints => ViewerPane::Projects,
                ViewerPane::Timeline | ViewerPane::Command => ViewerPane::Checkpoints,
            };
            sync_viewer_focus(&mut s.app);
        }
        CanopyKey::Right => {
            s.app.viewer.pane = match s.app.viewer.pane {
                ViewerPane::Projects => ViewerPane::Checkpoints,
                ViewerPane::Checkpoints | ViewerPane::Timeline | ViewerPane::Command => {
                    ViewerPane::Timeline
                }
            };
            sync_viewer_focus(&mut s.app);
        }
        CanopyKey::Char('j') | CanopyKey::Down => {
            effects = move_viewer(s, true, false);
        }
        CanopyKey::Char('k') | CanopyKey::Up => {
            effects = move_viewer(s, false, false);
        }
        CanopyKey::PageDown => {
            effects = move_viewer(s, true, true);
        }
        CanopyKey::PageUp => {
            effects = move_viewer(s, false, true);
        }
        CanopyKey::Home => match s.app.viewer.pane {
            ViewerPane::Checkpoints => {
                if s.app.viewer.checkpoint_edge(false) {
                    effects = viewer_selection_effects(s, true);
                }
            }
            ViewerPane::Timeline => {
                s.app.viewer.timeline_cursor = 0;
                s.app.viewer.timeline_scroll = 0;
            }
            _ => {}
        },
        CanopyKey::End => match s.app.viewer.pane {
            ViewerPane::Checkpoints => {
                if s.app.viewer.checkpoint_edge(true) {
                    effects = viewer_selection_effects(s, true);
                }
            }
            ViewerPane::Timeline => {
                let end = timeline_line_count(&s.app.viewer).saturating_sub(1);
                s.app.viewer.timeline_cursor = end;
                clamp_viewer_timeline(&mut s.app, s.size);
            }
            _ => {}
        },
        CanopyKey::Enter | CanopyKey::Char(' ') => match s.app.viewer.pane {
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
        },
        _ => {}
    }
    debug_assert_eq!(s.app.surface, Surface::RecallViewer);
    clamp_viewer_scrolls(&mut s.app, s.size);
    s.dirty = true;
    Flow::Continue(effects)
}

fn sync_viewer_focus(app: &mut App) {
    app.focus = match app.viewer.pane {
        crate::recall_viewer::ViewerPane::Projects => Panel::Projects,
        crate::recall_viewer::ViewerPane::Checkpoints => Panel::Detail,
        crate::recall_viewer::ViewerPane::Timeline => Panel::Graphs,
        crate::recall_viewer::ViewerPane::Command => Panel::Command,
    };
}

fn move_viewer(s: &mut AppState, down: bool, page: bool) -> Effects {
    use crate::recall_viewer::{timeline_line_count, ViewerPane, ViewerRequest};
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
            let Some(project) = s.app.selected_project().map(|project| project.path.clone()) else {
                return Effects::default();
            };
            let generation = s.app.viewer.start_project(project.clone());
            s.app.viewer.pane = ViewerPane::Projects;
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
        ViewerPane::Checkpoints => {
            if s.app.viewer.move_checkpoint(down, page) {
                viewer_selection_effects(s, true)
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

fn viewer_selection_effects(s: &mut AppState, include_excerpts: bool) -> Effects {
    use crate::recall_viewer::ViewerRequest;
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
    if include_excerpts {
        let start = s.app.viewer.selected.saturating_sub(4);
        let end = (s.app.viewer.selected + 12).min(s.app.viewer.visible.len());
        let checkpoint_ids = s
            .app
            .viewer
            .visible
            .iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .filter_map(|index| s.app.viewer.catalogue.get(*index))
            .filter(|row| row.excerpt.is_none())
            .map(|row| row.checkpoint_id.clone())
            .collect::<Vec<_>>();
        if !checkpoint_ids.is_empty() {
            requests.push(DataRequest::Viewer(ViewerRequest::VisibleExcerpts {
                project,
                generation: s.app.viewer.catalogue_generation,
                checkpoint_ids,
            }));
        }
    }
    Effects {
        data_requests: requests,
        ..Effects::default()
    }
}

fn clamp_viewer_scrolls(app: &mut App, size: Size) {
    clamp_viewer_checkpoint(app, size);
    clamp_viewer_timeline(app, size);
}

fn clamp_viewer_checkpoint(app: &mut App, size: Size) {
    let rect = crate::recall_viewer::geometry(size, app.viewer.pane).checkpoints;
    let header = if app.viewer.filter_progress.is_some() || app.viewer.new_count > 0 {
        4
    } else {
        3
    };
    let rows = usize::from(rect.h.saturating_sub(header))
        / usize::from(crate::recall_viewer::CHECKPOINT_ROW_HEIGHT);
    let rows = rows.max(1);
    let max = app.viewer.visible.len().saturating_sub(rows);
    app.viewer.checkpoint_scroll = app.viewer.checkpoint_scroll.min(max);
    if app.viewer.selected < app.viewer.checkpoint_scroll {
        app.viewer.checkpoint_scroll = app.viewer.selected;
    } else if app.viewer.selected >= app.viewer.checkpoint_scroll.saturating_add(rows) {
        app.viewer.checkpoint_scroll = app.viewer.selected + 1 - rows;
    }
}

fn clamp_viewer_timeline(app: &mut App, size: Size) {
    let rect = crate::recall_viewer::geometry(size, app.viewer.pane).timeline;
    let visible = usize::from(crate::recall_viewer::timeline_controls(rect).content.h).max(1);
    let len = crate::recall_viewer::timeline_line_count(&app.viewer);
    app.viewer.timeline_cursor = app.viewer.timeline_cursor.min(len.saturating_sub(1));
    app.viewer.timeline_scroll = app.viewer.timeline_scroll.min(len.saturating_sub(visible));
    if app.viewer.timeline_cursor < app.viewer.timeline_scroll {
        app.viewer.timeline_scroll = app.viewer.timeline_cursor;
    } else if app.viewer.timeline_cursor >= app.viewer.timeline_scroll.saturating_add(visible) {
        app.viewer.timeline_scroll = app.viewer.timeline_cursor + 1 - visible;
    }
}

/// Fold a generation-tagged viewer response. Project and generation must both
/// match; otherwise the response is stale and is dropped without dirtying.
fn apply_viewer_event(s: &mut AppState, event: crate::recall_viewer::ViewerEvent) -> Effects {
    use crate::recall_viewer::{ViewerErrorArea, ViewerEvent, ViewerLoadState};
    match event {
        ViewerEvent::Catalogue {
            project,
            generation,
            rows,
            selected,
            notice,
            complete,
        } if s.app.viewer.project.as_deref() == Some(project.as_str())
            && s.app.viewer.catalogue_generation == generation =>
        {
            s.app
                .viewer
                .apply_catalogue(rows, selected.as_deref(), notice, complete);
            s.dirty = true;
            return viewer_selection_effects(s, true);
        }
        ViewerEvent::Detail {
            project,
            generation,
            detail,
            complete,
        } if s.app.viewer.project.as_deref() == Some(project.as_str())
            && s.app.viewer.detail_generation == generation =>
        {
            s.app.viewer.apply_detail(detail, complete);
            s.dirty = true;
        }
        ViewerEvent::FilterProgress {
            project,
            generation,
            matching_ids,
            progress,
        } if s.app.viewer.project.as_deref() == Some(project.as_str())
            && s.app.viewer.filter_generation == generation =>
        {
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
            if before != after {
                return viewer_selection_effects(s, true);
            }
        }
        ViewerEvent::Error {
            project,
            generation,
            area,
            message,
        } if s.app.viewer.project.as_deref() == Some(project.as_str()) => {
            let applies = match area {
                ViewerErrorArea::Catalogue => s.app.viewer.catalogue_generation == generation,
                ViewerErrorArea::Detail => s.app.viewer.detail_generation == generation,
                ViewerErrorArea::Filter => s.app.viewer.filter_generation == generation,
            };
            if !applies {
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
        }
        _ => {}
    }
    Effects::default()
}

/// After a possible selection move, reset the per-project sections, ask the bus
/// for the newly-selected project's data, and set
/// `eff.load_layout` so the loop restores the new project's saved arrangement
/// (per-project layout persistence). A no-op when the selection did not change. Shared by the keyboard
/// and mouse paths so a click and an arrow key are behaviorally identical.
fn selection_change_effects(s: &mut AppState, before: &Option<String>, eff: &mut Effects) {
    let after = s.app.selected_project().map(|p| p.path.clone());
    if *before == after {
        return;
    }
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
    if let Some(path) = after {
        eff.data_requests.push(DataRequest::Refresh(path.clone()));
        eff.load_layout = Some(path);
    }
}

/// The `SetLayout` persist op for the selected project's current arrangement
/// (scoped to the active profile) — `None` layout when
/// it equals the default (a reset). `None` when no project is selected.
fn layout_persist_op(app: &App) -> Option<PersistOp> {
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

/// Handle a mouse event by hit-testing it against the current view and mapping
/// the target onto the **same** state changes the keyboard produces. Only left
/// presses and the scroll wheel act in normal use; moves/drags/releases are
/// ignored (no per-move work, keeping the ~0% CPU idle). The **one** exception
/// is panel-editor mode: while it holds a live drag, `Drag`/`Up` move and
/// drop the grabbed unit — scoped so normal-mode idle is byte-identical. While
/// the splash is up, any click dismisses it and is otherwise swallowed.
fn on_mouse(s: &mut AppState, m: MouseInput) -> Flow {
    if !s.app.splash.done() {
        s.app.splash.skip();
        return Flow::Continue(Effects::default());
    }
    let editing_drag = s.app.mode == Mode::PanelEditor && s.app.editor.drag.is_some();
    // The snapshot is taken PER ACTING ARM, never up front: a `Moved` event
    // must do no per-event work at all (the ~0% idle invariant this function's
    // doc comment promises), and the snapshot now carries the destination
    // explorer's listing — up to `MAX_ENTRIES` owned names.
    match m.kind {
        MouseKind::Down(MouseButton::Left) => {
            let vm = s.snapshot();
            match hit_test(&vm, m.col, m.row) {
                Some(target) => apply_click(s, target),
                None => Flow::Continue(Effects::default()),
            }
        }
        // The scoped drag exception: only while a pickup/divider drag is live.
        MouseKind::Drag(MouseButton::Left) if editing_drag => {
            let vm = s.snapshot();
            drag_update(s, &vm, m)
        }
        MouseKind::Up(MouseButton::Left) if editing_drag => drop_drag(s),
        MouseKind::ScrollUp | MouseKind::ScrollDown => {
            let vm = s.snapshot();
            let down = matches!(m.kind, MouseKind::ScrollDown);
            apply_scroll(s, &vm, m.col, m.row, down)
        }
        // Right/middle presses, moves, and drags/releases outside an edit-mode
        // drag don't drive the UI (the idle invariant holds in normal mode).
        _ => Flow::Continue(Effects::default()),
    }
}

/// Advance a live editor drag toward the pointer: a unit drag moves
/// its hover cursor to the grip under the pointer; a divider drag resizes one
/// step toward the pointer. Dirties only on a real change.
fn drag_update(s: &mut AppState, vm: &ViewModel, m: MouseInput) -> Flow {
    match s.app.editor.drag {
        Some(crate::editor::Drag::Unit(_)) => {
            if let Some(Target::GripBar(h)) = hit_test(vm, m.col, m.row) {
                if s.app.editor.cursor != h {
                    s.app.editor.cursor = h;
                    s.dirty = true;
                }
            }
        }
        Some(crate::editor::Drag::Divider(i)) => {
            // Resize toward the pointer: compare it to the divider's current x.
            let body = crate::layout::Rect {
                x: 0,
                y: 1,
                w: s.size.w,
                h: s.size.h.saturating_sub(1),
            };
            let sl = crate::layout_model::solve_model(&s.app.layout, body);
            if let Some(colr) = sl.columns.get(i) {
                let divider_x = colr.rect.x + colr.rect.w.saturating_sub(1);
                let cur = crate::editor::Cursor { col: i, unit: 0 };
                let saved = s.app.editor.cursor;
                s.app.editor.cursor = cur;
                if m.col > divider_x {
                    crate::editor::resize(&s.app.editor, &mut s.app.layout, true);
                    s.dirty = true;
                } else if m.col < divider_x {
                    crate::editor::resize(&s.app.editor, &mut s.app.layout, false);
                    s.dirty = true;
                }
                s.app.editor.cursor = saved;
                s.app.editor.cursor.col = i;
            }
        }
        None => {}
    }
    Flow::Continue(Effects::default())
}

/// Release a live editor drag: a unit drag drops the grabbed unit at
/// its hover cursor (conserving the multiset); a divider drag just ends. Clears
/// the drag and the pickup. The arrangement is persisted when the panel editor
/// is left, not on every drop.
fn drop_drag(s: &mut AppState) -> Flow {
    if let Some(crate::editor::Drag::Unit(src)) = s.app.editor.drag {
        let dst = s.app.editor.cursor;
        s.app.editor.cursor = crate::editor::drop_unit(&mut s.app.layout, src, dst.col, dst.unit);
    }
    s.app.editor.picked = false;
    s.app.editor.drag = None;
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// Apply a left-click target: focus panels, select a project, flip a toggle, or
/// open the command bar — each the same mutation as the equivalent keypress.
/// The modal button a keyboard activation (Enter/Space) would flash,
/// mirroring `on_key_overlay`'s activation rule — Space never fires the mutating
/// Confirm, and a focused Body/none flashes nothing.
fn modal_activation_flash(app: &App, key: crate::input::CanopyKey) -> Option<Target> {
    use crate::dispatch::OverlayControl;
    use crate::input::CanopyKey;
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

#[allow(clippy::too_many_lines)] // one arm per click target — a dispatch table
fn apply_click(s: &mut AppState, target: Target) -> Flow {
    if s.app.mode == Mode::ProjectSettings && s.app.project_settings.running {
        return Flow::Continue(Effects::default());
    }
    // Acknowledge a button press with the flash — keyed by target so
    // the right button lights; a repeat press re-arms. Selection/focus/body
    // targets don't flash (the rail row has its own selection pulse).
    if target.is_button() {
        s.arm_press_pulse(target.clone());
    }
    match target {
        Target::PanelFocus(panel) => {
            if s.app.surface == crate::recall_viewer::Surface::RecallViewer {
                s.app.viewer.pane = match panel {
                    Panel::Projects => crate::recall_viewer::ViewerPane::Projects,
                    Panel::Detail => crate::recall_viewer::ViewerPane::Checkpoints,
                    Panel::Graphs => crate::recall_viewer::ViewerPane::Timeline,
                    Panel::Command => crate::recall_viewer::ViewerPane::Command,
                };
                sync_viewer_focus(&mut s.app);
            } else {
                s.app.set_focus(panel);
            }
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::CommandBar => {
            if s.app.surface == crate::recall_viewer::Surface::RecallViewer {
                if s.app.viewer.pane != crate::recall_viewer::ViewerPane::Command {
                    s.app.viewer.pane_before_command = s.app.viewer.pane;
                }
                s.app.viewer.pane = crate::recall_viewer::ViewerPane::Command;
                s.app.focus = Panel::Command;
            } else {
                s.app.set_focus(Panel::Command);
            }
            s.app.mode = Mode::Command;
            s.app.command_line.clear();
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::ProjectRow(i) => {
            if s.app.surface == crate::recall_viewer::Surface::RecallViewer {
                s.app.viewer.pane = crate::recall_viewer::ViewerPane::Projects;
                s.app.focus = Panel::Projects;
                let before = s.app.selected;
                if i < s.app.projects.len() {
                    s.app.selected = i;
                }
                s.dirty = true;
                if before == s.app.selected {
                    return Flow::Continue(Effects::default());
                }
                let Some(project) = s.app.selected_project().map(|project| project.path.clone())
                else {
                    return Flow::Continue(Effects::default());
                };
                let generation = s.app.viewer.start_project(project.clone());
                s.app.viewer.pane = crate::recall_viewer::ViewerPane::Projects;
                return Flow::Continue(Effects {
                    data_requests: vec![DataRequest::Viewer(
                        crate::recall_viewer::ViewerRequest::Catalogue {
                            project,
                            generation,
                            target: None,
                            refresh: false,
                        },
                    )],
                    ..Effects::default()
                });
            }
            s.app.set_focus(Panel::Projects);
            let before = s.app.selected_project().map(|p| p.path.clone());
            if i < s.app.projects.len() {
                s.app.selected = i;
            }
            s.dirty = true;
            let mut eff = Effects::default();
            selection_change_effects(s, &before, &mut eff);
            Flow::Continue(eff)
        }
        // The rail action bar: the same typed effects the `a` / `d` /
        // ⇧-arrow keys raise — no new capability, only a new trigger. The press
        // flash is armed above (is_button).
        Target::RailAdd | Target::RailRemove | Target::RailReorderUp | Target::RailReorderDown => {
            apply_rail_action(s, &target)
        }
        Target::RepositorySettings => open_project_settings(s),
        Target::RepositoryRecallViewer => Flow::Continue(s.open_recall_viewer(None)),
        Target::RepositoryAdopt => open_adopt_confirm(s),
        Target::ViewerFilter => {
            s.app.viewer.pane = crate::recall_viewer::ViewerPane::Checkpoints;
            sync_viewer_focus(&mut s.app);
            s.app.viewer.begin_filter_editing();
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::ViewerLatest => {
            s.app.viewer.pane = crate::recall_viewer::ViewerPane::Checkpoints;
            sync_viewer_focus(&mut s.app);
            let effects = if s.app.viewer.select_latest() {
                viewer_selection_effects(s, true)
            } else {
                Effects::default()
            };
            s.dirty = true;
            Flow::Continue(effects)
        }
        Target::ViewerFullSession => {
            s.app.viewer.pane = crate::recall_viewer::ViewerPane::Timeline;
            sync_viewer_focus(&mut s.app);
            s.app.viewer.full_session = !s.app.viewer.full_session;
            clamp_viewer_timeline(&mut s.app, s.size);
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::ViewerDashboard => {
            s.close_recall_viewer();
            Flow::Continue(Effects::default())
        }
        Target::ViewerCheckpointRow(index) => {
            s.app.viewer.pane = crate::recall_viewer::ViewerPane::Checkpoints;
            sync_viewer_focus(&mut s.app);
            let changed = index < s.app.viewer.visible.len() && s.app.viewer.selected != index;
            if index < s.app.viewer.visible.len() {
                s.app.viewer.selected = index;
            }
            let effects = if changed {
                viewer_selection_effects(s, true)
            } else {
                Effects::default()
            };
            s.dirty = true;
            Flow::Continue(effects)
        }
        Target::ViewerTimelineLine(line) => {
            s.app.viewer.pane = crate::recall_viewer::ViewerPane::Timeline;
            sync_viewer_focus(&mut s.app);
            s.app.viewer.timeline_cursor = line;
            if let Some(id) = crate::recall_viewer::timeline_item_at(&s.app.viewer, line) {
                s.app.viewer.toggle_expanded(&id);
            }
            clamp_viewer_timeline(&mut s.app, s.size);
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::ViewerTab(pane) => {
            s.app.viewer.pane = pane;
            sync_viewer_focus(&mut s.app);
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::ProjectSettingsAction(i) => {
            s.app.project_settings.select(i);
            s.dirty = true;
            s.app.project_settings.activate().map_or_else(
                || Flow::Continue(Effects::default()),
                |action| {
                    Flow::Continue(Effects {
                        project_settings_actions: vec![action],
                        ..Effects::default()
                    })
                },
            )
        }
        Target::ProjectSettingsClose => {
            s.app.mode = Mode::Normal;
            s.app.project_settings.close();
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::ProjectSettingsCancel => {
            s.app.project_settings.confirm_raw = false;
            s.app.project_settings.result = None;
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::Section(_) => {
            s.app.set_focus(Panel::Graphs);
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        // The overlay's mouse surface: Confirm is the mouse Enter,
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
        // The close X — the click/Tab equivalent of Esc; dismisses
        // any modal that carries a Close control. An empty registry keeps its
        // wizard (dismissing it just reveals the empty rail; it re-raises only if
        // the surface is otherwise free).
        Target::OverlayClose => {
            s.app.overlay = None;
            s.app.overlay_focus = 0;
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        // The wizard's primary action (the message-routing rule) — raise the destination
        // explorer, the click equivalent of the `a` key from the first-run
        // modal (the destination explorer: all three affordances, one door).
        Target::WizardAdd => {
            s.dirty = true;
            let action = crate::input::enter_explorer(&mut s.app);
            profile_or_fold_action(s, action)
        }
        // An inert click surface: the overlay body, and a scrollbar (an overflow
        // indicator only — the scrollbar rule; the wheel scrolls the region in apply_scroll,
        // and drag/click-to-page is a later rung on the same track/thumb geometry).
        Target::OverlayBody | Target::Scrollbar(_) => Flow::Continue(Effects::default()),
        // A suggestion click INSERTS only — Tab's mouse equivalent, never
        // execution.
        Target::SuggestRow(i) => {
            s.app.suggest_highlight = i;
            crate::input::insert_suggestion(&mut s.app);
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        // The USD-savings chip: opens the read-only breakdown
        // as an `Overlay::Results` — the existing modal surface, so the
        // shared close X, focus ring, scroll, and hints come with zero new
        // parity wiring. Reads the TRUTH col3 (never the animated display).
        // The token panel's `Σ` total (the family view): opens the "by
        // worktree" axis of exactly that number — the pointer twin of `w`,
        // through the SAME opener, so the two doors cannot drift apart. Like
        // the USD chip it is an `Overlay::Results`: no new modal surface, and
        // therefore no new close X, focus ring, scroll or hint table.
        Target::TokensByWorktree => {
            s.dirty = true;
            open_worktree_axis(s)
        }
        // A period button: adopt the window in the same frame as the press —
        // the panel must be able to say which period it is drawing before the
        // bus has answered, or the pressed button would light a frame late —
        // and ask the data plane to recompute the card over it. Re-pressing
        // the live window is inert beyond the press flash: the figures are
        // already that window's, and a needless read would still cost one.
        Target::RecallWindow(window) => {
            s.dirty = true;
            select_recall_window(s, window)
        }
        Target::RecallUnitToggle => {
            s.dirty = true;
            toggle_recall_unit(s)
        }
        Target::RecallCost => open_savings_breakdown(s),
        // The header actions map onto the exact same effects their
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
        // The header's panel-editor toggle: enter the mode, or exit
        // it if already editing — the click equivalent of `e` / `Esc`. Exiting
        // persists the arrangement.
        Target::HeaderEditor => {
            s.dirty = true;
            if s.app.mode == Mode::PanelEditor {
                s.app.mode = Mode::Normal;
                s.app.editor.reset();
                let mut eff = Effects::default();
                if let Some(op) = layout_persist_op(&s.app) {
                    eff.persist.push(op);
                }
                Flow::Continue(eff)
            } else {
                crate::input::enter_panel_editor(&mut s.app);
                Flow::Continue(Effects::default())
            }
        }
        Target::HeaderQuit => Flow::Quit,
        // The header's identity button: open the profile menu, or
        // close it if already open — the click equivalent of `p` / `Esc`.
        Target::HeaderProfile => {
            s.dirty = true;
            if s.app.mode == Mode::ProfileMenu {
                s.app.mode = Mode::Normal;
                s.app.profile_menu.reset();
                s.app.command_line.clear();
            } else {
                crate::input::enter_profile_menu(&mut s.app);
            }
            Flow::Continue(Effects::default())
        }
        // A profile row in the open menu: click-to-activate — move
        // the cursor there and switch, exactly like `Enter`.
        Target::ProfileRow(i) => {
            s.dirty = true;
            if i < s.app.profiles.len() {
                s.app.profile_menu.cursor = i;
                switch_profile_flow(s)
            } else {
                Flow::Continue(Effects::default())
            }
        }
        // A hub row's `[settings]` chip: open that profile's
        // settings page — the click twin of `s` on its row.
        Target::ProfileSettings(i) => {
            s.dirty = true;
            if let Some(p) = s.app.profiles.get(i) {
                let id = p.profile_id.clone();
                s.app.profile_menu.cursor = i;
                crate::input::enter_profile_settings(&mut s.app, id);
            }
            Flow::Continue(Effects::default())
        }
        // The hub's `[ + new profile ]` row — the click twin of `n`.
        Target::ProfileNew => {
            s.dirty = true;
            s.app.profile_menu.stage = crate::profile_menu::MenuStage::NamingCreate;
            s.app.profile_menu.name_error = false;
            s.app.command_line.clear();
            Flow::Continue(Effects::default())
        }
        // The menu's close `[x]`: closes the MODE — the pointer
        // twin of Esc-from-Browse, never the overlay close path.
        Target::ProfileMenuClose => {
            s.dirty = true;
            s.app.mode = Mode::Normal;
            s.app.profile_menu.reset();
            s.app.command_line.clear();
            Flow::Continue(Effects::default())
        }
        // A members-editor row: click-to-select (the chips act on the cursor).
        Target::MemberRow(i) => {
            s.dirty = true;
            let last = s.app.profile_menu.members.len().saturating_sub(1);
            s.app.profile_menu.member_cursor = i.min(last);
            Flow::Continue(Effects::default())
        }
        // An add-stage candidate row: include it, exactly like `Enter`.
        Target::MemberAddRow(i) => {
            s.dirty = true;
            let last = s.app.profile_menu.candidates().len().saturating_sub(1);
            s.app.profile_menu.add_cursor = i.min(last);
            profile_or_fold_action(s, Action::MemberAdd)
        }
        // The editor's action chips (the rail pattern): the click twins of
        // ⇧k / ⇧j / d / a on the cursor row (modal-surface parity).
        Target::MemberUp => profile_or_fold_action(s, Action::MemberReorder(true)),
        Target::MemberDown => profile_or_fold_action(s, Action::MemberReorder(false)),
        Target::MemberRemove => profile_or_fold_action(s, Action::MemberRemove),
        Target::MemberAddOpen => {
            s.dirty = true;
            if let crate::profile_menu::MenuStage::Members { profile } =
                s.app.profile_menu.stage.clone()
            {
                s.app.profile_menu.stage = crate::profile_menu::MenuStage::MembersAdd { profile };
                s.app.profile_menu.add_cursor = 0;
            }
            Flow::Continue(Effects::default())
        }
        // A settings row (the profile hub): activate it, exactly like `Enter`.
        Target::SettingsRow(i) => {
            use crate::profile_menu::{MenuStage, SettingsRow};
            s.dirty = true;
            let rows_last = SettingsRow::ALL.len() - 1;
            if let (MenuStage::Settings { profile }, Some(row)) = (
                s.app.profile_menu.stage.clone(),
                SettingsRow::ALL.get(i.min(rows_last)).copied(),
            ) {
                s.app.profile_menu.settings_cursor = i.min(rows_last);
                let action = crate::input::activate_settings_row(&mut s.app, &profile, row);
                profile_or_fold_action(s, action)
            } else {
                Flow::Continue(Effects::default())
            }
        }
        // A picker row: the mouse twin of cursoring + `Enter` — a
        // slot click opens its presets; a preset/reset click applies live.
        Target::PickerRow(i) => {
            use crate::colour_picker::PickerStage;
            s.dirty = true;
            match s.app.colour_picker.stage {
                PickerStage::Slots => {
                    s.app.colour_picker.slot_cursor = i.min(crate::theme::ThemeSlot::ALL.len() - 1);
                    s.app.colour_picker.stage = PickerStage::Presets;
                    s.app.colour_picker.preset_cursor = 0;
                    Flow::Continue(Effects::default())
                }
                PickerStage::Presets => {
                    s.app.colour_picker.preset_cursor = i.min(crate::colour_picker::RESET_ROW);
                    let slot = s.app.colour_picker.slot();
                    let colour = crate::theme::PRESETS.get(i).map(|(_, c)| *c);
                    apply_theme_slot(s, slot, colour)
                }
            }
        }
        // The picker's close `[x]`: back to the profile menu it
        // opened from — the menu stage underneath is intact.
        Target::PickerClose => {
            s.dirty = true;
            s.app.mode = Mode::ProfileMenu;
            s.app.colour_picker.reset();
            Flow::Continue(Effects::default())
        }
        // The destination explorer's mouse plane — every arm
        // delegates to the SAME act helper its key uses, so click ≡ key by
        // construction rather than by a parallel implementation.
        Target::ExplorerRow(i) => {
            s.dirty = true;
            s.app.explorer.cursor = i.min(s.app.explorer.visible_len().saturating_sub(1));
            s.app.clamp_scrolls(s.size);
            // A directory row descends (cursor + Enter); a file row only moves
            // the cursor — files are context, never choices.
            let action = crate::input::explorer_descend(&s.app);
            profile_or_fold_action(s, action)
        }
        Target::ExplorerUp => {
            s.dirty = true;
            let action = crate::input::explorer_up(&s.app);
            profile_or_fold_action(s, action)
        }
        Target::ExplorerSelect => {
            s.dirty = true;
            let action = crate::input::explorer_select(&mut s.app);
            profile_or_fold_action(s, action)
        }
        Target::ExplorerTypePath => {
            s.dirty = true;
            crate::input::explorer_type_path(&mut s.app);
            Flow::Continue(Effects::default())
        }
        Target::ExplorerDotfiles => {
            s.dirty = true;
            crate::input::explorer_toggle_hidden(&mut s.app);
            s.app.clamp_scrolls(s.size);
            Flow::Continue(Effects::default())
        }
        // The explorer's close `[x]`: closes the MODE — the pointer
        // twin of Esc, never the overlay close path.
        Target::ExplorerClose => {
            s.dirty = true;
            s.app.mode = Mode::Normal;
            s.app.explorer.reset();
            Flow::Continue(Effects::default())
        }
        // A grip press begins a unit drag: pick the unit up and arm the
        // drag, which `drag_update`/`drop_drag` carry to the release. The keyboard
        // (pick/move/drop) is the equivalent path.
        Target::GripBar(c) => {
            s.app.editor.cursor = c;
            s.app.editor.clamp(&s.app.layout);
            s.app.editor.picked = true;
            s.app.editor.drag = Some(crate::editor::Drag::Unit(c));
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        // A divider press begins a resize drag toward the pointer.
        Target::ColumnDivider(i) => {
            s.app.editor.cursor.col = i;
            s.app.editor.clamp(&s.app.layout);
            s.app.editor.drag = Some(crate::editor::Drag::Divider(i));
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
    }
}

/// Apply a scroll-wheel event over the panel under the cursor, using the same
/// per-panel vertical semantics as the keyboard: projects move the selection;
/// detail and graphs scroll their viewports.
#[allow(clippy::too_many_lines, clippy::unnested_or_patterns)] // one arm per wheel-bearing target
fn apply_scroll(s: &mut AppState, vm: &ViewModel, col: u16, row: u16, down: bool) -> Flow {
    if s.app.surface == crate::recall_viewer::Surface::RecallViewer && s.app.overlay.is_none() {
        let pane = match hit_test(vm, col, row) {
            Some(
                Target::ProjectRow(_)
                | Target::PanelFocus(Panel::Projects)
                | Target::Scrollbar(ScrollRegion::Rail),
            ) => Some(crate::recall_viewer::ViewerPane::Projects),
            Some(
                Target::ViewerCheckpointRow(_)
                | Target::ViewerFilter
                | Target::ViewerLatest
                | Target::PanelFocus(Panel::Detail)
                | Target::Scrollbar(ScrollRegion::ViewerCheckpoints),
            ) => Some(crate::recall_viewer::ViewerPane::Checkpoints),
            Some(
                Target::ViewerTimelineLine(_)
                | Target::ViewerFullSession
                | Target::ViewerDashboard
                | Target::PanelFocus(Panel::Graphs)
                | Target::Scrollbar(ScrollRegion::ViewerTimeline),
            ) => Some(crate::recall_viewer::ViewerPane::Timeline),
            _ => None,
        };
        if let Some(pane) = pane {
            s.app.viewer.pane = pane;
            sync_viewer_focus(&mut s.app);
            s.dirty = true;
            return Flow::Continue(move_viewer(s, down, false));
        }
        return Flow::Continue(Effects::default());
    }
    let panel = match hit_test(vm, col, row) {
        Some(Target::ProjectRow(_) | Target::Scrollbar(ScrollRegion::Rail)) => Panel::Projects,
        Some(Target::Scrollbar(ScrollRegion::Detail)) => Panel::Detail,
        Some(
            Target::Section(_)
            | Target::RecallCost
            | Target::RecallWindow(_)
            | Target::RecallUnitToggle
            | Target::TokensByWorktree
            | Target::Scrollbar(ScrollRegion::Graphs),
        ) => Panel::Graphs,
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
        Some(
            Target::ProjectSettingsAction(_)
            | Target::ProjectSettingsClose
            | Target::ProjectSettingsCancel
            | Target::OverlayBody
            | Target::Scrollbar(ScrollRegion::Modal),
        ) if s.app.mode == Mode::ProjectSettings && s.app.overlay.is_none() => {
            let body = crate::view::project_settings_body_rect(crate::view::modal_rect(s.size));
            let max = s
                .app
                .project_settings
                .display_line_count()
                .saturating_sub(usize::from(body.h));
            s.app.project_settings.scroll = if down {
                s.app.project_settings.scroll.saturating_add(1).min(max)
            } else {
                s.app.project_settings.scroll.saturating_sub(1)
            };
            s.app.project_settings.follow_cursor = false;
            s.dirty = true;
            return Flow::Continue(Effects::default());
        }
        // The destination explorer's wheel — the first mode-modal
        // to scroll. Placed BEFORE the generic overlay-body arm, and guarded on
        // the mode with no overlay, so a Notice over the explorer still owns the
        // wheel (an overlay always outranks a mode).
        Some(
            Target::ExplorerRow(_)
            | Target::ExplorerUp
            | Target::ExplorerSelect
            | Target::ExplorerTypePath
            | Target::ExplorerDotfiles
            | Target::ExplorerClose
            | Target::OverlayBody
            | Target::Scrollbar(ScrollRegion::Modal),
        ) if s.app.mode == Mode::Explorer && s.app.overlay.is_none() => {
            return explorer_wheel(s, down);
        }
        // The wheel scrolls an open Results overlay like `j`/`k`.
        Some(
            Target::OverlayBody
            | Target::OverlayConfirm
            | Target::OverlayCancel
            | Target::OverlayClose
            | Target::Scrollbar(ScrollRegion::Modal),
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
            | Target::HeaderEditor
            | Target::HeaderQuit
            | Target::HeaderProfile
            | Target::ProfileRow(_)
            | Target::ProfileSettings(_)
            | Target::ProfileNew
            | Target::ProfileMenuClose
            | Target::SettingsRow(_)
            | Target::MemberRow(_)
            | Target::MemberAddRow(_)
            | Target::MemberUp
            | Target::MemberDown
            | Target::MemberRemove
            | Target::MemberAddOpen
            | Target::PickerRow(_)
            | Target::PickerClose
            | Target::ExplorerRow(_)
            | Target::ExplorerUp
            | Target::ExplorerSelect
            | Target::ExplorerTypePath
            | Target::ExplorerDotfiles
            | Target::ExplorerClose
            | Target::WizardAdd
            | Target::RailAdd
            | Target::RailReorderUp
            | Target::RailReorderDown
            | Target::RailRemove
            | Target::RepositorySettings
            | Target::RepositoryRecallViewer
            | Target::RepositoryAdopt
            | Target::ViewerFilter
            | Target::ViewerLatest
            | Target::ViewerFullSession
            | Target::ViewerDashboard
            | Target::ViewerCheckpointRow(_)
            | Target::ViewerTimelineLine(_)
            | Target::ViewerTab(_)
            | Target::ProjectSettingsAction(_)
            | Target::ProjectSettingsClose
            | Target::ProjectSettingsCancel
            | Target::GripBar(_)
            | Target::ColumnDivider(_)
            | Target::Scrollbar(ScrollRegion::ViewerCheckpoints)
            | Target::Scrollbar(ScrollRegion::ViewerTimeline),
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
            let mut eff = Effects::default();
            selection_change_effects(s, &before, &mut eff);
            Flow::Continue(eff)
        }
        Panel::Detail => {
            s.app.scroll.detail = if down {
                s.app.scroll.detail.saturating_add(1)
            } else {
                s.app.scroll.detail.saturating_sub(1)
            };
            s.app.clamp_scrolls(s.size);
            Flow::Continue(Effects::default())
        }
        Panel::Graphs => {
            s.app.scroll.graphs = if down {
                s.app.scroll.graphs.saturating_add(1)
            } else {
                s.app.scroll.graphs.saturating_sub(1)
            };
            s.app.clamp_scrolls(s.size); // never past the end
            Flow::Continue(Effects::default())
        }
        Panel::Command => Flow::Continue(Effects::default()),
    }
}

/// Scroll the destination explorer's listing by one row — the
/// first mode-modal wheel. Clamped to the listing's true overflow against the
/// SAME viewport solver the renderer draws with, so the wheel can never run
/// past the end into empty space.
fn explorer_wheel(s: &mut AppState, down: bool) -> Flow {
    let list_h = usize::from(crate::view::explorer_list_area(crate::view::modal_rect(s.size)).h);
    let max = s.app.explorer.visible_len().saturating_sub(list_h);
    s.app.explorer.scroll = if down {
        s.app.explorer.scroll.saturating_add(1).min(max)
    } else {
        s.app.explorer.scroll.saturating_sub(1)
    };
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// Select the Recall card's period — the ONE act behind the `[24h][7d][30d]
/// [all]` buttons and the `1`–`4` keys, so the two doors cannot drift.
///
/// The window is adopted on the model immediately, before the bus has
/// answered: the pressed button has to light in the frame of the press, and
/// the panel has to be able to say which period it is *drawing* rather than
/// which it last received. The figures follow when the recompute lands.
///
/// Re-selecting the live window is inert beyond the press flash. The card is
/// already showing that period, and a request for it would still cost a read.
fn select_recall_window(s: &mut AppState, window: crate::col3::RecallWindow) -> Flow {
    if s.col3.recall_window == window {
        return Flow::Continue(Effects::default());
    }
    let mut col3 = (*s.col3).clone();
    col3.recall_window = window;
    s.set_col3(col3);
    Flow::Continue(Effects {
        data_requests: vec![DataRequest::SetRecallWindow(window)],
        ..Effects::default()
    })
}

/// Flip the Recall card's bars between tokens and dollars — the ONE act
/// behind the `[tok]`/`[usd]` chip and the `$` key.
///
/// It emits **no data request**, and that is the difference from a period
/// change rather than an omission: both figures ride the same feed frame, so
/// the flip is a rendering choice the current frame can already answer. A
/// recompute here would cost a read to redraw numbers Canopy is holding.
fn toggle_recall_unit(s: &mut AppState) -> Flow {
    let mut col3 = (*s.col3).clone();
    col3.recall_unit = col3.recall_unit.flipped();
    s.set_col3(col3);
    Flow::Continue(Effects::default())
}

/// Fold the existing [`Action`] (from [`on_key`]) into a [`Flow`]/[`Effects`].
/// `RemoveProject` and `ReorderProject` are handled in [`update`] because they
/// need the selected project, so they never reach here.
fn fold_action(action: Action) -> Flow {
    match action {
        Action::Quit => Flow::Quit,
        Action::None
        | Action::RemoveProject
        | Action::ReorderProject(_)
        | Action::OpenProjectSettings
        | Action::OpenRecallViewer
        | Action::OpenWorktreeAxis
        | Action::OpenSavingsBreakdown
        | Action::SetRecallWindow(_)
        | Action::ToggleRecallUnit
        | Action::AdoptWorktree
        | Action::ApplyProjectSettings(_)
        | Action::RunLine(_)
        | Action::ConfirmDispatch
        | Action::SwitchProfile
        | Action::CreateProfile(_)
        | Action::RenameProfile { .. }
        | Action::DeleteProfile(_)
        | Action::DuplicateProfile { .. }
        | Action::ToggleStartupPin(_)
        | Action::LoadMembers(_)
        | Action::MemberRemove
        | Action::MemberReorder(_)
        | Action::MemberAdd
        | Action::OpenColours(_)
        | Action::SetThemeSlot { .. } => Flow::Continue(Effects::default()),
        Action::AddProject(path) => Flow::Continue(Effects {
            persist: vec![PersistOp::AddProject { path }],
            ..Effects::default()
        }),
        // The lister seam (the destination explorer): the reducer requests, the loop walks.
        Action::ExplorerList(path) => Flow::Continue(Effects {
            list_dir: Some(path),
            ..Effects::default()
        }),
    }
}

/// Submit one palette buffer: parse; a span error renders inline
/// (buffer + mode kept so the underline points at the typed text); a verb
/// routes by tier — dashboard verbs to typed panel effects, read-only
/// delegation straight to the dispatcher behind a Running frame, mutating
/// delegation to the confirm overlay.
fn run_line(s: &mut AppState, buf: &str) -> Flow {
    match crate::command::parse(buf) {
        Err(e) => {
            // An incomplete line accepts the highlighted suggestion first
            // (Enter runs, accepting the highlight when the line is
            // incomplete); only if THAT parses does it run.
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
            if s.app.surface == crate::recall_viewer::Surface::RecallViewer
                && !viewer_command_allowed(&verb)
            {
                s.open_alert(crate::dispatch::Message::warning(
                    "Recall Viewer is read-only",
                    vec![
                        "allowed: viewer, select, focus, help, quit, status, checkpoint list/show, session list/show, explain, doctor, projects list, gc --dry-run".into(),
                    ],
                ));
                return Flow::Continue(Effects::default());
            }
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
                        s.app.open_overlay(Overlay::running(req.verb_line()));
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

fn viewer_command_allowed(verb: &Verb) -> bool {
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

/// The confirm overlay's Enter/Confirm: swap Confirm → Running and emit the
/// pending dispatch with the repo resolved at confirm time.
fn confirm_dispatch(s: &mut AppState) -> Flow {
    let Some(Overlay::Confirm { request, repo }) = s.app.overlay.take() else {
        return Flow::Continue(Effects::default());
    };
    s.app.open_overlay(Overlay::running(request.verb_line()));
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
/// emit — the palette can never exceed a panel's privilege (the injected dispatcher).
#[allow(clippy::too_many_lines)] // one arm per closed palette verb
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
            if s.app.surface == crate::recall_viewer::Surface::RecallViewer {
                use crate::recall_viewer::ViewerPane;
                s.app.viewer.pane = match p {
                    PanelName::Projects => ViewerPane::Projects,
                    PanelName::Detail | PanelName::Checkpoints => ViewerPane::Checkpoints,
                    PanelName::Graphs | PanelName::Timeline => ViewerPane::Timeline,
                    PanelName::Command => ViewerPane::Command,
                };
            } else {
                s.app.set_focus(match p {
                    PanelName::Projects => Panel::Projects,
                    PanelName::Detail | PanelName::Checkpoints => Panel::Detail,
                    PanelName::Graphs | PanelName::Timeline => Panel::Graphs,
                    PanelName::Command => Panel::Command,
                });
            }
            Flow::Continue(Effects::default())
        }
        Verb::Viewer(args) => {
            if let Some(repo) = args.repo {
                // Keep the reducer pure: repository discovery/canonicalisation
                // belongs to the viewer data worker. A palette deep link is a
                // session-only row until that worker reports whether it can be
                // opened, and is never written to the project registry.
                let path = repo;
                if let Some(index) = s
                    .app
                    .projects
                    .iter()
                    .position(|project| project.path == path)
                {
                    s.app.selected = index;
                } else {
                    let name = std::path::Path::new(&path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("transient repository")
                        .to_owned();
                    s.app.projects.push(crate::Project {
                        path: path.clone(),
                        name,
                        status: ProjectStatus::Idle,
                        kind: crate::EntryKind::Repo,
                    });
                    s.app.selected = s.app.projects.len().saturating_sub(1);
                }
            }
            Flow::Continue(s.open_recall_viewer(args.target))
        }
        Verb::Select(r) => {
            let before = s.app.selected_project().map(|p| p.path.clone());
            let Some(path) = resolve_project_ref(&s.app, &r) else {
                return no_match_status(s);
            };
            s.app.select_path(&path);
            if s.app.surface == crate::recall_viewer::Surface::RecallViewer {
                let Some(project) = s.app.selected_project().map(|project| project.path.clone())
                else {
                    return Flow::Continue(Effects::default());
                };
                let generation = s.app.viewer.start_project(project.clone());
                s.app.viewer.pane = crate::recall_viewer::ViewerPane::Projects;
                return Flow::Continue(Effects {
                    data_requests: vec![DataRequest::Viewer(
                        crate::recall_viewer::ViewerRequest::Catalogue {
                            project,
                            generation,
                            target: None,
                            refresh: false,
                        },
                    )],
                    ..Effects::default()
                });
            }
            let mut eff = Effects::default();
            selection_change_effects(s, &before, &mut eff);
            Flow::Continue(eff)
        }
        Verb::Add => {
            let Some(path) = s.app.launch_cwd.clone() else {
                s.open_alert(crate::dispatch::Message::error(
                    "cannot add project",
                    vec!["launch directory unknown — run `rift init` there".into()],
                ));
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
            let op = remove_project_op(&s.app, path);
            persist_flow(Some(op))
        }
        Verb::Relabel(r, label) => {
            let Some(path) = resolve_project_ref(&s.app, &r) else {
                return no_match_status(s);
            };
            persist_flow(Some(PersistOp::RelabelProject { path, label }))
        }
        Verb::Settings => open_project_settings(s),
        // The /profile family: the same typed effects the menu
        // emits — two surfaces, one grammar.
        Verb::Profile(pv) => profile_verb_flow(s, pv),
        // Delegable verbs never reach here (request_for handled them).
        _ => Flow::Continue(Effects::default()),
    }
}

/// Apply one `/profile` palette verb. Names resolve against the
/// live profiles; a miss or a refused name/guard is a soft warning Alert
/// (the message-routing rule). Every accepted act is the same op + reload the menu emits.
fn profile_verb_flow(s: &mut AppState, pv: crate::command::ProfileVerb) -> Flow {
    use crate::command::ProfileVerb;
    let warn = |s: &mut AppState, body: &str| {
        s.open_alert(crate::dispatch::Message::warning(
            "profiles",
            vec![body.to_string()],
        ));
        Flow::Continue(Effects::default())
    };
    if s.app.profiles.is_empty() {
        return warn(s, "the profile plane is not wired in this run");
    }
    match pv {
        ProfileVerb::New(raw) => {
            match crate::profile::normalize_profile_name(&raw, &s.app.profiles, None) {
                Some(name) => profile_persist_flow(PersistOp::CreateProfile { name }),
                None => warn(s, "name must be 1–24 chars and unique"),
            }
        }
        ProfileVerb::Rename(raw) => {
            let active = s.app.active_profile.clone();
            match crate::profile::normalize_profile_name(&raw, &s.app.profiles, Some(&active)) {
                Some(name) => profile_persist_flow(PersistOp::RenameProfile {
                    profile: active,
                    name,
                }),
                None => warn(s, "name must be 1–24 chars and unique"),
            }
        }
        ProfileVerb::Switch(name) => {
            let target = s
                .app
                .profiles
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.profile_id.clone());
            match target {
                Some(id) if id == s.app.active_profile => Flow::Continue(Effects::default()),
                Some(id) => profile_persist_flow(PersistOp::SelectProfile { profile: id }),
                None => warn(s, "no profile by that name"),
            }
        }
        ProfileVerb::Delete(name) => {
            let target = s
                .app
                .profiles
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.profile_id.clone());
            match target {
                // The removal guards refuse here first — and again in the store.
                Some(id) if id == s.app.active_profile => {
                    warn(s, "cannot delete the active profile")
                }
                Some(_) if s.app.profiles.len() <= 1 => warn(s, "cannot delete the last profile"),
                Some(id) => profile_persist_flow(PersistOp::RemoveProfile { profile: id }),
                None => warn(s, "no profile by that name"),
            }
        }
        // `/profile duplicate <new-name>` clones the ACTIVE profile whole —
        // parallel to `/profile new`.
        ProfileVerb::Duplicate(raw) => {
            match crate::profile::normalize_profile_name(&raw, &s.app.profiles, None) {
                Some(name) => profile_persist_flow(PersistOp::DuplicateProfile {
                    source: s.app.active_profile.clone(),
                    name,
                }),
                None => warn(s, "name must be 1–24 chars and unique"),
            }
        }
        // `/profile startup <name>` pins; the bare verb unpins (launch
        // follows last-selected again) — never a warn.
        ProfileVerb::Startup(None) => {
            profile_persist_flow(PersistOp::SetStartupProfile { profile: None })
        }
        ProfileVerb::Startup(Some(name)) => {
            let target = s
                .app
                .profiles
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.profile_id.clone());
            match target {
                Some(id) => {
                    profile_persist_flow(PersistOp::SetStartupProfile { profile: Some(id) })
                }
                None => warn(s, "no profile by that name"),
            }
        }
    }
}

/// The catalog word a dashboard verb bumps in the frecency log.
fn verb_word_of(verb: &Verb) -> &'static str {
    match verb {
        Verb::Select(_) => "select",
        Verb::Add => "add",
        Verb::Remove(_) => "remove",
        Verb::Relabel(..) => "relabel",
        Verb::Settings => "settings",
        Verb::Viewer(_) => "viewer",
        Verb::Focus(_) => "focus",
        Verb::Profile(_) => "profile",
        Verb::Help => "help",
        Verb::Quit => "quit",
        // Delegated verbs bump on DispatchDone, not here.
        _ => "",
    }
}

/// The "no matching project" alert (shared by the project-ref verbs) — a soft
/// miss, so a warning Alert.
fn no_match_status(s: &mut AppState) -> Flow {
    s.open_alert(crate::dispatch::Message::warning(
        "no match",
        vec!["no matching project".into()],
    ));
    Flow::Continue(Effects::default())
}

/// The rail action bar's clicks — the add/remove/reorder buttons raise
/// the same typed effects the `a` / `d` / ⇧-arrow keys do. Add enters
/// AddProject mode; remove/reorder act on the selected project (no selection ⇒
/// inert). Split out of `apply_click` to keep that reducer under the line limit.
fn apply_rail_action(s: &mut AppState, target: &Target) -> Flow {
    s.app.set_focus(Panel::Projects);
    s.dirty = true;
    match target {
        // The `+` chip raises the destination explorer through the ONE entry
        // door (the destination explorer) — the click twin of `a`, not a second seeding.
        Target::RailAdd => {
            let action = crate::input::enter_explorer(&mut s.app);
            profile_or_fold_action(s, action)
        }
        Target::RailRemove => {
            let sel = s.app.selected_project().map(|p| p.path.clone());
            let op = sel.map(|path| remove_project_op(&s.app, path));
            persist_flow(op)
        }
        Target::RailReorderUp | Target::RailReorderDown => {
            let up = matches!(target, Target::RailReorderUp);
            let sel = s.app.selected_project().map(|p| p.path.clone());
            let op = sel.map(|path| reorder_project_op(&s.app, path, up));
            persist_flow(op)
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

/// Fold the profile-plane actions — each one superseding
/// persist op plus its reload (or, for a picker selection, the same-frame
/// re-theme) — deferring every other action to [`fold_action`].
fn profile_or_fold_action(s: &mut AppState, action: Action) -> Flow {
    match action {
        Action::SwitchProfile => switch_profile_flow(s),
        Action::CreateProfile(name) => profile_persist_flow(PersistOp::CreateProfile { name }),
        Action::RenameProfile { profile, name } => {
            profile_persist_flow(PersistOp::RenameProfile { profile, name })
        }
        Action::DeleteProfile(profile) => {
            profile_persist_flow(PersistOp::RemoveProfile { profile })
        }
        Action::DuplicateProfile { source, name } => {
            profile_persist_flow(PersistOp::DuplicateProfile { source, name })
        }
        // The settings toggle resolves pin-vs-unpin against the CURRENT pin;
        // the reload keeps the rendered state honest.
        Action::ToggleStartupPin(profile) => {
            let profile = if s.app.startup_profile.as_deref() == Some(profile.as_str()) {
                None
            } else {
                Some(profile)
            };
            profile_persist_flow(PersistOp::SetStartupProfile { profile })
        }
        // A picker selection applies LIVE in the same frame: the
        // snapshot re-themes here; the superseding event persists.
        Action::SetThemeSlot { slot, colour } => apply_theme_slot(s, slot, colour),
        // The picker opened for a profile: seed its swatches —
        // purely when the target is active; through the seam otherwise.
        Action::OpenColours(profile) => {
            s.dirty = true;
            if profile == s.app.active_profile {
                s.app.colour_picker.target_overrides = s.theme().overrides();
                Flow::Continue(Effects::default())
            } else {
                Flow::Continue(Effects {
                    load_picker_theme: Some(profile),
                    ..Effects::default()
                })
            }
        }
        // The members editor: entry loads the lists; every op is a
        // superseding membership insert plus a re-read of the editor's rows.
        Action::LoadMembers(profile) => Flow::Continue(Effects {
            load_membership: Some(profile),
            ..Effects::default()
        }),
        Action::MemberRemove => member_op(s, |profile, path| PersistOp::RemoveProfileProject {
            profile,
            path,
        }),
        Action::MemberReorder(up) => member_op(s, move |profile, path| {
            PersistOp::ReorderProfileProject { profile, path, up }
        }),
        Action::MemberAdd => {
            let crate::profile_menu::MenuStage::MembersAdd { profile } =
                s.app.profile_menu.stage.clone()
            else {
                return Flow::Continue(Effects::default());
            };
            let Some(path) = s
                .app
                .profile_menu
                .candidates()
                .get(s.app.profile_menu.add_cursor)
                .map(|p| p.path.clone())
            else {
                return Flow::Continue(Effects::default());
            };
            s.dirty = true;
            Flow::Continue(Effects {
                persist: vec![PersistOp::AddProfileProject {
                    profile: profile.clone(),
                    path,
                }],
                load_membership: Some(profile),
                ..Effects::default()
            })
        }
        other => fold_action(other),
    }
}

/// One members-editor op on the row under the cursor: resolve (profile, path)
/// from the stage + the loaded member list, persist the superseding event, and
/// re-read the editor's rows through the seam.
fn member_op(s: &mut AppState, op: impl FnOnce(String, String) -> PersistOp) -> Flow {
    let crate::profile_menu::MenuStage::Members { profile } = s.app.profile_menu.stage.clone()
    else {
        return Flow::Continue(Effects::default());
    };
    let Some(path) = s
        .app
        .profile_menu
        .members
        .get(s.app.profile_menu.member_cursor)
        .map(|p| p.path.clone())
    else {
        return Flow::Continue(Effects::default());
    };
    s.dirty = true;
    Flow::Continue(Effects {
        persist: vec![op(profile.clone(), path)],
        load_membership: Some(profile),
        ..Effects::default()
    })
}

/// A profile persist op plus the whole-view reload it implies:
/// the loop re-reads names, active id, membership, selection, and arrangement
/// through the loader seam after the op lands.
fn profile_persist_flow(op: PersistOp) -> Flow {
    Flow::Continue(Effects {
        persist: vec![op],
        reload_profiles: true,
        ..Effects::default()
    })
}

/// The menu's switch (`Enter` on a row, the profile menu and header identity): select the cursored
/// profile and close the menu — the whole view follows through the reload.
/// Switching to the already-active profile just closes the menu.
fn switch_profile_flow(s: &mut AppState) -> Flow {
    let Some(target) = s
        .app
        .profiles
        .get(s.app.profile_menu.cursor)
        .map(|p| p.profile_id.clone())
    else {
        return Flow::Continue(Effects::default());
    };
    s.app.mode = Mode::Normal;
    s.app.profile_menu.reset();
    if target == s.app.active_profile {
        return Flow::Continue(Effects::default());
    }
    profile_persist_flow(PersistOp::SelectProfile { profile: target })
}

/// Apply one theme-slot selection (the colour picker's presets, applied to the
/// profile whose page opened it):
/// the event persists to the picker's TARGET profile always; the running
/// dashboard re-themes in the same frame **only when the target is the active
/// profile** — editing a non-active profile's colours never disturbs the
/// shown view. The picker's own swatches update purely either way, so its
/// slot stage tells the truth without a reload. No profile wired ⇒ the
/// preview alone (nothing durable to scope it to).
fn apply_theme_slot(
    s: &mut AppState,
    slot: crate::theme::ThemeSlot,
    colour: Option<crate::render::frame::Color>,
) -> Flow {
    let target = if s.app.colour_picker.target_profile.is_empty() {
        s.app.active_profile.clone()
    } else {
        s.app.colour_picker.target_profile.clone()
    };
    s.app.colour_picker.target_overrides.set(slot, colour);
    if target == s.app.active_profile {
        let mut overrides = s.theme().overrides();
        overrides.set(slot, colour);
        s.set_theme(overrides);
    }
    s.dirty = true;
    if target.is_empty() {
        return Flow::Continue(Effects::default());
    }
    Flow::Continue(Effects {
        persist: vec![PersistOp::SetProfileTheme {
            profile: target,
            slot: slot.key().to_string(),
            colour: colour.and_then(crate::theme::colour_to_hex),
        }],
        ..Effects::default()
    })
}

/// The remove op for a column-1 project: profile-scoped (this view only —
/// the profile plane) when a profile is wired; the catalogue removal
/// otherwise (headless/tests keep pre-profile behavior).
fn remove_project_op(app: &App, path: String) -> PersistOp {
    if app.active_profile.is_empty() {
        PersistOp::RemoveProject { path }
    } else {
        PersistOp::RemoveProfileProject {
            profile: app.active_profile.clone(),
            path,
        }
    }
}

/// The reorder op for a column-1 project: profile-scoped when a profile is
/// wired (the view reorders; the catalogue keeps its order), global otherwise.
fn reorder_project_op(app: &App, path: String, up: bool) -> PersistOp {
    if app.active_profile.is_empty() {
        PersistOp::ReorderProject { path, up }
    } else {
        PersistOp::ReorderProfileProject {
            profile: app.active_profile.clone(),
            path,
            up,
        }
    }
}

fn open_project_settings(s: &mut AppState) -> Flow {
    let Some(project) = s.app.selected_project() else {
        return Flow::Continue(Effects::default());
    };
    let view = s
        .detail
        .as_deref()
        .filter(|detail| detail.path == project.path)
        .map_or_else(
            || crate::ProjectSettingsView::thin(project.path.clone(), project.name.clone()),
            |detail| (*detail.project_settings).clone(),
        );
    let path = view.path.clone();
    s.app.project_settings.open(view);
    s.app.mode = Mode::ProjectSettings;
    s.dirty = true;
    Flow::Continue(Effects {
        data_requests: vec![DataRequest::Refresh(path)],
        ..Effects::default()
    })
}

/// Open the graphs column's "by worktree" axis (the family view) as an
/// `Overlay::Results` — the **one** opener behind both the `w` key and a
/// click on the token panel's `Σ` total, so the two doors can never show
/// different things.
///
/// Reads the TRUTH col3, never the animated display: a mid-tween frame's
/// series must never become a table of counts. Always opens — with no facts
/// in scope the body says so — because a hinted key that sometimes does
/// nothing is a dead key some of the time (modal-surface parity).
fn open_worktree_axis(s: &mut AppState) -> Flow {
    let lines = crate::col3::worktree_axis_lines(&s.col3);
    s.app.open_overlay(Overlay::Results {
        title: "token usage by worktree".to_owned(),
        lines,
        scroll: 0,
        footer: None,
        truncated: false,
    });
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// The recall savings breakdown — the **one** opener behind all three doors:
/// the `[~$…]` chip, the `[of N% target]` chip beside it and the `S` key.
///
/// The chips were the only way in until this existed, which made the whole
/// disclosed basis — the per-model rows, the per-family share, the window it
/// covers — mouse-only, on a dashboard whose standing rule is that anything a
/// hint teaches is reachable both ways. It was not caught by the mechanical
/// parity checks because the chips are not drawn as a modal surface; now the
/// key is hinted where it is bound (`keymap::GRAPHS`) and the hint-derivation
/// proof covers it like every other.
///
/// Always opens, like the worktree axis and for the same reason: a hinted key
/// that sometimes does nothing is a dead key some of the time. With no savings
/// data the body says what is absent.
///
/// Reads the TRUTH col3, never the animated display.
fn open_savings_breakdown(s: &mut AppState) -> Flow {
    let (title, lines) = match &s.col3.savings {
        crate::col3::RecallView::Data(sd) => (
            match sd.usd_label.as_deref() {
                Some(label) => format!("recall savings — {label}"),
                None => "recall savings".to_owned(),
            },
            crate::col3::usd_breakdown_lines(sd),
        ),
        crate::col3::RecallView::Quiet => (
            "recall savings".to_owned(),
            vec![
                "nothing has been served in this period, so there is".to_owned(),
                "no saving to break down.".to_owned(),
            ],
        ),
        crate::col3::RecallView::Disabled => (
            "recall savings".to_owned(),
            vec![
                "recall is not enabled for this repository, so".to_owned(),
                "nothing has been avoided here — open Project".to_owned(),
                "Settings to turn it on.".to_owned(),
            ],
        ),
    };
    s.app.open_overlay(Overlay::Results {
        title,
        lines,
        scroll: 0,
        footer: None,
        truncated: false,
    });
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// The Repository card's adopt act, from either door (the family view).
///
/// **One opener behind both**, exactly as the worktree axis has: the `A` key
/// and a click on the unadopted member's status row raise the SAME confirm
/// overlay for the SAME request, so the two can never show different things.
///
/// It is a MUTATING request and therefore confirms first (the injected dispatcher): adoption
/// writes descriptors and client fragments into a checkout, which is not
/// something a stray keystroke may do. The repository is the selected
/// project's path, resolved here and again at confirm time, like every other
/// delegated act.
fn open_adopt_confirm(s: &mut AppState) -> Flow {
    let repo = s
        .app
        .selected_project()
        .map(|p| p.path.clone())
        .unwrap_or_default();
    s.app.open_overlay(Overlay::Confirm {
        request: DispatchRequest::Adopt,
        repo,
    });
    s.dirty = true;
    Flow::Continue(Effects::default())
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
    /// Top-level dashboard/Recall Viewer surface.
    pub surface: crate::recall_viewer::Surface,
    /// Session-local Recall Viewer snapshot and navigation state.
    pub viewer: crate::recall_viewer::RecallViewerState,
    /// The binary's version (header segment; empty hides it).
    pub version: String,
    /// The data-plane health the header dot shows.
    pub health: crate::theme::Health,
    /// Column-1 project rows (shared, rebuilt only when the rows change).
    pub projects: Arc<Vec<ProjectRow>>,
    /// The selected project's index (selection highlight + scroll-to-selection).
    pub selected: usize,
    /// Column-2 selected-project detail (shared with the state's cached copy).
    pub detail: Option<Arc<DetailView>>,
    /// The column-3 sections: the six live-graph view-models.
    pub col3: Arc<crate::col3::Col3>,
    /// The selected project's activity-card data; `Default` placeholder.
    pub activity: Arc<crate::col3::ActivityView>,
    /// Column-2 detail viewport top (scroll offset).
    pub detail_scroll: usize,
    /// Column-3 graphs viewport top (scroll offset).
    pub graphs_scroll: usize,
    /// The command/add-project edit buffer.
    pub command_line: String,
    /// The modal surface (results / confirm / running / alert / notice / wizard),
    /// if open — the one home for every message.
    pub overlay: Option<Overlay>,
    /// The modal focus ring's cursor — an index into `overlay.controls()`;
    /// the renderer highlights the focused control.
    pub overlay_focus: usize,
    /// The inline parse error under the command bar, if any.
    pub command_error: Option<crate::command::ParseError>,
    /// The suggest panel's ranked rows (empty unless command mode).
    pub suggestions: Vec<crate::suggest::Suggestion>,
    /// The highlighted suggestion row.
    pub suggest_highlight: usize,
    /// The panel's fade-in progress (0..=1).
    pub suggest_fade: f32,
    /// The one-shot startup-sweep reveal progress (0..=1). 1.0 = fully
    /// revealed (reduced motion / headless rest here).
    pub sweep: f32,
    /// The transient selection pulse (1.0 → 0.0).
    pub select_pulse: f32,
    /// The transient button press-flash (1.0 → 0.0), keyed by
    /// [`Self::pressed`].
    pub press_pulse: f32,
    /// The button whose press-flash is in flight (keys `press_pulse`).
    pub pressed: Option<crate::hit::Target>,
    /// The active dashboard arrangement (the data-driven layout model), shared from `App.layout` so
    /// the renderer and the pure hit-test read one model. Pointer-equal when
    /// unchanged — a quiescent snapshot stays cheap (the ~0% idle invariant).
    pub layout: Arc<crate::layout_model::LayoutModel>,
    /// The panel-editor state, copied from `App.editor` — the grip
    /// renderer and the editor hit-test read it while `mode == PanelEditor`.
    pub editor: crate::editor::EditorState,
    /// The active profile's display name, as the header identity shows it, or
    /// `None` until the profile plane is wired — the button hides.
    pub profile_name: Option<String>,
    /// The live profiles — the menu's rows.
    pub profiles: Vec<crate::profile::ProfileEntry>,
    /// The active profile's stable id (marks the active row in the menu).
    pub active_profile: String,
    /// The pinned startup profile, or `None` when launch follows
    /// last-selected — the settings surface's pin-toggle row reads it.
    pub startup_profile: Option<String>,
    /// The profile menu's state, copied from `App.profile_menu` —
    /// the menu renderer and its hit-test read it while `mode == ProfileMenu`.
    pub profile_menu: crate::profile_menu::ProfileMenuState,
    /// The active profile's surface theme — the palette rides the
    /// snapshot exactly like the version and the layout, so the renderer stays
    /// pure and a no-override frame is byte-identical to Rift dark.
    pub theme: crate::theme::Theme,
    /// The colour picker's state, copied from `App.colour_picker` —
    /// the picker renderer and its hit-test read it while
    /// `mode == ColourPicker`.
    pub colour_picker: crate::colour_picker::ColourPickerState,
    /// The destination explorer's state, copied from `App.explorer`
    /// — the explorer renderer and its hit-test read it while
    /// `mode == Explorer`.
    pub explorer: crate::explorer::ExplorerState,
    /// Repository Project Settings state, copied while its mode-modal is open.
    pub project_settings: crate::ProjectSettingsModalState,
}

impl ViewModel {
    /// The press-flash fraction (0.0 at rest) for the button drawn as `target`
    /// — the shared way every button draw site asks "am I flashing?".
    #[must_use]
    pub fn button_flash(&self, target: &crate::hit::Target) -> f32 {
        if self.pressed.as_ref() == Some(target) {
            self.press_pulse
        } else {
            0.0
        }
    }

    /// The modal control the focus ring rests on — the render side's
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
        s.app.overlay = None; // dismiss the first-run wizard; drive the dashboard
        s.app.overlay_focus = 0;
        s.dirty = false;
        s
    }

    fn project_settings_state(initialization: crate::ProjectInitialization) -> AppState {
        let mut s = ready(vec![project("a")]);
        let status = match initialization {
            crate::ProjectInitialization::Initialized => ProjectStatus::Idle,
            crate::ProjectInitialization::NotInitialized => ProjectStatus::NotInitialized,
            crate::ProjectInitialization::NotProvisioned => ProjectStatus::NotProvisioned,
            crate::ProjectInitialization::NotRepository => ProjectStatus::NotRepository,
            crate::ProjectInitialization::SetupIncomplete => ProjectStatus::SetupIncomplete,
        };
        s.app.projects[0].status = status;
        let settings = crate::ProjectSettingId::ALL
            .iter()
            .map(|id| crate::ProjectSettingRow {
                id: *id,
                enabled: false,
                actionable: true,
                detail: None,
            })
            .collect();
        let integrations = (0..9)
            .map(|index| crate::NativeAgentIntegrationRow {
                agent: format!("agent-{index}"),
                display_name: format!("Agent {index}"),
                detection: crate::NativeAgentDetection::NotDetected,
                executable_name: None,
                installations: Vec::new(),
                wiring: crate::NativeAgentWiring::Missing,
                hook_wiring: None,
                alpha_supported: true,
                selected: false,
            })
            .collect();
        let mut detail = DetailView::thin("/r/a".into(), "a".into(), status);
        detail.project_settings = Box::new(crate::ProjectSettingsView {
            path: "/r/a".into(),
            project_name: "a".into(),
            initialization,
            settings,
            policy: crate::ProjectPolicySummary::default(),
            integrations,
        });
        update(
            &mut s,
            Event::ProjectData(ProjectData {
                project: "/r/a".into(),
                status,
                detail: Some(detail),
            }),
        );
        s.dirty = false;
        s
    }

    /// The add flow's deferred wizard: a project just registered opens
    /// Project Settings on its first status read when there is a setup to
    /// offer — never initialized, or configured by the repository but not set
    /// up on this machine — and stays quiet when it is already set up here
    /// or is not a repository at all. One read decides, whatever it says.
    #[test]
    fn a_newly_added_project_opens_setup_only_when_there_is_setup_to_offer() {
        for (status, opens) in [
            (ProjectStatus::NotProvisioned, true),
            (ProjectStatus::NotInitialized, true),
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
            let init = vec![crate::ProjectSettingsAction {
                path: "/r/b".into(),
                kind: crate::ProjectSettingsActionKind::Init,
            }];
            assert_eq!(
                effects.project_settings_actions,
                if opens { init } else { Vec::new() },
                "{status:?}: adding IS initializing, with no page in the way"
            );
            assert_ne!(
                s.app.mode,
                Mode::ProjectSettings,
                "{status:?}: the page is not opened — the dialog is"
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
    fn project_settings_key_palette_actions_confirmation_and_lock_are_one_surface() {
        let mut s = project_settings_state(crate::ProjectInitialization::Initialized);
        let opened = effects_of(update(&mut s, Event::Key(CanopyKey::Char('s'))));
        assert_eq!(s.app.mode, Mode::ProjectSettings);
        assert_eq!(
            opened.data_requests,
            vec![DataRequest::Refresh("/r/a".into())]
        );
        let applied = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert!(matches!(
            applied.project_settings_actions.as_slice(),
            [crate::ProjectSettingsAction {
                kind: crate::ProjectSettingsActionKind::Set {
                    setting: crate::ProjectSettingId::TotalRecall,
                    enabled: true,
                },
                ..
            }]
        ));

        // Raw Off → On pauses for an explicit acknowledgement; its cancel
        // chip and Esc share the same return path.
        s.app.project_settings.cursor = 6;
        let warning = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert!(warning.project_settings_actions.is_empty());
        assert!(s.app.project_settings.confirm_raw);
        let _ = apply_click(&mut s, Target::ProjectSettingsCancel);
        assert!(!s.app.project_settings.confirm_raw);
        assert_eq!(s.app.mode, Mode::ProjectSettings);
        update(&mut s, Event::Key(CanopyKey::Enter));
        let confirmed = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert!(matches!(
            confirmed.project_settings_actions[0].kind,
            crate::ProjectSettingsActionKind::Set {
                setting: crate::ProjectSettingId::RawTranscripts,
                enabled: true
            }
        ));

        // A presented operation owns and locks the modal.
        s.app.project_settings.running = true;
        update(&mut s, Event::Key(CanopyKey::Esc));
        assert_eq!(s.app.mode, Mode::ProjectSettings);
        assert!(effects_of(apply_click(&mut s, Target::ProjectSettingsClose)).is_empty());
        assert_eq!(s.app.mode, Mode::ProjectSettings);

        // `/settings` lands on the same modal after closing it.
        s.app.project_settings.running = false;
        update(&mut s, Event::Key(CanopyKey::Esc));
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for ch in "settings".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(ch)));
        }
        update(&mut s, Event::Key(CanopyKey::Enter));
        assert_eq!(s.app.mode, Mode::ProjectSettings);
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

        let modal = crate::view::modal_rect(s.size);
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

    /// A left mouse event of `kind` at `(col,row)`.
    fn mouse(kind: MouseKind, col: u16, row: u16) -> Event {
        Event::Mouse(MouseInput { kind, col, row })
    }

    #[test]
    fn normal_mode_drag_and_up_are_inert_and_do_not_dirty() {
        // The ~0% idle invariant: outside panel-editor mode, Drag and
        // Up produce the empty effect and never dirty — byte-identical to before.
        let mut s = ready(vec![project("a")]);
        for kind in [
            MouseKind::Drag(MouseButton::Left),
            MouseKind::Up(MouseButton::Left),
            MouseKind::Moved,
        ] {
            s.dirty = false;
            let flow = update(&mut s, mouse(kind, 30, 10));
            assert!(matches!(flow, Flow::Continue(e) if e.is_empty()));
            assert!(!s.dirty, "{kind:?} in normal mode must not dirty");
        }
    }

    #[test]
    fn edit_mode_drag_without_a_live_pickup_is_inert() {
        // In panel-editor mode but with no drag armed, Drag/Up are still inert.
        let mut s = ready(vec![project("a")]);
        crate::input::enter_panel_editor(&mut s.app);
        s.dirty = false;
        let flow = update(&mut s, mouse(MouseKind::Drag(MouseButton::Left), 30, 10));
        assert!(matches!(flow, Flow::Continue(e) if e.is_empty()));
        assert!(!s.dirty, "an edit-mode drag with no pickup must not dirty");
    }

    #[test]
    fn a_grip_press_drag_release_moves_a_unit_and_conserves_the_multiset() {
        let mut s = ready(vec![project("a")]);
        crate::input::enter_panel_editor(&mut s.app);
        // Press a grip → a live unit drag is armed and the unit is picked up.
        let _ = apply_click(
            &mut s,
            Target::GripBar(crate::editor::Cursor { col: 1, unit: 0 }),
        );
        assert!(s.app.editor.picked);
        assert!(matches!(
            s.app.editor.drag,
            Some(crate::editor::Drag::Unit(_))
        ));
        // Drag onto a different grip in the same column, then release → the unit
        // moved and the set is conserved.
        s.app.editor.cursor = crate::editor::Cursor { col: 1, unit: 2 };
        let _ = update(&mut s, mouse(MouseKind::Up(MouseButton::Left), 40, 10));
        assert!(s.app.editor.drag.is_none());
        assert!(!s.app.editor.picked);
        assert!(crate::editor::is_conserved(&s.app.layout));
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

    // --- the startup sweep + the selection pulse ----------------------

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

    /// A period button and its digit key are the same act: both adopt the
    /// window on the model in the frame of the press, both ask the data plane
    /// to recompute over it, and both arm the same press flash.
    ///
    /// Resolved through the pure hit-test, so the click half exercises the
    /// drawn cells rather than a target the test invented.
    #[test]
    fn a_period_button_and_its_digit_key_are_one_act() {
        use crate::col3::RecallWindow;
        for (digit, window) in [
            ('1', RecallWindow::Today),
            ('2', RecallWindow::Last7Days),
            ('3', RecallWindow::Last30Days),
        ] {
            // The click half.
            let mut clicked = ready(vec![project("a")]);
            clicked.app.set_focus(Panel::Graphs);
            let click = click_on(Target::RecallWindow(window), &mut clicked);
            let click_effects = effects_of(update(&mut clicked, Event::Mouse(click)));

            // The key half — the digits are bound only with the graphs column
            // focused, which is where the hint teaches them.
            let mut keyed = ready(vec![project("a")]);
            keyed.app.set_focus(Panel::Graphs);
            let key_effects = effects_of(update(&mut keyed, Event::Key(CanopyKey::Char(digit))));

            assert_eq!(clicked.col3.recall_window, window, "click: {digit}");
            assert_eq!(keyed.col3.recall_window, window, "key: {digit}");
            assert_eq!(
                click_effects.data_requests,
                vec![DataRequest::SetRecallWindow(window)]
            );
            assert_eq!(key_effects.data_requests, click_effects.data_requests);
            assert_eq!(clicked.pressed, Some(Target::RecallWindow(window)));
            assert_eq!(
                keyed.pressed, clicked.pressed,
                "a keyboard activation flashes the same button a click does"
            );
        }
    }

    /// The unit chip and `$` are one act, and it asks the data plane for
    /// nothing: both figures already ride the frame the card is drawing, so a
    /// flip is a rendering choice and a recompute would cost a read to redraw
    /// numbers Canopy is holding.
    #[test]
    fn the_unit_chip_and_the_dollar_key_flip_one_act_without_a_read() {
        use crate::col3::RecallUnit;
        let mut clicked = ready(vec![project("a")]);
        clicked.app.set_focus(Panel::Graphs);
        assert_eq!(clicked.col3.recall_unit, RecallUnit::Tokens, "the default");
        let click = click_on(Target::RecallUnitToggle, &mut clicked);
        let click_effects = effects_of(update(&mut clicked, Event::Mouse(click)));

        let mut keyed = ready(vec![project("a")]);
        keyed.app.set_focus(Panel::Graphs);
        let key_effects = effects_of(update(&mut keyed, Event::Key(CanopyKey::Char('$'))));

        assert_eq!(clicked.col3.recall_unit, RecallUnit::Usd);
        assert_eq!(keyed.col3.recall_unit, RecallUnit::Usd);
        assert!(
            click_effects.data_requests.is_empty(),
            "no read to flip a unit"
        );
        assert!(key_effects.data_requests.is_empty());
        assert_eq!(clicked.pressed, Some(Target::RecallUnitToggle));
        assert_eq!(keyed.pressed, clicked.pressed);

        // And it flips back — a toggle, not a latch.
        update(&mut keyed, Event::Key(CanopyKey::Char('$')));
        assert_eq!(keyed.col3.recall_unit, RecallUnit::Tokens);
    }

    /// Re-selecting the live period is inert beyond the flash: the card is
    /// already showing it, and asking the bus again would still cost a read.
    #[test]
    fn re_selecting_the_live_period_asks_for_no_recompute() {
        use crate::col3::RecallWindow;
        let mut s = ready(vec![project("a")]);
        s.app.set_focus(Panel::Graphs);
        assert_eq!(s.col3.recall_window, RecallWindow::AllTime, "the default");
        let click = click_on(Target::RecallWindow(RecallWindow::AllTime), &mut s);
        let effects = effects_of(update(&mut s, Event::Mouse(click)));
        assert!(effects.data_requests.is_empty());
        assert_eq!(s.col3.recall_window, RecallWindow::AllTime);
    }

    /// The digits are bound where they are hinted and nowhere else — the rail
    /// and the detail column must not swallow them.
    #[test]
    fn the_period_digits_are_bound_only_with_the_graphs_column_focused() {
        use crate::col3::RecallWindow;
        for panel in [Panel::Projects, Panel::Detail] {
            let mut s = ready(vec![project("a")]);
            s.app.set_focus(panel);
            let effects = effects_of(update(&mut s, Event::Key(CanopyKey::Char('1'))));
            assert!(effects.data_requests.is_empty(), "{panel:?}");
            assert_eq!(s.col3.recall_window, RecallWindow::AllTime, "{panel:?}");
            update(&mut s, Event::Key(CanopyKey::Char('$')));
            assert_eq!(
                s.col3.recall_unit,
                crate::col3::RecallUnit::Tokens,
                "`$` is bound with the periods, and only there: {panel:?}"
            );
        }
    }

    /// the priced savings estimate: the chip opens the breakdown as the existing read-only
    /// `Overlay::Results` — full disclosed basis, no new modal surface.
    #[test]
    fn clicking_the_cost_chip_opens_the_breakdown_modal() {
        use crate::col3::{RecallView, SavingsData, UsdRow};
        let mut s = ready(vec![project("a")]);
        let mut col3 = (*s.col3).clone();
        col3.savings = RecallView::Data(SavingsData {
            target_pct: Some(90),
            usd_saved_micros: Some(6_250_000),
            usd_label: Some("~$6.25".to_owned()),
            usd_rows: vec![
                UsdRow {
                    model: "claude-opus-4-8".to_owned(),
                    tokens: 750_000,
                    price_per_mtok_micros: Some(5_000_000),
                    subtotal_micros: Some(3_750_000),
                    rate_label: Some("$5.00/MTok".to_owned()),
                    subtotal_label: Some("~$3.75".to_owned()),
                },
                UsdRow {
                    model: "claude-fable-5".to_owned(),
                    tokens: 250_000,
                    price_per_mtok_micros: Some(10_000_000),
                    subtotal_micros: Some(2_500_000),
                    rate_label: Some("$10.00/MTok".to_owned()),
                    subtotal_label: Some("~$2.50".to_owned()),
                },
            ],
            usd_read_micros: None,
            usd_read_label: None,
            by_worktree: crate::col3::WorktreeAxis::default(),
            share: Box::new(crate::col3::ShareData::default()),
        });
        s.set_col3(col3);
        let _ = apply_click(&mut s, Target::RecallCost);
        let Some(Overlay::Results { title, lines, .. }) = &s.app.overlay else {
            panic!(
                "expected the breakdown Results overlay, got {:?}",
                s.app.overlay
            );
        };
        assert_eq!(title, "recall savings — ~$6.25");
        let body = lines.join("\n");
        assert!(body.contains("net tokens avoided   1,000,000"), "{body}");
        assert!(body.contains("claude-opus-4-8"), "{body}");
        assert!(body.contains("$5.00/MTok"), "{body}");
        assert!(body.contains("~$3.75"), "{body}");
        assert!(body.contains("total = ~$6.25"), "{body}");
        assert!(body.contains("estimate, not a bill"), "{body}");
        assert!(
            body.contains("cl100k_base"),
            "the tokenizer basis is disclosed: {body}"
        );
    }

    /// A Col3 whose token panel carries a total and a two-member axis.
    fn axis_col3() -> crate::col3::Col3 {
        use crate::col3::{
            BreakdownData, BreakdownView, TokensData, TokensView, WorktreeAxis, WorktreeSlice,
        };
        crate::col3::Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0, 2.0],
                total: 1_050.0,
                by_worktree: WorktreeAxis {
                    rows: vec![
                        WorktreeSlice {
                            name: Some("main".to_owned()),
                            tokens: 750,
                        },
                        WorktreeSlice {
                            name: None,
                            tokens: 300,
                        },
                    ],
                    family_only: 0,
                },
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 1.0,
                read: 1.0,
                output: 1.0,
                cache_write: 1.0,
                read_fraction: 0.5,
            }),
            ..crate::col3::Col3::default()
        }
    }

    /// the family view: the axis has two doors — `w` in the graphs column and
    /// a click on the token panel's `Σ` total — and **one** opener behind
    /// them, so they can never show different things — modal-surface parity in
    /// both directions: the hinted key is clickable and the clickable is
    /// keyed.
    #[test]
    fn the_worktree_axis_opens_from_both_doors_through_one_opener() {
        let by_key = {
            let mut s = ready(vec![project("a")]);
            s.set_col3(axis_col3());
            s.app.set_focus(Panel::Graphs);
            let _ = update(&mut s, Event::Key(CanopyKey::Char('w')));
            s
        };
        let by_click = {
            let mut s = ready(vec![project("a")]);
            s.set_col3(axis_col3());
            let _ = apply_click(&mut s, Target::TokensByWorktree);
            s
        };
        let Some(Overlay::Results { title, lines, .. }) = &by_key.app.overlay else {
            panic!("`w` opens the axis, got {:?}", by_key.app.overlay);
        };
        assert_eq!(title, "token usage by worktree");
        assert_eq!(
            Some(Overlay::Results {
                title: title.clone(),
                lines: lines.clone(),
                scroll: 0,
                footer: None,
                truncated: false,
            }),
            by_click.app.overlay,
            "the click's door opens byte-identically to the key's"
        );
        let body = lines.join("\n");
        assert!(body.contains("family total  1,050 tok"), "{body}");
        assert!(body.contains("main     750 tok"), "{body}");
        assert!(body.contains("unknown  300 tok"), "{body}");
        // The hint that teaches `w` is the graphs column's, and it is live.
        assert!(
            crate::keymap::hints(crate::keymap::HintContext::Graphs)
                .iter()
                .any(|h| h.keys == "w" && h.verb == "by worktree"),
            "the key is taught where it is bound"
        );
    }

    /// A member with nothing wired in it, selected, with the card's own detail
    /// carrying the same status — the one premise the adopt act is offered on.
    fn unadopted() -> AppState {
        let mut s = ready(vec![Project {
            name: "member".into(),
            path: "/r/member".into(),
            status: ProjectStatus::NotAdopted,
            kind: EntryKind::Worktree,
        }]);
        s.app.set_focus(Panel::Detail);
        s
    }

    /// ★ the family view + modal-surface parity: the adopt act has two doors — `A` and a
    /// click on the Repository card's status row — and **one** opener behind
    /// them, so they can never do different things.
    ///
    /// Parity in both directions is asserted, not assumed: the hinted key is
    /// clickable, the clickable is keyed, and the hint that teaches `A` is live
    /// in exactly the stage the key is bound in. The request is MUTATING, so
    /// both doors open a confirm rather than dispatching — adoption
    /// writes into a checkout and a stray keystroke may not.
    #[test]
    fn the_adopt_act_opens_from_both_doors_through_one_opener() {
        let by_key = {
            let mut s = unadopted();
            let _ = update(&mut s, Event::Key(CanopyKey::Char('A')));
            s
        };
        let by_click = {
            let mut s = unadopted();
            let _ = apply_click(&mut s, Target::RepositoryAdopt);
            s
        };
        let Some(Overlay::Confirm { request, repo }) = &by_key.app.overlay else {
            panic!("`A` opens the confirm, got {:?}", by_key.app.overlay);
        };
        assert_eq!(*request, DispatchRequest::Adopt);
        assert_eq!(repo, "/r/member");
        assert_eq!(
            by_key.app.overlay, by_click.app.overlay,
            "the click's door opens exactly what the key's does"
        );
        assert!(DispatchRequest::Adopt.is_mutating(), "confirm-gated");
        assert!(Target::RepositoryAdopt.is_button(), "button semantics");

        // The key is taught where it is bound, and NOWHERE else: an adopted
        // row's Detail bar must not advertise it (modal-surface parity's per-stage rule).
        let taught = |ctx| {
            crate::keymap::hints(ctx)
                .iter()
                .any(|h| h.keys == "A" && h.verb == "adopt")
        };
        assert!(taught(crate::keymap::HintContext::DetailNotAdopted));
        assert!(!taught(crate::keymap::HintContext::Detail));
        assert!(!taught(crate::keymap::HintContext::Rail));
        // …and the stage is derived from the same premise the reducer binds on.
        assert_eq!(
            crate::keymap::context(
                Mode::Normal,
                Panel::Detail,
                None,
                &crate::profile_menu::MenuStage::Browse,
                false,
                crate::input::selection_is_unadopted(&unadopted().app),
            ),
            crate::keymap::HintContext::DetailNotAdopted
        );

        // An ADOPTED row swallows the key rather than offering an act its bar
        // does not teach.
        let mut idle = ready(vec![project("a")]);
        idle.app.set_focus(Panel::Detail);
        let _ = update(&mut idle, Event::Key(CanopyKey::Char('A')));
        assert!(idle.app.overlay.is_none(), "unhinted stage, unbound key");
    }

    /// The two typed palette verbs the family view adds, parsed and routed:
    /// `/worktrees` reads in process, `/adopt` mutates and therefore confirms.
    #[test]
    fn the_palette_routes_worktrees_and_adopt_to_their_tiers() {
        use crate::command::{parse, Tier, Verb};
        assert_eq!(parse("/worktrees"), Ok(Verb::Worktrees));
        assert_eq!(parse("/adopt"), Ok(Verb::Adopt));
        assert_eq!(Verb::Worktrees.tier(), Tier::ReadOnly);
        assert_eq!(Verb::Adopt.tier(), Tier::Mutating);
        assert_eq!(
            crate::dispatch::request_for(&Verb::Worktrees),
            Some(DispatchRequest::Worktrees)
        );
        assert_eq!(
            crate::dispatch::request_for(&Verb::Adopt),
            Some(DispatchRequest::Adopt)
        );

        // `/worktrees` runs at once; `/adopt` stops at the confirm.
        let mut s = ready(vec![project("a")]);
        let e = effects_of(run_line(&mut s, "/worktrees"));
        assert_eq!(
            e.dispatches,
            vec![(DispatchRequest::Worktrees, "/r/a".to_owned())]
        );
        let mut s = ready(vec![project("a")]);
        let e = effects_of(run_line(&mut s, "/adopt"));
        assert!(e.dispatches.is_empty(), "a mutation asks first");
        assert!(matches!(s.app.overlay, Some(Overlay::Confirm { .. })));
        // …and the confirm's Enter is what dispatches it.
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.dispatches,
            vec![(DispatchRequest::Adopt, "/r/a".to_owned())]
        );
    }

    /// A hinted key is never dead (modal-surface parity): with nothing in scope the axis
    /// still opens and says so, rather than swallowing the keystroke.
    #[test]
    fn the_axis_key_opens_an_honest_statement_when_there_is_nothing_to_split() {
        let mut s = ready(vec![project("a")]);
        s.app.set_focus(Panel::Graphs);
        let _ = update(&mut s, Event::Key(CanopyKey::Char('w')));
        let Some(Overlay::Results { lines, .. }) = &s.app.overlay else {
            panic!("expected the axis overlay, got {:?}", s.app.overlay);
        };
        assert!(lines.join(" ").contains("no token facts in scope"));
        // …and `w` is bound only where it is hinted: the rail keeps it inert.
        let mut rail = ready(vec![project("a")]);
        rail.app.set_focus(Panel::Projects);
        let _ = update(&mut rail, Event::Key(CanopyKey::Char('w')));
        assert!(rail.app.overlay.is_none(), "unhinted column, unbound key");
    }

    /// ★ the family view: "No new modal surface is introduced — the axis
    /// lives in the existing `Overlay::Results`."
    ///
    /// The inventory is asserted rather than assumed. The two exhaustive
    /// matches below are the guard: adding an `Overlay` variant or a `Mode`
    /// stops this test compiling, which forces whoever adds one to classify
    /// it here and — if it is modal — to join it to
    /// `every_modal_surface_carries_the_close_affordance`, `keymap`'s
    /// `ALL`/`app_in`, and `Target::is_button` (modal-surface parity). The counts name
    /// what the classification found, so a silent addition cannot pass.
    #[test]
    fn no_new_modal_surface_was_introduced() {
        fn overlay_kind(o: &Overlay) -> &'static str {
            match o {
                Overlay::Results { .. } => "results",
                Overlay::Confirm { .. } => "confirm",
                Overlay::Running { .. } => "running",
                Overlay::Alert(_) => "alert",
                Overlay::Notice(_) => "notice",
            }
        }
        fn draws_a_modal_frame(m: Mode) -> bool {
            match m {
                Mode::ProfileMenu | Mode::ColourPicker | Mode::Explorer | Mode::ProjectSettings => {
                    true
                }
                Mode::Normal | Mode::Command | Mode::AddProject | Mode::PanelEditor => false,
            }
        }
        const OVERLAY_KINDS: [&str; 5] = ["results", "confirm", "running", "alert", "notice"];
        const MODAL_MODES: [Mode; 4] = [
            Mode::ProfileMenu,
            Mode::ColourPicker,
            Mode::Explorer,
            Mode::ProjectSettings,
        ];
        assert_eq!(
            OVERLAY_KINDS.len(),
            5,
            "five overlay kinds before this change, five after"
        );
        assert!(MODAL_MODES.into_iter().all(draws_a_modal_frame));
        assert_eq!(
            MODAL_MODES.len(),
            4,
            "four mode-modals, and every one of them is in the close-X sweep"
        );
        // The axis reuses one of them, from both doors, and disturbs no mode.
        for open in [
            &(|s: &mut AppState| {
                s.app.set_focus(Panel::Graphs);
                let _ = update(s, Event::Key(CanopyKey::Char('w')));
            }) as &dyn Fn(&mut AppState),
            &|s: &mut AppState| {
                let _ = apply_click(s, Target::TokensByWorktree);
            },
        ] {
            let mut s = ready(vec![project("a")]);
            s.set_col3(axis_col3());
            open(&mut s);
            assert_eq!(
                s.app.overlay.as_ref().map(overlay_kind),
                Some("results"),
                "the axis is a results overlay — a surface that already existed"
            );
            assert_eq!(s.app.mode, Mode::Normal, "and it opens no mode-modal");
            // Its hints are the existing results table, so no hint context
            // was minted for it either.
            assert_eq!(
                crate::keymap::context(
                    s.app.mode,
                    s.app.focus,
                    s.app.overlay.as_ref(),
                    &s.app.profile_menu.stage,
                    s.app.project_settings.running,
                    crate::input::selection_is_unadopted(&s.app),
                ),
                crate::keymap::HintContext::OverlayResults
            );
        }
    }

    /// The breakdown **always** opens and says what is absent, through either
    /// chip or through `S`.
    ///
    /// It used to open nothing without a dollar figure, which was defensible
    /// while the chips were the only doors — there was no chip to click. A
    /// hinted key cannot work that way: a key that sometimes does nothing is
    /// a dead key some of the time, so the modal answers on a card with no
    /// savings by saying it has none (the worktree axis's rule, and the same
    /// wording discipline: absence is stated, never drawn as a zero).
    #[test]
    fn the_savings_modal_answers_even_with_nothing_to_break_down() {
        let by_click = |s: &mut AppState| {
            let _ = apply_click(s, Target::RecallCost);
        };
        let by_key = |s: &mut AppState| {
            s.app.set_focus(crate::Panel::Graphs);
            let _ = update(s, Event::Key(CanopyKey::Char('S')));
        };
        let doors: [&dyn Fn(&mut AppState); 2] = [&by_click, &by_key];
        for door in doors {
            let mut s = ready(vec![project("a")]);
            door(&mut s);
            let Some(Overlay::Results { title, lines, .. }) = &s.app.overlay else {
                panic!("the breakdown opens from every door");
            };
            assert_eq!(title, "recall savings");
            assert!(
                lines.iter().any(|l| l.contains("recall is not enabled")),
                "it says what is absent: {lines:?}"
            );
        }
    }

    #[test]
    fn a_button_press_flashes_and_re_presses_re_arm() {
        let mut s = ready(vec![project("a")]);
        // A button press arms the flash keyed by its target.
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

    // --- the activity + metrics feeds ------------------------------

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
    fn metrics_feed_patches_col3_metrics_without_a_tween() {
        use crate::col3::PanelMetrics;
        let mut s = ready(vec![project("a")]);
        let metrics = PanelMetrics {
            events_5m: Some(3),
            tokens_5m: Some(600),
            serve_calls_5m: Some(2),
            projection_bytes: Some(4096),
            activity_scope: crate::col3::SystemScope::CurrentSessions,
            ..PanelMetrics::default()
        };
        update(
            &mut s,
            Event::Feed(FeedUpdate {
                project: "/r/a".into(),
                payload: FeedPayload::System {
                    context: crate::col3::NicView::NoData,
                    metrics: metrics.clone(),
                    active_sessions: crate::col3::ActiveSessionsView::NoSessions,
                },
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

    // --- the data-plane health dot ------------------------------------

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
        // `a` browses (the destination explorer); `i` falls back to the typed flow, then a
        // path and Enter — the same funnel the explorer's select reuses.
        update(&mut s, Event::Key(CanopyKey::Char('a')));
        update(&mut s, Event::Key(CanopyKey::Char('i')));
        assert_eq!(s.app.mode, Mode::AddProject);
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

    // ---- the profile plane's state flows ------------------------------------

    /// A ready state with three live profiles wired (active = `pf0`).
    fn ready_with_profiles(projects: Vec<crate::Project>) -> AppState {
        let mut s = ready(projects);
        s.app.set_profiles(
            ["default", "work", "lab"]
                .iter()
                .enumerate()
                .map(|(i, name)| crate::profile::ProfileEntry {
                    profile_id: format!("pf{i}"),
                    name: (*name).to_string(),
                    created_at: i64::try_from(i).unwrap(),
                })
                .collect(),
            "pf0".into(),
        );
        s
    }

    /// Unwrap a `Flow::Continue`'s effects.
    fn effects_of(flow: Flow) -> Effects {
        match flow {
            Flow::Continue(e) => e,
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn wired_remove_and_reorder_are_profile_scoped() {
        // Story 8: a column-1 remove/reorder touches only the active profile's
        // view — the catalogue op is the unwired fallback, not the default.
        let mut s = ready_with_profiles(vec![project("a"), project("b")]);
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Char('d'))));
        assert_eq!(
            e.persist,
            vec![PersistOp::RemoveProfileProject {
                profile: "pf0".into(),
                path: "/r/a".into()
            }]
        );
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::ShiftDown)));
        assert_eq!(
            e.persist,
            vec![PersistOp::ReorderProfileProject {
                profile: "pf0".into(),
                path: "/r/a".into(),
                up: false
            }]
        );
    }

    #[test]
    fn menu_switch_emits_select_plus_reload_and_closes() {
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::SelectProfile {
                profile: "pf1".into()
            }]
        );
        assert!(e.reload_profiles, "the whole view reloads on a switch");
        assert_eq!(s.app.mode, Mode::Normal, "the menu closed");
    }

    #[test]
    fn switching_to_the_active_profile_just_closes_the_menu() {
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert!(e.persist.is_empty());
        assert!(!e.reload_profiles);
        assert_eq!(s.app.mode, Mode::Normal);
    }

    #[test]
    fn menu_create_rename_delete_emit_their_ops_with_reload() {
        // Create.
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('n')));
        for c in "client".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::CreateProfile {
                name: "client".into()
            }]
        );
        assert!(e.reload_profiles);

        // Rename — through the settings page now (the profile hub): the Name row
        // opens the prefilled input; append a char.
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        let _ = update(&mut s, Event::Key(CanopyKey::Enter)); // Name row opens
        update(&mut s, Event::Key(CanopyKey::Char('2')));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::RenameProfile {
                profile: "pf1".into(),
                name: "work2".into()
            }]
        );
        assert!(e.reload_profiles);

        // Delete, through the inline confirm.
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        update(&mut s, Event::Key(CanopyKey::Char('d')));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::RemoveProfile {
                profile: "pf1".into()
            }]
        );
        assert!(e.reload_profiles);
    }

    /// Type a slash line in command mode and press Enter.
    fn run_palette(s: &mut AppState, line: &str) -> Effects {
        update(s, Event::Key(CanopyKey::Char('/')));
        for c in line.chars() {
            update(s, Event::Key(CanopyKey::Char(c)));
        }
        effects_of(update(s, Event::Key(CanopyKey::Enter)))
    }

    #[test]
    fn profile_palette_verbs_map_onto_the_menu_ops() {
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile switch lab");
        assert_eq!(
            e.persist,
            vec![PersistOp::SelectProfile {
                profile: "pf2".into()
            }]
        );
        assert!(e.reload_profiles);

        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile new client");
        assert_eq!(
            e.persist,
            vec![PersistOp::CreateProfile {
                name: "client".into()
            }]
        );

        // Rename targets the ACTIVE profile.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile rename main");
        assert_eq!(
            e.persist,
            vec![PersistOp::RenameProfile {
                profile: "pf0".into(),
                name: "main".into()
            }]
        );

        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile delete lab");
        assert_eq!(
            e.persist,
            vec![PersistOp::RemoveProfile {
                profile: "pf2".into()
            }]
        );

        // Duplicate clones the ACTIVE profile whole.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile duplicate spare");
        assert_eq!(
            e.persist,
            vec![PersistOp::DuplicateProfile {
                source: "pf0".into(),
                name: "spare".into()
            }]
        );
        assert!(e.reload_profiles);

        // Startup pins by name; the bare verb unpins.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile startup lab");
        assert_eq!(
            e.persist,
            vec![PersistOp::SetStartupProfile {
                profile: Some("pf2".into())
            }]
        );
        let e = run_palette(&mut s, "profile startup");
        assert_eq!(
            e.persist,
            vec![PersistOp::SetStartupProfile { profile: None }]
        );
    }

    #[test]
    fn profile_palette_guards_and_misses_alert_softly() {
        // Deleting the active profile is refused with a warning Alert.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile delete default");
        assert!(e.persist.is_empty());
        assert!(matches!(s.app.overlay, Some(Overlay::Alert(_))));

        // An unknown name is a soft miss.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile switch ghost");
        assert!(e.persist.is_empty());
        assert!(matches!(s.app.overlay, Some(Overlay::Alert(_))));

        // A duplicate new-name is refused.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile new work");
        assert!(e.persist.is_empty());
        assert!(matches!(s.app.overlay, Some(Overlay::Alert(_))));

        // A duplicate's colliding clone-name is refused the same way…
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile duplicate work");
        assert!(e.persist.is_empty());
        assert!(matches!(s.app.overlay, Some(Overlay::Alert(_))));

        // …and pinning an unknown name is a soft miss.
        let mut s = ready_with_profiles(vec![project("a")]);
        let e = run_palette(&mut s, "profile startup ghost");
        assert!(e.persist.is_empty());
        assert!(matches!(s.app.overlay, Some(Overlay::Alert(_))));
    }

    #[test]
    fn identity_click_toggles_the_menu_and_row_click_switches() {
        let mut s = ready_with_profiles(vec![project("a")]);
        let _ = apply_click(&mut s, Target::HeaderProfile);
        assert_eq!(s.app.mode, Mode::ProfileMenu);
        let _ = apply_click(&mut s, Target::HeaderProfile);
        assert_eq!(s.app.mode, Mode::Normal, "a second click closes");

        let _ = apply_click(&mut s, Target::HeaderProfile);
        let e = effects_of(apply_click(&mut s, Target::ProfileRow(2)));
        assert_eq!(
            e.persist,
            vec![PersistOp::SelectProfile {
                profile: "pf2".into()
            }]
        );
        assert!(e.reload_profiles);
        assert_eq!(s.app.mode, Mode::Normal, "click-to-activate closes");
    }

    #[test]
    fn snapshot_carries_the_profile_plane() {
        let mut s = ready_with_profiles(vec![project("a")]);
        let vm = s.snapshot();
        assert_eq!(vm.profile_name.as_deref(), Some("default"));
        assert_eq!(vm.profiles.len(), 3);
        assert_eq!(vm.active_profile, "pf0");
    }

    #[test]
    fn settings_opens_from_key_chip_and_enter_dispatches_rows() {
        use crate::profile_menu::MenuStage;
        // `s` opens the cursor row's settings page…
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::Settings {
                profile: "pf1".into()
            }
        );
        // …Esc steps back to the hub…
        update(&mut s, Event::Key(CanopyKey::Esc));
        assert_eq!(s.app.profile_menu.stage, MenuStage::Browse);
        assert_eq!(s.app.mode, Mode::ProfileMenu);
        // …and the row chip is the click twin (modal-surface parity).
        let click = click_on(Target::ProfileSettings(2), &mut s);
        let e = effects_of(update(&mut s, Event::Mouse(click)));
        assert!(e.persist.is_empty());
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::Settings {
                profile: "pf2".into()
            },
            "the chip opens ITS row's settings page"
        );
    }

    #[test]
    fn duplicate_from_settings_names_then_emits_full_clone_op() {
        use crate::profile_menu::{MenuStage, SettingsRow};
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j'))); // a NON-active row
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        // Cursor to the Duplicate row, activate, type the clone's name.
        let dup = SettingsRow::ALL
            .iter()
            .position(|r| matches!(r, SettingsRow::Duplicate))
            .unwrap();
        for _ in 0..dup {
            update(&mut s, Event::Key(CanopyKey::Char('j')));
        }
        update(&mut s, Event::Key(CanopyKey::Enter));
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::NamingDuplicate {
                source: "pf1".into()
            }
        );
        for c in "work-copy".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::DuplicateProfile {
                source: "pf1".into(),
                name: "work-copy".into()
            }],
            "the clone op carries the SETTINGS page's profile as its source"
        );
        assert!(e.reload_profiles);
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::Browse,
            "a landed duplicate returns to the hub (the reducer never learns the minted id)"
        );
    }

    #[test]
    fn startup_toggle_pins_and_unpins() {
        use crate::profile_menu::SettingsRow;
        let pin_row = SettingsRow::ALL
            .iter()
            .position(|r| matches!(r, SettingsRow::StartupPin))
            .unwrap();
        // Unpinned → the toggle pins the page's profile.
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        for _ in 0..pin_row {
            update(&mut s, Event::Key(CanopyKey::Char('j')));
        }
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::SetStartupProfile {
                profile: Some("pf1".into())
            }]
        );
        assert!(e.reload_profiles);
        // Already the pin → the same act unpins (NULL).
        s.app.startup_profile = Some("pf1".into());
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::SetStartupProfile { profile: None }]
        );
    }

    #[test]
    fn delete_from_settings_confirms_then_returns_to_browse() {
        use crate::profile_menu::{MenuStage, SettingsRow};
        let del_row = SettingsRow::ALL
            .iter()
            .position(|r| matches!(r, SettingsRow::Delete))
            .unwrap();
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        for _ in 0..del_row {
            update(&mut s, Event::Key(CanopyKey::Char('j')));
        }
        update(&mut s, Event::Key(CanopyKey::Enter));
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::ConfirmDelete {
                profile: "pf1".into(),
                back_to_settings: true
            }
        );
        // Esc cancels BACK to the settings page it came from…
        update(&mut s, Event::Key(CanopyKey::Esc));
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::Settings {
                profile: "pf1".into()
            }
        );
        // …while a landed delete returns to the hub (the page's profile is gone).
        update(&mut s, Event::Key(CanopyKey::Enter));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::RemoveProfile {
                profile: "pf1".into()
            }]
        );
        assert_eq!(s.app.profile_menu.stage, MenuStage::Browse);
    }

    #[test]
    fn members_editor_removes_reorders_and_adds_for_a_non_active_profile() {
        use crate::profile_menu::{MenuStage, SettingsRow};
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j'))); // a NON-active row
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        let projects_row = SettingsRow::ALL
            .iter()
            .position(|r| matches!(r, SettingsRow::Projects))
            .unwrap();
        for _ in 0..projects_row {
            update(&mut s, Event::Key(CanopyKey::Char('j')));
        }
        // Entering requests the lists through the seam (the reducer never
        // reads a store).
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(e.load_membership.as_deref(), Some("pf1"));
        assert!(e.persist.is_empty());
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::Members {
                profile: "pf1".into()
            }
        );
        // The loop lands the lists (simulated here); every op then carries
        // the EDITED profile's id and re-requests the read.
        s.app.set_membership_lists(
            vec![project("m1"), project("m2")],
            vec![project("m1"), project("m2"), project("free")],
        );
        update(&mut s, Event::Key(CanopyKey::Char('j'))); // cursor → m2
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Char('d'))));
        assert_eq!(
            e.persist,
            vec![PersistOp::RemoveProfileProject {
                profile: "pf1".into(),
                path: "/r/m2".into()
            }]
        );
        assert_eq!(e.load_membership.as_deref(), Some("pf1"));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::ShiftUp)));
        assert_eq!(
            e.persist,
            vec![PersistOp::ReorderProfileProject {
                profile: "pf1".into(),
                path: "/r/m2".into(),
                up: true
            }]
        );
        // The add stage lists only NON-members; Enter includes the candidate.
        update(&mut s, Event::Key(CanopyKey::Char('a')));
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::MembersAdd {
                profile: "pf1".into()
            }
        );
        assert_eq!(
            s.app
                .profile_menu
                .candidates()
                .iter()
                .map(|p| p.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/r/free"],
            "candidates are the catalogue minus the members"
        );
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::AddProfileProject {
                profile: "pf1".into(),
                path: "/r/free".into()
            }]
        );
        assert_eq!(e.load_membership.as_deref(), Some("pf1"));
        // Esc steps back: add → members → settings.
        update(&mut s, Event::Key(CanopyKey::Esc));
        assert!(matches!(
            s.app.profile_menu.stage,
            MenuStage::Members { .. }
        ));
        update(&mut s, Event::Key(CanopyKey::Esc));
        assert!(matches!(
            s.app.profile_menu.stage,
            MenuStage::Settings { .. }
        ));
    }

    #[test]
    fn members_chips_mirror_the_keyboard() {
        use crate::profile_menu::MenuStage;
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        s.app.profile_menu.stage = MenuStage::Members {
            profile: "pf1".into(),
        };
        s.app.set_membership_lists(
            vec![project("m1"), project("m2")],
            vec![project("m1"), project("m2"), project("free")],
        );
        // [remove] acts on the cursor row, exactly like `d` (modal-surface parity).
        let e = effects_of(apply_click(&mut s, Target::MemberRemove));
        assert_eq!(
            e.persist,
            vec![PersistOp::RemoveProfileProject {
                profile: "pf1".into(),
                path: "/r/m1".into()
            }]
        );
        // A row click selects; [↓] then reorders THAT row.
        let e = effects_of(apply_click(&mut s, Target::MemberRow(1)));
        assert!(e.is_empty());
        let e = effects_of(apply_click(&mut s, Target::MemberDown));
        assert_eq!(
            e.persist,
            vec![PersistOp::ReorderProfileProject {
                profile: "pf1".into(),
                path: "/r/m2".into(),
                up: false
            }]
        );
        // [ add ] opens the add stage; a candidate click includes it.
        let e = effects_of(apply_click(&mut s, Target::MemberAddOpen));
        assert!(e.is_empty());
        assert!(matches!(
            s.app.profile_menu.stage,
            MenuStage::MembersAdd { .. }
        ));
        let e = effects_of(apply_click(&mut s, Target::MemberAddRow(0)));
        assert_eq!(
            e.persist,
            vec![PersistOp::AddProfileProject {
                profile: "pf1".into(),
                path: "/r/free".into()
            }]
        );
    }

    #[test]
    fn picker_for_a_non_active_profile_persists_without_retheming() {
        use crate::profile_menu::SettingsRow;
        use crate::theme::{ThemeSlot, PRESETS};
        let mut s = ready_with_profiles(vec![project("a")]);
        let before = s.theme();
        // Open the NON-active pf1's settings → Colours.
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        let colours_row = SettingsRow::ALL
            .iter()
            .position(|r| matches!(r, SettingsRow::Colours))
            .unwrap();
        for _ in 0..colours_row {
            update(&mut s, Event::Key(CanopyKey::Char('j')));
        }
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.load_picker_theme.as_deref(),
            Some("pf1"),
            "a non-active target's swatches load through the seam"
        );
        assert_eq!(s.app.colour_picker.target_profile, "pf1");
        // Apply a preset: the event carries pf1; the LIVE theme is untouched;
        // the picker's own swatches update purely.
        update(&mut s, Event::Key(CanopyKey::Enter)); // background's presets
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter))); // red
        assert_eq!(
            e.persist,
            vec![PersistOp::SetProfileTheme {
                profile: "pf1".into(),
                slot: "background".into(),
                colour: crate::theme::colour_to_hex(PRESETS[0].1),
            }]
        );
        assert_eq!(
            s.theme(),
            before,
            "editing a non-active profile never re-themes the shown view"
        );
        assert_eq!(
            s.app
                .colour_picker
                .target_overrides
                .get(ThemeSlot::Background),
            Some(PRESETS[0].1),
            "the picker's swatches stay honest without a reload"
        );
    }

    #[test]
    fn picker_close_x_returns_to_the_menu() {
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        update(&mut s, Event::Key(CanopyKey::Char('j'))); // → Colours
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        assert_eq!(s.app.mode, Mode::ColourPicker);
        // The `[x]` closes the PICKER — back to the menu it opened from,
        // its settings stage intact (modal-surface parity).
        let e = effects_of(apply_click(&mut s, Target::PickerClose));
        assert!(e.is_empty());
        assert_eq!(s.app.mode, Mode::ProfileMenu);
        assert!(matches!(
            s.app.profile_menu.stage,
            crate::profile_menu::MenuStage::Settings { .. }
        ));
    }

    // One stanza per modal surface: the length IS the inventory (modal-surface
    // parity), and shortening it by dropping a surface is the failure this test
    // exists to prevent.
    #[allow(clippy::too_many_lines)]
    #[test]
    fn every_modal_surface_carries_the_close_affordance() {
        use crate::profile_menu::MenuStage;
        // modal-surface parity: every mode drawn as a modal carries the close `[x]` —
        // the pointer twin of Esc — resolved through the pure hit-test and
        // closing through the MODE's own path. (The Overlay shell's X is
        // proven by the modal focus-and-close rule suite; this sweep covers the mode-modals, so
        // a future stage that forgets its X turns CI red.)
        let stages = vec![
            MenuStage::Browse,
            MenuStage::Settings {
                profile: "pf0".into(),
            },
            MenuStage::Members {
                profile: "pf0".into(),
            },
            MenuStage::MembersAdd {
                profile: "pf0".into(),
            },
            MenuStage::NamingCreate,
            MenuStage::ConfirmDelete {
                profile: "pf1".into(),
                back_to_settings: false,
            },
        ];
        for stage in stages {
            let mut s = ready_with_profiles(vec![project("a")]);
            update(&mut s, Event::Key(CanopyKey::Char('p')));
            s.app.profile_menu.stage = stage.clone();
            let vm = s.snapshot();
            let cr = crate::view::modal_close_rect(crate::view::modal_rect(vm.size)).unwrap();
            assert_eq!(
                crate::hit::hit_test(&vm, cr.x, cr.y),
                Some(Target::ProfileMenuClose),
                "menu stage {stage:?} lost its close X"
            );
            let _ = apply_click(&mut s, Target::ProfileMenuClose);
            assert_eq!(s.app.mode, Mode::Normal, "the X closes from {stage:?}");
        }
        for picker_stage in [
            crate::colour_picker::PickerStage::Slots,
            crate::colour_picker::PickerStage::Presets,
        ] {
            let mut s = ready_with_profiles(vec![project("a")]);
            update(&mut s, Event::Key(CanopyKey::Char('p')));
            update(&mut s, Event::Key(CanopyKey::Char('s')));
            update(&mut s, Event::Key(CanopyKey::Char('j'))); // → Colours
            let _ = update(&mut s, Event::Key(CanopyKey::Enter));
            s.app.colour_picker.stage = picker_stage;
            let vm = s.snapshot();
            let cr = crate::view::modal_close_rect(crate::view::modal_rect(vm.size)).unwrap();
            assert_eq!(
                crate::hit::hit_test(&vm, cr.x, cr.y),
                Some(Target::PickerClose),
                "picker stage {picker_stage:?} lost its close X"
            );
            let _ = apply_click(&mut s, Target::PickerClose);
            assert_eq!(
                s.app.mode,
                Mode::ProfileMenu,
                "the picker's X returns to the menu"
            );
        }
        // The destination explorer, driven through REAL entry —
        // and in both its visual states, because the X must survive every one.
        //
        // modal-surface parity, for whoever adds the next modal surface: registering
        // here is a DUTY, not a courtesy. The draw and hit `if vm.mode ==`
        // chains compile clean when a surface is forgotten, and these
        // hand-listed fixtures — this sweep, `keymap`'s `ALL`/`app_in`/
        // `fingerprint`, `Target::is_button`, and the `click_on` mirrors — are
        // the only things that turn CI red when it is.
        for with_entries in [true, false] {
            let mut s = ready(vec![project("a")]);
            s.app.home_dir = Some("/home/dev".into());
            let e = effects_of(update(&mut s, Event::Key(CanopyKey::Char('a'))));
            assert_eq!(
                e.list_dir,
                Some(std::path::PathBuf::from("/home/dev")),
                "entry requests the start listing on the way in"
            );
            s.app.explorer.apply(if with_entries {
                home_listing()
            } else {
                crate::explorer::DirListing {
                    path: std::path::PathBuf::from("/home/dev"),
                    entries: vec![],
                    omitted: 0,
                    error: Some("permission denied".into()),
                }
            });
            let vm = s.snapshot();
            let cr = crate::view::modal_close_rect(crate::view::modal_rect(vm.size)).unwrap();
            assert_eq!(
                crate::hit::hit_test(&vm, cr.x, cr.y),
                Some(Target::ExplorerClose),
                "the explorer lost its close X (entries={with_entries})"
            );
            let _ = apply_click(&mut s, Target::ExplorerClose);
            assert_eq!(s.app.mode, Mode::Normal, "the X closes the MODE");
            assert!(s.app.overlay.is_none(), "…and never touches the overlay");
            assert_eq!(
                s.app.explorer,
                crate::explorer::ExplorerState::default(),
                "closing rests the state"
            );
        }
        // Project Settings — the surface this sweep OMITTED (the family view
        // joins it). Driven through real entry (`s` on the rail), in the two
        // states it accepts acts in: the plain list, and the raw-transcript
        // acknowledgement, which puts its own Cancel chip on the same frame.
        for (label, arm) in [
            ("plain", &(|_: &mut AppState| {}) as &dyn Fn(&mut AppState)),
            ("confirm-raw", &|s: &mut AppState| {
                s.app.project_settings.confirm_raw = true;
            }),
        ] {
            let mut s = project_settings_state(crate::ProjectInitialization::Initialized);
            let _ = update(&mut s, Event::Key(CanopyKey::Char('s')));
            assert_eq!(s.app.mode, Mode::ProjectSettings, "real entry ({label})");
            arm(&mut s);
            let vm = s.snapshot();
            let cr = crate::view::modal_close_rect(crate::view::modal_rect(vm.size)).unwrap();
            assert_eq!(
                crate::hit::hit_test(&vm, cr.x, cr.y),
                Some(Target::ProjectSettingsClose),
                "project settings lost its close X ({label})"
            );
            let _ = apply_click(&mut s, Target::ProjectSettingsClose);
            assert_eq!(s.app.mode, Mode::Normal, "the X closes the MODE ({label})");
            assert!(s.app.overlay.is_none(), "…and never touches the overlay");
        }
        // Its third state is the one exception, and it is symmetric rather
        // than missing: while an operation is in flight the modal is FROZEN
        // — `apply_click` refuses every target and `on_key_project_settings`
        // returns `Action::None` — so no act exists by either door, and the
        // hint bar now teaches none (`HintContext::ProjectSettingsRunning`).
        // That is modal-surface parity satisfied, not waived; the X's cells stay resolved
        // so the parity returns with the operation.
        {
            let mut s = project_settings_state(crate::ProjectInitialization::Initialized);
            let _ = update(&mut s, Event::Key(CanopyKey::Char('s')));
            s.app.project_settings.running = true;
            let vm = s.snapshot();
            let cr = crate::view::modal_close_rect(crate::view::modal_rect(vm.size)).unwrap();
            assert_eq!(
                crate::hit::hit_test(&vm, cr.x, cr.y),
                Some(Target::ProjectSettingsClose),
                "the X keeps its cells while running"
            );
            let _ = apply_click(&mut s, Target::ProjectSettingsClose);
            assert_eq!(s.app.mode, Mode::ProjectSettings, "the click is refused");
            let _ = update(&mut s, Event::Key(CanopyKey::Esc));
            assert_eq!(
                s.app.mode,
                Mode::ProjectSettings,
                "and so is the key — the freeze is symmetric"
            );
            assert!(
                crate::keymap::hints(crate::keymap::HintContext::ProjectSettingsRunning).is_empty(),
                "a frozen surface teaches nothing"
            );
        }
    }

    // ---- the destination explorer -------------------------------------------

    fn dir(name: &str) -> crate::explorer::ExplorerEntry {
        crate::explorer::ExplorerEntry {
            name: name.into(),
            is_dir: true,
            tag: None,
        }
    }

    fn file(name: &str) -> crate::explorer::ExplorerEntry {
        crate::explorer::ExplorerEntry {
            name: name.into(),
            is_dir: false,
            tag: None,
        }
    }

    /// The explorer's fixture listing of `/home/dev` (never a filesystem read).
    fn home_listing() -> crate::explorer::DirListing {
        crate::explorer::DirListing {
            path: std::path::PathBuf::from("/home/dev"),
            entries: vec![dir(".config"), dir("alpha"), dir("beta"), file("notes.md")],
            omitted: 0,
            error: None,
        }
    }

    /// A dashboard browsing `/home/dev` over [`home_listing`].
    fn browsing() -> AppState {
        let mut s = ready(vec![project("a")]);
        s.app.home_dir = Some("/home/dev".into());
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Char('a'))));
        assert_eq!(e.list_dir, Some(std::path::PathBuf::from("/home/dev")));
        s.app.explorer.apply(home_listing());
        s
    }

    #[test]
    fn an_explorer_list_action_folds_to_the_lister_seam() {
        // the destination explorer: the reducer never reads the filesystem — it requests.
        let path = std::path::PathBuf::from("/home/dev/alpha");
        let e = effects_of(fold_action(Action::ExplorerList(path.clone())));
        assert_eq!(e.list_dir, Some(path));
        assert!(!e.is_empty(), "a listing request is real work for the loop");
        assert!(e.persist.is_empty() && e.data_requests.is_empty());
    }

    #[test]
    fn every_add_project_affordance_arrives_through_the_one_door() {
        // the destination explorer: `a`, the rail `+`, and both twins of the wizard primary
        // land in Mode::Explorer requesting the SAME start directory.
        let start = Some(std::path::PathBuf::from("/home/dev"));

        let mut s = ready(vec![project("a")]);
        s.app.home_dir = Some("/home/dev".into());
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Char('a'))));
        assert_eq!((s.app.mode, e.list_dir), (Mode::Explorer, start.clone()));

        let mut s = ready(vec![project("a")]);
        s.app.home_dir = Some("/home/dev".into());
        let m = click_on(Target::RailAdd, &mut s);
        let e = effects_of(update(&mut s, Event::Mouse(m)));
        assert_eq!((s.app.mode, e.list_dir), (Mode::Explorer, start.clone()));

        // The wizard, by key and by click, on an empty registry.
        for by_click in [false, true] {
            let mut s = AppState::new(vec![], Size::new(120, 40));
            s.app.splash.skip();
            s.app.home_dir = Some("/home/dev".into());
            assert!(s.app.showing_wizard());
            let e = if by_click {
                let m = click_on(Target::WizardAdd, &mut s);
                effects_of(update(&mut s, Event::Mouse(m)))
            } else {
                effects_of(update(&mut s, Event::Key(CanopyKey::Enter)))
            };
            assert_eq!(s.app.mode, Mode::Explorer, "by_click={by_click}");
            assert_eq!(e.list_dir, start.clone(), "by_click={by_click}");
            assert!(s.app.overlay.is_none(), "the wizard's overlay dropped");
        }
    }

    #[test]
    fn explorer_clicks_mirror_their_keys() {
        // modal-surface parity: every act the hints teach is reachable by click, and every
        // click is a key's twin — proven through the PURE hit-test (`click_on`
        // finds the drawn cell), so a solver drift shows up here.
        //
        // A directory-row click ≡ cursor + Enter.
        let mut s = browsing();
        let m = click_on(Target::ExplorerRow(1), &mut s); // → beta
        let e = effects_of(update(&mut s, Event::Mouse(m)));
        assert_eq!(e.list_dir, Some(std::path::PathBuf::from("/home/dev/beta")));
        let mut keyed = browsing();
        update(&mut keyed, Event::Key(CanopyKey::Char('j')));
        let ek = effects_of(update(&mut keyed, Event::Key(CanopyKey::Enter)));
        assert_eq!(e.list_dir, ek.list_dir, "row-click ≡ Enter");

        // The back row ≡ Backspace.
        let mut s = browsing();
        let m = click_on(Target::ExplorerUp, &mut s);
        let e = effects_of(update(&mut s, Event::Mouse(m)));
        assert_eq!(e.list_dir, Some(std::path::PathBuf::from("/home")));

        // The select chip ≡ `s` — both persist the OPEN folder and land in
        // Normal (so the success notice reaches the dashboard).
        let mut s = browsing();
        let m = click_on(Target::ExplorerSelect, &mut s);
        let e = effects_of(update(&mut s, Event::Mouse(m)));
        assert_eq!(
            e.persist,
            vec![PersistOp::AddProject {
                path: "/home/dev".into()
            }]
        );
        assert_eq!(s.app.mode, Mode::Normal);
        let mut keyed = browsing();
        let ek = effects_of(update(&mut keyed, Event::Key(CanopyKey::Char('s'))));
        assert_eq!(e.persist, ek.persist, "the chip ≡ `s`");
        assert_eq!(keyed.app.mode, Mode::Normal);

        // The type-path chip ≡ `i`.
        let mut s = browsing();
        let m = click_on(Target::ExplorerTypePath, &mut s);
        update(&mut s, Event::Mouse(m));
        assert_eq!(s.app.mode, Mode::AddProject);
        assert!(s.app.command_line.is_empty(), "today's flow exactly");

        // The dotfiles chip ≡ `.` — the act the status line teaches, reachable
        // by pointer (modal-surface parity; the one mirror the first cut was missing).
        let mut s = browsing();
        let m = click_on(Target::ExplorerDotfiles, &mut s);
        update(&mut s, Event::Mouse(m));
        assert!(s.app.explorer.show_hidden, "the chip revealed dot-entries");
        assert_eq!(s.app.explorer.visible_len(), 4);
        let mut keyed = browsing();
        update(&mut keyed, Event::Key(CanopyKey::Char('.')));
        assert_eq!(
            s.app.explorer, keyed.app.explorer,
            "the chip and the key leave IDENTICAL state"
        );
        // …and it toggles back.
        let m = click_on(Target::ExplorerDotfiles, &mut s);
        update(&mut s, Event::Mouse(m));
        assert!(!s.app.explorer.show_hidden);

        // The X ≡ Esc.
        let mut s = browsing();
        let m = click_on(Target::ExplorerClose, &mut s);
        update(&mut s, Event::Mouse(m));
        assert_eq!(s.app.mode, Mode::Normal);
        assert_eq!(s.app.explorer, crate::explorer::ExplorerState::default());
        let mut keyed = browsing();
        update(&mut keyed, Event::Key(CanopyKey::Esc));
        assert_eq!(keyed.app.mode, Mode::Normal);
    }

    #[test]
    fn a_file_row_click_only_moves_the_cursor() {
        let mut s = browsing();
        let m = click_on(Target::ExplorerRow(2), &mut s); // → notes.md
        let e = effects_of(update(&mut s, Event::Mouse(m)));
        assert!(e.list_dir.is_none(), "files are context, never choices");
        assert_eq!(s.app.explorer.cursor, 2, "the cursor still moved");
        assert_eq!(s.app.mode, Mode::Explorer);
    }

    #[test]
    fn the_explorer_wheel_clamps_at_both_ends_and_yields_to_an_overlay() {
        let mut s = browsing();
        s.app.explorer.entries = (0..200).map(|i| dir(&format!("d{i:03}"))).collect();
        let list_h =
            usize::from(crate::view::explorer_list_area(crate::view::modal_rect(s.size)).h);
        let modal = crate::view::modal_rect(s.size);
        let (col, row) = (modal.x + 2, crate::view::explorer_list_area(modal).y);
        let wheel = |down| crate::input::MouseInput {
            kind: if down {
                crate::input::MouseKind::ScrollDown
            } else {
                crate::input::MouseKind::ScrollUp
            },
            col,
            row,
        };
        update(&mut s, Event::Mouse(wheel(false)));
        assert_eq!(s.app.explorer.scroll, 0, "clamped at the top");
        for _ in 0..500 {
            update(&mut s, Event::Mouse(wheel(true)));
        }
        assert_eq!(
            s.app.explorer.scroll,
            200 - list_h,
            "never past the listing's end (the scrollbar rule)"
        );
        // An overlay over the explorer outranks the mode and owns the wheel.
        let mut s = browsing();
        s.app.explorer.entries = (0..200).map(|i| dir(&format!("d{i:03}"))).collect();
        s.app
            .open_overlay(Overlay::notice("added", vec!["ok".into()]));
        update(&mut s, Event::Mouse(wheel(true)));
        assert_eq!(s.app.explorer.scroll, 0, "the notice swallowed the wheel");
    }

    #[test]
    fn keyboard_movement_follows_the_cursor_without_stealing_a_free_offset() {
        // the destination explorer: the first mode-modal follow-cursor clamp.
        let mut s = browsing();
        s.app.explorer.entries = (0..200).map(|i| dir(&format!("d{i:03}"))).collect();
        let list_h =
            usize::from(crate::view::explorer_list_area(crate::view::modal_rect(s.size)).h);
        for _ in 0..list_h + 5 {
            update(&mut s, Event::Key(CanopyKey::Char('j')));
        }
        assert_eq!(s.app.explorer.cursor, list_h + 5);
        assert_eq!(
            s.app.explorer.scroll,
            list_h + 5 + 1 - list_h,
            "the viewport followed the cursor down"
        );
        // A wheel-set offset that still shows the cursor survives a redraw.
        let mut s = browsing();
        s.app.explorer.entries = (0..200).map(|i| dir(&format!("d{i:03}"))).collect();
        s.app.explorer.cursor = 30;
        s.app.explorer.scroll = 28;
        s.app.clamp_scrolls(s.size);
        assert_eq!(s.app.explorer.scroll, 28, "an in-view cursor moves nothing");
    }

    #[test]
    fn selecting_a_destination_registers_it_and_add_is_untouched() {
        // The funnel (the destination explorer): select yields exactly the typed flow's op.
        let mut s = browsing();
        update(&mut s, Event::Key(CanopyKey::Enter)); // descend to alpha…
        s.app.explorer.apply(crate::explorer::DirListing {
            path: std::path::PathBuf::from("/home/dev/alpha"),
            entries: vec![dir("src")],
            omitted: 0,
            error: None,
        });
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Char('s'))));
        assert_eq!(
            e.persist,
            vec![PersistOp::AddProject {
                path: "/home/dev/alpha".into()
            }],
            "the folder that is OPEN, one level down"
        );

        // `/add` still registers the launch cwd and never raises the explorer.
        let mut s = ready(vec![project("a")]);
        s.app.launch_cwd = Some("/r/here".into());
        s.app.home_dir = Some("/home/dev".into());
        let e = effects_of(run_line(&mut s, "add"));
        assert_eq!(
            e.persist,
            vec![PersistOp::AddProject {
                path: "/r/here".into()
            }]
        );
        assert!(e.list_dir.is_none(), "/add browses nothing");
        assert_eq!(s.app.mode, Mode::Normal);
    }

    #[test]
    fn menu_close_x_closes_the_mode_not_an_overlay() {
        use crate::profile_menu::MenuStage;
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        assert!(matches!(
            s.app.profile_menu.stage,
            MenuStage::Settings { .. }
        ));
        // The `[x]` closes the MODE from any stage and never
        // touches the overlay plane.
        let e = effects_of(apply_click(&mut s, Target::ProfileMenuClose));
        assert!(e.is_empty());
        assert_eq!(s.app.mode, Mode::Normal);
        assert_eq!(s.app.profile_menu.stage, MenuStage::Browse);
        assert!(s.app.overlay.is_none());
    }

    /// A left click on the drawn cell of `target` (resolved via the pure
    /// hit-test so the test exercises pixels == targets).
    fn click_on(target: Target, s: &mut AppState) -> crate::input::MouseInput {
        let vm = s.snapshot();
        let (w, h) = (vm.size.w, vm.size.h);
        for row in 0..h {
            for col in 0..w {
                if crate::hit::hit_test(&vm, col, row).as_ref() == Some(&target) {
                    return crate::input::MouseInput {
                        kind: crate::input::MouseKind::Down(crate::input::MouseButton::Left),
                        col,
                        row,
                    };
                }
            }
        }
        panic!("target {target:?} has no drawn cell");
    }

    #[test]
    fn a_picker_selection_rethemes_the_same_frame_and_persists_one_event() {
        use crate::theme::{ThemeSlot, PRESETS};
        let mut s = ready_with_profiles(vec![project("a")]);
        // p → menu, s → settings, j+Enter → the Colours row opens the picker
        // (the profile hub), Enter → background's presets, Enter → red.
        for k in ['p', 's', 'j'] {
            update(&mut s, Event::Key(CanopyKey::Char(k)));
        }
        update(&mut s, Event::Key(CanopyKey::Enter));
        update(&mut s, Event::Key(CanopyKey::Enter));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        // Applied live: the snapshot already wears the override.
        assert_eq!(
            s.snapshot().theme.overrides().get(ThemeSlot::Background),
            Some(PRESETS[0].1),
            "the dashboard is the preview — re-themed in the same frame"
        );
        // Exactly one superseding event persists, with the resolved hex.
        assert_eq!(
            e.persist,
            vec![PersistOp::SetProfileTheme {
                profile: "pf0".into(),
                slot: "background".into(),
                colour: crate::theme::colour_to_hex(PRESETS[0].1),
            }]
        );
        assert!(!e.reload_profiles, "live apply needs no view reload");

        // The reset row supersedes back to default (a NULL colour).
        for _ in 0..20 {
            update(&mut s, Event::Key(CanopyKey::Down));
        }
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            s.snapshot().theme.overrides().get(ThemeSlot::Background),
            None,
            "reset returns the slot to Rift dark"
        );
        assert_eq!(
            e.persist,
            vec![PersistOp::SetProfileTheme {
                profile: "pf0".into(),
                slot: "background".into(),
                colour: None,
            }]
        );
    }

    #[test]
    fn picker_clicks_mirror_the_keyboard() {
        use crate::colour_picker::PickerStage;
        use crate::theme::{ThemeSlot, PRESETS};
        let mut s = ready_with_profiles(vec![project("a")]);
        for k in ['p', 'c'] {
            update(&mut s, Event::Key(CanopyKey::Char(k)));
        }
        // A slot click opens its presets…
        let _ = apply_click(&mut s, Target::PickerRow(4)); // text
        assert_eq!(s.app.colour_picker.stage, PickerStage::Presets);
        assert_eq!(s.app.colour_picker.slot_cursor, 4);
        // …and a preset click applies live, exactly like Enter.
        let e = effects_of(apply_click(&mut s, Target::PickerRow(2))); // green
        assert_eq!(
            s.snapshot().theme.overrides().get(ThemeSlot::Text),
            Some(PRESETS[2].1)
        );
        assert_eq!(
            e.persist,
            vec![PersistOp::SetProfileTheme {
                profile: "pf0".into(),
                slot: "text".into(),
                colour: crate::theme::colour_to_hex(PRESETS[2].1),
            }]
        );
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
        // harmless, and Space (unlike Enter) never activates the Confirm control.
        // These keep focus on the default Confirm control.
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
        // Add → raises the destination explorer (like the `a` key, through the
        // one entry door — the destination explorer) and press-flashes.
        let mut s = ready(vec![project("a"), project("b")]);
        s.app.home_dir = Some("/home/dev".into());
        let e = effects_of(apply_click(&mut s, Target::RailAdd));
        assert_eq!(s.app.mode, Mode::Explorer);
        assert_eq!(e.list_dir, Some(std::path::PathBuf::from("/home/dev")));
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
    fn every_button_target_flashes_on_click() {
        for target in [
            Target::HeaderHelp,
            Target::HeaderPalette,
            Target::HeaderQuit,
            Target::OverlayConfirm,
            Target::OverlayCancel,
            Target::OverlayClose,
            Target::RailAdd,
            Target::RailReorderUp,
            Target::RailReorderDown,
            Target::RailRemove,
            Target::RepositorySettings,
            Target::WizardAdd,
            Target::ExplorerUp,
            Target::ExplorerSelect,
            Target::ExplorerTypePath,
            Target::ExplorerDotfiles,
            Target::ExplorerClose,
        ] {
            assert!(target.is_button(), "{target:?} must be a button");
            let mut s = ready(vec![project("a")]);
            let _ = apply_click(&mut s, target.clone());
            assert_eq!(
                s.pressed.as_ref(),
                Some(&target),
                "clicking {target:?} arms the press-pulse"
            );
        }
        // A selection/focus target is not a button and does not flash.
        assert!(!Target::ProjectRow(0).is_button());
        assert!(
            !Target::ExplorerRow(0).is_button(),
            "a list row selects; it is not a chip"
        );
        let mut s = ready(vec![project("a")]);
        let _ = apply_click(&mut s, Target::ProjectRow(0));
        assert!(s.pressed.is_none());
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
        // the animator contract: progress is a pure function of accumulated dt; after the
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
        // reorder/layout and the profile ops are all superseding
        // appends). If anyone adds an Update/Delete variant, this stops
        // compiling — the immutability guard.
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
            PersistOp::SetLayout {
                profile: String::new(),
                path: String::new(),
                layout: None,
            },
            PersistOp::CreateProfile {
                name: String::new(),
            },
            PersistOp::DuplicateProfile {
                source: String::new(),
                name: String::new(),
            },
            PersistOp::RenameProfile {
                profile: String::new(),
                name: String::new(),
            },
            PersistOp::RemoveProfile {
                profile: String::new(),
            },
            PersistOp::SelectProfile {
                profile: String::new(),
            },
            PersistOp::AddProfileProject {
                profile: String::new(),
                path: String::new(),
            },
            PersistOp::RemoveProfileProject {
                profile: String::new(),
                path: String::new(),
            },
            PersistOp::ReorderProfileProject {
                profile: String::new(),
                path: String::new(),
                up: true,
            },
            PersistOp::SetProfileTheme {
                profile: String::new(),
                slot: String::new(),
                colour: None,
            },
            PersistOp::SetStartupProfile { profile: None },
        ];
        for op in ops {
            match op {
                PersistOp::AddProject { .. }
                | PersistOp::RemoveProject { .. }
                | PersistOp::RelabelProject { .. }
                | PersistOp::ReorderProject { .. }
                | PersistOp::SetLayout { .. }
                | PersistOp::CreateProfile { .. }
                | PersistOp::DuplicateProfile { .. }
                | PersistOp::RenameProfile { .. }
                | PersistOp::RemoveProfile { .. }
                | PersistOp::SelectProfile { .. }
                | PersistOp::AddProfileProject { .. }
                | PersistOp::RemoveProfileProject { .. }
                | PersistOp::ReorderProfileProject { .. }
                | PersistOp::SetProfileTheme { .. }
                | PersistOp::SetStartupProfile { .. } => {}
            }
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
                tokens: TokensView::Data(TokensData {
                    rate,
                    total,
                    by_worktree: crate::col3::WorktreeAxis::default(),
                }),
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

    /// A Recall feed whose **call chart** carries `rate`.
    ///
    /// The chart is the card's one series over time and therefore the only
    /// part of it that animates: the savings figures are headlines over one
    /// window and snap whole.
    fn recall_feed(rate: Vec<f64>) -> Event {
        use crate::col3::{CallsData, ChannelCallSeries, RecallView, SavingsData};
        Event::Feed(FeedUpdate {
            project: "/r/a".into(),
            payload: FeedPayload::Recall {
                savings: RecallView::Data(SavingsData {
                    target_pct: Some(90),
                    ..SavingsData::default()
                }),
                calls: RecallView::Data(CallsData {
                    total: 3,
                    window_end_ms: 1_800_000,
                    series: vec![ChannelCallSeries {
                        channel: "hook_inject".into(),
                        last_call_at: 1_799_000,
                        rate,
                    }],
                }),
            },
        })
    }

    #[test]
    fn token_data_to_data_feed_snaps_atomically() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, tokens_feed(vec![1.0, 2.0], 3.0));
        assert!(!s.anim_active(), "the first population snaps");
        update(&mut s, tokens_feed(vec![5.0, 6.0], 11.0));
        assert!(!s.anim_active(), "token composition never arms a tween");
        assert_eq!(tokens_of(&s.snapshot()), (vec![5.0, 6.0], 11.0));
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
        update(&mut s, recall_feed(vec![1.0, 2.0])); // placeholder→Data: snaps
        update(&mut s, recall_feed(vec![5.0, 6.0])); // Data→Data: arms a tween
        assert!(s.anim_active());
        // A nic feed snaps (nic is never tweened); it must NOT cancel the tokens
        // recall tween, and its own value must show immediately.
        let nic = Event::Feed(FeedUpdate {
            project: "/r/a".into(),
            payload: FeedPayload::System {
                context: crate::col3::NicView::Reading(crate::col3::NicReading {
                    pct: 50,
                    band: "Moderate",
                    model: "m".into(),
                    active_sessions: 1,
                }),
                metrics: crate::col3::PanelMetrics::default(),
                active_sessions: crate::col3::ActiveSessionsView::Count(1),
            },
        });
        update(&mut s, nic);
        assert!(
            s.anim_active(),
            "an unrelated (snapping) nic update must not cancel the recall tween"
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
        // A short Full3Col terminal: the fully-expanded panels overflow the
        // column, so the graphs offset moves content (the scrollbar rule) — the old inert
        // code clamped it away, so this is the regression the old code fails.
        let mut s = ready(vec![project("a")]);
        s.size = Size::new(100, 16);
        s.app.clamp_scrolls(s.size);
        let wheel = |kind| {
            Event::Mouse(MouseInput {
                kind,
                col: 90,
                row: 6,
            })
        };
        update(&mut s, wheel(MouseKind::ScrollDown));
        assert_eq!(
            s.app.scroll.graphs, 1,
            "the wheel scrolls the graphs viewport"
        );
        update(&mut s, wheel(MouseKind::ScrollUp));
        assert_eq!(s.app.scroll.graphs, 0);
        // The offset can never scroll past the fully-expanded content: it settles
        // at a positive ceiling and further scrolling is a no-op.
        for _ in 0..40 {
            update(&mut s, wheel(MouseKind::ScrollDown));
        }
        let ceiling = s.app.scroll.graphs;
        assert!(ceiling > 0, "there is real overflow to scroll");
        update(&mut s, wheel(MouseKind::ScrollDown));
        assert_eq!(
            s.app.scroll.graphs, ceiling,
            "already at the end — no further"
        );
    }

    fn exercise_wheel_targets(state: &mut AppState) -> Vec<Target> {
        let view = state.snapshot();
        let mut targets = Vec::new();
        for row in 0..view.size.h {
            for col in 0..view.size.w {
                let Some(target) = crate::hit::hit_test(&view, col, row) else {
                    continue;
                };
                if targets.contains(&target) {
                    continue;
                }
                targets.push(target);
                for down in [false, true] {
                    let effects = effects_of(apply_scroll(state, &view, col, row, down));
                    assert!(
                        effects.persist.is_empty(),
                        "wheel routing over {:?} must stay read-only",
                        targets.last()
                    );
                }
            }
        }
        targets
    }

    fn populated_viewer_for_wheel(w: u16, h: u16) -> AppState {
        use crate::recall_viewer::{
            ViewerCheckpointRow, ViewerDetail, ViewerFilterProgress, ViewerLine, ViewerLineKind,
            ViewerTimelineItem, ViewerTimelineSegment, ViewerTimestampProvenance,
        };

        let mut state = ready(vec![project("a"), project("b")]);
        state.size = Size::new(w, h);
        let _ = state.open_recall_viewer(None);
        state.app.viewer.catalogue = std::sync::Arc::new(
            (0_u32..80)
                .map(|index| ViewerCheckpointRow {
                    checkpoint_id: format!("cp-{index:03}"),
                    effective_at_ms: Some(i64::from(index)),
                    timestamp_provenance: ViewerTimestampProvenance::Recorded,
                    compact_utc: Some("2026-08-28 12:00Z".to_owned()),
                    excerpt: None,
                    excerpt_loading: false,
                    anchor_commit: Some(format!("oid-{index:03}")),
                    origins: Vec::new(),
                    chain_position: Some(u64::from(index)),
                    local_branches: vec!["main".to_owned()],
                    strategy: Some("native".to_owned()),
                    projection: "projected".to_owned(),
                    issues: Vec::new(),
                    metadata_search: format!("cp-{index:03}"),
                    worktree: None,
                })
                .collect(),
        );
        state.app.viewer.visible = (0..80).collect();
        state.app.viewer.new_count = 2;
        state.app.viewer.filter_progress = Some(ViewerFilterProgress {
            scanned: 5,
            total: 80,
            complete: false,
        });
        state.app.viewer.detail = std::sync::Arc::new(ViewerDetail {
            checkpoint_id: "cp-000".to_owned(),
            segments: vec![ViewerTimelineSegment {
                title: "captured".to_owned(),
                boundary: Some("ordered by recorded time".to_owned()),
                ordered_by_time: true,
                items: (0..80)
                    .map(|index| ViewerTimelineItem {
                        id: format!("item-{index}"),
                        title: format!("event {index}"),
                        timestamp: Some("12:00:00Z".to_owned()),
                        summary: vec![ViewerLine::plain(
                            ViewerLineKind::Heading,
                            format!("summary {index}"),
                        )],
                        body: vec![ViewerLine::plain(ViewerLineKind::Text, "body")],
                        expanded_body: None,
                        collapsed: false,
                        full_session_only: false,
                    })
                    .collect(),
            }],
            notice: None,
        });
        state
    }

    #[test]
    fn wheel_routing_is_read_only_across_every_live_surface() {
        let mut dashboard = ready(
            (0..80)
                .map(|index| project(&format!("p{index:03}")))
                .collect(),
        );
        dashboard.size = Size::new(100, 16);
        assert!(!exercise_wheel_targets(&mut dashboard).is_empty());

        let mut settings = project_settings_state(crate::ProjectInitialization::Initialized);
        settings.size = Size::new(80, 24);
        let _ = update(&mut settings, Event::Key(CanopyKey::Char('s')));
        assert!(!exercise_wheel_targets(&mut settings).is_empty());

        let mut profiles = ready_with_profiles(vec![project("a"), project("b"), project("c")]);
        crate::input::enter_profile_menu(&mut profiles.app);
        assert!(!exercise_wheel_targets(&mut profiles).is_empty());
        profiles.app.set_membership_lists(
            vec![project("a"), project("b")],
            vec![project("a"), project("b"), project("c")],
        );
        profiles.app.profile_menu.stage = crate::profile_menu::MenuStage::Members {
            profile: "pf0".to_owned(),
        };
        assert!(!exercise_wheel_targets(&mut profiles).is_empty());
        profiles.app.profile_menu.stage = crate::profile_menu::MenuStage::MembersAdd {
            profile: "pf0".to_owned(),
        };
        assert!(!exercise_wheel_targets(&mut profiles).is_empty());
        profiles.app.mode = Mode::ColourPicker;
        profiles.app.colour_picker.stage = crate::colour_picker::PickerStage::Presets;
        assert!(!exercise_wheel_targets(&mut profiles).is_empty());

        let mut explorer = browsing();
        explorer.app.explorer.entries = (0..200).map(|i| dir(&format!("d{i:03}"))).collect();
        assert!(!exercise_wheel_targets(&mut explorer).is_empty());

        let mut results = ready(vec![project("a")]);
        results.app.open_overlay(Overlay::Results {
            title: "results".to_owned(),
            lines: (0..100).map(|index| format!("line {index}")).collect(),
            scroll: 10,
            footer: Some("footer".to_owned()),
            truncated: false,
        });
        assert!(!exercise_wheel_targets(&mut results).is_empty());

        let mut suggestions = ready(vec![project("a")]);
        suggestions.app.mode = Mode::Command;
        suggestions.app.command_line = "/".to_owned();
        assert!(!exercise_wheel_targets(&mut suggestions).is_empty());

        let mut viewer = populated_viewer_for_wheel(120, 40);
        assert!(!exercise_wheel_targets(&mut viewer).is_empty());
        for pane in [
            crate::recall_viewer::ViewerPane::Projects,
            crate::recall_viewer::ViewerPane::Checkpoints,
            crate::recall_viewer::ViewerPane::Timeline,
        ] {
            let mut viewer = populated_viewer_for_wheel(40, 20);
            viewer.app.viewer.pane = pane;
            assert!(!exercise_wheel_targets(&mut viewer).is_empty());
        }
    }

    #[test]
    fn viewer_key_matrix_is_read_only_in_every_pane_and_editor_state() {
        let base = populated_viewer_for_wheel(120, 40);
        let keys = [
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
        ];
        for pane in [
            crate::recall_viewer::ViewerPane::Projects,
            crate::recall_viewer::ViewerPane::Checkpoints,
            crate::recall_viewer::ViewerPane::Timeline,
            crate::recall_viewer::ViewerPane::Command,
        ] {
            for key in keys {
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

    #[test]
    #[allow(clippy::too_many_lines)]
    fn viewer_response_matrix_accepts_only_matching_generations() {
        use crate::recall_viewer::{
            ViewerErrorArea, ViewerEvent, ViewerFilterProgress, ViewerLoadState,
        };

        let base = populated_viewer_for_wheel(120, 40);
        let project = base.app.viewer.project.clone().expect("viewer project");
        let catalogue_generation = base.app.viewer.catalogue_generation;
        let detail_generation = base.app.viewer.detail_generation;
        let filter_generation = base.app.viewer.filter_generation;
        let rows = (*base.app.viewer.catalogue).clone();
        let detail = (*base.app.viewer.detail).clone();

        for complete in [false, true] {
            let mut state = base.clone();
            let effects = effects_of(update(
                &mut state,
                Event::Viewer(ViewerEvent::Catalogue {
                    project: project.clone(),
                    generation: catalogue_generation,
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
                    generation: detail_generation,
                    detail: detail.clone(),
                    complete,
                }),
            ))
            .persist
            .is_empty());
        }

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
                project: project.clone(),
                generation: filter_generation,
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

        for (area, generation) in [
            (ViewerErrorArea::Catalogue, catalogue_generation),
            (ViewerErrorArea::Detail, detail_generation),
            (ViewerErrorArea::Filter, filter_generation),
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

        let mut foreign = base;
        foreign.dirty = false;
        assert!(effects_of(update(
            &mut foreign,
            Event::Viewer(ViewerEvent::Catalogue {
                project: "/other".to_owned(),
                generation: catalogue_generation,
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

    // ---- message routing --------------------------------

    use crate::dispatch::{Message, Overlay, Severity};

    #[test]
    fn an_empty_registry_raises_the_wizard_modal() {
        // Construction raises it; `ready` dismisses it, so build directly.
        let s = AppState::new(vec![], Size::new(120, 40));
        assert!(s.app.showing_wizard(), "empty startup raises the wizard");
    }

    #[test]
    fn removing_the_last_project_re_raises_the_wizard() {
        let mut app = App::new(vec![project("a")]);
        app.overlay = None;
        app.set_projects(vec![]); // the registry emptied
        assert!(
            app.showing_wizard(),
            "emptying the registry re-raises the wizard"
        );
    }

    #[test]
    fn a_soft_miss_opens_a_warning_alert() {
        // `/select nope` with no matching project opens a warning Alert, not a
        // silent toast.
        let mut s = ready(vec![project("a")]);
        let flow = no_match_status(&mut s);
        assert!(matches!(flow, Flow::Continue(_)));
        assert!(
            matches!(&s.app.overlay, Some(Overlay::Alert(m)) if m.severity == Severity::Warning),
            "a no-match opens a warning Alert: {:?}",
            s.app.overlay
        );
    }

    #[test]
    fn add_without_a_launch_cwd_opens_an_error_alert() {
        let mut s = ready(vec![project("a")]);
        s.app.launch_cwd = None;
        let _ = dashboard_verb_flow(&mut s, Verb::Add);
        assert!(
            matches!(&s.app.overlay, Some(Overlay::Alert(m)) if m.severity == Severity::Error),
            "add with no cwd opens an error Alert: {:?}",
            s.app.overlay
        );
    }

    #[test]
    fn open_notice_arms_a_bounded_auto_dismiss() {
        let mut s = ready(vec![project("a")]);
        s.reduced_motion = false;
        s.open_notice(Message::success("ok", vec!["done".into()]));
        assert!(matches!(s.app.overlay, Some(Overlay::Notice(_))));
        assert!(s.anim_active(), "a pending Notice keeps the loop awake");
        // A partial dt keeps it open; a dt past the settle window closes it.
        assert!(s.advance_anim(Duration::from_millis(200)));
        assert!(
            matches!(s.app.overlay, Some(Overlay::Notice(_))),
            "still up"
        );
        let _ = s.advance_anim(Duration::from_secs(3));
        assert!(s.app.overlay.is_none(), "the Notice auto-dismissed by dt");
        assert!(!s.anim_active(), "and the loop returns to idle");
    }

    #[test]
    fn a_notice_dismissed_early_does_not_keep_the_loop_awake() {
        let mut s = ready(vec![project("a")]);
        s.open_notice(Message::success("ok", vec!["done".into()]));
        s.app.overlay = None; // the user pressed Esc / X before the settle
        assert!(!s.anim_active(), "an early dismiss ends the settle");
        assert!(!s.advance_anim(Duration::from_millis(16)));
    }

    #[test]
    fn open_alert_does_not_arm_the_settle() {
        let mut s = ready(vec![project("a")]);
        s.open_alert(Message::error("e", vec!["boom".into()]));
        assert!(matches!(s.app.overlay, Some(Overlay::Alert(_))));
        assert!(!s.anim_active(), "an Alert blocks; it never auto-dismisses");
    }

    #[test]
    fn the_wizard_primary_click_raises_the_explorer_and_flashes() {
        let mut s = AppState::new(vec![], Size::new(120, 40));
        s.app.splash.skip();
        s.reduced_motion = false;
        s.app.home_dir = Some("/home/dev".into());
        assert!(s.app.showing_wizard());
        let e = effects_of(apply_click(&mut s, Target::WizardAdd));
        assert_eq!(
            e.list_dir,
            Some(std::path::PathBuf::from("/home/dev")),
            "the click twin requests the same start listing the key does"
        );
        assert_eq!(
            s.app.mode,
            Mode::Explorer,
            "the wizard begins with browsing (the destination explorer)"
        );
        assert!(s.app.overlay.is_none(), "the wizard dismissed");
        assert_eq!(s.press_pulse, 1.0, "the primary button flashed on click");
        assert_eq!(s.pressed, Some(Target::WizardAdd));
    }

    #[test]
    fn a_success_notice_over_an_emptying_registry_re_raises_the_wizard() {
        // The removal-to-empty case: a Notice shows, then its settle-close hands
        // the surface back to the wizard (the empty registry keeps its prompt).
        let mut s = AppState::new(vec![], Size::new(120, 40));
        s.open_notice(Message::success("registry", vec!["removed /a".into()]));
        assert!(matches!(s.app.overlay, Some(Overlay::Notice(_))));
        let _ = s.advance_anim(Duration::from_secs(3));
        assert!(
            s.app.showing_wizard(),
            "the wizard returns after the notice"
        );
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
