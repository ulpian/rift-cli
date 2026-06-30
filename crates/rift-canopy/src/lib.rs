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
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(clippy::module_name_repetitions, clippy::doc_markdown)]

pub mod command;
pub mod gauge;
pub mod input;
pub mod layout;
pub mod render;
pub mod splash;
#[cfg(feature = "xre")]
pub mod xre;

pub use command::{autocomplete, parse_command, Command, COMMANDS};
pub use gauge::{render_bar, GaugeState};
pub use input::{on_key, Action, CanopyKey, Mode};
pub use layout::{three_column, Rect};
pub use render::{AsciiBackend, Frame, RenderBackend};
pub use splash::SplashState;

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
    pub status: ProjStatus,
}

/// The live status of a project.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProjStatus {
    /// An agent session is active.
    Active,
    /// Idle / no active session.
    Idle,
    /// A capture or verify error.
    Error,
}

impl ProjStatus {
    /// A single-char status indicator.
    #[must_use]
    pub fn glyph(self) -> char {
        match self {
            Self::Active => '●',
            Self::Idle => '○',
            Self::Error => '!',
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

/// The whole dashboard state.
#[derive(Clone, Debug)]
pub struct App {
    /// Watched projects.
    pub projects: Vec<Project>,
    /// Selected project index.
    pub selected: usize,
    /// Focused panel.
    pub focus: Panel,
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

    /// The currently-selected project, if any.
    #[must_use]
    pub fn selected_project(&self) -> Option<&Project> {
        self.projects.get(self.selected)
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
                status: ProjStatus::Active,
            },
            Project {
                name: "b".into(),
                path: "/b".into(),
                status: ProjStatus::Idle,
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
        assert_eq!(ProjStatus::Active.glyph(), '●');
        assert_eq!(ProjStatus::Error.glyph(), '!');
    }
}
