//! rift-canopy — the terminal dashboard (the `rift` entrypoint).
//!
//! Canopy is a 3-column dashboard (projects · detail · live graphs) with a
//! bottom slash-command palette and a startup splash. The production renderer is
//! the first-party `xre-rs` engine (3D splash, braille graphs), but it sits
//! behind the [`render::RenderBackend`] seam so a degraded **ASCII fallback**
//! ([`render::AsciiBackend`]) always exists — for terminals (or builds) without
//! it. All the dashboard *logic* (layout math, panel state, the nic-gauge, the
//! command parser, the splash timeline) is pure and rendered through that seam,
//! so it is fully testable without a real terminal.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod app;
pub mod axis;
pub mod bars;
pub mod cards;
pub mod chrome;
pub mod col1;
pub mod col2;
pub mod col3;
pub mod colour_picker;
pub mod command;
pub mod dispatch;
pub mod editor;
pub mod hit;
pub mod input;
pub mod keymap;
pub mod layout;
pub mod layout_model;
pub mod plot;
pub mod profile;
pub mod profile_menu;
pub mod render;
pub mod splash;
pub mod stack;
pub mod state;
pub mod suggest;
pub mod theme;
pub mod view;

pub use app::{load_profile_plane, prime_startup, CanopyOpts};
pub use col1::{
    current_registry, derive_status, EntryKind, LiveSessions, ProjectEntry, ProjectEvent,
    ProjectOp, ProjectionState, SessionsRead, StatusInputs,
};
pub use col2::{
    current_toggles, plan_flip, sanitize_for_terminal, toggle_view, CaptureSummary, DetailView,
    Feature, FeatureAction, FeatureActionKind, FeatureManager, ModelUsage, NullFeatureManager,
    SettingsSummary, ToggleChange, ToggleEvent, ToggleRow, ToggleState, ToggleView,
};
pub use col3::{
    fmt_km, ActivityData, ActivityView, BreakdownData, BreakdownView, CallsData, Col3, DayBar,
    NicReading, NicView, PanelMetrics, RecallView, SavingsData, ThreadsView, TokensData,
    TokensView,
};
pub use colour_picker::{ColourPickerState, PickerStage, RESET_ROW};
pub use command::{
    parse, CheckpointVerb, ConfigureVerb, MemoryVerb, PanelName, ParseError, ProfileVerb,
    ProjectRef, SessionVerb, SplashMode, Tier, Verb, MAX_INPUT_LEN, VERB_WORDS,
};
pub use dispatch::{
    request_for, CommandDispatcher, DispatchOutcome, DispatchRequest, Message, MessageAction,
    NullDispatcher, OutcomeClass, Overlay, OverlayControl, Severity, OVERLAY_MAX_LINES,
};
pub use editor::EditorState;
pub use hit::{hit_test, Target};
pub use input::{on_key, Action, CanopyKey, Mode, MouseButton, MouseInput, MouseKind};
pub use layout::{solve, PanelRects, Rect};
pub use layout_model::{Column, ColumnRole, LayoutModel, SolvedColumn, SolvedLayout, Unit};
pub use profile::{
    active_profile_id, current_membership, current_profiles, default_profile_id,
    normalize_profile_name, resolve_launch_profile, startup_profile_id, MembershipEntry,
    NullProfileLoader, ProfileEntry, ProfileEvent, ProfileLoader, ProfileOp, ProfileProjectEvent,
    ProfileProjectOp, ProfileStartupEvent, DEFAULT_PROFILE_NAME, MAX_PROFILE_NAME,
};
pub use profile_menu::{MenuStage, ProfileMenuState};
pub use render::{select_backend, AsciiBackend, Capabilities, Frame, RenderBackend};
pub use splash::SplashState;
pub use state::{
    update, AppState, DataRequest, Effects, Event, FeedPayload, FeedUpdate, Flow, Focus,
    HealthEvent, PersistOp, ProjectData, ViewModel,
};
pub use suggest::{suggest, ArgKind, CatalogEntry, Frecency, SuggestContext, Suggestion, CATALOG};
pub use theme::{colour_to_hex, parse_hex_colour, Theme, ThemeOverrides, ThemeSlot, PRESETS};

