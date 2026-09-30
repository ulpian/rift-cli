//! The live data bus: an off-thread reader that turns the dashboard's data
//! requests into scoped, **read-only** projection queries and feeds the results
//! back to the synchronous render loop as [`Event::ProjectData`].
//!
//! The bus runs on a dedicated OS thread with its own current-thread async
//! runtime (the `rift-store` seam is async-over-sync rusqlite). It keeps the pure
//! render loop synchronous — the loop drains the bus's updates through a merged
//! event source ([`rift_canopy::app::live`]). The bus has **no write path**: it
//! only reads each project's own per-repo projection (MVCC via SQLite WAL,
//! concurrent with capture). Any failure degrades to an `Error` status — never a
//! panic — so the UI stays live even if a projection is missing or locked.
//!
//! **Change observation (the rebuild-epoch rule).** Beyond serving requests, the
//! worker polls a `WatermarkPoller`: each tracked project's
//! `{cursor, epoch}` [`Watermark`] — the append cursor
//! ([`rift_store::CanonicalStore::read_watermark`]) plus the rebuild epoch
//! ([`rift_store::CanonicalStore::read_rebuild_epoch`]) — is re-read on an
//! adaptive cadence (fast right after a change, slow when idle). A moved
//! **epoch** means the projection was rebuilt from git
//! (`ChangeKind::Rebuilt`), which re-bootstraps every tracked project — a
//! rebuild that lands *more* rows than before is detected exactly like any
//! other (the old cursor-regression heuristic is retired; it was blind to
//! that case). A moved cursor at the same epoch is new capture activity
//! (`ChangeKind::Ingested`); a watermark that vanishes is
//! `ChangeKind::Unavailable`. The observer is **non-authoritative** — it
//! only decides *when* to re-read (git stays canonical), so a missed poll
//! only delays and a duplicate only repeats a harmless read. The reference
//! engine is [`rift_store::NotifyCap::PollOnly`]; a native-notify engine
//! would drive the same path without polling.

mod outbox;
mod project_read;
mod settings_view;
mod wake;
mod watermark;
mod worker;

#[cfg(test)]
mod fixtures;

use std::path::PathBuf;
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender};
use std::thread;

use rift_canopy::{DataRequest, Event};
use rift_store::resolve_cache_root;

use self::wake::{NotifyWatcher, PollOnlyWatcher, WakeWatcher};
use self::worker::{viewer_only_worker, worker};

pub use self::settings_view::{project_settings_view, project_settings_view_under};
pub use self::watermark::Watermark;

/// Handles to a running data bus: the request sender (loop → bus) and the update
/// receiver (bus → loop). Dropping either end disconnects the worker, which then
/// exits — so the bus is torn down simply by the dashboard returning.
pub struct Bus {
    /// The reducer's data requests are sent here (loop → bus).
    pub req_tx: Sender<DataRequest>,
    /// The loop drains bus updates from here (bus → loop).
    pub evt_rx: Receiver<Event>,
    /// Owns the worker lifetime. Its drop synchronously tears down the native
    /// watcher, so a finished dashboard cannot leave platform watcher state
    /// behind while a new bus starts.
    _worker: WorkerGuard,
}

