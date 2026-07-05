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
pub mod col1;
pub mod col2;
pub mod command;
pub mod gauge;
pub mod input;
pub mod layout;
pub mod plot;
pub mod render;
pub mod splash;
pub mod state;
pub mod view;

pub use app::CanopyOpts;
pub use col1::{
    current_registry, derive_status, EntryKind, LiveSessions, ProjectEntry, ProjectEvent,
    ProjectOp, ProjectionState, SessionsRead, StatusInputs,
};
pub use col2::{
    current_toggles, plan_flip, sanitize_for_terminal, toggle_view, CaptureSummary, DetailView,
    Feature, FeatureAction, FeatureActionKind, FeatureManager, ModelUsage, NullFeatureManager,
    SettingsSummary, ToggleChange, ToggleEvent, ToggleRow, ToggleState, ToggleView,
};
pub use command::{autocomplete, parse_command, Command, COMMANDS};
pub use gauge::{render_bar, GaugeState};
pub use input::{on_key, Action, CanopyKey, Mode};
pub use layout::{solve, PanelRects, Rect};
pub use render::{select_backend, AsciiBackend, Capabilities, Frame, RenderBackend};
pub use splash::SplashState;
pub use state::{
    update, AppState, DataRequest, Effects, Event, Flow, Focus, PersistOp, ProjectData, ViewModel,
};

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
    /// Context-window % full (drives the nic-gauge).
    pub context_pct: u8,
    /// Recent token-usage series (for the column-3 sparkline).
    pub token_series: Vec<u64>,
    /// Recall savings % (tokens avoided / total).
    pub savings_pct: u8,
    /// The splash timeline.
    pub splash: SplashState,
    /// Whether the first-run wizard is shown (registry empty).
    pub show_wizard: bool,
    /// The current interaction mode (drives key handling + the command bar).
    pub mode: Mode,
    /// Set once the user has asked to quit; the event loop exits next iteration.
    pub quit: bool,
    /// A transient status/toast line (e.g. "registered X" or an error reason).
    pub status: Option<String>,
}

impl App {
    /// A new dashboard for the given projects. Shows the first-run wizard when
    /// the project registry is empty.
    #[must_use]
    pub fn new(projects: Vec<Project>) -> Self {
        let show_wizard = projects.is_empty();
        Self {
            projects,
            selected: 0,
            focus: Panel::Projects,
            focus_prev: Panel::Projects,
            scroll: PanelScroll::default(),
            toggle_cursor: 0,
            command_line: String::new(),
            context_pct: 0,
            token_series: Vec::new(),
            savings_pct: 0,
            splash: SplashState::new(),
            show_wizard,
            mode: Mode::Normal,
            quit: false,
            status: None,
        }
    }

    /// Replace the watched projects (e.g. after a project is registered),
    /// recomputing the first-run wizard flag and clamping the selection.
    pub fn set_projects(&mut self, projects: Vec<Project>) {
        self.show_wizard = projects.is_empty();
        self.selected = self.selected.min(projects.len().saturating_sub(1));
        self.projects = projects;
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

    /// Clamp scroll offsets so they stay in range after a resize. A coarse cap by
    /// terminal height for now; content-aware clamping lands with the reflow work
    /// (P3 S2) and the column steps.
    pub fn clamp_scrolls(&mut self, size: crate::render::frame::Size) {
        let cap = usize::from(size.h);
        self.scroll.detail = self.scroll.detail.min(cap);
        self.scroll.graphs = self.scroll.graphs.min(cap);
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

    /// The nic-gauge state from the current context %.
    #[must_use]
    pub fn gauge(&self) -> GaugeState {
        GaugeState::from_pct(self.context_pct)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mentions_canopy() {
        assert!(status().contains("canopy"));
    }

    #[test]
    fn empty_registry_shows_wizard() {
        let app = App::new(vec![]);
        assert!(app.show_wizard);
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
