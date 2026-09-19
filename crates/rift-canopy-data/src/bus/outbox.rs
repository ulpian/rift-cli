//! The staging buffer between the worker and the bounded update channel:
//! latest wins per (project, feed), so a write storm can neither stall the
//! worker nor flood the loop.
//!
//! A separate module because the coalescing key is a contract of its own —
//! which updates supersede which — and the two workers share it.

use std::collections::BTreeMap;
use std::sync::mpsc::{SyncSender, TrySendError};

use rift_canopy::Event;

/// The staging buffer in front of the bounded update channel: **latest wins
/// per (project, feed)**, so a write storm can neither stall the worker (sends
/// never block) nor flood the loop (the channel never exceeds its bound, and
/// superseded updates are simply replaced while they wait).
#[derive(Default)]
pub(super) struct Outbox {
    pending: BTreeMap<(String, FeedKind), Event>,
}

/// The feed kinds an update can belong to — the coalescing axis (the data-plane split's
/// Tokens/Nic/Recall trio alongside the status/detail flow and the global
/// health report).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum FeedKind {
    /// Per-project status (+ detail for the active project).
    Status,
    /// Token spend + breakdown.
    Tokens,
    /// Atomic System-panel snapshot.
    System,
    /// Recall savings/calls.
    Recall,
    /// The activity card's trailing-month heat strips.
    Activity,
    /// The dispatch card's window facts.
    Dispatch,
    /// The recap card's window facts.
    Recap,
    /// The data-plane health report (global — one key for the whole plane).
    Health,
    /// Recall Viewer checkpoint catalogue/enrichment.
    ViewerCatalogue,
    /// Recall Viewer selected detail.
    ViewerDetail,
    /// Recall Viewer progressive filter scan.
    ViewerFilter,
}

impl Outbox {
    /// Stage an update, replacing any staged one for the same (project, feed).
    pub(super) fn stage(&mut self, ev: Event) {
        self.pending.insert(feed_key(&ev), ev);
    }

    /// Push staged updates through the bounded channel without blocking: a
    /// full channel keeps the remainder staged (newer stages keep replacing
    /// them — latest-per-feed under backpressure). `Err` iff the loop is gone.
    pub(super) fn flush(&mut self, tx: &SyncSender<Event>) -> Result<(), ()> {
        let keys: Vec<(String, FeedKind)> = self.pending.keys().cloned().collect();
        for key in keys {
            let Some(ev) = self.pending.remove(&key) else {
                continue;
            };
            match tx.try_send(ev) {
                Ok(()) => {}
                Err(TrySendError::Full(ev)) => {
                    // Back under pressure: keep it staged; a newer update for
                    // the same key replaces it rather than queueing behind it.
                    self.pending.insert(key, ev);
                    return Ok(());
                }
                Err(TrySendError::Disconnected(_)) => return Err(()),
            }
        }
        Ok(())
    }
}

/// The coalescing key for an update.
fn feed_key(ev: &Event) -> (String, FeedKind) {
    match ev {
        Event::ProjectData(pd) => (pd.project.clone(), FeedKind::Status),
        Event::Feed(f) => {
            let kind = match f.payload {
                rift_canopy::FeedPayload::Tokens { .. } => FeedKind::Tokens,
                rift_canopy::FeedPayload::System { .. } => FeedKind::System,
                rift_canopy::FeedPayload::Recall { .. } => FeedKind::Recall,
                rift_canopy::FeedPayload::Activity(_) => FeedKind::Activity,
                rift_canopy::FeedPayload::Dispatch(_) => FeedKind::Dispatch,
                rift_canopy::FeedPayload::Recap(_) => FeedKind::Recap,
            };
            (f.project.clone(), kind)
        }
        // `WatchMode` is never staged — sent once, directly, at worker start.
        Event::Health(_) | Event::WatchMode(_) => (String::new(), FeedKind::Health),
        Event::Viewer(event) => match event {
            rift_canopy::ViewerEvent::Catalogue { project, .. }
            | rift_canopy::ViewerEvent::Error {
                project,
                area: rift_canopy::ViewerErrorArea::Catalogue,
                ..
            } => (project.clone(), FeedKind::ViewerCatalogue),
            rift_canopy::ViewerEvent::Detail { project, .. }
            | rift_canopy::ViewerEvent::Error {
                project,
                area: rift_canopy::ViewerErrorArea::Detail,
                ..
            } => (project.clone(), FeedKind::ViewerDetail),
            rift_canopy::ViewerEvent::FilterProgress { project, .. }
            | rift_canopy::ViewerEvent::Error {
                project,
                area: rift_canopy::ViewerErrorArea::Filter,
                ..
            } => (project.clone(), FeedKind::ViewerFilter),
        },
        // Non-project events have no coalescing axis today; key them globally.
        _ => (String::new(), FeedKind::Status),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::sync_channel;

    use rift_canopy::{ProjectData, ProjectStatus};

    use super::*;

    #[test]
    fn outbox_bounds_the_queue_and_keeps_the_latest_per_feed() {
        // A synthetic storm: 100 updates for one project against a channel of 2.
        let (tx, rx) = sync_channel::<Event>(2);
        let mut outbox = Outbox::default();
        for i in 0..100u16 {
            outbox.stage(Event::ProjectData(ProjectData {
                project: "/r/a".into(),
                status: ProjectStatus::Running {
                    since_ms: i64::from(i),
                    active_sessions: 1,
                },
                detail: None,
            }));
            outbox.flush(&tx).unwrap();
        }
        // The channel never exceeded its bound: at most 2 queued...
        let mut drained = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            drained.push(ev);
        }
        assert!(
            drained.len() <= 2,
            "bounded channel overfilled: {}",
            drained.len()
        );
        // ...and the LATEST staged update survives in the outbox for the next
        // flush (latest-per-(project, feed) replacement, not queue growth).
        outbox.flush(&tx).unwrap();
        let last = rx.try_recv().expect("the latest staged update flushes");
        match last {
            Event::ProjectData(pd) => assert_eq!(
                pd.status,
                ProjectStatus::Running {
                    since_ms: 99,
                    active_sessions: 1
                },
                "an older superseded update leaked through"
            ),
            other => panic!("expected ProjectData, got {other:?}"),
        }
    }

    #[test]
    fn distinct_projects_each_keep_their_latest_under_backpressure() {
        let (tx, rx) = sync_channel::<Event>(1);
        let mut outbox = Outbox::default();
        for round in 0..10i64 {
            for proj in ["/r/a", "/r/b", "/r/c"] {
                outbox.stage(Event::ProjectData(ProjectData {
                    project: proj.into(),
                    status: ProjectStatus::Running {
                        since_ms: round,
                        active_sessions: 1,
                    },
                    detail: None,
                }));
            }
            outbox.flush(&tx).unwrap();
        }
        // Drain everything (alternating drain + flush until dry).
        let mut latest: BTreeMap<String, i64> = BTreeMap::new();
        loop {
            while let Ok(ev) = rx.try_recv() {
                if let Event::ProjectData(pd) = ev {
                    if let ProjectStatus::Running { since_ms, .. } = pd.status {
                        latest.insert(pd.project, since_ms);
                    }
                }
            }
            if outbox.pending.is_empty() {
                break;
            }
            outbox.flush(&tx).unwrap();
        }
        // Every project's LATEST update ultimately got through.
        assert_eq!(latest.get("/r/a"), Some(&9));
        assert_eq!(latest.get("/r/b"), Some(&9));
        assert_eq!(latest.get("/r/c"), Some(&9));
    }
}
