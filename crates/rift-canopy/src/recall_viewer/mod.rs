//! Read-only Recall Viewer model, geometry, and terminal rendering.
//!
//! The data worker hands this module immutable, display-safe snapshots. The
//! reducer mutates only session-local navigation/filter/expansion state; there
//! is deliberately no persistence operation for any field here.
//!
//! The snapshot types (`model`), the worker's request/response vocabulary
//! (`worker`) and the reducer-side state (`state`) are the model; `geometry`
//! is the one solver both drawing and `hit/viewer.rs` read; `draw`,
//! `catalogue` and `timeline` paint the body and its two content panes.

mod catalogue;
mod draw;
mod geometry;
mod model;
mod state;
mod timeline;
mod worker;

#[cfg(test)]
mod fixtures;

pub(crate) use draw::draw;
pub use geometry::{
    geometry, timeline_controls, ViewerGeometry, ViewerTimelineControls, CHECKPOINT_ROW_HEIGHT,
};
pub use model::{
    SyntaxClass, ViewerCheckpointRow, ViewerDetail, ViewerFilterProgress, ViewerLine,
    ViewerLineKind, ViewerOrigin, ViewerSpan, ViewerTimelineItem, ViewerTimelineSegment,
    ViewerTimestampProvenance,
};
pub use state::RecallViewerState;
pub use timeline::{timeline_item_at, timeline_line_count};
pub use worker::{ViewerErrorArea, ViewerEvent, ViewerRequest};

/// Top-level Canopy surface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Surface {
    /// The normal Repository/Token dashboard.
    #[default]
    Dashboard,
    /// Read-only Projects/Checkpoints/Timeline inspection surface.
    RecallViewer,
}

/// Independent focus ring for the Recall Viewer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewerPane {
    /// Projects rail.
    #[default]
    Projects,
    /// Checkpoint catalogue.
    Checkpoints,
    /// Selected-checkpoint timeline.
    Timeline,
    /// Bottom command panel.
    Command,
}

impl ViewerPane {
    /// Cycle in the locked Projects → Checkpoints → Timeline → Command order.
    #[must_use]
    pub fn cycle(self, forward: bool) -> Self {
        let order = [Self::Checkpoints, Self::Timeline, Self::Command];
        let index = order.iter().position(|pane| *pane == self).unwrap_or(0);
        let next = if forward {
            (index + 1) % order.len()
        } else {
            (index + order.len() - 1) % order.len()
        };
        order.get(next).copied().unwrap_or_default()
    }
}

/// Worker-request generation. A response applies only when both project and
/// generation still match the viewer state.
pub type ViewerGeneration = u64;

/// A direct launch/deep-link request, resolved by the read-only data worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerLaunch {
    /// Repository path selected by CLI resolution.
    pub repo: String,
    /// Optional checkpoint id/prefix or Git commit-ish.
    pub target: Option<String>,
    /// The repository is a session-only row and must never be registered.
    pub transient: bool,
}

/// Loading posture for a viewer section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ViewerLoadState {
    /// No request has been made.
    #[default]
    Idle,
    /// A request is in flight.
    Loading,
    /// Some independently valid facts arrived; more enrichment is in flight.
    Partial,
    /// The requested data is complete for the current generation.
    Ready,
    /// A safe category/detail. Existing partial facts remain rendered.
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_focus_order_wraps_both_directions() {
        assert_eq!(ViewerPane::Projects.cycle(true), ViewerPane::Checkpoints);
        assert_eq!(ViewerPane::Command.cycle(true), ViewerPane::Projects);
        assert_eq!(ViewerPane::Projects.cycle(false), ViewerPane::Command);
    }
}
