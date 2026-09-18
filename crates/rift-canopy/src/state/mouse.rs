//! The pointer's entry into the reducer: hit-testing a press against the
//! current snapshot, the panel editor's live drag, and the wheel's per-panel
//! scroll semantics.
//!
//! Separate from the click table because a mouse event is mostly *not* a
//! click — moves, releases and the wheel must stay inert or read-only, and
//! the ~0% idle invariant is decided here before any target is resolved.

use crate::dispatch::Overlay;
use crate::editor::{resize, Cursor, Drag};
use crate::hit::{hit_test, ScrollRegion, Target};
use crate::input::{MouseButton, MouseInput, MouseKind};
use crate::recall_viewer::{Surface, ViewerPane};
use crate::{App, Mode, Panel};

use super::click::apply_click;
use super::reduce::selection_change_effects;
use super::viewer::{move_viewer, sync_viewer_focus};
use super::{AppState, Effects, Flow, ViewModel};

/// What a wheel notch over a target scrolls: a dashboard column, the suggest
/// highlight, a mode-modal's own list, an open Results overlay, or nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WheelRegion {
    Panel(Panel),
    Suggest,
    ProjectSettings,
    Explorer,
    Results,
    Inert,
}

/// Handle a mouse event by hit-testing it against the current view and mapping
/// the target onto the **same** state changes the keyboard produces. Only left
/// presses and the scroll wheel act in normal use; moves/drags/releases are
/// ignored (no per-move work, keeping the ~0% CPU idle). The **one** exception
/// is panel-editor mode: while it holds a live drag, `Drag`/`Up` move and
/// drop the grabbed unit — scoped so normal-mode idle is byte-identical. While
/// the splash is up, any click dismisses it and is otherwise swallowed.
pub(super) fn on_mouse(s: &mut AppState, m: MouseInput) -> Flow {
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

/// Apply a scroll-wheel event over the panel under the cursor, using the same
/// per-panel vertical semantics as the keyboard: projects move the selection;
/// detail and graphs scroll their viewports. On the viewer surface the wheel
/// moves the pane under the cursor instead, and a mode-modal's list scrolls
/// only while no overlay outranks it.
pub(super) fn apply_scroll(
    s: &mut AppState,
    vm: &ViewModel,
    col: u16,
    row: u16,
    down: bool,
) -> Flow {
    let target = hit_test(vm, col, row);
    if s.app.surface == Surface::RecallViewer && s.app.overlay.is_none() {
        return viewer_wheel(s, target.as_ref(), down);
    }
    match wheel_region(&s.app, target.as_ref()) {
        WheelRegion::Panel(panel) => {
            s.dirty = true;
            scroll_panel(s, panel, down)
        }
        WheelRegion::Suggest => suggest_wheel(s, down),
        WheelRegion::ProjectSettings => project_settings_wheel(s, down),
        WheelRegion::Explorer => explorer_wheel(s, down),
        WheelRegion::Results => results_wheel(s, down),
        WheelRegion::Inert => Flow::Continue(Effects::default()),
    }
}

/// Advance a live editor drag toward the pointer: a unit drag moves
/// its hover cursor to the grip under the pointer; a divider drag resizes one
/// step toward the pointer. Dirties only on a real change.
fn drag_update(s: &mut AppState, vm: &ViewModel, m: MouseInput) -> Flow {
    match s.app.editor.drag {
        Some(Drag::Unit(_)) => {
            if let Some(Target::GripBar(h)) = hit_test(vm, m.col, m.row) {
                if s.app.editor.cursor != h {
                    s.app.editor.cursor = h;
                    s.dirty = true;
                }
            }
        }
        Some(Drag::Divider(i)) => drag_divider(s, i, m.col),
        None => (),
    }
    Flow::Continue(Effects::default())
}

/// Resize column `i` one step toward the pointer: compare it to the divider's
/// current x, with the editor cursor parked on that column for the resize.
fn drag_divider(s: &mut AppState, i: usize, pointer_col: u16) {
    let body = crate::layout::Rect {
        x: 0,
        y: 1,
        w: s.size.w,
        h: s.size.h.saturating_sub(1),
    };
    let solved = crate::layout_model::solve_model(&s.app.layout, body);
    let Some(column) = solved.columns.get(i) else {
        return;
    };
    let divider_x = column.rect.x + column.rect.w.saturating_sub(1);
    let saved = s.app.editor.cursor;
    s.app.editor.cursor = Cursor { col: i, unit: 0 };
    if pointer_col > divider_x {
        resize(&s.app.editor, &mut s.app.layout, true);
        s.dirty = true;
    } else if pointer_col < divider_x {
        resize(&s.app.editor, &mut s.app.layout, false);
        s.dirty = true;
    }
    s.app.editor.cursor = saved;
    s.app.editor.cursor.col = i;
}

/// Release a live editor drag: a unit drag drops the grabbed unit at
/// its hover cursor (conserving the multiset); a divider drag just ends. Clears
/// the drag and the pickup. The arrangement is persisted when the panel editor
/// is left, not on every drop.
fn drop_drag(s: &mut AppState) -> Flow {
    if let Some(Drag::Unit(src)) = s.app.editor.drag {
        let dst = s.app.editor.cursor;
        s.app.editor.cursor = crate::editor::drop_unit(&mut s.app.layout, src, dst.col, dst.unit);
    }
    s.app.editor.picked = false;
    s.app.editor.drag = None;
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// On the viewer surface the wheel moves the pane under the cursor one row
/// at a time; anywhere else it is inert.
fn viewer_wheel(s: &mut AppState, target: Option<&Target>, down: bool) -> Flow {
    let pane = match target {
        Some(
            Target::ProjectRow(_)
            | Target::PanelFocus(Panel::Projects)
            | Target::Scrollbar(ScrollRegion::Rail),
        ) => ViewerPane::Projects,
        Some(
            Target::ViewerCheckpointRow(_)
            | Target::ViewerFilter
            | Target::ViewerLatest
            | Target::PanelFocus(Panel::Detail)
            | Target::Scrollbar(ScrollRegion::ViewerCheckpoints),
        ) => ViewerPane::Checkpoints,
        Some(
            Target::ViewerTimelineLine(_)
            | Target::ViewerFullSession
            | Target::ViewerDashboard
            | Target::PanelFocus(Panel::Graphs)
            | Target::Scrollbar(ScrollRegion::ViewerTimeline),
        ) => ViewerPane::Timeline,
        _ => return Flow::Continue(Effects::default()),
    };
    s.app.viewer.pane = pane;
    sync_viewer_focus(&mut s.app);
    s.dirty = true;
    Flow::Continue(move_viewer(s, down, false))
}

/// The region a dashboard wheel over `target` scrolls. Every target is
/// classified, so a new one has to be placed here — as a wheel-bearing
/// surface or in the inert list.
fn wheel_region(app: &App, target: Option<&Target>) -> WheelRegion {
    let Some(target) = target else {
        return WheelRegion::Inert;
    };
    if let Some(region) = mode_wheel_region(app, target) {
        return region;
    }
    match target {
        Target::ProjectRow(_) | Target::Scrollbar(ScrollRegion::Rail) => {
            WheelRegion::Panel(Panel::Projects)
        }
        Target::Scrollbar(ScrollRegion::Detail) => WheelRegion::Panel(Panel::Detail),
        Target::Section(_)
        | Target::RecallCost
        | Target::RecallWindow(_)
        | Target::RecallUnitToggle
        | Target::TokensByWorktree
        | Target::Scrollbar(ScrollRegion::Graphs) => WheelRegion::Panel(Panel::Graphs),
        Target::PanelFocus(panel) => WheelRegion::Panel(*panel),
        Target::SuggestRow(_) => WheelRegion::Suggest,
        Target::OverlayBody
        | Target::OverlayConfirm
        | Target::OverlayCancel
        | Target::OverlayClose
        | Target::Scrollbar(ScrollRegion::Modal) => WheelRegion::Results,
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
        | Target::Scrollbar(ScrollRegion::ViewerCheckpoints | ScrollRegion::ViewerTimeline) => {
            WheelRegion::Inert
        }
    }
}

/// The wheel a mode-modal owns while it is up with no overlay over it: the
/// Project Settings page and the destination explorer scroll their own lists.
/// Decided before the generic overlay-body arm and guarded on the mode with
/// no overlay, so a Notice over the explorer still owns the wheel — an
/// overlay always outranks a mode.
fn mode_wheel_region(app: &App, target: &Target) -> Option<WheelRegion> {
    if app.overlay.is_some() {
        return None;
    }
    match (app.mode, target) {
        (
            Mode::ProjectSettings,
            Target::ProjectSettingsAction(_)
            | Target::ProjectSettingsClose
            | Target::ProjectSettingsCancel
            | Target::OverlayBody
            | Target::Scrollbar(ScrollRegion::Modal),
        ) => Some(WheelRegion::ProjectSettings),
        (
            Mode::Explorer,
            Target::ExplorerRow(_)
            | Target::ExplorerUp
            | Target::ExplorerSelect
            | Target::ExplorerTypePath
            | Target::ExplorerDotfiles
            | Target::ExplorerClose
            | Target::OverlayBody
            | Target::Scrollbar(ScrollRegion::Modal),
        ) => Some(WheelRegion::Explorer),
        _ => None,
    }
}

/// The keyboard's per-panel vertical semantics, one step per notch: the rail
/// moves the selection (with the bus effects a cursor move owes); detail and
/// graphs move their viewports, never past the end.
fn scroll_panel(s: &mut AppState, panel: Panel, down: bool) -> Flow {
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
            s.app.scroll.detail = nudge(s.app.scroll.detail, down, usize::MAX);
            s.app.clamp_scrolls(s.size);
            Flow::Continue(Effects::default())
        }
        Panel::Graphs => {
            s.app.scroll.graphs = nudge(s.app.scroll.graphs, down, usize::MAX);
            s.app.clamp_scrolls(s.size);
            Flow::Continue(Effects::default())
        }
        Panel::Command => Flow::Continue(Effects::default()),
    }
}

