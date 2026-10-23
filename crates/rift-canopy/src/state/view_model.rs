//! The immutable render snapshot: [`ViewModel`], its project rows, and the
//! [`AppState::snapshot`] that derives one per dirty frame.
//!
//! Separate so the renderer's whole input is one type in one file, and so
//! the sharing rule — caches compared, never invalidated — is read beside the
//! fields it protects.

use std::sync::Arc;

use crate::col2::DetailView;
use crate::dispatch::Overlay;
use crate::render::frame::Size;
use crate::{Mode, ProjectStatus};

use super::{AppState, Focus};

/// One project row in the render snapshot. Selection is not a row property —
/// the renderer derives it from [`ViewModel::selected`] — so the shared row
/// list never rebuilds on a selection move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRow {
    /// Repository/worktree path, used by the grouped project picker.
    pub path: String,
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
    /// Active full-width project view.
    pub dashboard_tab: crate::dashboard::DashboardTab,
    /// Temporary project drawer visibility.
    pub projects_open: bool,
    /// Picker highlight, independent of the active project.
    pub project_cursor: usize,
    /// Kind filter within the project picker.
    pub project_filter: Option<crate::EntryKind>,
    /// Launch-resolved home for compact picker paths; rendering never reads env.
    pub home_dir: Option<String>,
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

impl AppState {
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
                        path: p.path.clone(),
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
            dashboard_tab: self.app.dashboard_tab,
            projects_open: self.app.projects_open,
            project_cursor: self.app.project_cursor,
            project_filter: self.app.project_filter,
            home_dir: self.app.home_dir.clone(),
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
    use crate::input::CanopyKey;
    use crate::state::fixtures::*;
    use crate::state::{update, Event, HealthEvent, ProjectData};

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
    fn snapshot_carries_the_profile_plane() {
        let mut s = ready_with_profiles(vec![project("a")]);
        let vm = s.snapshot();
        assert_eq!(vm.profile_name.as_deref(), Some("default"));
        assert_eq!(vm.profiles.len(), 3);
        assert_eq!(vm.active_profile, "pf0");
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
        open_drawer(&mut s);
        update(&mut s, Event::Key(CanopyKey::Down));
        update(&mut s, Event::Key(CanopyKey::Enter));
        let v = s.snapshot();
        assert_eq!(v.selected, 1);
        assert_eq!(v.detail.unwrap().path, "/r/b");
    }
}