/// Scaffold/status string (kept for the bin entrypoint and smoke tests).
#[must_use]
pub fn status() -> &'static str {
    "rift-canopy: dashboard ready"
}

/// A watched project shown in column 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// Display name.
    pub name: String,
    /// Repo path.
    pub path: String,
    /// Live status.
    pub status: ProjectStatus,
    /// Repository root or linked worktree (drives the col1 child-row indent
    /// and the detail header; M4-07).
    pub kind: EntryKind,
}

/// The live status of a project, derived read-only from the projection + the
/// live-session cache (P4 S2 / M4-06). `Running` carries when activity began
/// (epoch-ms — not `Instant`, so the value stays `Eq`, snapshot-stable, and
/// clock-injectable) and how many sessions are active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProjectStatus {
    /// An agent session is active (a live entry in the session cache).
    Running {
        /// When the current activity began (epoch-ms).
        since_ms: i64,
        /// How many sessions are currently active.
        active_sessions: u16,
    },
    /// Initializing, or the projection is still ingesting.
    PickingUp,
    /// Registered and initialized, but quiet.
    Idle,
    /// The path is registered but `rift init` has not been run.
    NotInitialized,
    /// A problem reading the project's state.
    Error(StatusError),
}

/// Why a project's status could not be determined.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusError {
    /// The projection database is missing, locked, or unreadable.
    ProjectionUnavailable,
    /// A live-session cache file could not be parsed.
    CorruptSessionCache,
    /// The registered repository path is gone.
    RepoMissing,
}

impl ProjectStatus {
    /// A single-char status indicator (Unicode).
    #[must_use]
    pub fn glyph(self) -> char {
        match self {
            Self::Running { .. } => '●',
            Self::PickingUp => '◐',
            Self::Idle => '○',
            Self::NotInitialized => '·',
            Self::Error(_) => '!',
        }
    }

    /// The degraded ASCII status token for terminals without Unicode.
    #[must_use]
    pub fn glyph_ascii(self) -> &'static str {
        match self {
            Self::Running { .. } => "[R]",
            Self::PickingUp => "[~]",
            Self::Idle => "[ ]",
            Self::NotInitialized => "[-]",
            Self::Error(_) => "[!]",
        }
    }
}

/// Which panel currently has focus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Panel {
    /// Column 1: projects.
    Projects,
    /// Column 2: detail / feature toggles.
    Detail,
    /// Column 3: live graphs.
    Graphs,
    /// The bottom command line.
    Command,
}

/// The Tab / Shift-Tab focus-cycle order.
const FOCUS_ORDER: [Panel; 4] = [
    Panel::Projects,
    Panel::Detail,
    Panel::Graphs,
    Panel::Command,
];

/// The Left/Right focus order (columns only; the command row is reached via Tab
/// or `/`).
const COLUMN_ORDER: [Panel; 3] = [Panel::Projects, Panel::Detail, Panel::Graphs];

/// Per-panel scroll offsets (the top visible row of each scrollable panel). The
/// selection index lives separately on [`App`]; this is the viewport top used by
/// the reflow logic (P3 S2 / the column steps).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PanelScroll {
    /// Column 2 (detail) viewport top.
    pub detail: usize,
    /// Column 3 (graphs) viewport top.
    pub graphs: usize,
}

