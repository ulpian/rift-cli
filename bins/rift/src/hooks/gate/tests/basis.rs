//! What a recorded read owes the detached worker: the revision it kept, the
//! secrets it masked first, and the paths it never keeps at all.

use std::path::Path;

use rift_core::Result;

use crate::hooks::ingest::owe_basis_snapshots;
use crate::hooks::worker::snapshot_basis_job;

/// A work tree with a real git dir and one file in it.
fn snapshot_fixture(rel_path: &str, body: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    rift_git::test_support::fake_repo(root).unwrap();
    if let Some(parent) = Path::new(rel_path).parent() {
        std::fs::create_dir_all(root.join(parent)).unwrap();
    }
    std::fs::write(root.join(rel_path), body).unwrap();
    tmp
}

fn snapshot_job(work_tree: &Path, rel_path: &str, body: &str) -> crate::agent_queue::AgentJob {
    crate::agent_queue::AgentJob {
        schema_version: 1,
        work_tree: work_tree.to_path_buf(),
        queued_at_ms: 1_700_000_000_000,
        attempts: 0,
        kind: crate::agent_queue::JobKind::BasisSnapshot {
            rel_path: rel_path.to_owned(),
            content_sha256: rift_recall_substrate::worktree::sha256_hex(body.as_bytes()),
            blob_oid: rift_recall_substrate::worktree::git_blob_oid(body.as_bytes()),
        },
    }
}

fn basis_store_of(work_tree: &Path) -> rift_recall_serve::basis_store::BasisStore {
    let family = crate::family::try_at(work_tree).expect("a minted family");
    rift_recall_serve::basis_store::BasisStore::for_family(&family.common_dir)
}

fn run_snapshot(job: &crate::agent_queue::AgentJob) -> Result<()> {
    let crate::agent_queue::JobKind::BasisSnapshot {
        rel_path,
        content_sha256,
        blob_oid,
    } = &job.kind
    else {
        panic!("a snapshot job");
    };
    snapshot_basis_job(
        &crate::hooktime::HookTimer::from_env("test"),
        job,
        rel_path,
        content_sha256,
        blob_oid,
    )
}

/// ★ The worker keeps the bytes a read recorded, under the read's own
/// identity and in the family's store — which is what gives an
/// **uncommitted** revision a basis at all.
#[test]
fn the_worker_keeps_a_revision_a_read_recorded() {
    let body = "alpha\nbravo\ncharlie\n";
    let tmp = snapshot_fixture("src/a.rs", body);
    let work_tree = tmp.path();
    let job = snapshot_job(work_tree, "src/a.rs", body);
    run_snapshot(&job).expect("the snapshot lands");

    let store = basis_store_of(work_tree);
    let key = rift_recall_substrate::worktree::sha256_hex(body.as_bytes());
    let held = store.get_for_path("src/a.rs", &key).expect("the snapshot");
    assert_eq!(held.bytes, body.as_bytes());
    assert!(held.redacted_lines.is_empty(), "nothing was masked");

    // The working file moves on — the revision is now reachable nowhere
    // else, which is the whole case this store exists for.
    std::fs::write(work_tree.join("src/a.rs"), "alpha\nBRAVO\ncharlie\n").unwrap();
    assert!(
        store.get_for_path("src/a.rs", &key).is_some(),
        "the basis outlives the edit that made it interesting"
    );
}

/// ★ Nothing durable here skipped the gate: a secret in the bytes a read
/// delivered is masked in the snapshot, and the line it was on is
/// recorded so no diff can name it.
#[test]
fn a_secret_in_a_recorded_read_is_masked_before_it_is_kept() {
    let secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
    let body = format!("fn main() {{}}\nconst TOKEN: &str = \"{secret}\";\nfn other() {{}}\n");
    let tmp = snapshot_fixture("src/a.rs", &body);
    let work_tree = tmp.path();
    run_snapshot(&snapshot_job(work_tree, "src/a.rs", &body)).expect("the snapshot lands");

    let store = basis_store_of(work_tree);
    let key = rift_recall_substrate::worktree::sha256_hex(body.as_bytes());
    let held = store.get_for_path("src/a.rs", &key).expect("the snapshot");
    let text = String::from_utf8(held.bytes).expect("text");
    assert!(!text.contains(secret), "the raw secret is not durable");
    assert_eq!(
        held.redacted_lines,
        vec![(2, 2)],
        "the masked line is recorded so nothing names it as a change"
    );
    assert!(!held.policy_fingerprint.is_empty(), "under a named policy");
}

/// A path the repository calls sensitive is never re-encoded here, exactly
/// as it is never converted into a document sidecar. The job succeeds and
/// records nothing: a saving forgone, never a failure.
#[test]
fn a_sensitive_path_is_never_snapshotted() {
    let body = "KEY=1\n";
    let tmp = snapshot_fixture("secrets/.env", body);
    let work_tree = tmp.path();
    std::fs::create_dir_all(work_tree.join(".rift")).unwrap();
    std::fs::write(
        work_tree.join(".rift/config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n",
    )
    .unwrap();
    run_snapshot(&snapshot_job(work_tree, "secrets/.env", body)).expect("no failure");
    assert_eq!(basis_store_of(work_tree).survey().snapshots, 0);
}

/// Bytes that match neither the working file nor a blob are not kept under
/// a guessed identity — the same ladder, and the same refusal, the
/// deferred token count lives by.
#[test]
fn bytes_that_match_no_source_are_not_kept() {
    let tmp = snapshot_fixture("src/a.rs", "alpha\n");
    let work_tree = tmp.path();
    // A job naming a revision nothing here has.
    let job = snapshot_job(work_tree, "src/a.rs", "a revision nobody wrote\n");
    assert!(
        run_snapshot(&job).is_err(),
        "said once, retried, never faked"
    );
    assert_eq!(basis_store_of(work_tree).survey().snapshots, 0);
}

/// One revision read many times owes one snapshot, keyed by the revision
/// rather than by the read.
#[test]
fn many_reads_of_one_revision_owe_one_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    let read = |call: &str, sha: &str| rift_shadow::RecordedRead {
        tool_call_id: call.to_owned(),
        rel_path: "src/a.rs".into(),
        blob_oid: "ee".repeat(20),
        content_sha256: sha.to_owned(),
        tokens_recorded: true,
        lines: None,
        schema_version: 1,
    };
    let same = "aa".repeat(32);
    let other = "bb".repeat(32);
    let owed = owe_basis_snapshots(
        &git_dir,
        tmp.path(),
        &[
            read(&"11".repeat(32), &same),
            read(&"22".repeat(32), &same),
            read(&"33".repeat(32), &other),
        ],
    );
    assert_eq!(owed, 2, "two revisions, three reads");
    let queued = crate::agent_queue::jobs(&git_dir)
        .into_iter()
        .filter(|(_, job)| matches!(job.kind, crate::agent_queue::JobKind::BasisSnapshot { .. }))
        .count();
    assert_eq!(queued, 2);
}

// ------------------------------------------------ the search index ---
