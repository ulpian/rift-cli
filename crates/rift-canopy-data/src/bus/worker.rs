//! The worker loop: one read cycle, top to bottom — drain the request burst,
//! poll the change observer, re-read what changed, stage the feeds, flush.
//!
//! A separate module because the loop is where the request grammar is decided
//! (`collect`) and where the feeds are recomputed (`stage_read`); the reads it
//! calls live beside their own tests, and the loop's tests drive it through
//! an injected watcher with no filesystem timing.

use std::collections::BTreeSet;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::time::Duration;

use rift_canopy::{
    DataRequest, Event, HealthEvent, ProjectData, RecallWindow, ViewerRequest, WatchMode,
};

use crate::feeds;
use crate::store::runtime;
use crate::viewer::ViewerWorker;
use crate::workflows::compute_workflow_cards;

use super::outbox::Outbox;
use super::project_read::{build_detail, read_project};
use super::wake::WakeWatcher;
use super::watermark::{ChangeKind, WatermarkPoller};
use super::{now_ms, WorkerMsg};

/// Everything the worker carries from one cycle to the next.
struct WorkerState {
    /// The cache root the projections live under.
    root: PathBuf,
    observer: WatermarkPoller,
    outbox: Outbox,
    subscription: Subscription,
    /// The active project's family's session-cache dirs — the narrow second
    /// watch target (widened to the whole family by the family view).
    /// Re-enumerated when the active project changes or its projection is
    /// rebuilt (a rebuild is also how a `git worktree add` since the last read
    /// becomes visible); re-asserted every cycle (a cheap no-op once attached)
    /// so a dir created after SetActive still gets its watch.
    session_dirs: Vec<PathBuf>,
    /// The last health report sent: the dot rides the reads the worker
    /// already performs — no new polling — and only a *change* stages an
    /// event, so a steady data plane adds zero traffic.
    last_health: Option<HealthEvent>,
    viewer_worker: ViewerWorker,
}

/// What the loop is subscribed to, across cycles: the foreground project — the
/// only one the expensive rich detail and the column-3 feeds are computed for
/// (status is cheap and read for every project) — and the period the Recall
/// feed is computed over.
///
/// Worker state, not a per-request parameter: every wake recomputes the feeds,
/// so a window living only as long as one request would snap back to the
/// default the next time the filesystem twitched.
#[derive(Default)]
struct Subscription {
    active: Option<String>,
    recall_window: RecallWindow,
}

/// One cycle's coalesced work: the burst of messages folded by set-union, so a
/// storm of requests and wakes collapses to one read per project.
#[derive(Default)]
struct Batch {
    /// The projects to (re-)read this cycle.
    to_read: BTreeSet<String>,
    /// The viewer's requests, latest per (kind, project).
    viewer_requests: Vec<ViewerRequest>,
    /// A filesystem event fired: poll, and recompute the active project.
    woke_by_fs: bool,
    /// The poll interval elapsed with no message: poll.
    timed_out: bool,
}

/// What one read cycle observed about the worker's own infrastructure —
/// folded into the header dot's [`HealthEvent`] (the dashboard information architecture).
#[derive(Default)]
pub(super) struct HealthProbe {
    /// The machine-global canopy store failed to open this cycle.
    pub(super) store_error: bool,
    /// The worker's async runtime could not be created.
    pub(super) runtime_error: bool,
}

