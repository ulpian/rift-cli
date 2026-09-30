//! Test fixtures the bus modules share: a project with a seeded family
//! projection, the rows appended to it, a two-member family with its session
//! caches, a bus spawned over a tempdir with a watcher that wakes nothing,
//! and the waits that read the worker's answers.
//!
//! Every wait here ends on something the worker does, never on the wall
//! clock: a barrier request the worker answers after everything queued
//! before it, or a count of the worker's own cycles. A worker starved under
//! load is slow, not wrong, so a fixed deadline was the flake; a worker that
//! dies drops its senders, so a blocking receive fails instead of hanging.
//!
//! A separate module because the status, the observer, the settings view and
//! the worker loop all read the same seeded repository, and a fixture copied
//! per module drifts.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvError, Sender};

use rift_canopy::{DataRequest, Event};
// The legacy per-path derivations as the seeding oracle: a main work tree's
// family adopts exactly these ids (family identity).
use rift_git::repo_id_for;
use rift_store::{projection_db_under, CanonicalStore, SqliteStore, Value};

use crate::feeds::project_family;

use super::wake::WakeWatcher;
use super::{spawn_with_watcher, Bus, WorkerMsg};

pub(super) const EVENT_COLS: [&str; 9] = [
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

/// How many worker cycles a poll-driven change needs to reach the channel,
/// counted from the first cycle report read after the change was made.
///
/// That report's cycle may have polled before the change; the cycle after it
/// polls after the change and flushes what it saw; the third report is sent
/// only once that flush is done. Every cycle of an idle worker is a poll, so
/// three is exact, not slack.
const POLL_CYCLES: usize = 3;

/// Makes each barrier's path unique: a rebuild re-reads every tracked path,
/// barriers included, so a reused path could answer a later barrier early.
static BARRIERS: AtomicUsize = AtomicUsize::new(0);

// --- wake-only notify + the bounded latest-per-feed channel -------------

/// A watcher that installs nothing and wakes nothing: unit tests exercise
/// the poll path deterministically, with no platform watcher (many
/// concurrent FSEvents streams stall on macOS — the real watcher is
/// integration-tested in tests/live_sections.rs).
pub(super) struct NoopWatcher;

impl WakeWatcher for NoopWatcher {
    fn watch(&mut self, _root: &Path, _wake: Sender<WorkerMsg>) -> Result<(), String> {
        Ok(())
    }
}

/// A watcher that reports every cycle of the worker: the watch set it is
/// handed each cycle goes down `cycles`, and the wake sender it is given at
/// start goes down `wakes`. The worker re-asserts the session-dir watch once
/// per cycle, which is what makes the watch set a cycle clock.
struct ProbeWatcher {
    cycles: Sender<Vec<PathBuf>>,
    wakes: Sender<Sender<WorkerMsg>>,
    /// Whether `watch` succeeds; a failing probe is the injected poll-only
    /// degrade.
    native: bool,
}

impl WakeWatcher for ProbeWatcher {
    fn watch(&mut self, _root: &Path, wake: Sender<WorkerMsg>) -> Result<(), String> {
        if !self.native {
            return Err("no watch backend on this filesystem".into());
        }
        let _ = self.wakes.send(wake);
        Ok(())
    }

    fn set_active_session_dirs(&mut self, dirs: &[PathBuf]) {
        let _ = self.cycles.send(dirs.to_vec());
    }
}

/// The test's side of a [`ProbeWatcher`].
pub(super) struct Probe {
    cycles: Receiver<Vec<PathBuf>>,
    wakes: Receiver<Sender<WorkerMsg>>,
}

impl Probe {
    /// The worker's wake sender, handed over when it installed the watcher.
    pub(super) fn wake_sender(&self) -> Sender<WorkerMsg> {
        self.wakes
            .recv()
            .expect("the worker stopped before installing its watcher")
    }

    /// Append every watch set the worker asked for up to the cycle that
    /// answers a fresh barrier. The watch is set before the cycle's reads, so
    /// the barrier's answer proves its cycle's set is already recorded.
    pub(super) fn record_watch_sets(
        &self,
        bus: &Bus,
        root: &Path,
        recorded: &mut Vec<Vec<PathBuf>>,
    ) {
        events_until_barrier(bus, root);
        recorded.extend(self.cycles.try_iter());
    }

    /// Make `change`, then wait for the event a poll of it produces, bounded
    /// by [`POLL_CYCLES`] of the worker's own cycles rather than by a
    /// deadline. For changes only the observer's poll can see: a request or a
    /// wake would answer them without polling.
    pub(super) fn poll_for(
        &self,
        bus: &Bus,
        change: impl FnOnce(),
        what: &str,
        want: impl Fn(&Event) -> bool,
    ) -> Event {
        change();
        // Reports already queued are cycles that may have polled before the
        // change; only the ones sent from here on are counted.
        let _ = self.cycles.try_iter().count();
        let mut seen = Vec::new();
        for _ in 0..POLL_CYCLES {
            assert!(
                self.cycles.recv().is_ok(),
                "the worker stopped before {what}; it emitted {seen:?}"
            );
            for event in bus.evt_rx.try_iter() {
                if want(&event) {
                    return event;
                }
                seen.push(event);
            }
        }
        panic!("{what} within {POLL_CYCLES} worker cycles; it emitted {seen:?}")
    }
}

pub(super) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

/// Insert captured-event rows `start..start+n` into an open projection.
pub(super) fn insert_events(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    start: usize,
    n: usize,
) {
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
pub(super) fn make_project(root: &Path, name: &str, events: usize) -> PathBuf {
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
pub(super) fn provision(work_tree: &Path) {
    let state = work_tree.join(".rift/state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("native-integrations.json"), "{}").unwrap();
}

/// Where `repo`'s family keeps its manifest and its binary of record.
pub(super) fn family_dir_of(repo: &Path) -> PathBuf {
    let family = project_family(repo).unwrap();
    rift_git::family::family_dir_under(|k| std::env::var(k).ok(), &family.common_dir)
}

/// Record a live binary of record for `repo`'s family: the note the git
/// hook install leaves behind, and the second half of "set up here". The
/// path recorded is this test binary, which is an executable that exists.
pub(super) fn record_hooks_installed(repo: &Path) {
    rift_shadow_hooks::record_binary_of_record(
        &family_dir_of(repo),
        &std::env::current_exe().unwrap(),
        rift_shadow_hooks::RecordedBy::Init,
    )
    .unwrap();
}

/// Write a live session cache under `git_dir`, claiming `work_tree` — the
/// shape the capture hooks write, discriminator included.
pub(super) fn live_session(git_dir: &Path, id: &str, work_tree: &Path, updated_at_ms: i64) {
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
pub(super) fn append_events(root: &Path, proj: &Path, start: usize, n: usize) {
    let repo = repo_id_for(proj).unwrap();
    let db = projection_db_under(root, &repo);
    let store = SqliteStore::open(&db).unwrap();
    insert_events(&store, &runtime(), start, n);
}

pub(super) fn path_of(proj: &Path) -> String {
    proj.to_string_lossy().into_owned()
}

/// The next ProjectData update the worker emitted before answering a
/// barrier, skipping feed, health and mode updates (SetActive emits several
/// kinds). The barrier consumes whatever else that cycle emitted.
pub(super) fn recv_project_data(bus: &Bus, root: &Path) -> Event {
    let seen = events_until_barrier(bus, root);
    let first = seen.iter().position(|event| {
        !matches!(
            event,
            Event::Feed(_) | Event::Health(_) | Event::WatchMode(_)
        )
    });
    match first {
        Some(at) => seen.into_iter().nth(at).unwrap(),
        None => panic!("no ProjectData update before the barrier: {seen:?}"),
    }
}

/// Send a barrier and return every event the worker emitted before answering
/// it.
///
/// The barrier is a refresh of a path no project uses. Requests reach the
/// worker in order, so everything asked before it is read in its cycle or an
/// earlier one; within one flush updates leave sorted by project path, and
/// `~` sorts after every fixture project's name, so the barrier's answer is
/// the last thing its cycle sends. A worker that is alive always answers a
/// refresh, so the wait ends without a clock; one that died fails it.
pub(super) fn events_until_barrier(bus: &Bus, root: &Path) -> Vec<Event> {
    let n = BARRIERS.fetch_add(1, Ordering::Relaxed);
    let barrier = path_of(&root.join(format!("~barrier-{n}")));
    bus.req_tx
        .send(DataRequest::Refresh(barrier.clone()))
        .expect("the worker stopped before the barrier was sent");
    let mut seen = Vec::new();
    loop {
        match bus.evt_rx.recv() {
            Ok(Event::ProjectData(data)) if data.project == barrier => return seen,
            Ok(event) => seen.push(event),
            Err(RecvError) => {
                panic!("the worker stopped before answering the barrier; it emitted {seen:?}")
            }
        }
    }
}

// --- the family view: member status and family aggregation ------

/// A two-member family fixture: the main work tree (with a seeded family
/// projection) and one linked worktree, returned as `(main, member)`.
///
/// The main checkout is deliberately NOT called `main`: `member_label`
/// must name it by its role, not by whatever the folder happens to be
/// called, or the family header would read differently in every clone.
pub(super) fn two_member_family(root: &Path, events: usize) -> (PathBuf, PathBuf) {
    let main = make_project(root, "trunk", events);
    let member = root.join("member");
    rift_git::test_support::fake_worktree(&main.join(".git"), &member, "member").unwrap();
    (main, member)
}

/// Test spawn: the bus over a tempdir root with the no-op watcher.
pub(super) fn spawn_for_test(root: PathBuf, initial: Vec<String>) -> Bus {
    spawn_with_watcher(root, initial, Box::new(NoopWatcher))
}

/// Test spawn with a [`Probe`] on a watcher that installs.
pub(super) fn spawn_probed(root: PathBuf, initial: Vec<String>) -> (Bus, Probe) {
    spawn_with_probe(root, initial, true)
}

/// Test spawn with a [`Probe`] on a watcher that fails to install, so the
/// bus degrades to poll-only.
pub(super) fn spawn_probed_poll_only(root: PathBuf, initial: Vec<String>) -> (Bus, Probe) {
    spawn_with_probe(root, initial, false)
}

fn spawn_with_probe(root: PathBuf, initial: Vec<String>, native: bool) -> (Bus, Probe) {
    let (cycles_tx, cycles) = channel();
    let (wakes_tx, wakes) = channel();
    let watcher = ProbeWatcher {
        cycles: cycles_tx,
        wakes: wakes_tx,
        native,
    };
    let bus = spawn_with_watcher(root, initial, Box::new(watcher));
    (bus, Probe { cycles, wakes })
}
