//! The one layout solver the viewer draws with and `hit/viewer.rs` hit-tests against.

use crate::layout::{single_panel, PanelRects, Rect};
use crate::render::frame::Size;

use super::ViewerPane;

/// Shared renderer/hit-test geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewerGeometry {
    /// Projects panel.
    pub projects: Rect,
    /// Checkpoint panel.
    pub checkpoints: Rect,
    /// Timeline panel.
    pub timeline: Rect,
    /// Single-column tab strip.
    pub tabs: Rect,
    /// Hint row.
    pub hint: Rect,
    /// Command panel.
    pub command: Rect,
}

/// Shared timeline action/content geometry. Keeping this beside the viewer
/// layout ensures the visible chips and their mouse targets never drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewerTimelineControls {
    /// Toggle between the checkpoint-new compact slice and the full compact
    /// session artifact.
    pub full_session: Rect,
    /// Return to the dashboard surface.
    pub dashboard: Rect,
    /// Scrollable timeline content below the action row(s).
    pub content: Rect,
}

/// Catalogue rows are deliberately multi-line.
pub const CHECKPOINT_ROW_HEIGHT: u16 = 5;

/// The zero-sized rectangle the column tiers give the tab strip, which only
/// the single-column tier draws, and the too-small tier gives every chrome row.
const ORIGIN: Rect = Rect {
    x: 0,
    y: 0,
    w: 0,
    h: 0,
};

/// Resolve the two timeline action chips and remaining scrollable content.
#[must_use]
pub fn timeline_controls(timeline: Rect) -> ViewerTimelineControls {
    let inner = if timeline.w < 2 || timeline.h < 2 {
        timeline
    } else {
        Rect {
            x: timeline.x + 1,
            y: timeline.y + 1,
            w: timeline.w - 2,
            h: timeline.h - 2,
        }
    };
    let empty = Rect {
        x: inner.x,
        y: inner.y,
        w: 0,
        h: 0,
    };
    if inner.h == 0 || inner.w == 0 {
        return ViewerTimelineControls {
            full_session: empty,
            dashboard: empty,
            content: inner,
        };
    }
    let full_width = 16.min(inner.w);
    let dashboard_width = 13.min(inner.w);
    let stacked =
        inner.w < full_width.saturating_add(dashboard_width).saturating_add(1) && inner.h > 1;
    let full_session = Rect {
        x: inner.x,
        y: inner.y,
        w: full_width,
        h: 1,
    };
    let dashboard = Rect {
        x: if stacked {
            inner.x
        } else {
            inner.x + inner.w.saturating_sub(dashboard_width)
        },
        y: inner.y + u16::from(stacked),
        w: dashboard_width,
        h: 1,
    };
    let control_rows = 1 + u16::from(stacked);
    ViewerTimelineControls {
        full_session,
        dashboard,
        content: Rect {
            x: inner.x,
            y: inner.y.saturating_add(control_rows),
            w: inner.w,
            h: inner.h.saturating_sub(control_rows),
        },
    }
}

/// Resolve the locked responsive viewer layout. Full mode always uses the
/// default 20/40/40 solver, independent of Panel Editor state.
#[must_use]
pub fn geometry(size: Size, pane: ViewerPane) -> ViewerGeometry {
    let (body, hint, command) = crate::dashboard::geometry(size);
    if size.w < 76 {
        return single_panel_geometry(
            Rect {
                h: body.h + hint.h + command.h,
                ..body
            },
            pane,
        );
    }
    // The project drawer is shared with the dashboard; the viewer uses two panes.
    let hidden = Rect { w: 0, h: 0, ..body };
    let split = body.w / 3;
    let rects = PanelRects {
        col1: hidden,
        col2: Rect { w: split, ..body },
        col3: Rect {
            x: body.x + split,
            w: body.w.saturating_sub(split),
            ..body
        },
        hint,
        command,
    };
    column_geometry(&rects)
}

/// The full and two-column tiers: every pane is on screen at once, so the
/// dashboard's own column solve maps onto the viewer's panes one to one.
fn column_geometry(rects: &PanelRects) -> ViewerGeometry {
    ViewerGeometry {
        projects: rects.col1,
        checkpoints: rects.col2,
        timeline: rects.col3,
        tabs: ORIGIN,
        hint: rects.hint,
        command: rects.command,
    }
}

/// The single-column tier: only the focused pane gets the panel; the others
/// collapse to zero width at the panel's corner, so neither draw nor hit-test
/// ever answers for a pane that is not on screen.
fn single_panel_geometry(body: Rect, pane: ViewerPane) -> ViewerGeometry {
    let (tabs, panel, hint, command) = single_panel(body);
    let empty = Rect {
        x: panel.x,
        y: panel.y,
        w: 0,
        h: 0,
    };
    let panel_if = |shown: ViewerPane| if pane == shown { panel } else { empty };
    ViewerGeometry {
        projects: empty,
        checkpoints: if matches!(pane, ViewerPane::Projects | ViewerPane::Command) {
            panel
        } else {
            panel_if(ViewerPane::Checkpoints)
        },
        timeline: panel_if(ViewerPane::Timeline),
        tabs,
        hint,
        command,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_viewer_has_no_permanent_project_rail() {
        let geometry = geometry(Size::new(120, 40), ViewerPane::Timeline);
        assert_eq!(
            (
                geometry.projects.w,
                geometry.checkpoints.w,
                geometry.timeline.w
            ),
            (0, 40, 80)
        );
        assert_eq!(geometry.checkpoints.y, 2);
    }

    #[test]
    fn narrow_viewer_shows_one_content_pane_and_keeps_projects_in_the_drawer() {
        for pane in [
            ViewerPane::Projects,
            ViewerPane::Checkpoints,
            ViewerPane::Timeline,
            ViewerPane::Command,
        ] {
            let geometry = geometry(Size::new(60, 30), pane);
            assert_eq!(geometry.projects.w, 0);
            assert_eq!(geometry.checkpoints.w > 0, pane != ViewerPane::Timeline);
            assert_eq!(geometry.timeline.w > 0, pane == ViewerPane::Timeline);
        }
    }

    #[test]
    fn timeline_action_chips_never_overlap_the_scrollable_body() {
        let wide = timeline_controls(Rect {
            x: 10,
            y: 2,
            w: 50,
            h: 20,
        });
        assert!(wide.full_session.x + wide.full_session.w <= wide.dashboard.x);
        assert!(wide.full_session.y < wide.content.y);
        assert!(wide.dashboard.y < wide.content.y);

        let narrow = timeline_controls(Rect {
            x: 0,
            y: 0,
            w: 24,
            h: 12,
        });
        assert!(narrow.full_session.y < narrow.dashboard.y);
        assert!(narrow.dashboard.y < narrow.content.y);
    }
}
