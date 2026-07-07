//! Mouse hit-testing: a click coordinate → the dashboard element under it.
//!
//! [`hit_test`] is a **pure** function of the render [`ViewModel`], recomputing
//! geometry from the *same* layout solvers the renderer uses ([`crate::layout`],
//! [`crate::col3::solve_stack`], [`crate::view::visible_window`],
//! [`crate::view::detail_line_kinds`]) — so a click always resolves to the exact
//! element that was drawn, with no stored/mutable geometry and no resize race.
//! It is engine-free (no `xre_*`), so it compiles and tests without the `xre`
//! feature, exactly like the keyboard reducer it feeds.

use crate::col3;
use crate::layout::{
    choose_mode, command_split, single_panel, solve, two_col, LayoutMode, PanelRects, Rect,
};
use crate::state::ViewModel;
use crate::view::{detail_line_kinds, visible_window, LineKind};
use crate::Panel;

/// What a click resolves to — mapped by the reducer onto the same state changes
/// the keyboard produces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Focus a panel (a click in it that isn't a more specific target).
    PanelFocus(Panel),
    /// Select the project list row at this index (column 1).
    ProjectRow(usize),
    /// The feature-toggle at this index (column 2).
    ToggleRow(usize),
    /// A column-3 graph section.
    Section(col3::SectionId),
    /// The bottom command bar.
    CommandBar,
    /// The dispatch overlay's Confirm button (mouse Enter — confirm only).
    OverlayConfirm,
    /// The dispatch overlay's Cancel button (mouse Esc).
    OverlayCancel,
    /// The dispatch overlay body (wheel scroll / inert click).
    OverlayBody,
    /// A suggest-panel row: click INSERTS it (Tab's mouse equivalent — never
    /// execution); the wheel moves the highlight.
    SuggestRow(usize),
}

/// The element at cell `(col, row)`, or `None` for empty space / the title row /
/// a too-small terminal.
#[must_use]
pub fn hit_test(vm: &ViewModel, col: u16, row: u16) -> Option<Target> {
    let size = vm.size;
    if col >= size.w || row >= size.h || row == 0 {
        return None; // off-grid or the title row
    }
    if choose_mode(size) == LayoutMode::TooSmall {
        return None;
    }
    // The body sits one row below the title (mirrors `view::draw`).
    let body = Rect {
        x: 0,
        y: 1,
        w: size.w,
        h: size.h.saturating_sub(1),
    };
    // An open overlay owns its region (drawn last, so it is what the user
    // sees). Confirm buttons resolve exactly where the labels render; every
    // other overlay cell is the (scrollable) body. Clicks outside fall
    // through to nothing while the overlay is up — panels beneath are
    // covered visually, so they must not be clickable.
    if let Some(overlay) = &vm.overlay {
        let rect = crate::view::overlay_rect(body);
        if rect.w >= 8 && rect.h >= 3 && contains(rect, col, row) {
            if let crate::dispatch::Overlay::Confirm { .. } = overlay {
                // The buttons row mirrors `view::draw_overlay`: inner dy=3.
                let inner = inner_of(rect);
                let button_row = inner.y.saturating_add(3);
                if row == button_row {
                    let (confirm, cancel) = crate::view::CONFIRM_BUTTONS;
                    let confirm_w = u16::try_from(confirm.chars().count()).unwrap_or(0);
                    let gap = 3u16;
                    let cancel_w = u16::try_from(cancel.chars().count()).unwrap_or(0);
                    let cx = inner.x;
                    if col >= cx && col < cx.saturating_add(confirm_w) {
                        return Some(Target::OverlayConfirm);
                    }
                    let kx = cx.saturating_add(confirm_w).saturating_add(gap);
                    if col >= kx && col < kx.saturating_add(cancel_w) {
                        return Some(Target::OverlayCancel);
                    }
                }
            }
            return Some(Target::OverlayBody);
        }
        return None;
    }
    if vm.show_wizard {
        let (above, command) = command_split(body);
        if contains(command, col, row) {
            return Some(Target::CommandBar);
        }
        if contains(above, col, row) {
            return Some(Target::PanelFocus(Panel::Projects));
        }
        return None;
    }
    // The suggest panel (drawn over the body, above the bar) owns its rows
    // while open — a click inserts, exactly like Tab (M5-11).
    if !vm.suggestions.is_empty() {
        let rect = crate::view::suggest_panel_rect(vm.size, vm.suggestions.len());
        if rect.h > 0 && contains(rect, col, row) {
            let inner = inner_of(rect);
            if let Some(dy) = local_row(inner, col, row) {
                let idx = usize::from(dy);
                if idx < vm.suggestions.len().min(usize::from(inner.h)) {
                    return Some(Target::SuggestRow(idx));
                }
            }
            return Some(Target::PanelFocus(Panel::Command));
        }
    }
    match choose_mode(size) {
        LayoutMode::Full3Col => hit_grid(vm, solve(body), col, row),
        LayoutMode::TwoCol => hit_grid(vm, two_col(body), col, row),
        LayoutMode::SinglePanel => hit_single(vm, body, col, row),
        LayoutMode::TooSmall => None,
    }
}

