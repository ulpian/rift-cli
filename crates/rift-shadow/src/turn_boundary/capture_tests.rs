//! What a boundary snapshot leaves out, and what it costs: Rift's own state
//! never enters the working set, a boundary with nothing to observe never
//! lists HEAD's tree, a path written after the boundary's instant is unknown
//! to it, and a commit made since the boundary does not hide what it saw.
//! Driven through the real [`capture_boundary`] over a real repository, like
//! the tests beside the function.

use std::cell::Cell;
use std::path::Path;
use std::process::Command;

use rift_core::Result;
use rift_git::{BlobReader, GixBlobReader, RepoPaths, TreeEntry};
use rift_store::{
    record_turn_boundary_file, record_turn_boundary_snapshot, turn_diff_hunks_for_session,
    SqliteStore, TurnBoundaryFileRow, TurnBoundarySnapshotRow,
};

use super::{capture_boundary, previous_state, Boundary};
use crate::ids;

const AGENT: &str = "claude-code";
const NATIVE: &str = "sess-own-state";
const STATE_FILE: &str = ".rift/state/injection/x.json";
const STATE_BYTES: &[u8] = b"{\"served\":[\"a.txt\"]}\n";
/// A boundary instant after every edit these fixtures make, fixed so no
/// verdict reads the clock: 2100-01-01.
const AFTER_THE_EDITS: i64 = 4_102_444_800_000;
/// The instant a race fixture's boundary falls at, and the file times either
/// side of it (2030-01-01, a second before and a second after).
const INSTANT: i64 = 1_893_456_000_000;
const BEFORE_INSTANT: i64 = INSTANT - 1_000;
const AFTER_INSTANT: i64 = INSTANT + 1_000;

/// The production reader, counting how often HEAD's whole tree is listed.
struct CountingReader {
    inner: GixBlobReader,
    tree_listings: Cell<u32>,
}

impl BlobReader for CountingReader {
    fn read_blob(&self, oid_hex: &str) -> Result<Option<Vec<u8>>> {
        self.inner.read_blob(oid_hex)
    }
    fn tree_entries_at(&self, commit_hex: &str) -> Result<Vec<TreeEntry>> {
        self.tree_listings.set(self.tree_listings.get() + 1);
        self.inner.tree_entries_at(commit_hex)
    }
    fn parent_of(&self, commit_hex: &str) -> Result<Option<String>> {
        self.inner.parent_of(commit_hex)
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A real repo with one commit (`a.txt`), its paths and its HEAD oid.
fn repo_with_a_commit(dir: &Path) -> (RepoPaths, String) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@example.invalid"]);
    git(dir, &["config", "user.name", "T"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("a.txt"), b"one\ntwo\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "seed"]);
    let head = git(dir, &["rev-parse", "HEAD"]);
    (rift_git::discover(dir).unwrap(), head)
}

/// Plant a file of Rift's own state under `.rift/state/`.
fn plant_state_file(dir: &Path) {
    let path = dir.join(STATE_FILE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, STATE_BYTES).unwrap();
}

fn reader(dir: &Path) -> CountingReader {
    CountingReader {
        inner: GixBlobReader::open(dir).unwrap(),
        tree_listings: Cell::new(0),
    }
}

fn store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    s.create_canonical_schema().unwrap();
    s
}

async fn capture(
    s: &SqliteStore,
    repo: &RepoPaths,
    reader: &CountingReader,
    head: &str,
    boundary: Boundary,
) {
    capture_boundary(
        s,
        repo,
        reader,
        Some(head),
        AGENT,
        NATIVE,
        0,
        boundary,
        AFTER_THE_EDITS,
    )
    .await
    .unwrap();
}

/// Every hunk row the session recorded, as `(edit_kind, path)`.
async fn hunk_paths(s: &SqliteStore) -> Vec<(String, String)> {
    let session = ids::session_row_id(AGENT, NATIVE);
    let rows = turn_diff_hunks_for_session(s, &session).await.unwrap();
    rows.into_iter().map(|r| (r.edit_kind, r.path)).collect()
}

/// The paths the session's latest boundary anchors.
async fn anchored_paths(s: &SqliteStore) -> Vec<String> {
    let session = ids::session_row_id(AGENT, NATIVE);
    let state = previous_state(s, &session).await.unwrap();
    state.into_keys().collect()
}

/// Whether the object database holds `oid`.
fn object_exists(dir: &Path, oid: &str) -> bool {
    Command::new("git")
        .args(["cat-file", "-e", oid])
        .current_dir(dir)
        .status()
        .unwrap()
        .success()
}

/// An earlier boundary that anchored a `.rift/state/` file, as sessions
/// recorded before the exclusion carry.
async fn seed_boundary_with_state_file(s: &SqliteStore, dir: &Path) {
    let session = ids::session_row_id(AGENT, NATIVE);
    let oid = git(dir, &["hash-object", "-w", STATE_FILE]);
    let snapshot_id = ids::turn_boundary_snapshot_id(&session, 0, "start");
    let snapshot = TurnBoundarySnapshotRow {
        snapshot_id: snapshot_id.clone(),
        session_id: session,
        turn_ordinal: 0,
        boundary: "start".to_owned(),
        actor: "human".to_owned(),
        worktree_hash: "seed".to_owned(),
        captured_at: 0,
        schema_version: 1,
    };
    record_turn_boundary_snapshot(s, &snapshot).await.unwrap();
    let file = TurnBoundaryFileRow {
        snapshot_id,
        path: STATE_FILE.to_owned(),
        blob_oid: oid,
        content_sha256: "seed".to_owned(),
        schema_version: 1,
    };
    record_turn_boundary_file(s, &file).await.unwrap();
}

#[tokio::test]
async fn rift_state_files_are_neither_recorded_nor_written_while_a_user_file_beside_them_is() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, head) = repo_with_a_commit(tmp.path());
    plant_state_file(tmp.path());
    std::fs::write(tmp.path().join("new.txt"), b"alpha\nbeta\n").unwrap();
    let state_oid = git(tmp.path(), &["hash-object", STATE_FILE]);
    let s = store();

