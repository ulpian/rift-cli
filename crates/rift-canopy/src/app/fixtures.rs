//! The loop doubles every test module under `app/` shares: a present-counting
//! backend, a scripted event source, the inert persist and lister seams, and
//! the two ready dashboards.

use std::time::Duration;

use rift_core::Result as RiftResult;

use crate::render::frame::{Frame, Size};
use crate::render::{Capabilities, RenderBackend, RenderError};
use crate::state::{AppState, Event, PersistOp};
use crate::{EntryKind, Project};

use super::EventSource;

/// A render backend that counts presents (the headless loop probe).
pub(super) struct MockBackend {
    pub(super) presents: usize,
}
impl RenderBackend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::empty()
    }
    fn present(&mut self, _frame: &Frame) -> Result<(), RenderError> {
        self.presents += 1;
        Ok(())
    }
}

/// A scripted event source: each `poll` yields the next batch (then ticks).
pub(super) struct ScriptedEvents {
    steps: std::collections::VecDeque<Vec<Event>>,
}
impl ScriptedEvents {
    pub(super) fn new(steps: Vec<Vec<Event>>) -> Self {
        Self {
            steps: steps.into(),
        }
    }
}
impl EventSource for ScriptedEvents {
    fn poll(&mut self, _timeout: Duration) -> RiftResult<Vec<Event>> {
        Ok(self.steps.pop_front().unwrap_or_default())
    }
}

pub(super) fn no_persist(_op: &PersistOp) -> RiftResult<Vec<Project>> {
    Ok(vec![])
}

/// The lister seam's inert double (the destination explorer): every listing is empty, so
/// a loop test that never opens the explorer is unaffected by the seam.
pub(super) fn no_lister(_path: &std::path::Path) -> crate::explorer::DirListing {
    crate::explorer::DirListing::default()
}

pub(super) fn ready_state() -> AppState {
    let mut s = AppState::new(vec![], Size::new(80, 24));
    s.app.splash.skip();
    s.app.overlay = None; // dismiss the first-run wizard; drive the dashboard
    s
}

pub(super) fn one_project_state() -> AppState {
    let mut s = AppState::new(
        vec![Project {
            name: "a".into(),
            path: "/r/a".into(),
            status: crate::ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }],
        Size::new(100, 32),
    );
    s.app.splash.skip();
    s
}