impl WorkerState {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            observer: WatermarkPoller::new(root.to_path_buf()),
            outbox: Outbox::default(),
            subscription: Subscription::default(),
            session_dirs: Vec::new(),
            last_health: None,
            viewer_worker: ViewerWorker::new(root.to_path_buf()),
        }
    }

    /// Wait for the first message or the observer's poll interval, whichever
    /// comes first, then coalesce the burst behind it so a storm of requests
    /// and wakes collapses to one read per project (backpressure by set-union,
    /// not queue growth). `None` when the bus is shutting down: an explicit
    /// `Shutdown`, or the request side gone.
    fn drain(&mut self, msg_rx: &Receiver<WorkerMsg>) -> Option<Batch> {
        let mut batch = Batch::default();
        // A filter scan in flight is advanced before any wait, one segment
        // per cycle; a timeout while one is pending is not an idle tick.
        let filter_pending = self.viewer_worker.has_filter_work();
        let wait = if filter_pending {
            Duration::ZERO
        } else {
            self.observer.interval()
        };
        match msg_rx.recv_timeout(wait) {
            Ok(WorkerMsg::Shutdown) | Err(RecvTimeoutError::Disconnected) => return None,
            Ok(first) => {
                collect(first, &mut batch, &mut self.subscription);
                while let Ok(msg) = msg_rx.try_recv() {
                    if matches!(msg, WorkerMsg::Shutdown) {
                        return None;
                    }
                    collect(msg, &mut batch, &mut self.subscription);
                }
            }
            Err(RecvTimeoutError::Timeout) => batch.timed_out = !filter_pending,
        }
        Some(batch)
    }

    /// A wake or a timeout schedules the SAME fresh poll — the watermark read
    /// is the sole truth, so a spurious wake reads as quiet. Every observed
    /// change re-reads that project; a rebuild replaced the projection
    /// wholesale, so it re-bootstraps every tracked project rather than
    /// trusting any cached sibling. Returns whether a rebuild was observed.
    fn poll_changes(&mut self, batch: &mut Batch) -> bool {
        if !(batch.woke_by_fs || batch.timed_out) {
            return false;
        }
        let mut rebuilt = false;
        for change in self.observer.poll_once() {
            if change.kind == ChangeKind::Rebuilt {
                rebuilt = true;
                batch.to_read.extend(self.observer.tracked());
            }
            batch.to_read.insert(change.project);
        }
        rebuilt
    }

    /// Point the narrow session-dir watch at the active project's family.
    fn refresh_session_watch(&mut self, watcher: &mut dyn WakeWatcher, changed: bool) {
        if changed {
            self.session_dirs = self
                .subscription
                .active
                .as_deref()
                .map(session_dirs_for)
                .unwrap_or_default();
        }
        watcher.set_active_session_dirs(&self.session_dirs);
    }

    /// Read every scheduled project and stage its update. A cycle that read
    /// anything also folds what it saw of the plane's own health into the
    /// header dot — staged only on change, so a steady plane adds no traffic.
    fn read_projects(&mut self, to_read: BTreeSet<String>) {
        if to_read.is_empty() {
            return;
        }
        let mut probe = HealthProbe::default();
        for path in to_read {
            self.stage_read(path, &mut probe);
        }
        let health = probe.report();
        if self.last_health != Some(health) {
            self.last_health = Some(health);
            self.outbox.stage(Event::Health(health));
        }
    }

    /// Read a project's status + watermark once, record the watermark as the
    /// observer's polling baseline (so the next poll only fires on genuinely
    /// newer changes), and stage the update in the outbox. The rich detail is
    /// attached only when `path` is the active (foreground) project — building
    /// it reads several tables, so it stays scoped to the one project the
    /// user is looking at.
    fn stage_read(&mut self, path: String, probe: &mut HealthProbe) {
        probe.runtime_error |= runtime().is_err();
        let read = read_project(&self.root, &path);
        self.observer.sync(&path, read.mark);
        let is_active = self.subscription.active.as_deref() == Some(path.as_str());
        let detail = if is_active {
            Some(build_detail(
                &self.root,
                &path,
                read.status,
                read.running_members,
            ))
        } else {
            None
        };
        self.outbox.stage(Event::ProjectData(ProjectData {
            project: path.clone(),
            status: read.status,
            detail,
        }));
        if is_active {
            self.stage_active_feeds(&path);
        }
    }

    /// The column-3 feeds follow the active project (SetActive is the
    /// subscribe request): recompute full section state on every read of it —
    /// a Rebuilt-driven read is thereby the re-bootstrap, and the reducer's
    /// periodic re-stat keeps recall.db-only changes fresh too.
    fn stage_active_feeds(&mut self, path: &str) {
        // ONE active-sessions read per recomputation: the badge, context, and
        // recent-activity scope all derive from the same native identities.
        let now = now_ms();
        let sessions = feeds::read_active_sessions(path, now);
        self.outbox
            .stage(Event::Feed(feeds::compute_tokens(&self.root, path)));
        self.outbox.stage(Event::Feed(feeds::compute_system(
            &self.root, path, &sessions,
        )));
        self.outbox.stage(Event::Feed(feeds::compute_recall(
            &self.root,
            path,
            now,
            self.subscription.recall_window,
        )));
        // The activity card stays on the injected clock (calendar-day
        // bucketing); System activity is event-anchored and never uses `now`.
        self.outbox
            .stage(Event::Feed(feeds::compute_activity(&self.root, path, now)));
        // The two workflow cards come from ONE gather of the unbounded window.
        // They read no clock at all: a card whose numbers depend on when it
        // rendered cannot be compared with the `rift dispatch` the user runs
        // beside it.
        let (dispatch, recap) = compute_workflow_cards(&self.root, path);
        self.outbox.stage(Event::Feed(dispatch));
        self.outbox.stage(Event::Feed(recap));
    }

    /// Answer the viewer's requests and advance any filter scan in flight.
    fn serve_viewer(&mut self, requests: Vec<ViewerRequest>) {
        for request in requests {
            for event in self.viewer_worker.handle(request) {
                self.outbox.stage(event);
            }
        }
        for event in self.viewer_worker.advance_filter_scans() {
            self.outbox.stage(event);
        }
    }
}