    capture(&s, &repo, &reader(tmp.path()), &head, Boundary::Start).await;

    assert_eq!(
        hunk_paths(&s).await,
        vec![("human".to_owned(), "new.txt".to_owned())],
        "only the user's file is a human edit"
    );
    assert_eq!(anchored_paths(&s).await, vec!["new.txt".to_owned()]);
    assert!(
        !object_exists(tmp.path(), &state_oid),
        "the state file's bytes must not be written into the user's object store"
    );
}

#[tokio::test]
async fn a_rift_state_path_inherited_from_an_earlier_boundary_drops_out_of_the_set() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, head) = repo_with_a_commit(tmp.path());
    plant_state_file(tmp.path());
    let s = store();
    seed_boundary_with_state_file(&s, tmp.path()).await;

    std::fs::write(tmp.path().join(STATE_FILE), b"{\"served\":[]}\n").unwrap();
    capture(&s, &repo, &reader(tmp.path()), &head, Boundary::End).await;

    assert_eq!(
        hunk_paths(&s).await,
        Vec::new(),
        "no agent edit to Rift's state"
    );
    assert_eq!(anchored_paths(&s).await, Vec::<String>::new());
}

#[tokio::test]
async fn a_boundary_with_nothing_to_observe_never_lists_the_head_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, head) = repo_with_a_commit(tmp.path());
    let counting = reader(tmp.path());
    let s = store();

    capture(&s, &repo, &counting, &head, Boundary::Start).await;

    assert_eq!(counting.tree_listings.get(), 0);
    assert_eq!(anchored_paths(&s).await, Vec::<String>::new());
}

/// Stamp `path` with a modification time of `epoch_ms`, the way an editor's
/// write would have left it at that instant.
fn stamp(path: &Path, epoch_ms: i64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(system_time(epoch_ms))
        .unwrap();
}

fn system_time(epoch_ms: i64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_millis(epoch_ms.unsigned_abs())
}

/// A file edited before the boundary's instant is the closing window's; one
/// edited after it — by whoever's window came next, before the worker took
/// the snapshot — is unknown to the boundary: no hunk for it, and its
/// current bytes anchor the next window instead.
#[tokio::test]
async fn a_file_written_after_the_boundary_instant_records_no_hunk_for_that_window() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, head) = repo_with_a_commit(tmp.path());
    std::fs::write(tmp.path().join("a.txt"), b"one\ntwo\nlate\n").unwrap();
    stamp(&tmp.path().join("a.txt"), AFTER_INSTANT);
    std::fs::write(tmp.path().join("new.txt"), b"alpha\n").unwrap();
    stamp(&tmp.path().join("new.txt"), BEFORE_INSTANT);
    let s = store();

    capture_boundary(
        &s,
        &repo,
        &reader(tmp.path()),
        Some(&head),
        AGENT,
        NATIVE,
        1,
        Boundary::Start,
        INSTANT,
    )
    .await
    .unwrap();

    assert_eq!(
        hunk_paths(&s).await,
        vec![("human".to_owned(), "new.txt".to_owned())],
        "only the file written before the instant is the human's"
    );
    assert_eq!(
        anchored_paths(&s).await,
        vec!["a.txt".to_owned(), "new.txt".to_owned()],
        "the late file still anchors the next window"
    );
}

/// A tracked file deleted after the boundary's instant changed its directory
/// after it, and is unknown to the boundary rather than a deletion it saw.
///
/// Unix only for the fixture's sake: std opens a directory to set its time
/// only where a directory can be opened as a file.
#[cfg(unix)]
#[tokio::test]
async fn a_file_deleted_after_the_boundary_instant_records_no_deletion_for_that_window() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, head) = repo_with_a_commit(tmp.path());
    std::fs::remove_file(tmp.path().join("a.txt")).unwrap();
    std::fs::File::open(tmp.path())
        .unwrap()
        .set_modified(system_time(AFTER_INSTANT))
        .unwrap();
    let s = store();

    capture_boundary(
        &s,
        &repo,
        &reader(tmp.path()),
        Some(&head),
        AGENT,
        NATIVE,
        1,
        Boundary::End,
        INSTANT,
    )
    .await
    .unwrap();

    assert_eq!(hunk_paths(&s).await, Vec::new());
}

/// A commit made between the boundary and the snapshot takes the boundary's
/// edits out of `git status`; the snapshot still observes them against the
/// boundary's own `HEAD`, as a snapshot taken at the boundary would have.
#[tokio::test]
async fn a_commit_made_after_the_boundary_does_not_hide_what_the_boundary_saw() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, head) = repo_with_a_commit(tmp.path());
    std::fs::write(tmp.path().join("a.txt"), b"one\ntwo\nthree\n").unwrap();
    git(tmp.path(), &["commit", "-q", "-am", "the agent's work"]);
    let s = store();

    capture(&s, &repo, &reader(tmp.path()), &head, Boundary::End).await;

    assert_eq!(
        hunk_paths(&s).await,
        vec![("agent".to_owned(), "a.txt".to_owned())]
    );
}
