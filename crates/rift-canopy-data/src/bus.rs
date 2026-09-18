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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{
    channel, sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError,
    TrySendError,
};
use std::thread;
use std::time::Duration;

use notify::Watcher as _;

use rift_canopy::{
    derive_status, CaptureSummary, DataRequest, DetailView, EntryKind, Event, HealthEvent,
    ModelProvenance, ModelUsage, NativeAgentDetection, NativeAgentHookWiring, NativeAgentIdeHost,
    NativeAgentInstallSource, NativeAgentInstallation, NativeAgentIntegrationRow,
    NativeAgentWiring, ProjectData, ProjectInitialization, ProjectPolicyState,
    ProjectPolicySummary, ProjectSettingId, ProjectSettingRow, ProjectSettingsView, ProjectStatus,
    ProjectionState, SessionsRead, StatusInputs, WatchMode,
};
use rift_store::{projection_db_for_family_under, resolve_cache_root, CanonicalStore, SqliteStore};

use crate::feeds::project_family;
use crate::store::runtime;

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
    {
        let msg_tx = msg_tx.clone();
        thread::spawn(move || {
            while let Ok(req) = req_rx.recv() {
                if msg_tx.send(WorkerMsg::Request(req)).is_err() {
                    return;
                }
            }
            let _ = msg_tx.send(WorkerMsg::Shutdown);
        });
    }
    let shutdown_tx = msg_tx;
    let handle = thread::spawn(move || viewer_only_worker(&msg_rx, &evt_tx, &root));
    Bus {
        req_tx,
        evt_rx,
        _worker: WorkerGuard {
            shutdown_tx,
            handle: Some(handle),
        },
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

/// The wake-signal seam: implementations watch the cache root and deliver
/// [`WorkerMsg::Wake`]s for as long as they live. Failing to watch is not an
/// error path — the bus degrades to `PollOnly` (the adaptive cadence keeps
/// updating the dashboard; wakes only add promptness).
pub(crate) trait WakeWatcher: Send {
    /// Begin watching `root` recursively, sending wakes into `wake`.
    ///
    /// # Errors
    /// A human-readable reason when the platform watcher cannot start; the
    /// caller degrades to poll-only.
    fn watch(&mut self, root: &Path, wake: Sender<WorkerMsg>) -> Result<(), String>;

    /// Point the second, **narrow** watch at the ACTIVE project's family's
    /// session-cache dirs (`<git_dir>/rift-sessions` per member, the rebuild-epoch rule
    /// amendment as the family view widens it) — the one class of change the
    /// cache-root watch cannot see. An empty slice unwatches everything.
    ///
    /// A **set**, because session caches stay per gitdir (family identity) and the
    /// active project's family may be running agents in any of its members;
    /// watching only the vantage's own dir made a member's session prompt
    /// nowhere. Bounded by member count and re-enumerated only when the active
    /// project changes or its projection is rebuilt — never one stream per
    /// tracked project, which is the macOS FSEvents caution this trait was
    /// written around.
    ///
    /// Best-effort by the same contract as [`Self::watch`]: a failure (commonly
    /// a dir not existing before the first hook event) leaves session
    /// promptness to the poll cadence, and the worker re-calls every cycle so
    /// attach retries for free. Default: poll-only watchers ignore it.
    fn set_active_session_dirs(&mut self, _dirs: &[PathBuf]) {}
}

/// Watcher implementation for the reference poll-only fallback.
struct PollOnlyWatcher;

impl WakeWatcher for PollOnlyWatcher {
    fn watch(&mut self, _root: &Path, _wake: Sender<WorkerMsg>) -> Result<(), String> {
        Err("poll-only".to_string())
    }
}

/// The production watcher: `notify`'s recommended platform backend.
#[derive(Default)]
struct NotifyWatcher {
    /// Kept alive for the worker's lifetime; dropping it unwatches.
    inner: Option<notify::RecommendedWatcher>,
    /// The session dirs currently attached — exactly those the platform
    /// watcher accepted, so a dir that did not exist yet is retried next cycle.
    session_dirs: Vec<PathBuf>,
}

impl WakeWatcher for NotifyWatcher {
    fn watch(&mut self, root: &Path, wake: Sender<WorkerMsg>) -> Result<(), String> {
        let mut watcher = notify::recommended_watcher(move |_event| {
            // Payload deliberately ignored — wake-only (the rebuild-epoch rule).
            let _ = wake.send(WorkerMsg::Wake);
        })
        .map_err(|e| e.to_string())?;
        watcher
            .watch(root, notify::RecursiveMode::Recursive)
            .map_err(|e| e.to_string())?;
        self.inner = Some(watcher);
        Ok(())
    }

    fn set_active_session_dirs(&mut self, dirs: &[PathBuf]) {
        if self.session_dirs == dirs {
            return; // already attached to exactly this set (or both empty)
        }
        let Some(w) = self.inner.as_mut() else {
            return; // no platform watcher at all → PollOnly for sessions too
        };
        // Diffed, not torn down and rebuilt: a member that is still in the set
        // keeps one uninterrupted stream when a sibling is added or removed.
        for old in &self.session_dirs {
            if !dirs.contains(old) {
                let _ = w.unwatch(old);
            }
        }
        let mut attached = Vec::with_capacity(dirs.len());
        for d in dirs {
            if self.session_dirs.contains(d) {
                attached.push(d.clone()); // already watched; leave it alone
                continue;
            }
            // Non-recursive: rift-sessions is a flat dir of small JSON caches.
            // Failure (dir not created yet) leaves it out of `session_dirs`, so
            // the worker's next call retries the attach.
            if w.watch(d, notify::RecursiveMode::NonRecursive).is_ok() {
                attached.push(d.clone());
            }
        }
        self.session_dirs = attached;
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
    // Forward public requests into the worker's merged message stream. Exits
    // when the request sender is dropped (the dashboard quit).
    {
        let msg_tx = msg_tx.clone();
        thread::spawn(move || {
            while let Ok(req) = req_rx.recv() {
                if msg_tx.send(WorkerMsg::Request(req)).is_err() {
                    return;
                }
            }
            let _ = msg_tx.send(WorkerMsg::Shutdown);
        });
    }
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

/// The worker loop. It waits for a message **or** the observer's poll interval,
/// whichever comes first:
/// * a request → coalesce the burst (latest-per-project), read each distinct
///   project's status, and stage a [`ProjectData`];
/// * a wake (filesystem event) → run the same watermark poll a timeout would —
///   the poll is the only truth, the wake only made it prompt — **and**
///   re-read the ACTIVE project even when no watermark moved (the rebuild-epoch rule
///   amendment): recall.db and the session caches change without the
///   projection cursor, and the event-anchored recompute is idempotent, so
///   an unfounded wake converges on identical output the reducer's dirty
///   gate then drops;
/// * a timeout → poll every tracked project's watermark and re-read the ones
///   that changed (a rebuild additionally re-bootstraps *all* tracked
///   projects). Idle timeouts never force a recompute — the idle cost stays
///   one cheap watermark read.
///
/// Staged updates flush through the bounded channel latest-per-(project, feed)
/// ([`Outbox`]). Exits when either channel disconnects (the dashboard quit).
#[allow(clippy::too_many_lines)] // one read cycle, top to bottom; the order is the contract
fn worker(
    msg_rx: &Receiver<WorkerMsg>,
    wake_tx: Sender<WorkerMsg>,
    evt_tx: &SyncSender<Event>,
    root: &Path,
    mut watcher: Box<dyn WakeWatcher>,
) {
    // Native-notify capability is best-effort: watching the cache root makes
    // updates prompt; failure means PollOnly and the cadence is the heartbeat.
    let watching = watcher.watch(root, wake_tx).is_ok();
    // Report the wake mode ONCE, first: the
    // dashboard adapts its heartbeat to it and the System panel says it, so
    // the poll-only degrade is never silent. Best-effort, never blocking — a
    // full channel at start is impossible (nothing has been sent yet) and
    // must not stall the worker anyway.
    let _ = evt_tx.try_send(Event::WatchMode(if watching {
        WatchMode::Native
    } else {
        WatchMode::PollOnly
    }));
    let mut observer = WatermarkPoller::new(root.to_path_buf());
    let mut outbox = Outbox::default();
    // The foreground project — the only one for which the expensive rich detail is
    // computed (status is cheap and read for every project).
    let mut active: Option<String> = None;
    // The active project's family's session-cache dirs — the narrow second
    // watch target (widened to the whole family by the family view).
    // Re-enumerated when `active` changes or its projection is rebuilt (a
    // rebuild is also how a `git worktree add` since the last read becomes
    // visible); re-asserted every cycle (a cheap no-op once attached) so a dir
    // created after SetActive still gets its watch.
    let mut session_dirs: Vec<PathBuf> = Vec::new();
    // The period the Recall feed is computed over. Worker state, not a
    // per-request parameter: every wake recomputes that feed, so a window
    // living only as long as one request would snap back to the default the
    // next time the filesystem twitched.
    let mut recall_window = rift_canopy::RecallWindow::default();
    // The last health report sent: the dot rides the reads the worker
    // already performs — no new polling — and only a *change* stages an event,
    // so a steady data plane adds zero traffic.
    let mut last_health: Option<HealthEvent> = None;
    let mut viewer_worker = crate::viewer::ViewerWorker::new(root.to_path_buf());
    loop {
        let mut to_read = BTreeSet::new();
        let mut viewer_requests = Vec::new();
        let mut woke_by_fs = false;
        let mut timed_out = false;
        let active_before = active.clone();
        let filter_pending = viewer_worker.has_filter_work();
        let wait = if filter_pending {
            Duration::ZERO
        } else {
            observer.interval()
        };
        match msg_rx.recv_timeout(wait) {
            Ok(WorkerMsg::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(first) => {
                // Coalesce a burst so a storm of requests/wakes collapses to one
                // read per project (backpressure by set-union, not queue growth).
                collect(
                    first,
                    &mut to_read,
                    &mut viewer_requests,
                    &mut active,
                    &mut recall_window,
                    &mut woke_by_fs,
                );
                while let Ok(msg) = msg_rx.try_recv() {
                    if matches!(&msg, WorkerMsg::Shutdown) {
                        return;
                    }
                    collect(
                        msg,
                        &mut to_read,
                        &mut viewer_requests,
                        &mut active,
                        &mut recall_window,
                        &mut woke_by_fs,
                    );
                }
            }
            Err(RecvTimeoutError::Timeout) => timed_out = !filter_pending,
        }
        let mut rebuilt = false;
        if woke_by_fs || timed_out {
            // A wake or a timeout schedules the SAME fresh poll — the watermark
            // read is the sole truth, so a spurious wake reads as quiet.
            for change in observer.poll_once() {
                // Every observed change re-reads that project; a rebuild
                // replaced the projection wholesale, so re-bootstrap every
                // tracked project rather than trusting any cached sibling.
                if change.kind == ChangeKind::Rebuilt {
                    rebuilt = true;
                    to_read.extend(observer.tracked());
                }
                to_read.insert(change.project);
            }
        }
        if active != active_before || rebuilt {
            session_dirs = active.as_deref().map(session_dirs_for).unwrap_or_default();
        }
        watcher.set_active_session_dirs(&session_dirs);
        if woke_by_fs {
            // Filesystem activity → recompute the active project's feeds even
            // without a cursor move: this is how recall.db serves and
            // session-cache writes reach the panel (the poll stays the only
            // truth about the *projection*; the recompute is a fresh read).
            if let Some(a) = active.clone() {
                to_read.insert(a);
            }
        }
        let did_read = !to_read.is_empty();
        let mut probe = HealthProbe::default();
        for path in to_read {
            stage_read(
                &mut observer,
                &mut outbox,
                root,
                path,
                active.as_deref(),
                recall_window,
                &mut probe,
            );
        }
        for request in viewer_requests {
            for event in viewer_worker.handle(request) {
                outbox.stage(event);
            }
        }
        for event in viewer_worker.advance_filter_scans() {
            outbox.stage(event);
        }
        if did_read {
            let health = probe.report();
            if last_health != Some(health) {
                last_health = Some(health);
                outbox.stage(Event::Health(health));
            }
        }
        if outbox.flush(evt_tx).is_err() {
            return; // the loop is gone; end the worker
        }
    }
}

/// What one read cycle observed about the worker's own infrastructure —
/// folded into the header dot's [`HealthEvent`] (the dashboard information architecture).
#[derive(Default)]
struct HealthProbe {
    /// The machine-global canopy store failed to open this cycle.
    store_error: bool,
    /// The worker's async runtime could not be created.
    runtime_error: bool,
}

impl HealthProbe {
    /// Fold the cycle's observations: a dead store outranks a worker error.
    fn report(&self) -> HealthEvent {
        if self.store_error {
            HealthEvent::StoreUnreachable
        } else if self.runtime_error {
            HealthEvent::WorkerError
        } else {
            HealthEvent::Healthy
        }
    }
}

/// The staging buffer in front of the bounded update channel: **latest wins
/// per (project, feed)**, so a write storm can neither stall the worker (sends
/// never block) nor flood the loop (the channel never exceeds its bound, and
/// superseded updates are simply replaced while they wait).
#[derive(Default)]
struct Outbox {
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

impl Outbox {
    /// Stage an update, replacing any staged one for the same (project, feed).
    fn stage(&mut self, ev: Event) {
        self.pending.insert(feed_key(&ev), ev);
    }

    /// Push staged updates through the bounded channel without blocking: a
    /// full channel keeps the remainder staged (newer stages keep replacing
    /// them — latest-per-feed under backpressure). `Err` iff the loop is gone.
    fn flush(&mut self, tx: &SyncSender<Event>) -> Result<(), ()> {
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

/// Read a project's status + watermark once, record the watermark as the
/// observer's polling baseline (so the next poll only fires on genuinely newer
/// changes), and stage the update in the outbox. The rich detail is attached
/// only when `path` is the `active` (foreground) project — building it reads
/// several tables, so it stays scoped to the one project the user is looking
/// at.
fn stage_read(
    observer: &mut WatermarkPoller,
    outbox: &mut Outbox,
    root: &Path,
    path: String,
    active: Option<&str>,
    recall_window: rift_canopy::RecallWindow,
    probe: &mut HealthProbe,
) {
    probe.runtime_error |= runtime().is_err();
    let read = read_project(root, &path);
    let status = read.status;
    observer.sync(&path, read.mark);
    let detail = if active == Some(path.as_str()) {
        Some(build_detail(
            root,
            &path,
            status,
            read.running_members,
            probe,
        ))
    } else {
        None
    };
    let is_active = active == Some(path.as_str());
    outbox.stage(Event::ProjectData(ProjectData {
        project: path.clone(),
        status,
        detail,
    }));
    // The column-3 feeds follow the active project (SetActive is the
    // subscribe request, the data-plane split): recompute full section state on every
    // read of it — a Rebuilt-driven read is thereby the re-bootstrap, and the
    // reducer's periodic re-stat keeps recall.db-only changes fresh too.
    if is_active {
        // ONE active-sessions read per recomputation: the badge, context, and
        // recent-activity scope all derive from the same native identities.
        let now = now_ms();
        let sessions = crate::feeds::read_active_sessions(&path, now);
        outbox.stage(Event::Feed(crate::feeds::compute_tokens(root, &path)));
        outbox.stage(Event::Feed(crate::feeds::compute_system(
            root, &path, &sessions,
        )));
        outbox.stage(Event::Feed(crate::feeds::compute_recall(
            root,
            &path,
            now,
            recall_window,
        )));
        // The activity card stays on the injected clock (calendar-day
        // bucketing); System activity is event-anchored and never uses `now`.
        outbox.stage(Event::Feed(crate::feeds::compute_activity(
            root, &path, now,
        )));
        // The two workflow cards come from ONE gather of the unbounded
        // window. They read no clock at all: a card whose numbers
        // depend on when it rendered cannot be compared with the
        // `rift dispatch` the user runs beside it.
        let (dispatch, recap) = crate::workflows::compute_workflow_cards(root, &path);
        outbox.stage(Event::Feed(dispatch));
        outbox.stage(Event::Feed(recap));
    }
}

/// Fold a worker message into the set of projects to (re-)read, updating the
/// `active` (foreground) project and the fs-wake flag. `SetActive(None)`
/// clears the active project; a [`WorkerMsg::Wake`] requests a poll **and**
/// (in the worker) the active project's recompute — never any data of its
/// own.
fn collect(
    msg: WorkerMsg,
    out: &mut BTreeSet<String>,
    viewer: &mut Vec<rift_canopy::ViewerRequest>,
    active: &mut Option<String>,
    recall_window: &mut rift_canopy::RecallWindow,
    woke_by_fs: &mut bool,
) {
    match msg {
        WorkerMsg::Request(DataRequest::Refresh(p)) => {
            out.insert(p);
        }
        WorkerMsg::Request(DataRequest::SetActive(Some(p))) => {
            *active = Some(p.clone());
            out.insert(p);
        }
        WorkerMsg::Request(DataRequest::SetActive(None)) => *active = None,
        // Adopt the window and re-read the project it applies to. Coalescing
        // is by set-union like every other request here, so pressing four
        // buttons in one frame costs one read at the last window pressed.
        WorkerMsg::Request(DataRequest::SetRecallWindow(w)) => {
            *recall_window = w;
            if let Some(a) = active.clone() {
                out.insert(a);
            }
        }
        WorkerMsg::Request(DataRequest::Viewer(request)) => {
            coalesce_viewer_request(viewer, request);
        }
        WorkerMsg::Wake => *woke_by_fs = true,
        WorkerMsg::Shutdown => {}
    }
}

fn coalesce_viewer_request(
    requests: &mut Vec<rift_canopy::ViewerRequest>,
    request: rift_canopy::ViewerRequest,
) {
    let same_slot = |existing: &rift_canopy::ViewerRequest| match (existing, &request) {
        (
            rift_canopy::ViewerRequest::Catalogue { project: a, .. },
            rift_canopy::ViewerRequest::Catalogue { project: b, .. },
        )
        | (
            rift_canopy::ViewerRequest::VisibleExcerpts { project: a, .. },
            rift_canopy::ViewerRequest::VisibleExcerpts { project: b, .. },
        )
        | (
            rift_canopy::ViewerRequest::Detail { project: a, .. },
            rift_canopy::ViewerRequest::Detail { project: b, .. },
        )
        | (
            rift_canopy::ViewerRequest::FilterScan { project: a, .. },
            rift_canopy::ViewerRequest::FilterScan { project: b, .. },
        ) => a == b,
        _ => false,
    };
    if let Some(slot) = requests.iter_mut().find(|existing| same_slot(existing)) {
        *slot = request;
    } else {
        requests.push(request);
    }
}

fn viewer_only_worker(msg_rx: &Receiver<WorkerMsg>, evt_tx: &SyncSender<Event>, root: &Path) {
    let mut viewer = crate::viewer::ViewerWorker::new(root.to_path_buf());
    let mut outbox = Outbox::default();
    loop {
        let first = if viewer.has_filter_work() {
            match msg_rx.try_recv() {
                Ok(message) => Some(message),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        } else {
            let Ok(message) = msg_rx.recv() else {
                return;
            };
            Some(message)
        };
        let mut requests = Vec::new();
        if let Some(first) = first {
            if matches!(first, WorkerMsg::Shutdown) {
                return;
            }
            if let WorkerMsg::Request(DataRequest::Viewer(request)) = first {
                coalesce_viewer_request(&mut requests, request);
            }
        }
        while let Ok(message) = msg_rx.try_recv() {
            match message {
                WorkerMsg::Shutdown => return,
                WorkerMsg::Request(DataRequest::Viewer(request)) => {
                    coalesce_viewer_request(&mut requests, request);
                }
                // The viewer plane draws no Recall panel, so a window request
                // is one more dashboard request it ignores. Listed by name
                // rather than swept up by a wildcard: this match is what makes
                // "which requests does the read-only worker answer?" a
                // compile-time question.
                WorkerMsg::Request(
                    DataRequest::Refresh(_)
                    | DataRequest::SetActive(_)
                    | DataRequest::SetRecallWindow(_),
                )
                | WorkerMsg::Wake => {}
            }
        }
        for request in requests {
            for event in viewer.handle(request) {
                outbox.stage(event);
            }
        }
        for event in viewer.advance_filter_scans() {
            outbox.stage(event);
        }
        if outbox.flush(evt_tx).is_err() {
            return;
        }
    }
}

/// Every session-cache dir of the active project's **family**
/// (`<git_dir>/rift-sessions` per member) — the narrow second watch set
/// (the family view). Empty for a non-git project (nothing to watch; the status
/// path reads the same absence).
///
/// A `prunable` member is skipped: its checkout is gone, so its administrative
/// registry cannot gain a session, and a watch on it would only be a stream
/// that never fires. The vantage is always present ([`crate::session::roster`]),
/// so this is never narrower than the single dir it replaced.
fn session_dirs_for(path: &str) -> Vec<PathBuf> {
    let Ok(paths) = rift_git::discover(Path::new(path)) else {
        return Vec::new();
    };
    crate::session::roster(&paths)
        .members
        .iter()
        .filter(|m| !m.prunable)
        .map(|m| m.git_dir.join("rift-sessions"))
        .collect()
}

/// What one read of a project yields: its live status, its `{cursor, epoch}`
/// watermark, and — for a family row — which members its `Running` count came
/// from.
struct ProjectRead {
    status: ProjectStatus,
    mark: Option<Watermark>,
    running_members: Vec<String>,
}

impl ProjectRead {
    /// A read that names no running member: every early return, and every
    /// member row (whose count is its own and needs no breakdown).
    const fn plain(status: ProjectStatus, mark: Option<Watermark>) -> Self {
        Self {
            status,
            mark,
            running_members: Vec::new(),
        }
    }
}

/// Read a project's live status **and** its `{cursor, epoch}` watermark from
/// the **repository family's** projection in a single open — read-only, scoped
/// by the family's `repo_id`. The gathered facts (repo presence, projection
/// readiness, the live-session read, capture presence, whether this row is an
/// adopted member) are fed to the pure [`derive_status`] truth table, so the
/// classification is testable without a store; any failure degrades to
/// an `Error`/`NotInitialized` status, never a panic.
///
/// **The row decides which session read it gets** (the family view).
/// A **linked worktree** is a member row: its sessions are the ones credited to
/// it by the worktree discriminator ([`crate::session::family_sessions`], never
/// its own directory alone), and it additionally answers whether it has been
/// provisioned — an initialized family whose member carries no
/// `.rift/state/native-integrations.json` has nothing in that checkout wired to
/// Rift, which is `NotAdopted`. Anything else is the **family row**: its
/// `Running` counts every member's sessions, and it names them, so the number
/// can be read back to the checkouts it came from.
///
/// A member whose directory is gone but whose administrative entry survives —
/// git's `prunable` — canonicalises to nothing and takes the first branch:
/// `Error(RepoMissing)`, whose hint on a worktree row says prunable.
///
/// **A projection is not an install.** Before any capture-shaped reading, a
/// family whose binary of record names no live binary answers
/// `SetupIncomplete` — the same answer the settings page gives, so the card
/// and its modal cannot disagree about whether there is setup left to do.
///
/// The watermark is `Some` iff the projection is open and readable, and `None`
/// iff it is absent/unreadable — the same `None` the change observer treats as
/// [`ChangeKind::Unavailable`].
fn read_project(root: &Path, path: &str) -> ProjectRead {
    let dir = PathBuf::from(path);
    let now_ms = now_ms();

    // A registered path that no longer resolves → the repo is gone. This is a
    // question about the PATH, not about identity, so it stays a canonicalize:
    // a directory that is present but is not a repository is present.
    if std::fs::canonicalize(&dir).is_err() {
        return ProjectRead::plain(
            status_of(
                false,
                ProjectionState::Uninitialized,
                SessionsRead::Ok(None),
                false,
                None,
            ),
            None,
        );
    }
    if dir.join(".rift/state/project-setup.json").is_file() {
        return ProjectRead::plain(ProjectStatus::SetupIncomplete, None);
    }
    let family_read = read_family(&dir, now_ms);
    let sessions = family_read.sessions;
    let provisioned = family_read.provisioned;
    // The projection is the repository family's (family identity): a card opened on
    // a linked worktree reads the family's store, and a directory that is not a
    // repository has no family — which reads exactly like no projection,
    // because family identity mints no per-path store to give it one.
    let family = project_family(&dir);
    let db = family
        .as_ref()
        .map(|family| projection_db_for_family_under(root, family));
    let Some(db) = db.filter(|db| db.exists()) else {
        let projection = projection_absent(family_read.repository, &dir);
        return ProjectRead::plain(
            status_of(true, projection, sessions, false, provisioned),
            None,
        );
    };
    // The projection is not the install. An agent hook opens the store on
    // first contact, so a family nothing was ever wired in carries a
    // projection; the card reads it the way the settings page does, and the
    // add flow's act is the one this status asks for.
    if family.is_some_and(|family| !hooks_installed_here(&family)) {
        return ProjectRead::plain(ProjectStatus::SetupIncomplete, None);
    }
    // The projection exists: open once, read the watermark + freshest event.
    let Some((mark, last_ts)) = open_and_read(&db) else {
        return ProjectRead::plain(
            status_of(
                true,
                ProjectionState::Unavailable,
                sessions,
                false,
                provisioned,
            ),
            None,
        );
    };
    ProjectRead {
        status: status_of(
            true,
            ProjectionState::Ready,
            sessions,
            last_ts.is_some(),
            provisioned,
        ),
        mark: Some(mark),
        running_members: family_read.running_members,
    }
}

/// How a path with no projection under this machine's root reads.
///
/// `.rift/` is committed configuration and proves nothing about this machine,
/// so a checkout carrying it is configured elsewhere, never initialized; a
/// directory outside any repository has no family and can never hold one.
fn projection_absent(repository: bool, dir: &Path) -> ProjectionState {
    if !repository {
        ProjectionState::NotRepository
    } else if dir.join(".rift").is_dir() {
        ProjectionState::Configured
    } else {
        ProjectionState::Uninitialized
    }
}

/// The family-shaped half of a project read: whose sessions this row counts,
/// whether it is an adopted member, and which members are running.
struct FamilyRead {
    /// Whether the path is inside a git repository at all.
    repository: bool,
    sessions: SessionsRead,
    provisioned: Option<bool>,
    running_members: Vec<String>,
}

impl FamilyRead {
    /// A path with no family at all — not a repository, or one whose common dir
    /// cannot be read. No sessions to find, and "is this member provisioned?"
    /// is not a question that applies.
    const fn none() -> Self {
        Self {
            repository: false,
            sessions: SessionsRead::Ok(None),
            provisioned: None,
            running_members: Vec::new(),
        }
    }
}

/// Resolve `dir`'s family and read the session facts its row needs.
fn read_family(dir: &Path, now_ms: i64) -> FamilyRead {
    let Ok(paths) = rift_git::discover(dir) else {
        return FamilyRead::none();
    };
    let roster = crate::session::roster(&paths);
    let read = crate::session::family_sessions(&roster.members, now_ms);
    if paths.is_linked() {
        // A member row: its own credited sessions, and the adoption question.
        let sessions = read
            .per_member
            .get(roster.vantage)
            .map_or(SessionsRead::Ok(None), |detail| detail.read);
        return FamilyRead {
            repository: true,
            sessions,
            provisioned: Some(is_provisioned(&paths.work_tree)),
            running_members: Vec::new(),
        };
    }
    let running_members = roster
        .members
        .iter()
        .zip(&read.per_member)
        .filter(|(_, detail)| matches!(detail.read, SessionsRead::Ok(Some(_))))
        .map(|(member, _)| member_label(member))
        .collect();
    FamilyRead {
        repository: true,
        sessions: read.family.read,
        provisioned: None,
        running_members,
    }
}

/// Whether a member checkout has ever been provisioned: the native-integration
/// ownership manifest `rift_agent_install::native::enable` writes, which is the
/// same file `post-checkout`'s fast path stats (family wiring and hook custody).
///
/// A **file-presence** check on purpose: it is stable whatever the adoption
/// API grows into, it costs one `stat` on a path the bus already holds, and a
/// manifest that exists but cannot be parsed still proves that something once
/// provisioned this checkout — which is the question the status asks.
fn is_provisioned(work_tree: &Path) -> bool {
    work_tree
        .join(".rift/state/native-integrations.json")
        .is_file()
}

/// A member's name for the family header: `main` for the main work tree, the
/// administrative name for a linked one (worktree provenance's vocabulary, so a reader
/// never has to know this repository's folder name to recognise main).
fn member_label(member: &rift_git::FamilyMember) -> String {
    if member.is_main {
        "main".to_owned()
    } else {
        member.name.clone()
    }
}

/// Assemble the [`StatusInputs`] and run the pure truth table.
fn status_of(
    repo_present: bool,
    projection: ProjectionState,
    sessions: SessionsRead,
    has_captures: bool,
    provisioned: Option<bool>,
) -> ProjectStatus {
    derive_status(&StatusInputs {
        repo_present,
        projection,
        sessions,
        has_captures,
        provisioned,
    })
}

/// Open the projection read-only and return `(watermark, most-recent
/// occurred_at)`, or `None` if it cannot be opened/read
/// (→ [`ProjectionState::Unavailable`]).
fn open_and_read(db: &Path) -> Option<(Watermark, Option<i64>)> {
    let store = SqliteStore::open(db).ok()?;
    let rt = runtime().ok()?;
    let cursor = rt.block_on(store.read_watermark("captured_event")).ok()?;
    let epoch = rt.block_on(store.read_rebuild_epoch()).ok()?;
    // The freshest event's transaction time (0 rows → None, not an error).
    let last_ts = rt
        .block_on(store.query_opt_i64(
            "SELECT occurred_at_ms FROM captured_event ORDER BY occurred_at_ms DESC LIMIT 1",
            &[],
        ))
        .ok()?;
    Some((Watermark { cursor, epoch }, last_ts))
}

/// Build the rich column-2 [`DetailView`] for the active project: its effective
/// settings (from `.rift/config.toml`) plus the models and capture counts from its
/// own projection. Any unreadable part degrades to empty/default — never a panic.
/// The `status` is the already-derived one, so the detail and the list agree.
fn build_detail(
    root: &Path,
    path: &str,
    status: ProjectStatus,
    running_members: Vec<String>,
    _probe: &mut HealthProbe,
) -> DetailView {
    let dir = PathBuf::from(path);
    let label = dir
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned());
    let (models, capture) = read_projection_detail(root, &dir).unwrap_or_default();
    // A `.git` FILE marks a linked worktree; a directory is a repository root.
    let kind = if dir.join(".git").is_file() {
        EntryKind::Worktree
    } else {
        EntryKind::Repo
    };
    DetailView {
        path: path.to_string(),
        label,
        kind,
        status,
        models,
        capture,
        running_members,
        project_settings: Box::new(project_settings_view_under(root, &dir)),
    }
}

/// Build the complete read-only Project Settings projection for a repository.
///
/// This function has no write path and never probes or launches an agent. It is
/// also the binary manager's verification read after a lifecycle operation, so
/// the CLI, modal, and data bus share one interpretation of policy and adapter
/// state.
#[must_use]
pub fn project_settings_view(dir: &Path) -> ProjectSettingsView {
    project_settings_view_under(&resolve_cache_root(|key| std::env::var(key).ok()), dir)
}

/// [`project_settings_view`] against an explicit state root — the bus's,
/// which it resolved once at spawn, and a test's temporary directory.
#[must_use]
pub fn project_settings_view_under(root: &Path, dir: &Path) -> ProjectSettingsView {
    use rift_config::{EffectiveConfig, PolicySource};
    use rift_recall_policy::{load_serve_policy, ServePolicySource};

    let path = dir.to_string_lossy().into_owned();
    let project_name = dir
        .file_name()
        .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned());
    let initialization = project_initialization(root, dir);
    let rift_dir = dir.join(".rift");
    let (effective, config_state, config_detail) = match EffectiveConfig::load(&rift_dir) {
        Ok(effective) => {
            let state = match effective.policy_source() {
                PolicySource::Declared => ProjectPolicyState::Declared,
                PolicySource::Default => ProjectPolicyState::Default,
                PolicySource::Refused(_) => ProjectPolicyState::Refused,
            };
            let detail = effective.policy_source().diagnostic().map(str::to_owned);
            (Some(effective), state, detail)
        }
        Err(_) => (
            EffectiveConfig::secure_default().ok(),
            ProjectPolicyState::Invalid,
            Some(".rift/config.toml is invalid; repair it before changing settings".to_owned()),
        ),
    };
    let serve = load_serve_policy(&rift_dir);
    let serve_state = match serve.source {
        ServePolicySource::Declared => ProjectPolicyState::Declared,
        ServePolicySource::Default => ProjectPolicyState::Default,
        ServePolicySource::Invalid => ProjectPolicyState::Invalid,
        ServePolicySource::Refused => ProjectPolicyState::Refused,
    };
    let state = combine_policy_state(config_state, serve_state);
    let detail = config_detail.or_else(|| match serve.source {
        ServePolicySource::Invalid => {
            Some(".rift/policies/serve.toml is invalid; Recall serving is fail-closed".to_owned())
        }
        ServePolicySource::Refused => Some(
            ".rift/policies/serve.toml is refused; repair its ownership or file type".to_owned(),
        ),
        ServePolicySource::Declared | ServePolicySource::Default => None,
    });
    let packs = rift_config::load_redactor_packs(&rift_config::redactor_pack_dir(&rift_dir));
    let editable = !matches!(
        state,
        ProjectPolicyState::Invalid | ProjectPolicyState::Refused
    );
    let mut settings = project_setting_rows(
        effective.as_ref(),
        initialization,
        editable,
        ServeLevers {
            injection_budget: serve.policy.injection_turn_budget,
            doc_mode: serve.policy.doc.mode,
            search_mode: serve.policy.search.mode,
        },
    );
    let mut integrations = native_settings_integrations(dir, initialization);
    if let Some(plan) = pending_setup_plan(dir) {
        for row in &mut settings {
            let key = match row.id {
                ProjectSettingId::TotalRecall => "total_recall",
                ProjectSettingId::TurnInjection => "turn_injection",
                ProjectSettingId::Documents => "documents",
                ProjectSettingId::Search => "search",
                ProjectSettingId::ToolCallCapture => "tool_call_capture",
                ProjectSettingId::TokenUsageCapture => "token_usage_capture",
                ProjectSettingId::RawTranscripts => "raw_transcripts",
            };
            if let Some(enabled) = plan.get(key).and_then(serde_json::Value::as_bool) {
                row.enabled = enabled;
            }
        }
        let selected = plan
            .get("integrations")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect::<BTreeSet<_>>();
        for row in &mut integrations {
            row.selected = selected.contains(row.agent.as_str());
        }
    }
    ProjectSettingsView {
        path,
        project_name,
        initialization,
        settings,
        policy: ProjectPolicySummary {
            state,
            detail,
            sensitive_path_count: effective
                .as_ref()
                .map_or(0, |config| config.sensitive_paths().len()),
            loaded_redactor_packs: packs.packs.len(),
            failed_redactor_packs: packs.diagnostics.len(),
        },
        integrations,
    }
}

fn pending_setup_plan(dir: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(dir.join(".rift/state/project-setup.json")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    (value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        == Some(1))
    .then_some(value)
}

/// The serving-policy values the Recall rows are read from.
#[derive(Clone, Copy, Debug)]
struct ServeLevers {
    injection_budget: u32,
    doc_mode: rift_recall_policy::Mode,
    search_mode: rift_recall_policy::Mode,
}

/// A lever table as a binary row: `off` is off, and both `enforce` and an
/// explicit `observe` are on — the latter with the detail that says so, so
/// the toggle never overwrites a declaration nobody meant to flip.
fn lever_row(
    id: ProjectSettingId,
    mode: rift_recall_policy::Mode,
    actionable: bool,
) -> ProjectSettingRow {
    ProjectSettingRow {
        id,
        enabled: mode != rift_recall_policy::Mode::Off,
        actionable,
        detail: (mode == rift_recall_policy::Mode::Observe)
            .then(|| "custom effective mode: observe".to_owned()),
    }
}

fn project_setting_rows(
    effective: Option<&rift_config::EffectiveConfig>,
    initialization: ProjectInitialization,
    editable: bool,
    levers: ServeLevers,
) -> Vec<ProjectSettingRow> {
    let injection_budget = levers.injection_budget;
    // Recall is on unless the repository's configuration writes it off — the
    // setup default and the declared value are one answer, so a repository
    // that committed `total_recall = false` is not talked out of it by a
    // wizard. An unreadable configuration falls back to the secure default,
    // which is on.
    let recall = effective.is_none_or(rift_config::EffectiveConfig::recall_enabled);
    // A configured-elsewhere project's toggles are read-only facts: setup on
    // this machine must never rewrite the repository's committed file.
    let actionable = editable && initialization.settings_editable();
    vec![
        ProjectSettingRow {
            id: ProjectSettingId::TotalRecall,
            enabled: recall,
            actionable,
            detail: None,
        },
        ProjectSettingRow {
            id: ProjectSettingId::TurnInjection,
            enabled: injection_budget > 0,
            actionable,
            detail: (injection_budget > 0
                && injection_budget != rift_recall_policy::DEFAULT_INJECTION_TURN_BUDGET)
                .then(|| format!("custom effective budget: {injection_budget}")),
        },
        lever_row(ProjectSettingId::Documents, levers.doc_mode, actionable),
        lever_row(ProjectSettingId::Search, levers.search_mode, actionable),
        ProjectSettingRow {
            id: ProjectSettingId::ToolCallCapture,
            enabled: effective.is_some_and(rift_config::EffectiveConfig::captures_tool_calls),
            actionable,
            detail: None,
        },
        ProjectSettingRow {
            id: ProjectSettingId::TokenUsageCapture,
            enabled: effective.is_some_and(rift_config::EffectiveConfig::captures_token_usage),
            actionable,
            detail: None,
        },
        ProjectSettingRow {
            id: ProjectSettingId::RawTranscripts,
            enabled: effective.is_some_and(rift_config::EffectiveConfig::raw_transcripts_enabled),
            actionable,
            detail: Some("warning confirmation required when enabling".to_owned()),
        },
    ]
}

/// Whether the project is set up **on this machine**: the family's projection
/// exists under `root` **and** its git hooks were installed here.
///
/// Neither half is enough on its own. `.rift/` is committed configuration and
/// travels with a clone, so it proves nothing about this machine; reading it
/// as "initialized" is what kept a fresh clone's setup wizard (and its
/// pre-ticked detected agents) from ever appearing on a second machine. The
/// projection proves little more: an agent hook opens the store on first
/// contact and creates it, so a family nothing ever installed hooks for
/// carries one — and read as initialized, it offered no setup to the one
/// repository that needed it most.
fn project_initialization(root: &Path, dir: &Path) -> ProjectInitialization {
    if dir.join(".rift/state/project-setup.json").is_file() {
        return ProjectInitialization::SetupIncomplete;
    }
    let Some(family) = project_family(dir) else {
        return if rift_git::discover(dir).is_ok() {
            // A repository whose identity cannot be resolved from here reads
            // like one with no projection, because it has none to key.
            ProjectInitialization::NotInitialized
        } else {
            ProjectInitialization::NotRepository
        };
    };
    if !projection_db_for_family_under(root, &family).exists() {
        return if dir.join(".rift").is_dir() {
            ProjectInitialization::NotProvisioned
        } else {
            ProjectInitialization::NotInitialized
        };
    }
    if hooks_installed_here(&family) {
        ProjectInitialization::Initialized
    } else {
        ProjectInitialization::SetupIncomplete
    }
}

/// Whether this machine installed `family`'s git hooks: its binary of record
/// names an executable that is still there.
///
/// The record is the install's own note, written beside the family manifest,
/// so this is one small JSON file read during a status — never on a hook's
/// path, which decides nothing from here.
fn hooks_installed_here(family: &rift_git::Family) -> bool {
    let dir = rift_git::family::family_dir_under(|key| std::env::var(key).ok(), &family.common_dir);
    rift_shadow_hooks::read_binary_of_record(&dir).is_some_and(|record| record.is_live())
}

fn native_settings_integrations(
    dir: &Path,
    initialization: ProjectInitialization,
) -> Vec<NativeAgentIntegrationRow> {
    native_settings_rows(
        rift_agent_install::native::diagnose_all(dir),
        initialization,
    )
}

/// The Project Settings rows for one diagnostic — the pure half of
/// [`native_settings_integrations`], so the projection of the installer's
/// vocabulary (and the pre-selection rule it feeds) is testable without a
/// machine's real `PATH`, home or application roots behind `diagnose_all`.
fn native_settings_rows(
    statuses: Vec<rift_agent_install::native::NativeIntegrationStatus>,
    initialization: ProjectInitialization,
) -> Vec<NativeAgentIntegrationRow> {
    use rift_agent_install::native::{
        NativeDetection, NativeHookWiring, NativeIdeHost, NativeInstallSource, NativeWiring,
    };

    statuses
        .into_iter()
        .map(|row| {
            let detection = match row.detection {
                NativeDetection::Detected => NativeAgentDetection::Detected,
                NativeDetection::NotDetected => NativeAgentDetection::NotDetected,
            };
            let wiring = match row.wiring {
                NativeWiring::Missing => NativeAgentWiring::Missing,
                NativeWiring::Managed => NativeAgentWiring::Managed,
                NativeWiring::Conflict => NativeAgentWiring::Conflict,
                NativeWiring::Invalid => NativeAgentWiring::Invalid,
            };
            let hook_wiring = row.hook_wiring.map(|hooks| match hooks {
                NativeHookWiring::Missing => NativeAgentHookWiring::Missing,
                NativeHookWiring::Managed => NativeAgentHookWiring::Managed,
                NativeHookWiring::Stale => NativeAgentHookWiring::Stale,
                NativeHookWiring::Conflict => NativeAgentHookWiring::Conflict,
                NativeHookWiring::Invalid => NativeAgentHookWiring::Invalid,
            });
            // The one predicate. `detected()` is the same question `rift
            // init` asks before it wires a row, so the pre-tick can never
            // offer what init would not have wired, or the reverse.
            let detected = row.detected();
            let installations = row
                .installations
                .into_iter()
                .map(|installation| NativeAgentInstallation {
                    source: match installation.source {
                        NativeInstallSource::PathCommand => NativeAgentInstallSource::PathCommand,
                        NativeInstallSource::VendorInstall => {
                            NativeAgentInstallSource::VendorInstall
                        }
                        NativeInstallSource::IdeExtension(host) => {
                            NativeAgentInstallSource::IdeExtension(match host {
                                NativeIdeHost::VsCode => NativeAgentIdeHost::VsCode,
                                NativeIdeHost::VsCodeInsiders => NativeAgentIdeHost::VsCodeInsiders,
                                NativeIdeHost::Cursor => NativeAgentIdeHost::Cursor,
                                NativeIdeHost::Windsurf => NativeAgentIdeHost::Windsurf,
                            })
                        }
                        NativeInstallSource::JetBrainsPlugin => {
                            NativeAgentInstallSource::JetBrainsPlugin
                        }
                    },
                    version: installation.version,
                })
                .collect::<Vec<_>>();
            let alpha_supported = rift_agent_install::native::is_alpha_supported(&row.agent);
            NativeAgentIntegrationRow {
                agent: row.agent,
                display_name: row.display_name,
                detection,
                executable_name: row.executable.and_then(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                }),
                installations,
                wiring,
                hook_wiring,
                alpha_supported,
                selected: default_selection(wiring, initialization, detected, alpha_supported),
            }
        })
        .collect()
}

/// Whether a fresh Project Settings view selects an integration row before the
/// user touches it.
///
/// A managed row is always selected (it reflects reality). Wherever the
/// setup wizard is offered — not initialized, configured elsewhere, or a
/// setup in progress — a DETECTED row is pre-selected as a suggestion, but
/// only when this alpha can actually capture it: pre-selecting a coming-soon
/// agent stages a selection `initialize()` is guaranteed to refuse, which
/// turned the default setup plan into an error. Pure so the rule is testable
/// without a machine's real `PATH` behind `diagnose_all`.
const fn default_selection(
    wiring: NativeAgentWiring,
    initialization: ProjectInitialization,
    detected: bool,
    alpha_supported: bool,
) -> bool {
    matches!(wiring, NativeAgentWiring::Managed)
        || initialization.offers_setup() && detected && alpha_supported
}

const fn combine_policy_state(
    config: ProjectPolicyState,
    serve: ProjectPolicyState,
) -> ProjectPolicyState {
    if matches!(config, ProjectPolicyState::Refused) || matches!(serve, ProjectPolicyState::Refused)
    {
        ProjectPolicyState::Refused
    } else if matches!(config, ProjectPolicyState::Invalid)
        || matches!(serve, ProjectPolicyState::Invalid)
    {
        ProjectPolicyState::Invalid
    } else if matches!(config, ProjectPolicyState::Declared)
        || matches!(serve, ProjectPolicyState::Declared)
    {
        ProjectPolicyState::Declared
    } else {
        ProjectPolicyState::Default
    }
}

/// Read a project's models + capture counts from its **family's** projection
/// (read-only, scoped by `repo_id`). `None` if the projection is
/// missing/unreadable — or if the path has no family to key one.
fn read_projection_detail(root: &Path, dir: &Path) -> Option<(Vec<ModelUsage>, CaptureSummary)> {
    let db = projection_db_for_family_under(root, &project_family(dir)?);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open(&db).ok()?;
    let rt = runtime().ok()?;

    // Models: calls and later append-only observations. Runtime facts supersede
    // configured fallback facts; empty legacy session values are ignored.
    let mut counts: BTreeMap<String, (u64, ModelProvenance)> = BTreeMap::new();
    if let Ok(rows) =
        rt.block_on(store.query("SELECT model, COUNT(*) FROM model_call GROUP BY model", &[]))
    {
        for r in rows {
            if let (Ok(model), Ok(n)) = (r.get_text(0), r.get_i64(1)) {
                if !model.trim().is_empty() {
                    counts.insert(
                        model,
                        (u64::try_from(n).unwrap_or(0), ModelProvenance::Runtime),
                    );
                }
            }
        }
    }
    if let Ok(rows) = rt.block_on(store.query(
        "SELECT model, source FROM model_observation WHERE model <> ''",
        &[],
    )) {
        for row in rows {
            if let (Ok(model), Ok(source)) = (row.get_text(0), row.get_text(1)) {
                if model.trim().is_empty() {
                    continue;
                }
                let provenance = if source == "configured_client" {
                    ModelProvenance::Configured
                } else {
                    ModelProvenance::Runtime
                };
                counts
                    .entry(model)
                    .and_modify(|(_, old)| {
                        if provenance == ModelProvenance::Runtime {
                            *old = provenance;
                        }
                    })
                    .or_insert((0, provenance));
            }
        }
    }
    if let Ok(rows) = rt.block_on(store.query("SELECT DISTINCT model_primary FROM session", &[])) {
        for r in rows {
            if let Ok(model) = r.get_text(0) {
                if !model.trim().is_empty() {
                    counts.entry(model).or_insert((0, ModelProvenance::Runtime));
                }
            }
        }
    }
    let mut models: Vec<ModelUsage> = counts
        .into_iter()
        .map(|(model, (calls, source))| ModelUsage {
            model,
            calls,
            source,
        })
        .collect();
    models.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.model.cmp(&b.model)));

    let count = |t: &str| u64::try_from(rt.block_on(store.count(t)).unwrap_or(0)).unwrap_or(0);
    let capture = CaptureSummary {
        events: count("captured_event"),
        sessions: count("session"),
        checkpoints: count("checkpoint"),
        last_checkpoint_at_ms: rt
            .block_on(store.query_opt_i64(
                "SELECT created_at FROM checkpoint ORDER BY created_at DESC LIMIT 1",
                &[],
            ))
            .ok()
            .flatten(),
    };
    Some((models, capture))
}

/// Read a project's `{cursor, epoch}` watermark, or `None` if the projection
/// is missing/unreadable. The single tiny read the change observer polls;
/// agrees with [`read_project`] on `None` (iff the projection is
/// absent/unreadable).
fn read_watermark_pair(root: &Path, path: &str) -> Option<Watermark> {
    let dir = PathBuf::from(path);
    let db = projection_db_for_family_under(root, &project_family(&dir)?);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open(&db).ok()?;
    let rt = runtime().ok()?;
    let cursor = rt.block_on(store.read_watermark("captured_event")).ok()?;
    let epoch = rt.block_on(store.read_rebuild_epoch()).ok()?;
    Some(Watermark { cursor, epoch })
}

/// A projection's change watermark (the rebuild-epoch rule): the append `cursor` moves on
/// ingest; only a rebuild moves the `epoch`. The pair is complete — there is
/// no projection change the pair cannot distinguish from quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watermark {
    /// The `captured_event` append cursor.
    pub cursor: i64,
    /// The rebuild epoch (0 for a never-rebuilt projection).
    pub epoch: i64,
}

/// How a tracked project's watermark moved between polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    /// New capture activity — the cursor moved at the same epoch.
    Ingested,
    /// The projection was rebuilt from git — the epoch moved.
    Rebuilt,
    /// The projection is (now) missing/unreadable.
    Unavailable,
}

/// One observed change for one tracked project.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChangeEvent {
    /// The canonical project path whose projection changed.
    project: String,
    /// Which way its cursor moved.
    kind: ChangeKind,
}

/// Fast polling cadence, used for a short window right after a detected change.
const ACTIVE_INTERVAL: Duration = Duration::from_millis(250);
/// Idle polling cadence, used once activity has decayed.
const IDLE_INTERVAL: Duration = Duration::from_secs(2);
/// How many quiet polls it takes to decay from the fast cadence back to idle.
const ACTIVE_TICKS_AFTER_CHANGE: u8 = 8;

/// The baseline change observer for the `PollOnly` reference engine: it
/// re-reads each tracked project's `{cursor, epoch}` watermark on an adaptive
/// interval and reports the diffs. A `Native`-capable engine (Turso subscribe)
/// would push the same [`ChangeEvent`]s without polling.
struct WatermarkPoller {
    /// Cache root the projections live under.
    root: PathBuf,
    /// project path → last-seen watermark (`None` once observed as unavailable).
    seen: BTreeMap<String, Option<Watermark>>,
    /// How many more fast-interval ticks remain before decaying to idle cadence.
    active_ticks: u8,
}

impl WatermarkPoller {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            seen: BTreeMap::new(),
            active_ticks: 0,
        }
    }

    /// The current poll interval — fast while recently active, else idle.
    fn interval(&self) -> Duration {
        if self.active_ticks > 0 {
            ACTIVE_INTERVAL
        } else {
            IDLE_INTERVAL
        }
    }

    /// Every project the observer is tracking (for rebuild re-bootstrap).
    fn tracked(&self) -> impl Iterator<Item = String> + '_ {
        self.seen.keys().cloned()
    }

    /// Record a project's freshly-observed watermark as the polling baseline
    /// (and begin tracking it). Called after every request/observed read so the
    /// next poll only fires on genuinely newer changes.
    fn sync(&mut self, project: &str, mark: Option<Watermark>) {
        self.seen.insert(project.to_string(), mark);
    }

    /// Re-read every tracked project's watermark and report the diffs. A moved
    /// **epoch** is [`ChangeKind::Rebuilt`] (however the cursor moved — more
    /// rows, fewer rows, or the same count); a moved cursor at the same epoch
    /// is [`ChangeKind::Ingested`]; a vanished watermark is
    /// [`ChangeKind::Unavailable`]. Cursor direction is deliberately **not** a
    /// signal (the rebuild-epoch rule retired the regression heuristic). A quiet poll decays
    /// the cadence toward idle.
    fn poll_once(&mut self) -> Vec<ChangeEvent> {
        let mut out = Vec::new();
        let paths: Vec<String> = self.seen.keys().cloned().collect();
        for path in paths {
            let prev = self.seen.get(&path).copied().flatten();
            let now = read_watermark_pair(&self.root, &path);
            let kind = match (prev, now) {
                (Some(p), Some(n)) if n.epoch != p.epoch => Some(ChangeKind::Rebuilt),
                (Some(p), Some(n)) if n.cursor != p.cursor => Some(ChangeKind::Ingested),
                (None, Some(_)) => Some(ChangeKind::Ingested),
                (Some(_), None) => Some(ChangeKind::Unavailable),
                // watermark unchanged, or still-unavailable → nothing to report.
                (Some(_), Some(_)) | (None, None) => None,
            };
            self.seen.insert(path.clone(), now);
            if let Some(kind) = kind {
                out.push(ChangeEvent {
                    project: path,
                    kind,
                });
            }
        }
        if out.is_empty() {
            self.active_ticks = self.active_ticks.saturating_sub(1);
        } else {
            self.active_ticks = ACTIVE_TICKS_AFTER_CHANGE;
        }
        out
    }
}