/// The whole dashboard state.
#[derive(Clone, Debug)]
pub struct App {
    /// Watched projects.
    pub projects: Vec<Project>,
    /// Selected project index.
    pub selected: usize,
    /// Focused panel.
    pub focus: Panel,
    /// The panel focus returns to when a mode/command is cancelled.
    pub focus_prev: Panel,
    /// Per-panel scroll offsets (viewport tops); selection is separate.
    pub scroll: PanelScroll,
    /// Cursor within the column-2 feature-toggle block (P5 S2).
    pub toggle_cursor: usize,
    /// The current command-line buffer (after `/`).
    pub command_line: String,
    /// The splash timeline.
    pub splash: SplashState,
    /// The current interaction mode (drives key handling + the command bar).
    pub mode: Mode,
    /// Set once the user has asked to quit; the event loop exits next iteration.
    pub quit: bool,
    /// The modal surface (results / confirm / running / alert / notice / the
    /// first-run wizard), if open — the one home for every transient message
    /// (ADR-0029). `None` when the dashboard owns the surface.
    pub overlay: Option<crate::dispatch::Overlay>,
    /// The focus ring's cursor within the open modal — an index into
    /// `overlay.controls()` (ADR-0027). 0 is the default focus; reset when a
    /// modal opens via [`Self::open_overlay`].
    pub overlay_focus: usize,
    /// The canonicalized launch directory (`/add` registers it; set from
    /// `CanopyOpts` at startup — the reducer never reads process cwd).
    pub launch_cwd: Option<String>,
    /// The last parse error, rendered inline under the command bar until the
    /// next edit.
    pub command_error: Option<crate::command::ParseError>,
    /// The highlighted suggest-panel row (arrows move it; Tab inserts it).
    pub suggest_highlight: usize,
    /// The palette's in-memory frecency (M5-10): bumped only on successful
    /// dispatch; ranking ties break by it.
    pub frecency: suggest::Frecency,
    /// Feature-row runtime states (M5-12, ADR-0013): action results drive
    /// Enabling/Disabling → cleared-on-success / Failed-with-reason. Keyed by
    /// the feature key; the detail rows merge these over the persisted
    /// desired state (toasts stop standing in for state).
    pub feature_runtime: std::collections::BTreeMap<String, FeatureRuntime>,
    /// The active dashboard arrangement (ADR-0033): the data-driven grid the
    /// renderer and hit-test both read. Defaults to the canonical 20/40/40
    /// grid, so an untouched dashboard renders exactly as before; the panel
    /// editor (M5.9) mutates it and per-project persistence restores it.
    pub layout: crate::layout_model::LayoutModel,
    /// The panel-editor's transient interaction state (ADR-0034) — the cursor,
    /// the pickup flag, and any live mouse drag. Only meaningful while
    /// `mode == Mode::PanelEditor`; resting otherwise.
    pub editor: crate::editor::EditorState,
    /// The live profiles (ADR-0036), served through the profile loader seam at
    /// launch and after every profile persist op. Empty until wired.
    pub profiles: Vec<crate::profile::ProfileEntry>,
    /// The active profile's stable id (ADR-0036) — the view the dashboard
    /// shows; scopes every arrangement write. Empty until wired.
    pub active_profile: String,
    /// The pinned startup profile (ADR-0045), served through the loader at
    /// launch and refreshed on every profile-plane reload; `None` = launch
    /// follows last-selected.
    pub startup_profile: Option<String>,
    /// The profile menu's transient interaction state (ADR-0037) — the cursor
    /// and the naming/confirm stage. Only meaningful while
    /// `mode == Mode::ProfileMenu`; resting otherwise.
    pub profile_menu: crate::profile_menu::ProfileMenuState,
    /// The colour picker's transient interaction state (ADR-0039) — the stage
    /// and its two cursors. Only meaningful while `mode == Mode::ColourPicker`;
    /// resting otherwise.
    pub colour_picker: crate::colour_picker::ColourPickerState,
}

