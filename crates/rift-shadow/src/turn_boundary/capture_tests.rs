//! What a boundary snapshot leaves out, and what it costs: Rift's own state
//! never enters the working set, and a boundary with nothing to observe never
//! lists HEAD's tree. Driven through the real [`capture_boundary`] over a real
//! repository, like the tests beside the function.

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
    git(dir, &["config", "user.email", "t@t"]);
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
    capture_boundary(s, repo, reader, Some(head), AGENT, NATIVE, 0, boundary, 1)
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