/// Hit-test one of the multi-column grids (full or two-column).
fn hit_grid(vm: &ViewModel, r: PanelRects, col: u16, row: u16) -> Option<Target> {
    if contains(r.command, col, row) {
        return Some(Target::CommandBar);
    }
    if contains(r.col1, col, row) {
        return Some(project_or_focus(vm, r.col1, col, row));
    }
    if contains(r.col2, col, row) {
        return Some(toggle_or_focus(vm, r.col2, col, row));
    }
    if contains(r.col3, col, row) {
        return Some(section_or_focus(r.col3, col, row));
    }
    None
}

/// Hit-test the single-panel layout: a tab strip over one focused panel.
fn hit_single(vm: &ViewModel, body: Rect, col: u16, row: u16) -> Option<Target> {
    let (tabs, panel, command) = single_panel(body);
    if contains(command, col, row) {
        return Some(Target::CommandBar);
    }
    if contains(tabs, col, row) {
        return Some(Target::PanelFocus(tab_at(col, tabs)));
    }
    if contains(panel, col, row) {
        return Some(match vm.focus {
            Panel::Detail => toggle_or_focus(vm, panel, col, row),
            Panel::Graphs => section_or_focus(panel, col, row),
            Panel::Projects | Panel::Command => project_or_focus(vm, panel, col, row),
        });
    }
    None
}

/// The tab under `col` in the `[Projects] Detail Graphs` strip — mapped by
/// horizontal thirds (exact glyph widths aren't needed for a coarse tab click).
fn tab_at(col: u16, tabs: Rect) -> Panel {
    let rel = col.saturating_sub(tabs.x);
    let third = (tabs.w / 3).max(1);
    if rel < third {
        Panel::Projects
    } else if rel < third.saturating_mul(2) {
        Panel::Detail
    } else {
        Panel::Graphs
    }
}

/// A click in column 1 → the project row under it, else focus the panel.
fn project_or_focus(vm: &ViewModel, rect: Rect, col: u16, row: u16) -> Target {
    let inner = inner_of(rect);
    if let Some(dy) = local_row(inner, col, row) {
        if !vm.projects.is_empty() {
            let (start, end) = visible_window(vm.selected, vm.projects.len(), usize::from(inner.h));
            let idx = start + usize::from(dy);
            if idx < end {
                return Target::ProjectRow(idx);
            }
        }
    }
    Target::PanelFocus(Panel::Projects)
}

/// A click in column 2 → the feature-toggle row under it, else focus the panel.
fn toggle_or_focus(vm: &ViewModel, rect: Rect, col: u16, row: u16) -> Target {
    let inner = inner_of(rect);
    if let (Some(detail), Some(dy)) = (&vm.detail, local_row(inner, col, row)) {
        let lines = detail_line_kinds(detail, vm.toggle_cursor, vm.focus == Panel::Detail);
        let line_idx = vm.detail_scroll + usize::from(dy);
        if let Some(LineKind::Toggle(i)) = lines.get(line_idx).map(|l| l.kind) {
            return Target::ToggleRow(i);
        }
    }
    Target::PanelFocus(Panel::Detail)
}

/// A click in column 3 → the graph section under it, else focus the panel.
/// (The section stack is a pure function of the column's inner height, so it
/// needs no col3 data.)
fn section_or_focus(rect: Rect, col: u16, row: u16) -> Target {
    let inner = inner_of(rect);
    if let Some(dy) = local_row(inner, col, row) {
        for slot in col3::solve_stack(inner.h) {
            if dy >= slot.y && dy < slot.y.saturating_add(slot.rows) {
                return Target::Section(slot.id);
            }
        }
    }
    Target::PanelFocus(Panel::Graphs)
}

/// The content rect inside a bordered panel (mirrors [`crate::Frame::border`]:
/// a one-cell ring, so content is inset by 1). Panels too small to border keep
/// their whole area.
fn inner_of(r: Rect) -> Rect {
    if r.w < 2 || r.h < 2 {
        return r;
    }
    Rect {
        x: r.x + 1,
        y: r.y + 1,
        w: r.w - 2,
        h: r.h - 2,
    }
}

/// The 0-based row offset of `(col, row)` within `inner`, or `None` when the
/// point is outside it.
fn local_row(inner: Rect, col: u16, row: u16) -> Option<u16> {
    if !contains(inner, col, row) {
        return None;
    }
    Some(row - inner.y)
}