impl App {
    /// A new dashboard for the given projects. Shows the first-run wizard when
    /// the project registry is empty.
    #[must_use]
    pub fn new(projects: Vec<Project>) -> Self {
        // An empty registry raises the first-run wizard as a modal (ADR-0029),
        // not a bespoke column-1 panel.
        let overlay = projects.is_empty().then(crate::dispatch::Overlay::wizard);
        Self {
            projects,
            selected: 0,
            focus: Panel::Projects,
            focus_prev: Panel::Projects,
            scroll: PanelScroll::default(),
            toggle_cursor: 0,
            command_line: String::new(),
            splash: SplashState::new(),
            mode: Mode::Normal,
            quit: false,
            overlay,
            overlay_focus: 0,
            launch_cwd: None,
            command_error: None,
            suggest_highlight: 0,
            frecency: suggest::Frecency::new(),
            feature_runtime: std::collections::BTreeMap::new(),
            layout: crate::layout_model::LayoutModel::default(),
            editor: crate::editor::EditorState::default(),
            profiles: Vec::new(),
            active_profile: String::new(),
            startup_profile: None,
            profile_menu: crate::profile_menu::ProfileMenuState::default(),
            colour_picker: crate::colour_picker::ColourPickerState::default(),
        }
    }

    /// The active profile's display name (ADR-0037's header identity), or
    /// `None` until the profile plane is wired.
    #[must_use]
    pub fn active_profile_name(&self) -> Option<&str> {
        self.profiles
            .iter()
            .find(|p| p.profile_id == self.active_profile)
            .map(|p| p.name.as_str())
    }

    /// Replace the live profiles + active id (from the loader seam or after a
    /// profile persist op), clamping the menu cursor onto the new list and
    /// resetting any menu stage whose profile is no longer live (ADR-0044 §6
    /// — a palette-side delete cannot strand a settings page).
    pub fn set_profiles(&mut self, profiles: Vec<crate::profile::ProfileEntry>, active: String) {
        use crate::profile_menu::MenuStage;
        self.profile_menu.cursor = self
            .profile_menu
            .cursor
            .min(profiles.len().saturating_sub(1));
        let stage_profile = match &self.profile_menu.stage {
            MenuStage::Settings { profile }
            | MenuStage::NamingRename { profile }
            | MenuStage::ConfirmDelete { profile, .. }
            | MenuStage::NamingDuplicate { source: profile } => Some(profile.clone()),
            MenuStage::Browse | MenuStage::NamingCreate => None,
        };
        if let Some(p) = stage_profile {
            if !profiles.iter().any(|e| e.profile_id == p) {
                self.profile_menu.stage = MenuStage::Browse;
                self.command_line.clear();
            }
        }
        self.profiles = profiles;
        self.active_profile = active;
    }

    /// Replace the watched projects (e.g. after a project is registered),
    /// clamping the selection and raising the first-run wizard when the
    /// registry becomes empty (ADR-0029).
    pub fn set_projects(&mut self, projects: Vec<Project>) {
        self.selected = self.selected.min(projects.len().saturating_sub(1));
        self.projects = projects;
        self.maybe_raise_wizard();
    }

    /// Raise the first-run wizard modal when the registry is empty and no other
    /// modal owns the surface (ADR-0029) — empty-registry startup and the
    /// "removed the last project" transition both land here. Idempotent: a
    /// wizard already open, or a non-empty registry, is a no-op.
    pub fn maybe_raise_wizard(&mut self) {
        if self.projects.is_empty() && self.overlay.is_none() {
            self.open_overlay(crate::dispatch::Overlay::wizard());
        }
    }

    /// Begin registering a project from the wizard's primary action (ADR-0029):
    /// dismiss the modal and enter add-project mode so the command bar takes the
    /// path — the click/key equivalent of the `a` key, from the wizard.
    pub fn start_add_project(&mut self) {
        self.overlay = None;
        self.overlay_focus = 0;
        self.mode = Mode::AddProject;
        self.command_line.clear();
    }

    /// Whether the first-run wizard modal is the open overlay (the empty-registry
    /// surface) — used by the render/hit paths that treated `show_wizard` as a
    /// layout flag.
    #[must_use]
    pub fn showing_wizard(&self) -> bool {
        matches!(
            self.overlay,
            Some(crate::dispatch::Overlay::Alert(crate::dispatch::Message {
                action: crate::dispatch::MessageAction::AddProject,
                ..
            }))
        )
    }