/// Wall-clock epoch-ms — the bus's only clock (status ages against it). The
/// pure truth table stays clock-injected; only this worker samples the wall.
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_canopy::StatusError;
    // The legacy per-path derivations as the seeding oracle: a main work tree's
    // family adopts exactly these ids (family identity).
    use rift_git::repo_id_for;
    use rift_store::{projection_db_under, Value};

    const EVENT_COLS: [&str; 9] = [
        "idempotency_key",
        "session_id",
        "turn_ordinal",
        "event_ordinal",
        "kind",
        "trust",
        "payload_sha256",
        "occurred_at_ms",
        "schema_version",
    ];

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    }

    #[test]
    fn viewer_request_coalescing_keeps_only_the_newest_filter_generation() {
        let mut requests = Vec::new();
        coalesce_viewer_request(
            &mut requests,
            rift_canopy::ViewerRequest::FilterScan {
                project: "/repo".to_owned(),
                generation: 1,
                query: "old".to_owned(),
            },
        );
        coalesce_viewer_request(
            &mut requests,
            rift_canopy::ViewerRequest::FilterScan {
                project: "/repo".to_owned(),
                generation: 2,
                query: "new".to_owned(),
            },
        );
        assert!(matches!(
            requests.as_slice(),
            [rift_canopy::ViewerRequest::FilterScan {
                generation: 2,
                query,
                ..
            }] if query == "new"
        ));
    }

    #[test]
    fn viewer_only_bus_does_not_create_any_cache_file_on_failure() {
        let cache = tempfile::tempdir().unwrap();
        let not_a_repo = tempfile::tempdir().unwrap();
        assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
        let bus = spawn_read_only_viewer(cache.path().to_path_buf());
        bus.req_tx
            .send(DataRequest::Viewer(rift_canopy::ViewerRequest::Catalogue {
                project: not_a_repo.path().to_string_lossy().into_owned(),
                generation: 1,
                target: None,
                refresh: false,
            }))
            .unwrap();
        assert!(matches!(
            bus.evt_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::Viewer(rift_canopy::ViewerEvent::Error {
                area: rift_canopy::ViewerErrorArea::Catalogue,
                ..
            })
        ));
        drop(bus);
        assert_eq!(
            std::fs::read_dir(cache.path()).unwrap().count(),
            0,
            "viewer-only startup must leave an absent cache database absent"
        );
    }

    /// Insert captured-event rows `start..start+n` into an open projection.
    fn insert_events(store: &SqliteStore, rt: &tokio::runtime::Runtime, start: usize, n: usize) {
        for i in start..start + n {
            rt.block_on(store.insert(
                "captured_event",
                &EVENT_COLS,
                &[
                    Value::text(format!("k{i}")),
                    Value::text("sess"),
                    Value::Int(0),
                    Value::Int(i64::try_from(i).unwrap()),
                    Value::text("start"),
                    Value::text("trusted"),
                    Value::text("sha"),
                    Value::Int(1),
                    Value::Int(1),
                ],
            ))
            .unwrap();
        }
    }

    /// Build a project directory + its per-repo projection under `root`, seeded
    /// with `events` captured-event rows. Returns the project path.
    ///
    /// The family's hook install is recorded too, because that is what a
    /// repository set up on this machine has: a projection nobody installed
    /// hooks for is setup left half done, and every status below is about a
    /// project that finished setting up.
    fn make_project(root: &Path, name: &str, events: usize) -> PathBuf {
        let proj = root.join(name);
        std::fs::create_dir_all(&proj).unwrap();
        // A repository, because every store the bus opens is its family's.
        rift_git::test_support::fake_repo(&proj).unwrap();
        record_hooks_installed(&proj);
        let repo = repo_id_for(&proj).unwrap();
        let db = projection_db_under(root, &repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = SqliteStore::open(&db).unwrap();
        store.create_canonical_schema().unwrap();
        insert_events(&store, &runtime(), 0, events);
        proj
    }

    /// Mark a member checkout provisioned: write the native-integration
    /// ownership manifest adoption leaves behind (family wiring and hook custody). Contents
    /// are irrelevant — presence is the fact the status reads.
    fn provision(work_tree: &Path) {
        let state = work_tree.join(".rift/state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("native-integrations.json"), "{}").unwrap();
    }

    /// Where `repo`'s family keeps its manifest and its binary of record.
    fn family_dir_of(repo: &Path) -> PathBuf {
        let family = project_family(repo).unwrap();
        rift_git::family::family_dir_under(|k| std::env::var(k).ok(), &family.common_dir)
    }

    /// Record a live binary of record for `repo`'s family: the note the git
    /// hook install leaves behind, and the second half of "set up here". The
    /// path recorded is this test binary, which is an executable that exists.
    fn record_hooks_installed(repo: &Path) {
        rift_shadow_hooks::record_binary_of_record(
            &family_dir_of(repo),
            &std::env::current_exe().unwrap(),
            rift_shadow_hooks::RecordedBy::Init,
        )
        .unwrap();
    }

    /// Write a live session cache under `git_dir`, claiming `work_tree` — the
    /// shape the capture hooks write, discriminator included.
    fn live_session(git_dir: &Path, id: &str, work_tree: &Path, updated_at_ms: i64) {
        let dir = git_dir.join("rift-sessions");
        std::fs::create_dir_all(&dir).unwrap();
        // Serialised, not spliced: a Windows path carries `\`, which a raw
        // JSON string would read as an escape and the reader would call corrupt.
        let body = serde_json::json!({
            "schema": 1,
            "session_id": id,
            "state": "active",
            "agent": "claude-code",
            "current_turn_ordinal": 1,
            "turn_open": true,
            "compaction_boundary": null,
            "updated_at_ms": updated_at_ms,
            "native_id_map": {},
            "work_tree": work_tree.display().to_string(),
        });
        std::fs::write(dir.join(format!("{id}.json")), body.to_string()).unwrap();
    }

    /// Append `n` more captured-event rows (keys `start..`) to an existing project.
    fn append_events(root: &Path, proj: &Path, start: usize, n: usize) {
        let repo = repo_id_for(proj).unwrap();
        let db = projection_db_under(root, &repo);
        let store = SqliteStore::open(&db).unwrap();
        insert_events(&store, &runtime(), start, n);
    }

    /// Open an existing project's projection store.
    fn open_proj(root: &Path, proj: &Path) -> SqliteStore {
        let repo = repo_id_for(proj).unwrap();
        SqliteStore::open(projection_db_under(root, &repo)).unwrap()
    }

    /// Seed a `session` row (headline model).
    fn seed_session(store: &SqliteStore, rt: &tokio::runtime::Runtime, id: &str, model: &str) {
        rt.block_on(store.insert(
            "session",
            &[
                "session_id",
                "agent",
                "model_primary",
                "repo_id",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("claude-code"),
                Value::text(model),
                Value::text("r"),
                Value::Int(1),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// Seed a `model_call` row.
    fn seed_model_call(store: &SqliteStore, rt: &tokio::runtime::Runtime, id: &str, model: &str) {
        rt.block_on(store.insert(
            "model_call",
            &[
                "call_id",
                "turn_id",
                "model",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("t"),
                Value::text(model),
                Value::Int(1),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// Seed a `checkpoint` row at `created_at`.
    fn seed_checkpoint(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        id: &str,
        created_at: i64,
    ) {
        rt.block_on(store.insert(
            "checkpoint",
            &[
                "checkpoint_id",
                "strategy",
                "created_at",
                "merkle_root",
                "prev_hash",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("manual"),
                Value::Int(created_at),
                Value::text("root"),
                Value::Null,
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    fn path_of(proj: &Path) -> String {
        proj.to_string_lossy().into_owned()
    }

    /// Test wrapper: build the detail with a throwaway health probe.
    fn detail_of(root: &Path, path: &str) -> DetailView {
        build_detail(
            root,
            path,
            ProjectStatus::Idle,
            Vec::new(),
            &mut HealthProbe::default(),
        )
    }

    /// Receive the next ProjectData update, skipping feed updates (SetActive
    /// emits both kinds).
    fn recv_project_data(bus: &Bus, total: Duration) -> Event {
        let deadline = std::time::Instant::now() + total;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match bus.evt_rx.recv_timeout(left.max(Duration::from_millis(1))) {
                Ok(Event::Feed(_) | Event::Health(_) | Event::WatchMode(_)) => {} // skip feed/health/mode updates
                Ok(ev) => return ev,
                Err(e) => panic!("no ProjectData update: {e}"),
            }
        }
    }

    /// The status half of [`read_project`] — the read surface under test.
    fn status(root: &Path, path: &str) -> ProjectStatus {
        read_project(root, path).status
    }

    /// **The dashboard half of family identity.** A card opened on a *linked worktree*
    /// reads the family's projection — the one the main work tree owns — and
    /// therefore shows the repository's capture history instead of the blank
    /// the member's own path used to key.
    ///
    /// Measured on this machine before the change: from `~/rift-worktrees/
    /// canopy` the dashboard reported 0 checkpoints / 0 sessions while the
    /// family held 58,864 events. The negative assertion is the load-bearing
    /// one — no per-path projection is created for the member — because
    /// counting the member's own rows would pass under either rule.
    #[test]
    fn a_member_path_reads_the_family_projection() {
        let root = tempfile::tempdir().unwrap();
        // A family: a main work tree with a seeded projection, and a linked
        // worktree that has never been captured into.
        let main = make_project(root.path(), "main", 3);
        let main_git = main.join(".git");
        let member = root.path().join("member");
        rift_git::test_support::fake_worktree(&main_git, &member, "member").unwrap();
        // Adopted, so this test keeps stating what it is about: the family view
        // gives an UNprovisioned member `NotAdopted`, which would mask the
        // projection read this test exists to pin; the unadopted arm is
        // `an_unadopted_member_reads_not_adopted` below.
        provision(&member);

        assert_eq!(
            status(root.path(), &member.to_string_lossy()),
            ProjectStatus::Idle,
            "a member reads the family's captured history, not an empty store"
        );
        assert_eq!(
            status(root.path(), &main.to_string_lossy()),
            status(root.path(), &member.to_string_lossy()),
            "either member's vantage reports the one repository"
        );

        // The watermark the change observer polls comes from the same store.
        assert_eq!(
            read_watermark_pair(root.path(), &member.to_string_lossy()),
            read_watermark_pair(root.path(), &main.to_string_lossy()),
        );

        // And nothing minted a second projection for the member's own path —
        // the defect, not merely its symptom.
        let per_path = projection_db_under(
            root.path(),
            &repo_id_for(&member).expect("the member path canonicalizes"),
        );
        assert!(
            !per_path.exists(),
            "a member must not get a projection keyed by its own path"
        );
    }

    #[test]
    fn read_status_missing_repo_for_nonexistent_path() {
        let root = tempfile::tempdir().unwrap();
        let s = status(root.path(), "/definitely/not/a/real/path");
        assert_eq!(s, ProjectStatus::Error(StatusError::RepoMissing));
    }

    #[test]
    fn read_status_not_initialized_without_projection() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("fresh");
        std::fs::create_dir_all(&proj).unwrap();
        rift_git::test_support::fake_repo(&proj).unwrap();
        let s = status(root.path(), &proj.to_string_lossy());
        assert_eq!(s, ProjectStatus::NotInitialized);

        // A directory outside any repository has no family to project and
        // reads as exactly that — never as a repository awaiting `rift init`.
        let plain = root.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let s = status(root.path(), &plain.to_string_lossy());
        assert_eq!(s, ProjectStatus::NotRepository);
    }

    #[test]
    fn read_status_idle_when_projection_has_capture_data() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "captured", 3);
        let s = status(root.path(), &proj.to_string_lossy());
        assert_eq!(s, ProjectStatus::Idle);
    }

    #[test]
    fn read_is_read_only_leaves_row_count_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "ro", 2);
        let repo = repo_id_for(&proj).unwrap();
        let db = projection_db_under(root.path(), &repo);

        let _ = status(root.path(), &proj.to_string_lossy());

        // The bus never writes: the row count is exactly what we seeded.
        let store = SqliteStore::open(&db).unwrap();
        assert_eq!(
            runtime().block_on(store.count("captured_event")).unwrap(),
            2
        );
    }

    #[test]
    fn scope_is_per_project_projection() {
        // Two projects have independent projections; reading one never reflects
        // the other's capture data.
        let root = tempfile::tempdir().unwrap();
        let a = make_project(root.path(), "a", 5);
        let b = root.path().join("b");
        std::fs::create_dir_all(&b).unwrap();
        rift_git::test_support::fake_repo(&b).unwrap();
        assert_eq!(
            status(root.path(), &a.to_string_lossy()),
            ProjectStatus::Idle
        );
        assert_eq!(
            status(root.path(), &b.to_string_lossy()),
            ProjectStatus::NotInitialized
        );
    }

    #[test]
    fn spawn_refreshes_initial_projects_and_delivers_updates() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        // Inject the cache root (no process-env mutation → no test races).
        let bus = spawn_for_test(root.path().to_path_buf(), vec![path_of(&proj)]);

        // The seeded refresh yields one ProjectData update for the project.
        // Generous deadline: the parallel suite drives real stores hard.
        let ev = recv_project_data(&bus, Duration::from_secs(30));
        match ev {
            Event::ProjectData(pd) => {
                assert_eq!(pd.project, path_of(&proj));
                assert_eq!(pd.status, ProjectStatus::Idle);
            }
            other => panic!("expected ProjectData, got {other:?}"),
        }
    }

    // --- status derivation from the live-session cache ---------------------

    /// Write `<proj>/.git/rift-sessions/<name>.json` with `contents` (the
    /// hermetic stand-in for the capture hooks' cache writer).
    ///
    /// The journal lives inside a gitdir, so the fixture makes one: creating
    /// the session directory alone left `<proj>/.git` a bare directory that
    /// hardened discovery refuses (family identity), and the status these tests
    /// read is derived from the repository the reader discovers.
    fn write_session_cache(proj: &Path, name: &str, contents: &str) {
        rift_git::test_support::fake_repo(proj).unwrap();
        let dir = proj.join(".git").join("rift-sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.json")), contents).unwrap();
    }

    /// A live cache body updated "now" (the reader's staleness window is
    /// generous; now_ms comes from the wall clock inside read_project).
    fn live_cache_json() -> String {
        format!(
            r#"{{"schema":1,"session_id":"s1","state":"active","agent":"claude-code",
                 "current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,
                 "updated_at_ms":{}}}"#,
            super::now_ms()
        )
    }

    // --- the family view: member status and family aggregation ------

    /// A two-member family fixture: the main work tree (with a seeded family
    /// projection) and one linked worktree, returned as `(main, member)`.
    ///
    /// The main checkout is deliberately NOT called `main`: `member_label`
    /// must name it by its role, not by whatever the folder happens to be
    /// called, or the family header would read differently in every clone.
    fn two_member_family(root: &Path, events: usize) -> (PathBuf, PathBuf) {
        let main = make_project(root, "trunk", events);
        let member = root.join("member");
        rift_git::test_support::fake_worktree(&main.join(".git"), &member, "member").unwrap();
        (main, member)
    }

    #[test]
    fn a_member_with_a_fresh_session_reads_running_and_the_family_aggregates_it() {
        // the family view, in one fixture because they are one fact
        // read at two scopes. The member's session cache sits under its own
        // administrative gitdir, which the family row has never looked in:
        // before this, a repository whose work happens in worktrees reported
        // `Idle` on the row the developer actually watches.
        let root = tempfile::tempdir().unwrap();
        let (main, member) = two_member_family(root.path(), 1);
        provision(&member);
        let member_git = main.join(".git/worktrees/member");
        let now = super::now_ms();
        live_session(&member_git, "in-member", &member, now);

        assert!(
            matches!(
                status(root.path(), &member.to_string_lossy()),
                ProjectStatus::Running {
                    active_sessions: 1,
                    ..
                }
            ),
            "the member row counts the session driven in it"
        );
        assert!(
            matches!(
                status(root.path(), &main.to_string_lossy()),
                ProjectStatus::Running {
                    active_sessions: 1,
                    ..
                }
            ),
            "and the family row aggregates it, from a registry it is not in"
        );

        // A second session, this one in main: the family sums, and neither
        // member is credited with the other's.
        live_session(&main.join(".git"), "in-main", &main, now);
        assert!(matches!(
            status(root.path(), &main.to_string_lossy()),
            ProjectStatus::Running {
                active_sessions: 2,
                ..
            }
        ));
        assert!(matches!(
            status(root.path(), &member.to_string_lossy()),
            ProjectStatus::Running {
                active_sessions: 1,
                ..
            }
        ));

        // The family row names the members its count came from, so a badge of
        // 2 can be read back to the checkouts that produced it; a member row
        // names none, because its count is its own.
        let family = read_project(root.path(), &main.to_string_lossy());
        assert_eq!(family.running_members, vec!["main", "member"]);
        assert!(read_project(root.path(), &member.to_string_lossy())
            .running_members
            .is_empty());
    }

    #[test]
    fn an_unadopted_member_reads_not_adopted() {
        // The member of an initialized family that `rift worktree adopt` has
        // never touched: no adapter in it is wired, so nothing it does is
        // captured. It is NOT `SetupIncomplete`, which would mean
        // "resume the wizard", and there is no plan file here to resume — and
        // it is not `NotInitialized`, because the family IS initialized.
        let root = tempfile::tempdir().unwrap();
        let (main, member) = two_member_family(root.path(), 1);
        assert_eq!(
            status(root.path(), &member.to_string_lossy()),
            ProjectStatus::NotAdopted
        );
        assert_eq!(
            status(root.path(), &main.to_string_lossy()),
            ProjectStatus::Idle,
            "the family row is never told a member's adoption state"
        );

        // Even with a live session and captured history in the family, the
        // answer is the same: the checkout is not wired, so the counts that
        // reach it are not its own doing.
        live_session(
            &main.join(".git/worktrees/member"),
            "s",
            &member,
            super::now_ms(),
        );
        assert_eq!(
            status(root.path(), &member.to_string_lossy()),
            ProjectStatus::NotAdopted
        );

        // Adoption is one file, and the status follows it immediately.
        provision(&member);
        assert!(matches!(
            status(root.path(), &member.to_string_lossy()),
            ProjectStatus::Running { .. }
        ));
    }

    #[test]
    fn a_gone_member_reads_repo_missing_prunable() {
        // `git worktree remove`'s other half: the checkout is deleted, the
        // administrative entry survives, and git calls it prunable. The row
        // says the checkout is missing, and the hint on a worktree row is the
        // one act that fixes it — reported, never performed for the operator.
        let root = tempfile::tempdir().unwrap();
        let (main, member) = two_member_family(root.path(), 1);
        provision(&member);
        std::fs::remove_dir_all(&member).unwrap();

        assert_eq!(
            status(root.path(), &member.to_string_lossy()),
            ProjectStatus::Error(StatusError::RepoMissing)
        );
        assert_eq!(
            ProjectStatus::Error(StatusError::RepoMissing).hint(EntryKind::Worktree),
            Some("checkout gone \u{2014} run: git worktree prune")
        );
        // The family is unharmed by a member it can no longer read, and the
        // administrative entry is still there for `git worktree prune`.
        assert_eq!(
            status(root.path(), &main.to_string_lossy()),
            ProjectStatus::Idle
        );
        assert!(main.join(".git/worktrees/member").is_dir());
    }

    #[test]
    fn a_family_row_is_never_asked_whether_it_was_adopted() {
        // `rift init` provisions the main work tree, so the question is a
        // category error there: a repository row with no member manifest must
        // keep reading its own status, never `NotAdopted`.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "solo", 1);
        assert!(!proj.join(".rift/state/native-integrations.json").exists());
        assert_eq!(
            status(root.path(), &proj.to_string_lossy()),
            ProjectStatus::Idle
        );
    }

    #[test]
    fn live_session_cache_makes_status_running() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "live", 1);
        write_session_cache(&proj, "s1", &live_cache_json());
        assert!(matches!(
            status(root.path(), &proj.to_string_lossy()),
            ProjectStatus::Running {
                active_sessions: 1,
                ..
            }
        ));
    }

    #[test]
    fn corrupt_session_cache_is_a_distinct_error() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "bad", 1);
        write_session_cache(&proj, "s1", "not a valid cache file");
        assert_eq!(
            status(root.path(), &proj.to_string_lossy()),
            ProjectStatus::Error(StatusError::CorruptSessionCache)
        );
    }

    #[test]
    fn absent_session_cache_leaves_status_idle() {
        // No cache → no live session → old capture data reads as Idle (and
        // never Running: the recency proxy is gone).
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "quiet", 1);
        rift_git::test_support::fake_repo(&proj).unwrap();
        assert_eq!(
            status(root.path(), &proj.to_string_lossy()),
            ProjectStatus::Idle
        );
    }

    #[test]
    fn ended_session_cache_reads_idle_not_running() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "done", 1);
        write_session_cache(
            &proj,
            "s1",
            &format!(
                r#"{{"schema":1,"session_id":"s1","state":"ended","updated_at_ms":{}}}"#,
                super::now_ms()
            ),
        );
        assert_eq!(
            status(root.path(), &proj.to_string_lossy()),
            ProjectStatus::Idle
        );
    }

    // --- the detail view -----------------------------------------------------

    #[test]
    fn aggregates_distinct_models_with_counts() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "m", 1);
        let store = open_proj(root.path(), &proj);
        let rt = runtime();
        seed_model_call(&store, &rt, "c1", "A");
        seed_model_call(&store, &rt, "c2", "A");
        seed_model_call(&store, &rt, "c3", "B");
        seed_session(&store, &rt, "s1", "A"); // already seen
        seed_session(&store, &rt, "s2", "C"); // headline-only, 0 calls
        seed_session(&store, &rt, "s3", ""); // legacy unknown, never displayed

        let d = detail_of(root.path(), &path_of(&proj));
        // Most-used first; the headline-only model appears with 0 calls.
        let models: Vec<(&str, u64)> = d
            .models
            .iter()
            .map(|m| (m.model.as_str(), m.calls))
            .collect();
        assert_eq!(models, vec![("A", 2), ("B", 1), ("C", 0)]);
    }

    #[test]
    fn runtime_model_observation_supersedes_configured_provenance() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "model-source", 1);
        let store = open_proj(root.path(), &proj);
        let rt = runtime();
        for (id, model, source, at) in [
            ("o1", "antigravity-pro", "configured_client", 1),
            ("o2", "antigravity-pro", "runtime_hook", 2),
            ("o3", "configured-only", "configured_client", 3),
        ] {
            rt.block_on(store.insert(
                "model_observation",
                &[
                    "observation_id",
                    "session_id",
                    "model",
                    "source",
                    "observed_at",
                    "schema_version",
                ],
                &[
                    Value::text(id),
                    Value::text("s"),
                    Value::text(model),
                    Value::text(source),
                    Value::Int(at),
                    Value::Int(1),
                ],
            ))
            .unwrap();
        }
        let detail = detail_of(root.path(), &path_of(&proj));
        assert_eq!(
            detail
                .models
                .iter()
                .find(|model| model.model == "antigravity-pro")
                .map(|model| model.source),
            Some(ModelProvenance::Runtime)
        );
        assert_eq!(
            detail
                .models
                .iter()
                .find(|model| model.model == "configured-only")
                .map(|model| model.source),
            Some(ModelProvenance::Configured)
        );
    }

    #[test]
    fn capture_summary_reflects_projection() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "cap", 4); // 4 captured events
        let store = open_proj(root.path(), &proj);
        let rt = runtime();
        seed_session(&store, &rt, "s1", "A");
        seed_checkpoint(&store, &rt, "k1", 100);
        seed_checkpoint(&store, &rt, "k2", 200);

        let d = detail_of(root.path(), &path_of(&proj));
        assert_eq!(d.capture.events, 4);
        assert_eq!(d.capture.sessions, 1);
        assert_eq!(d.capture.checkpoints, 2);
        assert_eq!(d.capture.last_checkpoint_at_ms, Some(200));
    }

    #[test]
    fn detail_is_scope_filtered() {
        // Two projects with independent projections: one's models never bleed into
        // the other's detail.
        let root = tempfile::tempdir().unwrap();
        let a = make_project(root.path(), "a", 1);
        let b = make_project(root.path(), "b", 1);
        let sa = open_proj(root.path(), &a);
        seed_model_call(&sa, &runtime(), "c1", "OnlyInA");

        let da = detail_of(root.path(), &path_of(&a));
        let db = detail_of(root.path(), &path_of(&b));
        assert!(da.models.iter().any(|m| m.model == "OnlyInA"));
        assert!(
            db.models.iter().all(|m| m.model != "OnlyInA"),
            "project b's detail must not reflect project a's models"
        );
    }

    #[test]
    fn active_project_gets_detail_others_do_not() {
        // The bus attaches rich detail only for the SetActive (foreground) project.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        let bus = spawn_for_test(root.path().to_path_buf(), vec![]);
        bus.req_tx
            .send(DataRequest::SetActive(Some(path_of(&proj))))
            .unwrap();
        let ev = recv_project_data(&bus, Duration::from_secs(30));
        match ev {
            Event::ProjectData(pd) => {
                assert_eq!(pd.project, path_of(&proj));
                assert!(pd.detail.is_some(), "the active project carries detail");
            }
            other => panic!("expected ProjectData, got {other:?}"),
        }
    }

    // --- the change observer -------------------------------------------------

    #[test]
    fn watermark_advance_triggers_ingested() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 2);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 2,
                epoch: 0,
            }),
        ); // baseline

        append_events(root.path(), &proj, 2, 3); // now 5

        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Ingested,
            }]
        );
        // Baseline caught up: a subsequent quiet poll reports nothing.
        assert!(poller.poll_once().is_empty());
    }

    #[test]
    fn epoch_bump_triggers_rebuilt_and_rebootstrap() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 5);
        let repo = repo_id_for(&proj).unwrap();
        let db = projection_db_under(root.path(), &repo);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 5,
                epoch: 0,
            }),
        ); // baseline

        // A rebuild bumps the epoch (0 → 1); the cursor also resets, but the
        // epoch alone is the signal.
        rift_store::rebuild(&db).unwrap();
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Rebuilt,
            }]
        );

        // Capture re-populates (cursor 0 → 3 at the same epoch = Ingested),
        // then a second rebuild is a fresh Rebuilt — re-detected, not swallowed.
        append_events(root.path(), &proj, 0, 3);
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Ingested,
            }]
        );
        rift_store::rebuild(&db).unwrap();
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Rebuilt,
            }]
        );
    }

    #[test]
    fn rebuild_landing_more_rows_is_still_detected() {
        // The rebuild-epoch rule blind spot, closed: a rebuild that lands MORE rows than
        // the projection previously held is indistinguishable from ingest by
        // cursor alone — the epoch is what detects it.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 2);
        let repo = repo_id_for(&proj).unwrap();
        let db = projection_db_under(root.path(), &repo);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 2,
                epoch: 0,
            }),
        );

        // Rebuild, then land MORE rows (5 > 2) before the next poll.
        rift_store::rebuild(&db).unwrap();
        append_events(root.path(), &proj, 0, 5);
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Rebuilt,
            }],
            "a rebuild with a net-larger projection must read as Rebuilt, not Ingested"
        );
    }

    #[test]
    fn cursor_regression_alone_is_not_rebuilt() {
        // Cursor direction is retired as a signal: a baseline that is somehow
        // AHEAD of the store (e.g. a stale in-memory baseline) re-reads as
        // Ingested — only the epoch says Rebuilt.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 3);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 9,
                epoch: 0,
            }),
        );
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Ingested,
            }],
            "a backward cursor at the same epoch re-reads, but never as Rebuilt"
        );
    }

    proptest::proptest! {
        // Each case drives a real store through rebuilds — cap the case count
        // so the suite's wall-clock stays test-farm friendly.
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(24))]

        /// The watermark never goes backwards without an epoch bump, over an
        /// arbitrary interleaving of appends and rebuilds against a real store.
        #[test]
        fn watermark_never_backward_without_epoch_bump(
            ops in proptest::collection::vec(
                proptest::prop_oneof![
                    proptest::prelude::Strategy::prop_map(1usize..4, OpKind::Append),
                    proptest::strategy::Just(OpKind::Rebuild),
                ],
                1..8,
            ),
        ) {
            let root = tempfile::tempdir().unwrap();
            let proj = make_project(root.path(), "p", 1);
            let repo = repo_id_for(&proj).unwrap();
            let db = projection_db_under(root.path(), &repo);

            let mut prev = read_watermark_pair(root.path(), &path_of(&proj)).unwrap();
            let mut appended = 1usize;
            for op in ops {
                match op {
                    OpKind::Append(n) => {
                        append_events(root.path(), &proj, appended, n);
                        appended += n;
                    }
                    OpKind::Rebuild => {
                        rift_store::rebuild(&db).unwrap();
                        appended = 0;
                    }
                }
                let now = read_watermark_pair(root.path(), &path_of(&proj)).unwrap();
                proptest::prop_assert!(
                    now.cursor >= prev.cursor || now.epoch > prev.epoch,
                    "cursor went backward ({} -> {}) without an epoch bump ({} -> {})",
                    prev.cursor, now.cursor, prev.epoch, now.epoch
                );
                prev = now;
            }
        }
    }

    /// One projection mutation for the watermark property test.
    #[derive(Clone, Copy, Debug)]
    enum OpKind {
        Append(usize),
        Rebuild,
    }

    #[test]
    fn missing_projection_is_unavailable_not_panic() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 2);
        let repo = repo_id_for(&proj).unwrap();
        let db = projection_db_under(root.path(), &repo);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 2,
                epoch: 0,
            }),
        ); // baseline

        // The projection file disappears out from under the observer.
        std::fs::remove_file(&db).unwrap();
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Unavailable,
            }]
        );
        // Polling again does not panic and reports nothing new (still gone).
        assert!(poller.poll_once().is_empty());
    }

    #[test]
    fn observer_is_non_authoritative() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 1);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 1,
                epoch: 0,
            }),
        );

        // No underlying change: polling twice is idempotent (no spurious events).
        assert!(poller.poll_once().is_empty());
        assert!(poller.poll_once().is_empty());

        // A burst of appends between polls collapses to a single Ingested — a
        // "missed" intermediate state only delays, never loses, the update.
        append_events(root.path(), &proj, 1, 4); // 1 → 5 in one gap
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Ingested,
            }]
        );
        // Caught up again.
        assert!(poller.poll_once().is_empty());
    }

    #[test]
    fn interval_is_fast_after_change_then_decays_to_idle() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 1);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 1,
                epoch: 0,
            }),
        );
        assert_eq!(poller.interval(), IDLE_INTERVAL); // no activity yet

        append_events(root.path(), &proj, 1, 1); // a change
        let _ = poller.poll_once();
        assert_eq!(poller.interval(), ACTIVE_INTERVAL); // fast right after

        // Quiet polls decay the cadence back to idle.
        for _ in 0..ACTIVE_TICKS_AFTER_CHANGE {
            assert!(poller.poll_once().is_empty());
        }
        assert_eq!(poller.interval(), IDLE_INTERVAL);
    }

    // --- wake-only notify + the bounded latest-per-feed channel -------------

    /// A watcher that installs nothing and wakes nothing: unit tests exercise
    /// the poll path deterministically, with no platform watcher (many
    /// concurrent FSEvents streams stall on macOS — the real watcher is
    /// integration-tested in tests/live_sections.rs).
    struct NoopWatcher;
    impl WakeWatcher for NoopWatcher {
        fn watch(&mut self, _root: &Path, _wake: Sender<WorkerMsg>) -> Result<(), String> {
            Ok(())
        }
    }

    /// Test spawn: the bus over a tempdir root with the no-op watcher.
    fn spawn_for_test(root: PathBuf, initial: Vec<String>) -> Bus {
        spawn_with_watcher(root, initial, Box::new(NoopWatcher))
    }

    /// A watcher stub that hands the test its wake sender (no filesystem).
    struct StubWatcher {
        grabbed: std::sync::Arc<std::sync::Mutex<Option<Sender<WorkerMsg>>>>,
    }
    impl WakeWatcher for StubWatcher {
        fn watch(&mut self, _root: &Path, wake: Sender<WorkerMsg>) -> Result<(), String> {
            *self.grabbed.lock().unwrap() = Some(wake);
            Ok(())
        }
    }

    /// A watcher that always fails — the injected PollOnly degradation.
    struct FailingWatcher;
    impl WakeWatcher for FailingWatcher {
        fn watch(&mut self, _root: &Path, _wake: Sender<WorkerMsg>) -> Result<(), String> {
            Err("no watch backend on this filesystem".into())
        }
    }

    #[test]
    fn wake_triggers_a_fresh_poll_and_spurious_wakes_are_harmless() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        let grabbed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let bus = spawn_with_watcher(
            root.path().to_path_buf(),
            vec![path_of(&proj)],
            Box::new(StubWatcher {
                grabbed: std::sync::Arc::clone(&grabbed),
            }),
        );
        // Drain the seeded update; the observer baseline is now synced.
        let first = recv_project_data(&bus, Duration::from_secs(5));
        assert!(matches!(first, Event::ProjectData(_)));
        let wake = grabbed.lock().unwrap().clone().expect("watcher installed");

        // Spurious + duplicated wakes with no underlying change: the fresh
        // poll reads quiet, so nothing is emitted (wake-only, no data path).
        wake.send(WorkerMsg::Wake).unwrap();
        wake.send(WorkerMsg::Wake).unwrap();
        assert!(
            bus.evt_rx.recv_timeout(Duration::from_millis(600)).is_err(),
            "a spurious wake must not fabricate an update"
        );

        // A real change + a wake: the update arrives promptly (well inside the
        // idle cadence — the wake scheduled the poll, the poll found it).
        append_events(root.path(), &proj, 1, 2);
        wake.send(WorkerMsg::Wake).unwrap();
        let ev = recv_project_data(&bus, Duration::from_secs(5));
        assert!(matches!(ev, Event::ProjectData(_)));
    }

    #[test]
    fn injected_watch_failure_degrades_to_poll_only() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        let bus = spawn_with_watcher(
            root.path().to_path_buf(),
            vec![path_of(&proj)],
            Box::new(FailingWatcher),
        );
        // The FIRST event says how the bus is woken: the degrade is reported,
        // never silent.
        let mode = bus
            .evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the wake mode is reported first");
        assert_eq!(mode, Event::WatchMode(WatchMode::PollOnly));
        let first = recv_project_data(&bus, Duration::from_secs(5));
        assert!(matches!(first, Event::ProjectData(_)));

        // No watcher — the adaptive poll cadence still surfaces the change.
        append_events(root.path(), &proj, 1, 2);
        let ev = bus
            .evt_rx
            .recv_timeout(Duration::from_secs(8))
            .expect("poll-only still delivers");
        assert!(matches!(ev, Event::ProjectData(pd) if pd.project == path_of(&proj)));
    }

    // --- the rebuild-epoch rule amendment: the session-dir watch + wake-recompute-active --

    /// A watcher recording every `set_active_session_dirs` call (the wiring
    /// seam test — no filesystem).
    struct RecordingWatcher {
        calls: std::sync::Arc<std::sync::Mutex<Vec<Vec<PathBuf>>>>,
    }
    impl WakeWatcher for RecordingWatcher {
        fn watch(&mut self, _root: &Path, _wake: Sender<WorkerMsg>) -> Result<(), String> {
            Ok(())
        }
        fn set_active_session_dirs(&mut self, dirs: &[PathBuf]) {
            self.calls.lock().unwrap().push(dirs.to_vec());
        }
    }

    /// Spin until one of the recorded watch sets satisfies `want`, and return
    /// its index — or fail with everything that was recorded, so a wiring bug
    /// reads as the set that WAS asked for rather than a bare timeout.
    fn wait_for_watch_set(
        calls: &std::sync::Arc<std::sync::Mutex<Vec<Vec<PathBuf>>>>,
        what: &str,
        want: impl Fn(&[PathBuf]) -> bool,
    ) -> usize {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(at) = calls
                .lock()
                .unwrap()
                .iter()
                .position(|set| want(set.as_slice()))
            {
                return at;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{what}; recorded: {:?}",
                calls.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn set_active_swaps_the_session_dir_watch() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        // A git-ish project: discover() resolves `<proj>/.git`, so the worker
        // derives `<proj>/.git/rift-sessions` as the narrow watch target.
        rift_git::test_support::fake_repo(&proj).unwrap();
        // Canonical: the members the watch set is built from come from
        // `family_members`, which resolves each checkout (on macOS a tempdir's
        // `/var/...` is `/private/var/...`), and a watcher wants a real path.
        let expected = std::fs::canonicalize(proj.join(".git"))
            .unwrap()
            .join("rift-sessions");

        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let bus = spawn_with_watcher(
            root.path().to_path_buf(),
            vec![],
            Box::new(RecordingWatcher {
                calls: std::sync::Arc::clone(&calls),
            }),
        );

        bus.req_tx
            .send(DataRequest::SetActive(Some(path_of(&proj))))
            .unwrap();
        let attached_at = wait_for_watch_set(
            &calls,
            "SetActive points the watch at the session dir",
            |set| set == [expected.clone()],
        );

        // Clearing the active project unwatches.
        bus.req_tx.send(DataRequest::SetActive(None)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut cleared = false;
        while !cleared && std::time::Instant::now() < deadline {
            cleared = calls
                .lock()
                .unwrap()
                .iter()
                .skip(attached_at + 1)
                .any(Vec::is_empty);
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(cleared, "SetActive(None) clears the session-dir watch");
    }

    #[test]
    fn the_bus_watches_every_members_session_dir() {
        // the family view. Session caches stay per gitdir (family identity), so a
        // watch on the vantage's own dir alone leaves every agent running in a
        // linked worktree prompt nowhere — the poll cadence would eventually
        // notice, seconds later, which is the promptness this second watch
        // exists to buy.
        let root = tempfile::tempdir().unwrap();
        let (main, member) = two_member_family(root.path(), 1);
        let gone = root.path().join("gone");
        rift_git::test_support::fake_worktree(&main.join(".git"), &gone, "gone").unwrap();
        std::fs::remove_dir_all(&gone).unwrap();

        // Canonical, for the reason `set_active_swaps_the_session_dir_watch`
        // spells out.
        let main_git = std::fs::canonicalize(main.join(".git")).unwrap();
        let main_sessions = main_git.join("rift-sessions");
        let member_sessions = main_git.join("worktrees/member/rift-sessions");
        let gone_sessions = main_git.join("worktrees/gone/rift-sessions");

        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let bus = spawn_with_watcher(
            root.path().to_path_buf(),
            vec![],
            Box::new(RecordingWatcher {
                calls: std::sync::Arc::clone(&calls),
            }),
        );

        // Selecting the family row watches main AND the member.
        bus.req_tx
            .send(DataRequest::SetActive(Some(path_of(&main))))
            .unwrap();
        wait_for_watch_set(&calls, "the family row watches every member", |set| {
            set.contains(&main_sessions) && set.contains(&member_sessions)
        });

        // And a member whose checkout is gone is not watched: its registry can
        // never gain a session, so the stream would never fire.
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|set| set.contains(&gone_sessions)),
            "a prunable member is never given a watch"
        );

        // Selecting the MEMBER row watches the same set: the family is the
        // unit, whichever of its checkouts the card is opened on.
        bus.req_tx
            .send(DataRequest::SetActive(Some(path_of(&member))))
            .unwrap();
        wait_for_watch_set(&calls, "a member vantage watches the family too", |set| {
            set.contains(&main_sessions) && set.contains(&member_sessions)
        });
    }

    /// Seed the project's recall.db with served audit events — a write that
    /// moves **no** projection watermark (the recall-only change class).
    fn seed_recall_serves(root: &Path, proj: &Path, events: &[(i64, i64)]) {
        let repo = rift_store::recall_repo_id(proj).unwrap();
        let db = rift_store::recall_db_under(root, &repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        let rt = runtime();
        for (i, (t, avoided)) in events.iter().enumerate() {
            rt.block_on(store.insert(
                "memd_audit_event",
                &[
                    "audit_id",
                    "verb",
                    "handle",
                    "reason",
                    "tokens_returned",
                    "tokens_avoided",
                    "recorded_at",
                    "schema_version",
                ],
                &[
                    Value::text(format!("a{i}")),
                    Value::text("memd.summary_served"),
                    Value::text("h"),
                    Value::Null,
                    Value::Int(10),
                    Value::Int(*avoided),
                    Value::Int(*t),
                    Value::Int(1),
                ],
            ))
            .unwrap();
            rt.block_on(store.insert(
                "serve_call_event",
                &[
                    "call_id",
                    "channel",
                    "tool_name",
                    "terminal_verb",
                    "tokens_returned",
                    "tokens_avoided",
                    "recorded_at",
                    "schema_version",
                ],
                &[
                    Value::text(format!("c{i}")),
                    Value::text("skill_cli"),
                    Value::text("rift_read_summary"),
                    Value::text("memd.summary_served"),
                    Value::Int(10),
                    Value::Int(*avoided),
                    Value::Int(*t),
                    Value::Int(1),
                ],
            ))
            .unwrap();
        }
    }

    #[test]
    fn fs_wake_recomputes_the_active_project_without_a_cursor_move() {
        // The rebuild-epoch rule amendment's gap-closure: recall.db (and the session
        // caches) change without moving the projection {cursor, epoch}, so a
        // poll alone never re-reads. An fs wake must recompute the ACTIVE
        // project's feeds regardless — that is how a recall serve reaches
        // the panel promptly.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        let grabbed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let bus = spawn_with_watcher(
            root.path().to_path_buf(),
            vec![],
            Box::new(StubWatcher {
                grabbed: std::sync::Arc::clone(&grabbed),
            }),
        );
        bus.req_tx
            .send(DataRequest::SetActive(Some(path_of(&proj))))
            .unwrap();

        // The SetActive recompute lands (any Feed) — baseline fully synced.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut primed = false;
        while !primed && std::time::Instant::now() < deadline {
            if let Ok(Event::Feed(_)) = bus.evt_rx.recv_timeout(Duration::from_millis(250)) {
                primed = true;
            }
        }
        assert!(primed, "the SetActive recompute never arrived");
        // Drain the burst dry.
        while bus.evt_rx.recv_timeout(Duration::from_millis(700)).is_ok() {}

        // Recall-only change: serves land, no projection write of any kind.
        seed_recall_serves(root.path(), &proj, &[(1_000, 500)]);
        let wake = grabbed.lock().unwrap().clone().expect("watcher installed");
        wake.send(WorkerMsg::Wake).unwrap();

        // The wake alone (no cursor move) surfaces the served data.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut saw_serves = false;
        while !saw_serves && std::time::Instant::now() < deadline {
            if let Ok(Event::Feed(f)) = bus.evt_rx.recv_timeout(Duration::from_millis(250)) {
                if matches!(
                    f.payload,
                    rift_canopy::FeedPayload::Recall {
                        savings: rift_canopy::RecallView::Data(_),
                        calls: rift_canopy::RecallView::Data(_),
                        ..
                    }
                ) {
                    saw_serves = true;
                }
            }
        }
        assert!(
            saw_serves,
            "an fs wake must recompute the active project's feeds even when \
             no projection watermark moved"
        );
    }

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

    // --- the data-plane health report --------------------------------

    /// A window request adopts the period **and** schedules the read that
    /// applies it, and it does both by set-union like every other request —
    /// so a viewer walking the four buttons in one frame costs one read at
    /// the last window pressed, not four.
    ///
    /// `collect` is where the worker's whole request grammar is decided, and
    /// it is pure, so this needs no thread, no store and no clock.
    #[test]
    fn a_window_request_is_adopted_and_re_reads_the_active_project() {
        let mut to_read = BTreeSet::new();
        let mut viewer = Vec::new();
        let mut active = None;
        let mut window = rift_canopy::RecallWindow::default();
        let mut woke = false;
        let mut feed = |msg| {
            collect(
                msg,
                &mut to_read,
                &mut viewer,
                &mut active,
                &mut window,
                &mut woke,
            );
        };
        feed(WorkerMsg::Request(DataRequest::SetActive(Some(
            "/r/p".to_owned(),
        ))));
        feed(WorkerMsg::Request(DataRequest::SetRecallWindow(
            rift_canopy::RecallWindow::Last7Days,
        )));
        feed(WorkerMsg::Request(DataRequest::SetRecallWindow(
            rift_canopy::RecallWindow::Today,
        )));
        assert_eq!(
            window,
            rift_canopy::RecallWindow::Today,
            "the last window pressed is the one that sticks"
        );
        assert_eq!(
            to_read.len(),
            1,
            "three requests over one project coalesce into one read: {to_read:?}"
        );
        assert!(to_read.contains("/r/p"));
        assert!(!woke, "a window request is not a filesystem wake");
    }

    /// With nothing selected there is nothing to recompute — the window is
    /// still adopted, so the next `SetActive` reads at the chosen period
    /// rather than at the default.
    #[test]
    fn a_window_request_with_no_active_project_schedules_no_read() {
        let mut to_read = BTreeSet::new();
        let mut viewer = Vec::new();
        let mut active = None;
        let mut window = rift_canopy::RecallWindow::default();
        let mut woke = false;
        collect(
            WorkerMsg::Request(DataRequest::SetRecallWindow(
                rift_canopy::RecallWindow::Last30Days,
            )),
            &mut to_read,
            &mut viewer,
            &mut active,
            &mut window,
            &mut woke,
        );
        assert_eq!(window, rift_canopy::RecallWindow::Last30Days);
        assert!(to_read.is_empty());
        assert_eq!(active, None);
    }

    #[test]
    fn health_probe_priorities() {
        let mut p = HealthProbe::default();
        assert_eq!(p.report(), HealthEvent::Healthy);
        p.runtime_error = true;
        assert_eq!(p.report(), HealthEvent::WorkerError);
        p.store_error = true;
        assert_eq!(
            p.report(),
            HealthEvent::StoreUnreachable,
            "a dead store outranks a worker error"
        );
    }

    #[test]
    fn health_emits_once_then_only_on_change() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        let bus = spawn_for_test(root.path().to_path_buf(), vec![path_of(&proj)]);

        // Drain the seeded cycle: exactly one Health(Healthy) rides beside the
        // ProjectData (emit-on-change: the first report is the change).
        let mut healths = Vec::new();
        let mut saw_data = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while (!saw_data || healths.is_empty()) && std::time::Instant::now() < deadline {
            match bus.evt_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(Event::Health(h)) => healths.push(h),
                Ok(Event::ProjectData(_)) => saw_data = true,
                Ok(_) | Err(_) => {}
            }
        }
        assert_eq!(healths, vec![HealthEvent::Healthy]);

        // A second read of a healthy plane emits data but NO new health report.
        bus.req_tx
            .send(DataRequest::Refresh(path_of(&proj)))
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut extra_health = 0;
        let mut saw_second = false;
        while !saw_second && std::time::Instant::now() < deadline {
            match bus.evt_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(Event::Health(_)) => extra_health += 1,
                Ok(Event::ProjectData(_)) => saw_second = true,
                Ok(_) | Err(_) => {}
            }
        }
        assert!(saw_second, "the refresh delivered");
        assert_eq!(extra_health, 0, "an unchanged health is never re-sent");
    }

    #[test]
    fn bus_delivers_live_update_via_poll() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        let bus = spawn_for_test(root.path().to_path_buf(), vec![path_of(&proj)]);

        // Drain the initial request-driven update.
        let first = recv_project_data(&bus, Duration::from_secs(15));
        assert!(matches!(first, Event::ProjectData(_)));

        // A concurrent append must surface via the observer's poll — with no
        // further request sent, any subsequent update is poll-driven.
        append_events(root.path(), &proj, 1, 2);
        let second = recv_project_data(&bus, Duration::from_secs(15));
        match second {
            Event::ProjectData(pd) => {
                assert_eq!(pd.project, path_of(&proj));
                assert_eq!(pd.status, ProjectStatus::Idle);
            }
            other => panic!("expected ProjectData, got {other:?}"),
        }
    }

    #[test]
    fn uninitialized_settings_use_native_defaults_and_all_adapters() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(repo.path()).unwrap();
        let view = project_settings_view_under(root.path(), repo.path());
        assert_eq!(view.initialization, ProjectInitialization::NotInitialized);
        assert_eq!(view.integrations.len(), 9);
        assert_eq!(
            view.integrations
                .iter()
                .map(|row| row.agent.as_str())
                .collect::<Vec<_>>(),
            vec![
                "claude-code",
                "codex",
                "gemini",
                "antigravity",
                "cursor",
                "copilot-cli",
                "opencode",
                "factory-droid",
                "ollama",
            ]
        );
        assert!(view.settings[0].enabled, "Recall setup default is on");
        assert!(
            view.settings[1].enabled,
            "400-token injection default is on"
        );
        assert!(view.settings[2].enabled, "documents default is on");
        assert!(view.settings[3].enabled, "native search default is on");
        assert!(view.settings[4].enabled, "tool capture default is on");
        assert!(view.settings[5].enabled, "token capture default is on");
        assert!(!view.settings[6].enabled, "raw transcripts default off");
        assert_eq!(
            view.integrations
                .iter()
                .filter(|row| row.alpha_supported)
                .map(|row| row.agent.as_str())
                .collect::<Vec<_>>(),
            vec!["claude-code", "codex", "antigravity"],
            "each row carries the alpha capture scope, single-sourced from \
             the registry-side list"
        );
    }

    /// The pre-selection rule, driven pure: `diagnose_all` reads the
    /// machine's real `PATH`, so the extracted predicate is the testable
    /// seam. The load-bearing arm is the third: a DETECTED coming-soon agent
    /// must not be staged into a default setup plan `initialize()` is
    /// guaranteed to refuse.
    #[test]
    fn a_detected_coming_soon_agent_is_not_preselected_for_setup() {
        use rift_canopy::NativeAgentWiring as Wiring;
        use rift_canopy::ProjectInitialization as Init;
        assert!(default_selection(
            Wiring::Missing,
            Init::NotInitialized,
            true,
            true
        ));
        assert!(!default_selection(
            Wiring::Missing,
            Init::NotInitialized,
            true,
            false
        ));
        assert!(default_selection(
            Wiring::Managed,
            Init::Initialized,
            false,
            false
        ));
        assert!(!default_selection(
            Wiring::Missing,
            Init::Initialized,
            true,
            true
        ));
    }

    /// The projection of the installer's vocabulary into a row: every install
    /// source survives with its version, the row's availability sentence is
    /// the installer's own (so the dashboard and `rift recall status` cannot
    /// name different reasons), and an extension-only install pre-ticks a
    /// supported row before initialization exactly as a `PATH` command does —
    /// the gap where Canopy offered Codex and `rift init` skipped it.
    #[test]
    fn install_evidence_projects_into_rows_with_one_availability_sentence() {
        use rift_agent_install::native::{
            NativeDetection, NativeIdeHost, NativeInstallSource, NativeInstallation,
            NativeIntegrationStatus, NativeWiring,
        };
        let status =
            |agent: &str, installations: Vec<NativeInstallation>| NativeIntegrationStatus {
                agent: agent.to_owned(),
                display_name: agent.to_owned(),
                detection: if installations.is_empty() {
                    NativeDetection::NotDetected
                } else {
                    NativeDetection::Detected
                },
                wiring: NativeWiring::Missing,
                executable: None,
                installations,
                hook_wiring: None,
            };
        let install = |source, version: Option<&str>| NativeInstallation {
            source,
            version: version.map(str::to_owned),
            executable: None,
            path: std::path::PathBuf::from("x"),
        };
        let statuses = vec![
            status(
                "claude-code",
                vec![
                    install(
                        NativeInstallSource::IdeExtension(NativeIdeHost::VsCode),
                        Some("2.1.267"),
                    ),
                    install(NativeInstallSource::JetBrainsPlugin, None),
                ],
            ),
            status(
                "codex",
                vec![
                    install(NativeInstallSource::PathCommand, None),
                    install(NativeInstallSource::VendorInstall, Some("26.9.1")),
                    install(
                        NativeInstallSource::IdeExtension(NativeIdeHost::Windsurf),
                        Some("26.9"),
                    ),
                ],
            ),
            status(
                "ollama",
                vec![install(NativeInstallSource::PathCommand, None)],
            ),
            status("antigravity", Vec::new()),
        ];
        let expected: Vec<String> = statuses
            .iter()
            .map(NativeIntegrationStatus::availability)
            .collect();

        let rows = native_settings_rows(statuses, ProjectInitialization::NotInitialized);
        let actual: Vec<String> = rows
            .iter()
            .map(NativeAgentIntegrationRow::availability)
            .collect();
        assert_eq!(actual, expected, "the row spells the installer's sentence");
        assert_eq!(
            actual[0],
            "present via VS Code extension 2.1.267 + JetBrains plugin"
        );
        assert_eq!(
            actual[1],
            "present via PATH command + vendor install 26.9.1 + Windsurf extension 26.9"
        );
        assert_eq!(actual[3], "absent");

        assert_eq!(
            rows[1].installations[2].source,
            NativeAgentInstallSource::IdeExtension(NativeAgentIdeHost::Windsurf)
        );
        assert!(
            rows[0].detected() && rows[0].selected,
            "extension-only pre-ticks"
        );
        assert!(rows[1].selected);
        assert!(
            rows[2].detected() && !rows[2].selected,
            "detected but coming soon is never staged"
        );
        assert!(!rows[3].detected() && !rows[3].selected);
    }

    /// The card says what the settings page says. A family whose git hooks
    /// were never installed is setup left half done, however healthy its
    /// projection reads; with the install on record the same projection —
    /// nothing captured in it yet — is a repository picking up.
    #[test]
    fn a_projection_with_no_recorded_install_is_setup_incomplete_on_the_card() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "fresh", 0);
        std::fs::remove_file(rift_shadow_hooks::binary_of_record_path(&family_dir_of(
            &proj,
        )))
        .unwrap();
        assert_eq!(
            read_project(root.path(), &proj.to_string_lossy()).status,
            ProjectStatus::SetupIncomplete
        );

        record_hooks_installed(&proj);
        assert_eq!(
            read_project(root.path(), &proj.to_string_lossy()).status,
            ProjectStatus::PickingUp
        );
    }

    /// Initialized means **this machine holds the family's projection AND
    /// installed its hooks** — never that `.rift/` exists, and never the
    /// projection alone. A clone whose `.rift/config.toml` travelled
    /// with the repository reads as configured elsewhere: its committed
    /// values are shown read-only, the wizard is offered, and detected
    /// supported agents are pre-ticked for it, exactly as on a fresh
    /// repository. A plain directory outside any repository has no family
    /// to provision and is offered no wizard.
    #[test]
    fn a_committed_rift_dir_without_a_projection_is_not_set_up_here() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(repo.path()).unwrap();
        std::fs::create_dir_all(repo.path().join(".rift")).unwrap();
        std::fs::write(
            repo.path().join(".rift/config.toml"),
            "schema_version = 1\n[project]\nname = \"demo\"\n[features]\ntotal_recall = false\n",
        )
        .unwrap();

        let view = project_settings_view_under(root.path(), repo.path());
        assert_eq!(view.initialization, ProjectInitialization::NotProvisioned);
        assert!(view.initialization.offers_setup());
        assert!(
            view.settings.iter().all(|row| !row.actionable),
            "committed values are facts, not toggles: setup never rewrites the shared file"
        );
        assert!(
            !view.settings[0].enabled,
            "the committed Recall value is shown, not the setup default"
        );
        assert_eq!(
            read_project(root.path(), &repo.path().to_string_lossy()).status,
            ProjectStatus::NotProvisioned
        );

        // A projection is not provisioning. An agent hook creates one on
        // first contact, so a family whose hooks were never installed can
        // carry a projection and nothing else.
        let family = project_family(repo.path()).unwrap();
        let db = projection_db_for_family_under(root.path(), &family);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"").unwrap();
        let view = project_settings_view_under(root.path(), repo.path());
        assert_eq!(
            view.initialization,
            ProjectInitialization::SetupIncomplete,
            "a projection with no recorded hook install is setup left half done"
        );
        assert!(
            !view.settings[0].enabled,
            "and the committed Recall value is still the one shown"
        );

        // The recorded hook install is the other half.
        record_hooks_installed(repo.path());
        assert_eq!(
            project_settings_view_under(root.path(), repo.path()).initialization,
            ProjectInitialization::Initialized
        );

        // Outside any repository there is nothing to provision.
        let plain = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(plain.path().join(".rift")).unwrap();
        let view = project_settings_view_under(root.path(), plain.path());
        assert_eq!(view.initialization, ProjectInitialization::NotRepository);
        assert!(!view.initialization.offers_setup());
        assert!(view.integrations.iter().all(|row| !row.selected));
        assert_eq!(
            read_project(root.path(), &plain.path().to_string_lossy()).status,
            ProjectStatus::NotRepository
        );
    }

    #[test]
    fn settings_report_custom_injection_and_safe_policy_counts() {
        let root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(repo.path()).unwrap();
        let family = project_family(repo.path()).unwrap();
        let db = projection_db_for_family_under(root.path(), &family);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"").unwrap();
        record_hooks_installed(repo.path());
        std::fs::create_dir_all(repo.path().join(".rift/policies")).unwrap();
        std::fs::write(
            repo.path().join(".rift/config.toml"),
            "schema_version = 1\n[project]\nname = \"demo\"\n[features]\ntotal_recall = true\n[capture]\ntool_calls = false\ntoken_usage = true\nraw_transcripts = false\nacknowledge_risk = false\n",
        )
        .unwrap();
        std::fs::write(
            repo.path().join(".rift/policies/serve.toml"),
            "injection_turn_budget = 777\n[doc]\nmode = \"observe\"\n[search]\nmode = \"off\"\n",
        )
        .unwrap();
        let view = project_settings_view_under(root.path(), repo.path());
        assert_eq!(view.initialization, ProjectInitialization::Initialized);
        assert_eq!(view.policy.state, ProjectPolicyState::Declared);
        assert!(view.policy.sensitive_path_count >= 9);
        let row = |id| view.settings.iter().find(|row| row.id == id).unwrap();
        let injection = row(ProjectSettingId::TurnInjection);
        assert!(injection.enabled);
        assert_eq!(
            injection.detail.as_deref(),
            Some("custom effective budget: 777")
        );
        // The two lever rows read the tables: `observe` is on with its
        // detail, so a toggle never silently destroys it; `off` is off.
        let documents = row(ProjectSettingId::Documents);
        assert!(documents.enabled);
        assert_eq!(
            documents.detail.as_deref(),
            Some("custom effective mode: observe")
        );
        let search = row(ProjectSettingId::Search);
        assert!(!search.enabled);
        assert_eq!(search.detail, None);
        assert!(!row(ProjectSettingId::ToolCallCapture).enabled);
    }

    #[test]
    fn invalid_policy_is_truthful_and_locks_actions() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".rift")).unwrap();
        std::fs::write(repo.path().join(".rift/config.toml"), "not = [valid").unwrap();
        let view = project_settings_view(repo.path());
        assert_eq!(view.policy.state, ProjectPolicyState::Invalid);
        assert!(view.settings.iter().all(|row| !row.actionable));
        assert!(view
            .policy
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("repair")));
    }

    #[test]
    fn a_pending_setup_plan_owns_the_distinct_project_status() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join(".rift/state")).unwrap();
        std::fs::write(repo.join(".rift/state/project-setup.json"), "{}\n").unwrap();
        assert_eq!(
            read_project(root.path(), &repo.to_string_lossy()).status,
            ProjectStatus::SetupIncomplete
        );
        assert_eq!(
            project_settings_view(&repo).initialization,
            ProjectInitialization::SetupIncomplete
        );
    }

    #[test]
    fn a_pending_setup_plan_restores_revised_choices_for_resume() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".rift/state")).unwrap();
        std::fs::write(
            repo.path().join(".rift/state/project-setup.json"),
            r#"{
  "schema_version": 1,
  "total_recall": false,
  "turn_injection": false,
  "documents": false,
  "tool_call_capture": false,
  "token_usage_capture": true,
  "raw_transcripts": true,
  "integrations": ["ollama"]
}"#,
        )
        .unwrap();
        let view = project_settings_view(repo.path());
        assert_eq!(view.initialization, ProjectInitialization::SetupIncomplete);
        // Row order is `ProjectSettingId::ALL`. A key the plan does not carry
        // — `search` here, as in every plan written before the lever rows —
        // keeps the row's own default, which is on.
        assert_eq!(
            view.settings
                .iter()
                .map(|row| row.enabled)
                .collect::<Vec<_>>(),
            vec![false, false, false, true, false, true, true]
        );
        assert_eq!(
            view.integrations
                .iter()
                .filter(|row| row.selected)
                .map(|row| row.agent.as_str())
                .collect::<Vec<_>>(),
            vec!["ollama"]
        );
    }
}