impl HealthProbe {
    /// Fold the cycle's observations: a dead store outranks a worker error.
    pub(super) fn report(&self) -> HealthEvent {
        if self.store_error {
            HealthEvent::StoreUnreachable
        } else if self.runtime_error {
            HealthEvent::WorkerError
        } else {
            HealthEvent::Healthy
        }
    }
}

/// The worker loop. It waits for a message **or** the observer's poll interval,
/// whichever comes first:
/// * a request → coalesce the burst (latest-per-project), read each distinct
///   project's status, and stage a [`ProjectData`];
/// * a wake (filesystem event) → run the same watermark poll a timeout would —
///   the poll is the only truth, the wake only made it prompt — **and**
///   re-read the ACTIVE project even when no watermark moved: recall.db and
///   the session caches change without the projection cursor, and the
///   event-anchored recompute is idempotent, so an unfounded wake converges on
///   identical output the reducer's dirty gate then drops;
/// * a timeout → poll every tracked project's watermark and re-read the ones
///   that changed (a rebuild additionally re-bootstraps *all* tracked
///   projects). Idle timeouts never force a recompute — the idle cost stays
///   one cheap watermark read.
///
/// Staged updates flush through the bounded channel latest-per-(project, feed)
/// ([`Outbox`]). Exits when either channel disconnects (the dashboard quit).
pub(super) fn worker(
    msg_rx: &Receiver<WorkerMsg>,
    wake_tx: Sender<WorkerMsg>,
    evt_tx: &SyncSender<Event>,
    root: &Path,
    mut watcher: Box<dyn WakeWatcher>,
) {
    // Native-notify capability is best-effort: watching the cache root makes
    // updates prompt; failure means PollOnly and the cadence is the heartbeat.
    let mode = match watcher.watch(root, wake_tx) {
        Ok(()) => WatchMode::Native,
        Err(_) => WatchMode::PollOnly,
    };
    // Report the wake mode ONCE, first: the dashboard adapts its heartbeat to
    // it and the System panel says it, so the poll-only degrade is never
    // silent. Best-effort, never blocking — a full channel at start is
    // impossible (nothing has been sent yet) and must not stall the worker
    // anyway.
    let _ = evt_tx.try_send(Event::WatchMode(mode));
    let mut state = WorkerState::new(root);
    loop {
        let active_before = state.subscription.active.clone();
        let Some(mut batch) = state.drain(msg_rx) else {
            return;
        };
        let rebuilt = state.poll_changes(&mut batch);
        let active_changed = state.subscription.active != active_before;
        state.refresh_session_watch(watcher.as_mut(), active_changed || rebuilt);
        if batch.woke_by_fs {
            // Filesystem activity → recompute the active project's feeds even
            // without a cursor move: this is how recall.db serves and
            // session-cache writes reach the panel (the poll stays the only
            // truth about the *projection*; the recompute is a fresh read).
            if let Some(active) = state.subscription.active.clone() {
                batch.to_read.insert(active);
            }
        }
        state.read_projects(batch.to_read);
        state.serve_viewer(batch.viewer_requests);
        if state.outbox.flush(evt_tx).is_err() {
            return; // the loop is gone; end the worker
        }
    }
}