/// Synchronous lifetime guard for the worker that owns the native watcher.
///
/// The public request forwarder remains detached: after the request senders
/// close it exits on its own, and it owns no platform resources. The worker,
/// however, must be joined so dropping a [`Bus`] means its watcher is gone.
struct WorkerGuard {
    shutdown_tx: Sender<WorkerMsg>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(WorkerMsg::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The bound on the bus→loop update channel: a write
/// storm can stage at most this many in-flight updates; everything beyond is
/// coalesced latest-per-(project, feed) in the worker's [`Outbox`].
const UPDATE_CHANNEL_BOUND: usize = 64;

/// One internal worker message: a forwarded public request, or a wake.
pub(crate) enum WorkerMsg {
    /// A data request from the reducer.
    Request(DataRequest),
    /// A filesystem event fired under the cache root — schedule an immediate
    /// watermark poll. Deliberately carries **no data** (the rebuild-epoch rule): a fresh
    /// poll remains the sole truth, so missed/duplicated/spurious wakes are
    /// harmless by construction.
    Wake,
    /// The public request side closed; tear down the worker and its native
    /// watcher promptly instead of leaking them until process exit.
    Shutdown,
}

/// Spawn the data-bus worker, seeded to refresh every currently-registered
/// project so their statuses populate at startup. The seed updates queue until
/// the first main-loop poll drains them. The cache root is resolved from the
/// process environment ([`resolve_cache_root`]).
#[must_use]
pub fn spawn(initial_paths: Vec<String>) -> Bus {
    spawn_with_root(resolve_cache_root(|k| std::env::var(k).ok()), initial_paths)
}

/// Spawn the bus against an explicit cache `root` — the seam tests drive with a
/// tempdir, so they never touch the process environment.
#[must_use]
pub fn spawn_with_root(root: PathBuf, initial_paths: Vec<String>) -> Bus {
    spawn_with_watcher(root, initial_paths, Box::new(NotifyWatcher::default()))
}

/// Spawn against an explicit root with native notifications disabled.
///
/// This is the deterministic integration-test seam for feed and polling
/// behaviour. Filesystem-wake behaviour is tested separately through an
/// injected watcher, so tests that do not own native OS notification timing
/// can exercise the documented poll-only fallback directly.
#[doc(hidden)]
#[must_use]
pub fn spawn_poll_only_with_root(root: PathBuf, initial_paths: Vec<String>) -> Bus {
    spawn_with_watcher(root, initial_paths, Box::new(PollOnlyWatcher))
}

/// Spawn the standalone Recall Viewer bus.
///
/// This mode has no watcher, dashboard bootstrap, projection migration,
/// registry/profile seed, ingest, or write-capable feed path. It accepts only
/// [`DataRequest::Viewer`] and delegates those requests to the read-only
/// builder, whose SQLite opens use `SQLITE_OPEN_READ_ONLY`.
#[must_use]
pub fn spawn_read_only_viewer(root: PathBuf) -> Bus {
    let (req_tx, req_rx) = channel::<DataRequest>();
    let (msg_tx, msg_rx) = channel::<WorkerMsg>();
    let (evt_tx, evt_rx) = sync_channel::<Event>(UPDATE_CHANNEL_BOUND);
    forward_requests(req_rx, msg_tx.clone());
    let handle = thread::spawn(move || viewer_only_worker(&msg_rx, &evt_tx, &root));
    Bus {
        req_tx,
        evt_rx,
        _worker: WorkerGuard {
            shutdown_tx: msg_tx,
            handle: Some(handle),
        },
    }
}

/// Spawn the bus with an injected [`WakeWatcher`] (the unit-test seam: no real
/// filesystem timing required to exercise wakes or watch failure).
pub(crate) fn spawn_with_watcher(
    root: PathBuf,
    initial_paths: Vec<String>,
    watcher: Box<dyn WakeWatcher>,
) -> Bus {
    let (req_tx, req_rx) = channel::<DataRequest>();
    let (msg_tx, msg_rx) = channel::<WorkerMsg>();
    let (evt_tx, evt_rx) = sync_channel::<Event>(UPDATE_CHANNEL_BOUND);
    for p in initial_paths {
        let _ = req_tx.send(DataRequest::Refresh(p));
    }
    forward_requests(req_rx, msg_tx.clone());
    let shutdown_tx = msg_tx.clone();
    let handle = thread::spawn(move || worker(&msg_rx, msg_tx, &evt_tx, &root, watcher));
    Bus {
        req_tx,
        evt_rx,
        _worker: WorkerGuard {
            shutdown_tx,
            handle: Some(handle),
        },
    }
}

/// Wall-clock epoch-ms — the bus's only clock (status ages against it). The
/// pure truth table stays clock-injected; only this worker samples the wall.
pub(super) fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Forward public requests into the worker's merged message stream, on a
/// thread of its own so the worker has one receiver to wait on. Exits when
/// the request sender is dropped (the dashboard quit), telling the worker so.
fn forward_requests(req_rx: Receiver<DataRequest>, msg_tx: Sender<WorkerMsg>) {
    thread::spawn(move || {
        while let Ok(req) = req_rx.recv() {
            if msg_tx.send(WorkerMsg::Request(req)).is_err() {
                return;
            }
        }
        let _ = msg_tx.send(WorkerMsg::Shutdown);
    });
}

#[cfg(test)]
mod tests {
    use rift_canopy::{ProjectStatus, ViewerErrorArea, ViewerEvent, ViewerRequest};

    use super::fixtures::{make_project, path_of, recv_project_data, spawn_for_test};
    use super::*;

    #[test]
    fn viewer_only_bus_does_not_create_any_cache_file_on_failure() {
        let cache = tempfile::tempdir().unwrap();
        let not_a_repo = tempfile::tempdir().unwrap();
        assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
        let bus = spawn_read_only_viewer(cache.path().to_path_buf());
        let project = not_a_repo.path().to_string_lossy().into_owned();
        // The viewer-only worker answers nothing but viewer requests, so its
        // barrier is a second catalogue request: a path that sorts after the
        // first, answered in the same cycle or a later one.
        let barrier = format!("{project}/~barrier");
        for project in [project.clone(), barrier.clone()] {
            bus.req_tx
                .send(DataRequest::Viewer(catalogue_of(project)))
                .unwrap();
        }
        let mut seen = Vec::new();
        loop {
            match bus.evt_rx.recv() {
                Ok(Event::Viewer(event)) if viewer_project(&event) == Some(&barrier) => break,
                Ok(event) => seen.push(event),
                Err(std::sync::mpsc::RecvError) => {
                    panic!("the viewer worker stopped before answering: {seen:?}")
                }
            }
        }
        assert!(matches!(
            seen.first(),
            Some(Event::Viewer(ViewerEvent::Error {
                area: ViewerErrorArea::Catalogue,
                ..
            }))
        ));
        drop(bus);
        assert_eq!(
            std::fs::read_dir(cache.path()).unwrap().count(),
            0,
            "viewer-only startup must leave an absent cache database absent"
        );
    }

    #[test]
    fn spawn_refreshes_initial_projects_and_delivers_updates() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        // Inject the cache root (no process-env mutation → no test races).
        let bus = spawn_for_test(root.path().to_path_buf(), vec![path_of(&proj)]);

        // The seeded refresh yields one ProjectData update for the project.
        let ev = recv_project_data(&bus, root.path());
        match ev {
            Event::ProjectData(pd) => {
                assert_eq!(pd.project, path_of(&proj));
                assert_eq!(pd.status, ProjectStatus::Idle);
            }
            other => panic!("expected ProjectData, got {other:?}"),
        }
    }

    fn catalogue_of(project: String) -> ViewerRequest {
        ViewerRequest::Catalogue {
            project,
            generation: 1,
            target: None,
            refresh: false,
        }
    }

    fn viewer_project(event: &ViewerEvent) -> Option<&String> {
        match event {
            ViewerEvent::Error { project, .. } | ViewerEvent::Catalogue { project, .. } => {
                Some(project)
            }
            _ => None,
        }
    }
}
