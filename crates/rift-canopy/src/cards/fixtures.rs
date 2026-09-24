//! The seeders the card modules' tests share: a rect, a detail-bearing
//! project and the draw context over it.
//!
//! One file so every card's tests draw the same project, built one way.

use crate::col2::{CaptureSummary, DetailView, ModelProvenance, ModelUsage};
use crate::col3::ActivityView;
use crate::layout::Rect;
use crate::{EntryKind, ProjectStatus};

use super::CardCtx;

/// A rect from its four coordinates, for a card area or an expected chip.
pub(super) fn fixture_rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
    Rect { x, y, w, h }
}

/// A plain repository at `status` with two runtime models (15 calls) and a
/// capture summary.
pub(super) fn fixture_detail(status: ProjectStatus) -> DetailView {
    DetailView {
        running_members: Vec::new(),
        path: "/r/proj".into(),
        label: "proj".into(),
        kind: EntryKind::Repo,
        status,
        models: vec![
            ModelUsage {
                model: "claude-opus".into(),
                calls: 12,
                source: ModelProvenance::Runtime,
            },
            ModelUsage {
                model: "claude-haiku".into(),
                calls: 3,
                source: ModelProvenance::Runtime,
            },
        ],
        capture: CaptureSummary {
            events: 128,
            sessions: 6,
            checkpoints: 12,
            last_checkpoint_at_ms: Some(1),
        },
        project_settings: Box::new(crate::ProjectSettingsView::thin(
            "/r/proj".into(),
            "proj".into(),
        )),
    }
}

/// The draw context over `d` and `act`: focused, ASCII, nothing pressed.
pub(super) fn fixture_ctx<'a>(d: &'a DetailView, act: &'a ActivityView) -> CardCtx<'a> {
    CardCtx {
        detail: d,
        activity: act,
        focused: true,
        unicode: false,
        pressed: None,
        press_pulse: 0.0,
    }
}