    /// Move the project selection down (saturating).
    pub fn select_next(&mut self) {
        if !self.projects.is_empty() && self.selected + 1 < self.projects.len() {
            self.selected += 1;
        }
    }

    /// Move the project selection up (saturating).
    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Focus a panel, remembering the prior one so a cancel can return to it.
    pub fn set_focus(&mut self, panel: Panel) {
        if panel != self.focus {
            self.focus_prev = self.focus;
            self.focus = panel;
        }
    }

    /// Cycle focus to the next (`fwd`) or previous panel, wrapping (Tab / Shift-Tab).
    pub fn cycle_focus(&mut self, fwd: bool) {
        let i = FOCUS_ORDER
            .iter()
            .position(|&p| p == self.focus)
            .unwrap_or(0);
        let n = FOCUS_ORDER.len();
        let j = if fwd { (i + 1) % n } else { (i + n - 1) % n };
        if let Some(&next) = FOCUS_ORDER.get(j) {
            self.set_focus(next);
        }
    }

    /// Open a modal and focus its default control (index 0; ADR-0027), so a
    /// freshly-opened confirm has its confirm button focused ("open, Enter").
    pub fn open_overlay(&mut self, overlay: crate::dispatch::Overlay) {
        self.overlay = Some(overlay);
        self.overlay_focus = 0;
    }

    /// Move the modal focus ring to the next (`fwd`) / previous control,
    /// wrapping (Tab / Shift-Tab; ADR-0027). A no-op with no open modal or a
    /// modal that has no focusable controls (Running).
    pub fn cycle_overlay_focus(&mut self, fwd: bool) {
        let Some(overlay) = self.overlay.as_ref() else {
            return;
        };
        let n = overlay.controls().len();
        if n == 0 {
            return;
        }
        let cur = self.overlay_focus.min(n - 1);
        self.overlay_focus = if fwd {
            (cur + 1) % n
        } else {
            (cur + n - 1) % n
        };
    }

    /// The modal control the focus ring currently rests on (ADR-0027), or `None`
    /// with no open modal / no focusable controls.
    #[must_use]
    pub fn focused_overlay_control(&self) -> Option<crate::dispatch::OverlayControl> {
        let overlay = self.overlay.as_ref()?;
        let controls = overlay.controls();
        controls
            .get(self.overlay_focus.min(controls.len().checked_sub(1)?))
            .copied()
    }

    /// Move focus one column left/right (Projects↔Detail↔Graphs), clamped. A no-op
    /// when the command row holds focus.
    pub fn move_focus(&mut self, left: bool) {
        let Some(i) = COLUMN_ORDER.iter().position(|&p| p == self.focus) else {
            return;
        };
        let j = if left {
            i.saturating_sub(1)
        } else {
            (i + 1).min(COLUMN_ORDER.len() - 1)
        };
        if let Some(&next) = COLUMN_ORDER.get(j) {
            self.set_focus(next);
        }
    }

    /// Scroll the focused panel's viewport by one row (or a page). The projects
    /// panel scrolls its selection and the detail panel moves its toggle cursor
    /// (both handled by the caller); this adjusts the graphs viewport top,
    /// clamped at 0 (the upper bound is applied by [`Self::clamp_scrolls`]).
    pub fn scroll_focused(&mut self, down: bool, page: bool) {
        let step: usize = if page { 10 } else { 1 };
        let slot = match self.focus {
            Panel::Graphs => &mut self.scroll.graphs,
            Panel::Projects | Panel::Detail | Panel::Command => return,
        };
        *slot = if down {
            slot.saturating_add(step)
        } else {
            slot.saturating_sub(step)
        };
    }