/// Whether `(x, y)` lies within `r` (right/bottom exclusive).
fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && y >= r.y && x < r.x.saturating_add(r.w) && y < r.y.saturating_add(r.h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Size;
    use crate::state::AppState;
    use crate::{EntryKind, Project, ProjectStatus};

    fn project(name: &str) -> Project {
        Project {
            name: name.into(),
            path: format!("/r/{name}"),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }
    }

    /// A ready dashboard view-model at a full-grid size.
    fn vm(w: u16, h: u16) -> ViewModel {
        let mut s = AppState::new(
            vec![project("a"), project("b"), project("c")],
            Size::new(w, h),
        );
        s.app.splash.skip();
        s.snapshot()
    }

    #[test]
    fn title_row_and_offgrid_are_none() {
        let v = vm(120, 40);
        assert_eq!(hit_test(&v, 5, 0), None, "the title row is inert");
        assert_eq!(hit_test(&v, 999, 5), None, "off-grid is None");
        assert_eq!(hit_test(&v, 5, 999), None);
    }

    #[test]
    fn too_small_yields_no_targets() {
        let v = vm(10, 4);
        assert_eq!(hit_test(&v, 2, 2), None);
    }

    #[test]
    fn clicking_a_project_row_resolves_to_its_index() {
        let v = vm(120, 40);
        // col1 inner content begins at (1, 2): border at x=0,y=1; inset by 1.
        assert_eq!(hit_test(&v, 3, 2), Some(Target::ProjectRow(0)));
        assert_eq!(hit_test(&v, 3, 3), Some(Target::ProjectRow(1)));
        assert_eq!(hit_test(&v, 3, 4), Some(Target::ProjectRow(2)));
    }

    #[test]
    fn clicking_empty_col1_focuses_projects() {
        let v = vm(120, 40);
        // A row well below the three project rows is still inside col1 → focus.
        assert_eq!(
            hit_test(&v, 3, 20),
            Some(Target::PanelFocus(Panel::Projects))
        );
    }

    #[test]
    fn clicking_the_graphs_column_hits_a_section_or_focus() {
        let v = vm(120, 40);
        // col3 starts at x = col1.w + col2.w; sample a column well inside it.
        let hit = hit_test(&v, 100, 6);
        assert!(
            matches!(
                hit,
                Some(Target::Section(_) | Target::PanelFocus(Panel::Graphs))
            ),
            "graphs column resolves to a section or its focus, got {hit:?}"
        );
    }

    #[test]
    fn clicking_the_command_bar() {
        let v = vm(120, 40);
        // The command row is the last three rows.
        assert_eq!(hit_test(&v, 10, 39), Some(Target::CommandBar));
    }

    #[test]
    fn detail_column_focuses_or_hits_a_toggle() {
        let v = vm(120, 40);
        // col2 begins after col1; a mid-column click resolves to Detail focus or
        // a toggle row (the thin fallback detail has no toggles → focus).
        let hit = hit_test(&v, 40, 10);
        assert!(
            matches!(
                hit,
                Some(Target::ToggleRow(_) | Target::PanelFocus(Panel::Detail))
            ),
            "got {hit:?}"
        );
    }

    #[test]
    fn overlay_owns_its_region_and_buttons_resolve() {
        let mut s = AppState::new(vec![project("a")], Size::new(100, 32));
        s.app.splash.skip();
        s.app.overlay = Some(crate::dispatch::Overlay::Confirm {
            request: crate::dispatch::DispatchRequest::Rebuild,
            repo: "/r/a".into(),
        });
        let v = s.snapshot();
        let body = Rect {
            x: 0,
            y: 1,
            w: 100,
            h: 31,
        };
        let rect = crate::view::overlay_rect(body);
        let inner = inner_of(rect);
        let button_row = inner.y + 3;
        // The Confirm button's first cell.
        assert_eq!(
            hit_test(&v, inner.x, button_row),
            Some(Target::OverlayConfirm)
        );
        // The Cancel button (after the confirm label + the 3-cell gap).
        let confirm_w = u16::try_from(crate::view::CONFIRM_BUTTONS.0.chars().count()).unwrap();
        assert_eq!(
            hit_test(&v, inner.x + confirm_w + 3, button_row),
            Some(Target::OverlayCancel)
        );
        // Any other overlay cell is the body.
        assert_eq!(
            hit_test(&v, inner.x + 1, inner.y),
            Some(Target::OverlayBody)
        );
        // Outside the overlay nothing is clickable while it is up.
        assert_eq!(hit_test(&v, 1, body.y + body.h.saturating_sub(1)), None);
    }

    #[test]
    fn results_overlay_has_no_buttons() {
        let mut s = AppState::new(vec![project("a")], Size::new(100, 32));
        s.app.splash.skip();
        s.app.overlay = Some(crate::dispatch::Overlay::Results {
            title: "rift status".into(),
            lines: vec!["ok".into()],
            scroll: 0,
            footer: None,
            truncated: false,
        });
        let v = s.snapshot();
        let body = Rect {
            x: 0,
            y: 1,
            w: 100,
            h: 31,
        };
        let inner = inner_of(crate::view::overlay_rect(body));
        assert_eq!(
            hit_test(&v, inner.x, inner.y + 3),
            Some(Target::OverlayBody),
            "no confirm target on a results overlay"
        );
    }

    proptest::proptest! {
        /// Hit-testing is total and panic-free for any coordinate and size.
        #[test]
        fn hit_test_never_panics(w in 0u16..300, h in 0u16..120, col in 0u16..300, row in 0u16..120) {
            let mut s = AppState::new(vec![project("a")], Size::new(w, h));
            s.app.splash.skip();
            let v = s.snapshot();
            let _ = hit_test(&v, col, row);
        }
    }
}
