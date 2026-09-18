//! The Recall Viewer's mouse plane: the tab strip, the projects rail, the
//! checkpoint catalogue and the timeline, resolved through the same
//! `recall_viewer::geometry` the viewer draws with.

use crate::layout::Rect;
use crate::recall_viewer::{
    geometry, timeline_controls, timeline_line_count, RecallViewerState, ViewerPane,
    CHECKPOINT_ROW_HEIGHT,
};
use crate::state::ViewModel;
use crate::Panel;

use super::grid::rail_row_at;
use super::{contains, inner_of, local_row, on_scrollbar, third_of, ScrollRegion, Target};

/// The viewer's element under `(col, row)`: the command bar, the tab strip
/// (drawn on the single-column tier only), then whichever pane is under the
/// click — a pane the tier gave no width to is not on screen and never
/// answers.
pub(super) fn hit_recall_viewer(vm: &ViewModel, col: u16, row: u16) -> Option<Target> {
    let geometry = geometry(vm.size, vm.viewer.pane);
    if contains(geometry.command, col, row) {
        return Some(Target::CommandBar);
    }
    if geometry.tabs.h > 0 && contains(geometry.tabs, col, row) {
        return Some(Target::ViewerTab(viewer_tab_at(col, geometry.tabs)));
    }
    if geometry.projects.w > 0 && contains(geometry.projects, col, row) {
        let inner = inner_of(geometry.projects);
        let rail = rail_row_at(vm, inner, geometry.projects, col, row);
        return Some(rail.unwrap_or(Target::PanelFocus(Panel::Projects)));
    }
    if geometry.checkpoints.w > 0 && contains(geometry.checkpoints, col, row) {
        return Some(checkpoint_or_focus(
            &vm.viewer,
            geometry.checkpoints,
            col,
            row,
        ));
    }
    if geometry.timeline.w > 0 && contains(geometry.timeline, col, row) {
        return Some(timeline_line_or_focus(
            &vm.viewer,
            geometry.timeline,
            col,
            row,
        ));
    }
    None
}

/// The pane under `col` in the single-column tab strip, by horizontal thirds.
fn viewer_tab_at(col: u16, tabs: Rect) -> ViewerPane {
    match third_of(col, tabs) {
        0 => ViewerPane::Projects,
        1 => ViewerPane::Checkpoints,
        _ => ViewerPane::Timeline,
    }
}

/// The checkpoint catalogue: the filter chip on its first row, the `Latest`
/// action on the second while a live refresh holds new rows, then the list's
/// overflow scrollbar, then the catalogue row under the click (each row is
/// [`CHECKPOINT_ROW_HEIGHT`] cells tall), else the pane's focus.
fn checkpoint_or_focus(viewer: &RecallViewerState, pane: Rect, col: u16, row: u16) -> Target {
    let inner = inner_of(pane);
    if row == inner.y {
        return Target::ViewerFilter;
    }
    let has_second_header_row = viewer.filter_progress.is_some() || viewer.new_count > 0;
    let header_rows = if has_second_header_row { 2 } else { 1 };
    if viewer.new_count > 0 && row == inner.y.saturating_add(1) {
        return Target::ViewerLatest;
    }
    let list = Rect {
        x: inner.x,
        y: inner.y.saturating_add(header_rows),
        w: inner.w,
        h: inner.h.saturating_sub(header_rows),
    };
    let shown = viewer.visible.len();
    if on_scrollbar(pane, viewer.checkpoint_scroll, shown, col, row) {
        return Target::Scrollbar(ScrollRegion::ViewerCheckpoints);
    }
    let Some(dy) = local_row(list, col, row) else {
        return Target::PanelFocus(Panel::Detail);
    };
    let row_offset = usize::from(dy / CHECKPOINT_ROW_HEIGHT);
    let index = viewer.checkpoint_scroll.saturating_add(row_offset);
    if index < shown {
        return Target::ViewerCheckpointRow(index);
    }
    Target::PanelFocus(Panel::Detail)
}

/// The timeline: its two control chips, the content's overflow scrollbar,
/// then the flattened line under the click, else the pane's focus.
fn timeline_line_or_focus(viewer: &RecallViewerState, pane: Rect, col: u16, row: u16) -> Target {
    let controls = timeline_controls(pane);
    if contains(controls.full_session, col, row) {
        return Target::ViewerFullSession;
    }
    if contains(controls.dashboard, col, row) {
        return Target::ViewerDashboard;
    }
    let line_count = timeline_line_count(viewer);
    if on_scrollbar(
        controls.content,
        viewer.timeline_scroll,
        line_count,
        col,
        row,
    ) {
        return Target::Scrollbar(ScrollRegion::ViewerTimeline);
    }
    let Some(dy) = local_row(controls.content, col, row) else {
        return Target::PanelFocus(Panel::Graphs);
    };
    let line = viewer.timeline_scroll.saturating_add(usize::from(dy));
    if line < line_count {
        return Target::ViewerTimelineLine(line);
    }
    Target::PanelFocus(Panel::Graphs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hit::fixtures::state_with_detail;
    use crate::hit::hit_test;

    #[test]
    fn recall_viewer_action_chips_hit_the_exact_shared_rectangles() {
        let mut state = state_with_detail(120, 40);
        let _ = state.open_recall_viewer(None);
        let view = state.snapshot();
        let geometry = crate::recall_viewer::geometry(view.size, view.viewer.pane);
        let controls = crate::recall_viewer::timeline_controls(geometry.timeline);
        assert_eq!(
            hit_test(&view, controls.full_session.x, controls.full_session.y),
            Some(Target::ViewerFullSession)
        );
        assert_eq!(
            hit_test(&view, controls.dashboard.x, controls.dashboard.y),
            Some(Target::ViewerDashboard)
        );
        let checkpoints = inner_of(geometry.checkpoints);
        assert_eq!(
            hit_test(&view, checkpoints.x, checkpoints.y),
            Some(Target::ViewerFilter)
        );
    }
}
