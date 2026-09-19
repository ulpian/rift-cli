//! Test fixtures the bus modules share: a project with a seeded family
//! projection, the rows appended to it, a two-member family with its session
//! caches, and a bus spawned over a tempdir with a watcher that wakes nothing.
//!
//! A separate module because the status, the observer, the settings view and
//! the worker loop all read the same seeded repository, and a fixture copied
//! per module drifts.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use rift_canopy::Event;
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

/// Receive the next ProjectData update, skipping feed updates (SetActive
/// emits both kinds).
pub(super) fn recv_project_data(bus: &Bus, total: Duration) -> Event {
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