/// The wheel moves the suggest highlight (never inserts, never runs).
fn suggest_wheel(s: &mut AppState, down: bool) -> Flow {
    let len = crate::current_suggestions(&s.app).len();
    if len > 0 {
        s.app.suggest_highlight = nudge(s.app.suggest_highlight, down, len - 1);
        s.dirty = true;
    }
    Flow::Continue(Effects::default())
}

/// The Project Settings page scrolls its own body, and stops following the
/// cursor until a key moves it again.
fn project_settings_wheel(s: &mut AppState, down: bool) -> Flow {
    let body = crate::view::project_settings_body_rect(crate::view::modal_rect(s.size));
    let max = s
        .app
        .project_settings
        .display_line_count()
        .saturating_sub(usize::from(body.h));
    s.app.project_settings.scroll = nudge(s.app.project_settings.scroll, down, max);
    s.app.project_settings.follow_cursor = false;
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// Scroll the destination explorer's listing by one row — the
/// first mode-modal wheel. Clamped to the listing's true overflow against the
/// SAME viewport solver the renderer draws with, so the wheel can never run
/// past the end into empty space.
fn explorer_wheel(s: &mut AppState, down: bool) -> Flow {
    let list_h = usize::from(crate::view::explorer_list_area(crate::view::modal_rect(s.size)).h);
    let max = s.app.explorer.visible_len().saturating_sub(list_h);
    s.app.explorer.scroll = nudge(s.app.explorer.scroll, down, max);
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// The wheel scrolls an open Results overlay like `j`/`k`; any other overlay
/// swallows it.
fn results_wheel(s: &mut AppState, down: bool) -> Flow {
    if let Some(Overlay::Results { scroll, lines, .. }) = s.app.overlay.as_mut() {
        *scroll = nudge(*scroll, down, lines.len().saturating_sub(1));
        s.dirty = true;
    }
    Flow::Continue(Effects::default())
}

/// One wheel notch on an offset: down moves toward `max`, up toward zero,
/// never past either.
fn nudge(offset: usize, down: bool, max: usize) -> usize {
    if down {
        offset.saturating_add(1).min(max)
    } else {
        offset.saturating_sub(1)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::CanopyKey;
    use crate::render::frame::Size;
    use crate::state::fixtures::*;
    use crate::state::{update, Event};

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
}