/// The standalone Recall Viewer's loop: no observer, no watcher, no project
/// reads — only viewer requests, answered through the same outbox.
pub(super) fn viewer_only_worker(
    msg_rx: &Receiver<WorkerMsg>,
    evt_tx: &SyncSender<Event>,
    root: &Path,
) {
    let mut viewer = ViewerWorker::new(root.to_path_buf());
    let mut outbox = Outbox::default();
    loop {
        let Some(requests) = drain_viewer_requests(msg_rx, viewer.has_filter_work()) else {
            return;
        };
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

/// Fold a worker message into the cycle's batch, updating the subscription.
/// `SetActive(None)` clears the active project; a [`WorkerMsg::Wake`] requests
/// a poll **and** (in the worker) the active project's recompute — never any
/// data of its own.
fn collect(msg: WorkerMsg, batch: &mut Batch, subscription: &mut Subscription) {
    match msg {
        WorkerMsg::Request(DataRequest::Refresh(project)) => {
            batch.to_read.insert(project);
        }
        WorkerMsg::Request(DataRequest::SetActive(Some(project))) => {
            subscription.active = Some(project.clone());
            batch.to_read.insert(project);
        }
        WorkerMsg::Request(DataRequest::SetActive(None)) => subscription.active = None,
        // Adopt the window and re-read the project it applies to. Coalescing
        // is by set-union like every other request here, so pressing four
        // buttons in one frame costs one read at the last window pressed.
        WorkerMsg::Request(DataRequest::SetRecallWindow(window)) => {
            subscription.recall_window = window;
            if let Some(active) = subscription.active.clone() {
                batch.to_read.insert(active);
            }
        }
        WorkerMsg::Request(DataRequest::Viewer(request)) => {
            coalesce_viewer_request(&mut batch.viewer_requests, request);
        }
        WorkerMsg::Wake => batch.woke_by_fs = true,
        WorkerMsg::Shutdown => (),
    }
}

/// Keep the latest request per (kind, project): a viewer scrolling or typing
/// faster than the worker reads is served its newest position, not every one
/// it passed through.
fn coalesce_viewer_request(requests: &mut Vec<ViewerRequest>, request: ViewerRequest) {
    let same_slot = |existing: &ViewerRequest| match (existing, &request) {
        (
            ViewerRequest::Catalogue { project: a, .. },
            ViewerRequest::Catalogue { project: b, .. },
        )
        | (
            ViewerRequest::VisibleExcerpts { project: a, .. },
            ViewerRequest::VisibleExcerpts { project: b, .. },
        )
        | (ViewerRequest::Detail { project: a, .. }, ViewerRequest::Detail { project: b, .. })
        | (
            ViewerRequest::FilterScan { project: a, .. },
            ViewerRequest::FilterScan { project: b, .. },
        ) => a == b,
        _ => false,
    };
    match requests.iter_mut().find(|existing| same_slot(existing)) {
        Some(slot) => *slot = request,
        None => requests.push(request),
    }
}

/// The viewer worker's burst: block for the first message unless a filter
/// scan is waiting to advance, then coalesce whatever else is queued. `None`
/// when the bus is shutting down.
fn drain_viewer_requests(
    msg_rx: &Receiver<WorkerMsg>,
    filter_pending: bool,
) -> Option<Vec<ViewerRequest>> {
    let mut requests = Vec::new();
    let first = if filter_pending {
        match msg_rx.try_recv() {
            Ok(message) => Some(message),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => return None,
        }
    } else {
        Some(msg_rx.recv().ok()?)
    };
    if let Some(first) = first {
        if absorb_viewer_message(first, &mut requests).is_break() {
            return None;
        }
    }
    while let Ok(message) = msg_rx.try_recv() {
        if absorb_viewer_message(message, &mut requests).is_break() {
            return None;
        }
    }
    Some(requests)
}

/// Fold one message into the viewer's request burst; a shutdown breaks the loop.
fn absorb_viewer_message(message: WorkerMsg, requests: &mut Vec<ViewerRequest>) -> ControlFlow<()> {
    match message {
        WorkerMsg::Shutdown => ControlFlow::Break(()),
        WorkerMsg::Request(DataRequest::Viewer(request)) => {
            coalesce_viewer_request(requests, request);
            ControlFlow::Continue(())
        }
        // The viewer plane draws no Recall panel, so a window request is one
        // more dashboard request it ignores. Listed by name rather than swept
        // up by a wildcard: this match is what makes "which requests does the
        // read-only worker answer?" a compile-time question.
        WorkerMsg::Request(
            DataRequest::Refresh(_) | DataRequest::SetActive(_) | DataRequest::SetRecallWindow(_),
        )
        | WorkerMsg::Wake => ControlFlow::Continue(()),
    }
}

/// Every session-cache dir of the active project's **family** (each member's
/// sessions directory, [`crate::session::member_sessions`]) — the narrow
/// second watch set
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
        .map(|m| crate::session::member_sessions(&m.git_dir, &m.work_tree))
        // An unmoved member is watched in both trees: a new hook writes the
        // new one while an older binary still writes the old one.
        .flat_map(|dir| rift_shadow_hooks::run_state::read_dirs(&dir))
        .collect()
}

#[cfg(test)]
mod tests {
    use rift_canopy::ProjectStatus;
    use rift_store::{CanonicalStore, Value};

    use super::super::fixtures::{
        append_events, events_until_barrier, make_project, path_of, recv_project_data, runtime,
        spawn_for_test, spawn_probed, spawn_probed_poll_only, two_member_family, Probe,
    };
    use super::super::Bus;
    use super::*;

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
    fn active_project_gets_detail_others_do_not() {
        // The bus attaches rich detail only for the SetActive (foreground) project.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        let bus = spawn_for_test(root.path().to_path_buf(), vec![]);
        bus.req_tx
            .send(DataRequest::SetActive(Some(path_of(&proj))))
            .unwrap();
        let ev = recv_project_data(&bus, root.path());
        match ev {
            Event::ProjectData(pd) => {
                assert_eq!(pd.project, path_of(&proj));
                assert!(pd.detail.is_some(), "the active project carries detail");
            }
            other => panic!("expected ProjectData, got {other:?}"),
        }
    }

    #[test]
    fn wake_triggers_a_fresh_poll_and_spurious_wakes_are_harmless() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        let (bus, probe) = spawn_probed(root.path().to_path_buf(), vec![path_of(&proj)]);
        // Drain the seeded update; the observer baseline is now synced.
        let first = recv_project_data(&bus, root.path());
        assert!(matches!(first, Event::ProjectData(_)));
        let wake = probe.wake_sender();

        // Spurious + duplicated wakes with no underlying change: the fresh
        // poll reads quiet, so nothing is emitted (wake-only, no data path).
        // The wakes are queued before the barrier, so the worker has handled
        // them by the time it answers it.
        wake.send(WorkerMsg::Wake).unwrap();
        wake.send(WorkerMsg::Wake).unwrap();
        let spurious = events_until_barrier(&bus, root.path());
        assert!(
            spurious.is_empty(),
            "a spurious wake must not fabricate an update: {spurious:?}"
        );

        // A real change + a wake: the update arrives in the wake's own cycle
        // — the wake scheduled the poll, the poll found it; no request asked.
        append_events(root.path(), &proj, 1, 2);
        wake.send(WorkerMsg::Wake).unwrap();
        let ev = recv_project_data(&bus, root.path());
        assert!(matches!(ev, Event::ProjectData(_)));
    }

    #[test]
    fn injected_watch_failure_degrades_to_poll_only() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        let (bus, probe) = spawn_probed_poll_only(root.path().to_path_buf(), vec![path_of(&proj)]);
        // The FIRST event says how the bus is woken: the degrade is reported,
        // never silent. It is sent before the loop starts, so nothing else
        // can come first.
        let mode = bus.evt_rx.recv().expect("the wake mode is reported first");
        assert_eq!(mode, Event::WatchMode(WatchMode::PollOnly));
        let first = recv_project_data(&bus, root.path());
        assert!(matches!(first, Event::ProjectData(_)));

        // No watcher — the adaptive poll cadence still surfaces the change.
        let ev = probe.poll_for(
            &bus,
            || append_events(root.path(), &proj, 1, 2),
            "poll-only still delivers",
            |ev| matches!(ev, Event::ProjectData(_)),
        );
        assert!(matches!(ev, Event::ProjectData(pd) if pd.project == path_of(&proj)));
    }

    // --- the rebuild-epoch rule amendment: the session-dir watch + wake-recompute-active --

    /// What a watch-set test reads: the bus, its probe (every
    /// `set_active_session_dirs` call — the wiring seam, no filesystem), the
    /// root barriers live under, and every watch set recorded so far.
    struct WatchLog<'a> {
        bus: &'a Bus,
        probe: &'a Probe,
        root: &'a Path,
        recorded: Vec<Vec<PathBuf>>,
    }

    /// The index of the first recorded watch set that satisfies `want`, once
    /// the worker has answered a barrier sent after the request under test —
    /// or fail with everything that was recorded, so a wiring bug reads as
    /// the set that WAS asked for.
    fn wait_for_watch_set(
        log: &mut WatchLog<'_>,
        what: &str,
        want: impl Fn(&[PathBuf]) -> bool,
    ) -> usize {
        log.probe
            .record_watch_sets(log.bus, log.root, &mut log.recorded);
        let found = log.recorded.iter().position(|set| want(set.as_slice()));
        let Some(at) = found else {
            panic!("{what}; recorded: {:?}", log.recorded);
        };
        at
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

        let (bus, probe) = spawn_probed(root.path().to_path_buf(), vec![]);
        let mut log = WatchLog {
            bus: &bus,
            probe: &probe,
            root: root.path(),
            recorded: Vec::new(),
        };

        bus.req_tx
            .send(DataRequest::SetActive(Some(path_of(&proj))))
            .unwrap();
        let attached_at = wait_for_watch_set(
            &mut log,
            "SetActive points the watch at the session dir",
            |set| set == [expected.clone()],
        );

        // Clearing the active project unwatches.
        bus.req_tx.send(DataRequest::SetActive(None)).unwrap();
        probe.record_watch_sets(&bus, root.path(), &mut log.recorded);
        let cleared = log.recorded.iter().skip(attached_at + 1).any(Vec::is_empty);
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

        let (bus, probe) = spawn_probed(root.path().to_path_buf(), vec![]);
        let mut log = WatchLog {
            bus: &bus,
            probe: &probe,
            root: root.path(),
            recorded: Vec::new(),
        };

        // Selecting the family row watches main AND the member.
        bus.req_tx
            .send(DataRequest::SetActive(Some(path_of(&main))))
            .unwrap();
        wait_for_watch_set(&mut log, "the family row watches every member", |set| {
            set.contains(&main_sessions) && set.contains(&member_sessions)
        });

        // And a member whose checkout is gone is not watched: its registry can
        // never gain a session, so the stream would never fire.
        assert!(
            !log.recorded.iter().any(|set| set.contains(&gone_sessions)),
            "a prunable member is never given a watch"
        );

        // Selecting the MEMBER row watches the same set: the family is the
        // unit, whichever of its checkouts the card is opened on. Only the
        // sets recorded after the member request count.
        log.recorded.clear();
        bus.req_tx
            .send(DataRequest::SetActive(Some(path_of(&member))))
            .unwrap();
        wait_for_watch_set(&mut log, "a member vantage watches the family too", |set| {
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
        let (bus, probe) = spawn_probed(root.path().to_path_buf(), vec![]);
        bus.req_tx
            .send(DataRequest::SetActive(Some(path_of(&proj))))
            .unwrap();

        // The SetActive recompute lands (any Feed) — baseline fully synced.
        // The barrier also drains the burst: nothing of that cycle follows
        // its answer.
        let primed = events_until_barrier(&bus, root.path())
            .iter()
            .any(|ev| matches!(ev, Event::Feed(_)));
        assert!(primed, "the SetActive recompute never arrived");

        // Recall-only change: serves land, no projection write of any kind.
        seed_recall_serves(root.path(), &proj, &[(1_000, 500)]);
        let wake = probe.wake_sender();
        wake.send(WorkerMsg::Wake).unwrap();

        // The wake alone (no cursor move) surfaces the served data, in the
        // wake's cycle — which the barrier queued behind it waits out.
        let saw_serves = events_until_barrier(&bus, root.path()).iter().any(|ev| {
            matches!(
                ev,
                Event::Feed(f) if matches!(
                    f.payload,
                    rift_canopy::FeedPayload::Recall {
                        savings: rift_canopy::RecallView::Data(_),
                        calls: rift_canopy::RecallView::Data(_),
                        ..
                    }
                )
            )
        });
        assert!(
            saw_serves,
            "an fs wake must recompute the active project's feeds even when \
             no projection watermark moved"
        );
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
        let mut batch = Batch::default();
        let mut subscription = Subscription::default();
        let mut feed = |msg| collect(msg, &mut batch, &mut subscription);
        feed(WorkerMsg::Request(DataRequest::SetActive(Some(
            "/r/p".to_owned(),
        ))));
        feed(WorkerMsg::Request(DataRequest::SetRecallWindow(
            RecallWindow::Last7Days,
        )));
        feed(WorkerMsg::Request(DataRequest::SetRecallWindow(
            RecallWindow::Today,
        )));
        assert_eq!(
            subscription.recall_window,
            RecallWindow::Today,
            "the last window pressed is the one that sticks"
        );
        assert_eq!(
            batch.to_read.len(),
            1,
            "three requests over one project coalesce into one read: {:?}",
            batch.to_read
        );
        assert!(batch.to_read.contains("/r/p"));
        assert!(
            !batch.woke_by_fs,
            "a window request is not a filesystem wake"
        );
    }

    /// With nothing selected there is nothing to recompute — the window is
    /// still adopted, so the next `SetActive` reads at the chosen period
    /// rather than at the default.
    #[test]
    fn a_window_request_with_no_active_project_schedules_no_read() {
        let mut batch = Batch::default();
        let mut subscription = Subscription::default();
        collect(
            WorkerMsg::Request(DataRequest::SetRecallWindow(RecallWindow::Last30Days)),
            &mut batch,
            &mut subscription,
        );
        assert_eq!(subscription.recall_window, RecallWindow::Last30Days);
        assert!(batch.to_read.is_empty());
        assert_eq!(subscription.active, None);
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
        // ProjectData (emit-on-change: the first report is the change). The
        // barrier's own read of a healthy plane adds none.
        let seeded = events_until_barrier(&bus, root.path());
        let healths: Vec<HealthEvent> = seeded.iter().filter_map(health_of).collect();
        assert_eq!(healths, vec![HealthEvent::Healthy]);

        // A second read of a healthy plane emits data but NO new health report.
        bus.req_tx
            .send(DataRequest::Refresh(path_of(&proj)))
            .unwrap();
        let second = events_until_barrier(&bus, root.path());
        let saw_second = second.iter().any(|ev| matches!(ev, Event::ProjectData(_)));
        let extra_health = second.iter().filter_map(health_of).count();
        assert!(saw_second, "the refresh delivered");
        assert_eq!(extra_health, 0, "an unchanged health is never re-sent");
    }

    #[test]
    fn bus_delivers_live_update_via_poll() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        let (bus, probe) = spawn_probed(root.path().to_path_buf(), vec![path_of(&proj)]);

        // Drain the initial request-driven update.
        let first = recv_project_data(&bus, root.path());
        assert!(matches!(first, Event::ProjectData(_)));

        // A concurrent append must surface via the observer's poll — with no
        // further request sent, any subsequent update is poll-driven.
        let second = probe.poll_for(
            &bus,
            || append_events(root.path(), &proj, 1, 2),
            "the append surfaces through the poll",
            |ev| matches!(ev, Event::ProjectData(_)),
        );
        match second {
            Event::ProjectData(pd) => {
                assert_eq!(pd.project, path_of(&proj));
                assert_eq!(pd.status, ProjectStatus::Idle);
            }
            other => panic!("expected ProjectData, got {other:?}"),
        }
    }

    fn health_of(ev: &Event) -> Option<HealthEvent> {
        match ev {
            Event::Health(health) => Some(*health),
            _ => None,
        }
    }
}