    /// Clamp scroll offsets to their stacks' true overflow (ADR-0031), so an
    /// offset can never scroll past the end into empty space. The column areas
    /// are recomputed from the same layout the renderer dispatches to, and each
    /// offset is capped at its stack's `max_scroll` — 0 when the stack fits
    /// (a fitting column scrolls nothing).
    pub fn clamp_scrolls(&mut self, size: crate::render::frame::Size) {
        use crate::layout::{choose_mode, single_panel, solve, two_col, LayoutMode};
        let body = crate::layout::Rect {
            x: 0,
            y: 1,
            w: size.w,
            h: size.h.saturating_sub(1),
        };
        let (col2, col3) = match choose_mode(size) {
            LayoutMode::Full3Col => {
                let r = solve(body);
                (r.col2, r.col3)
            }
            LayoutMode::TwoCol => {
                let r = two_col(body);
                (r.col2, r.col3)
            }
            LayoutMode::SinglePanel => {
                let (_tabs, panel, _hint, _cmd) = single_panel(body);
                (panel, panel)
            }
            LayoutMode::TooSmall => {
                self.scroll = PanelScroll::default();
                return;
            }
        };
        // Clamp against the default card/panel lists — the scroll bounds are the
        // count/rows, invariant under reordering (a cross-column move can only
        // shrink a column, so the default over-estimate stays a safe ceiling; the
        // solver re-clamps the offset when it draws).
        self.scroll.detail = self
            .scroll
            .detail
            .min(crate::cards::solve_scrolled(&crate::cards::CardKind::ALL, col2, 0).1);
        // Graphs scroll over the fully-expanded panels (ADR-0031): the overflow
        // of the expanded content past the column height.
        self.scroll.graphs = self.scroll.graphs.min(crate::col3::max_scroll(
            &crate::col3::SectionId::ALL,
            col3.h,
        ));
    }

    /// Move the column-2 feature-toggle cursor down/up, clamped to the six toggles.
    pub fn move_toggle_cursor(&mut self, down: bool) {
        let last = Feature::ALL.len().saturating_sub(1);
        self.toggle_cursor = if down {
            (self.toggle_cursor + 1).min(last)
        } else {
            self.toggle_cursor.saturating_sub(1)
        };
    }

    /// The currently-selected project, if any.
    #[must_use]
    pub fn selected_project(&self) -> Option<&Project> {
        self.projects.get(self.selected)
    }

    /// Move the selection onto the project with `path`, if present. Used after an
    /// append-only registry change (remove/reorder) so the selection follows the
    /// same project rather than snapping to a stale index.
    pub fn select_path(&mut self, path: &str) {
        if let Some(i) = self.projects.iter().position(|p| p.path == path) {
            self.selected = i;
        }
    }

    /// Select the row the launch cwd names ([`launch_selection`]); `false` when
    /// no registered row matches (selection stays where it was — index 0 at
    /// launch).
    pub fn select_launch_cwd(&mut self, cwd: &str) -> bool {
        match launch_selection(&self.projects, cwd).map(str::to_owned) {
            Some(path) => {
                self.select_path(&path);
                true
            }
            None => false,
        }
    }
}

/// The status-line hint shown when `rift` is launched inside a git repository
/// that is not registered — registration stays an explicit mutation (M5-03);
/// the hint names the two ways in and vanishes on the next status change.
pub const UNREGISTERED_CWD_HINT: &str =
    "this repository isn't registered — /add it here, or run `rift init`";

/// The launch-cwd auto-select rule (M5-03): the registered row whose path
/// equals `cwd`, else the **longest** registered ancestor of it (worktree rows
/// count like any row), else `None` — the caller keeps today's index-0
/// default. Comparison is textual by path components; the caller passes the
/// canonicalized launch cwd and the registry stores canonical paths.
#[must_use]
pub fn launch_selection<'a>(projects: &'a [Project], cwd: &str) -> Option<&'a str> {
    let cwd_path = std::path::Path::new(cwd);
    let mut best: Option<&str> = None;
    for p in projects {
        if p.path == cwd {
            return Some(&p.path);
        }
        if cwd_path.starts_with(&p.path) && best.is_none_or(|b: &str| p.path.len() > b.len()) {
            best = Some(&p.path);
        }
    }
    best
}

/// A feature row's transient runtime state (M5-12): what the last
/// feature-action RESULT said, overlaid on the persisted desired state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeatureRuntime {
    /// An install/reinstall is in flight (the frame before the manager runs).
    Enabling,
    /// A removal is in flight.
    Disabling,
    /// The last action failed; the reason renders on the row.
    Failed(String),
}

/// The current ranked suggestions for `app`'s buffer — pure over the app
/// state, shared by the reducer's snapshot, the key handler's Tab-insert, and
/// the mouse click-insert, so what you see is exactly what inserts (M5-10/11).
#[must_use]
pub fn current_suggestions(app: &App) -> Vec<suggest::Suggestion> {
    if app.mode != Mode::Command || app.overlay.is_some() {
        return Vec::new();
    }
    let ctx = suggest::SuggestContext {
        projects: app.projects.iter().map(|p| p.name.clone()).collect(),
        features: Feature::ALL.iter().map(|f| f.key().to_string()).collect(),
    };
    suggest::suggest(&app.command_line, &ctx, &app.frecency)
}

/// Apply the launch-cwd policy to a fresh dashboard (M5-03): auto-select the
/// registered row the cwd falls under, or — when the cwd is an unregistered
/// git repository — show [`UNREGISTERED_CWD_HINT`] (never auto-add). Shared by
/// the interactive launch and the headless frame, like the startup prime.
pub fn apply_launch_cwd(app: &mut App, cwd: Option<&str>, cwd_is_repo: bool) {
    let Some(cwd) = cwd else { return };
    // The hint is an info Alert (ADR-0029), raised only when no modal already
    // owns the surface — an empty registry keeps its first-run wizard instead.
    if !app.select_launch_cwd(cwd) && cwd_is_repo && app.overlay.is_none() {
        app.open_overlay(crate::dispatch::Overlay::alert(
            "unregistered repository",
            vec![UNREGISTERED_CWD_HINT.to_owned()],
            crate::dispatch::Severity::Info,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mentions_canopy() {
        assert!(status().contains("canopy"));
    }

    fn proj(path: &str, kind: EntryKind) -> Project {
        Project {
            name: path.rsplit('/').next().unwrap_or(path).to_owned(),
            path: path.to_owned(),
            status: ProjectStatus::Idle,
            kind,
        }
    }

    #[test]
    fn launch_selection_exact_match_wins() {
        let ps = vec![
            proj("/home/dev/other", EntryKind::Repo),
            proj("/home/dev/rift", EntryKind::Repo),
        ];
        assert_eq!(
            launch_selection(&ps, "/home/dev/rift"),
            Some("/home/dev/rift")
        );
    }

    #[test]
    fn launch_selection_prefers_the_longest_ancestor() {
        let ps = vec![
            proj("/home/dev", EntryKind::Repo),
            proj("/home/dev/rift", EntryKind::Repo),
        ];
        // A cwd deep inside both rows selects the nearest (longest) root.
        assert_eq!(
            launch_selection(&ps, "/home/dev/rift/crates/core"),
            Some("/home/dev/rift")
        );
    }

    #[test]
    fn launch_selection_counts_worktree_rows() {
        let ps = vec![
            proj("/home/dev/rift", EntryKind::Repo),
            proj("/home/dev/rift-wt/feature", EntryKind::Worktree),
        ];
        assert_eq!(
            launch_selection(&ps, "/home/dev/rift-wt/feature/src"),
            Some("/home/dev/rift-wt/feature")
        );
    }

    #[test]
    fn launch_selection_is_component_wise_never_prefix_string() {
        let ps = vec![proj("/home/dev/rift", EntryKind::Repo)];
        // "/home/dev/rift-extra" is NOT under "/home/dev/rift".
        assert_eq!(launch_selection(&ps, "/home/dev/rift-extra"), None);
    }

    /// The body lines of an open Alert overlay (test helper for the routed hints).
    fn alert_body(app: &App) -> Option<Vec<String>> {
        match &app.overlay {
            Some(crate::dispatch::Overlay::Alert(m)) => Some(m.body.clone()),
            _ => None,
        }
    }

    #[test]
    fn unregistered_repo_cwd_hints_and_keeps_index_zero() {
        let mut app = App::new(vec![
            proj("/home/dev/a", EntryKind::Repo),
            proj("/home/dev/b", EntryKind::Repo),
        ]);
        apply_launch_cwd(&mut app, Some("/home/dev/unregistered"), true);
        assert_eq!(app.selected, 0, "selection stays at index 0");
        // The two-ways-in hint shows as an info Alert (ADR-0029), not a toast.
        assert_eq!(
            alert_body(&app).as_deref(),
            Some([UNREGISTERED_CWD_HINT.to_owned()].as_slice()),
            "the two-ways-in hint shows as a modal"
        );
    }

    #[test]
    fn non_repo_cwd_neither_selects_nor_hints() {
        let mut app = App::new(vec![proj("/home/dev/a", EntryKind::Repo)]);
        apply_launch_cwd(&mut app, Some("/tmp/scratch"), false);
        assert_eq!(app.selected, 0);
        assert!(app.overlay.is_none(), "no hint outside a git repository");
    }

    #[test]
    fn launch_cwd_selects_the_matching_row() {
        let mut app = App::new(vec![
            proj("/home/dev/a", EntryKind::Repo),
            proj("/home/dev/b", EntryKind::Repo),
        ]);
        apply_launch_cwd(&mut app, Some("/home/dev/b/src/deep"), true);
        assert_eq!(app.selected, 1, "selection follows the launch cwd");
        assert!(app.overlay.is_none(), "a successful match needs no hint");
    }

    #[test]
    fn empty_registry_shows_wizard() {
        let app = App::new(vec![]);
        assert!(
            app.showing_wizard(),
            "an empty registry raises the wizard modal"
        );
    }

    #[test]
    fn selection_is_bounded() {
        let mut app = App::new(vec![
            Project {
                name: "a".into(),
                path: "/a".into(),
                status: ProjectStatus::Running {
                    since_ms: 0,
                    active_sessions: 1,
                },
                kind: EntryKind::Repo,
            },
            Project {
                name: "b".into(),
                path: "/b".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            },
        ]);
        app.select_prev(); // already at 0
        assert_eq!(app.selected, 0);
        app.select_next();
        assert_eq!(app.selected, 1);
        app.select_next(); // saturates
        assert_eq!(app.selected, 1);
        assert_eq!(app.selected_project().unwrap().name, "b");
    }

    #[test]
    fn status_glyphs() {
        assert_eq!(
            ProjectStatus::Running {
                since_ms: 0,
                active_sessions: 1
            }
            .glyph(),
            '●'
        );
        assert_eq!(
            ProjectStatus::Error(StatusError::ProjectionUnavailable).glyph(),
            '!'
        );
        assert_eq!(ProjectStatus::NotInitialized.glyph(), '·');
        assert_eq!(ProjectStatus::PickingUp.glyph_ascii(), "[~]");
    }

    #[test]
    fn focus_cycles_and_moves() {
        let mut app = App::new(vec![]);
        assert_eq!(app.focus, Panel::Projects);
        app.cycle_focus(true);
        assert_eq!(app.focus, Panel::Detail);
        app.cycle_focus(false);
        assert_eq!(app.focus, Panel::Projects);
        app.cycle_focus(false); // wraps to the last panel
        assert_eq!(app.focus, Panel::Command);
        // Left/Right stay within the columns; a no-op from the command row.
        app.move_focus(true);
        assert_eq!(app.focus, Panel::Command);
        app.set_focus(Panel::Graphs);
        app.move_focus(false); // already rightmost column, clamps
        assert_eq!(app.focus, Panel::Graphs);
        app.move_focus(true);
        assert_eq!(app.focus, Panel::Detail);
    }
}
