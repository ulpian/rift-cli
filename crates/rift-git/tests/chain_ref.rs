//! `refs/rift/chain/v1` end-to-end: the order git records, and the fork it stops.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;
use std::process::Command;

use rift_git::chain;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Lay one checkpoint shard down on the orphan checkpoint branch, exactly the
/// way the archive does: a commit parented on the then-tip.
fn add_checkpoint(dir: &Path, id: &str) {
    let (hi, rest) = id.split_at(2);
    let shard = dir.join(hi).join(rest);
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::write(
        shard.join("metadata.json"),
        r#"{"strategy":"manual-commit"}"#,
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", &format!("checkpoint {id}")]);
}

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "rift/checkpoints/v1"]);
    d
}

const A: &str = "4c5c0a053e73b924663009e1602882b6073d3fdd10ad33aa92c988a643e040f3";
const B: &str = "b970a014c1a3dd6508c5382679051a37f7cc1cd238b66543ecb1472a746d23c7";
const MID: &str = "93ae336492a162b429cf27a5952d8d8f0a7a878eca243585d5bc33893d7d0d9a";

/// THE REGRESSION THAT BLOCKED THE CHAIN REPAIR, permanently pinned.
///
/// `A < MID < B` by content hash. Written A, B, then MID. Sorted-hash linkage
/// gave MID the same predecessor as B — two rows, one parent, a fork that
/// append-only could never correct. Write order is the only honest answer, and
/// git recorded it in the branch all along.
#[test]
fn a_checkpoint_whose_hash_sorts_between_two_others_chains_and_does_not_fork() {
    assert!(
        A < MID && MID < B,
        "the fixture must reproduce the sort hazard"
    );
    let d = repo();
    let git_dir = d.path().join(".git");
    for id in [A, B, MID] {
        add_checkpoint(d.path(), id);
    }

    let n =
        chain::init_from_branch(&git_dir, "refs/heads/rift/checkpoints/v1", 1_700_000_000).unwrap();
    assert_eq!(n, 3);

    // WRITE order, not sorted order.
    let order = chain::read_order(&git_dir).unwrap();
    assert_eq!(order, vec![A.to_string(), B.to_string(), MID.to_string()]);
    assert_ne!(order[1], MID, "sorted order would have put MID here");

    // Each entry has exactly one predecessor: no id appears twice, so no two
    // checkpoints can claim the same parent.
    let mut seen = order.clone();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), order.len(), "an id may appear once");
}

#[test]
fn the_chain_ref_is_append_only_and_init_refuses_to_run_twice() {
    let d = repo();
    let git_dir = d.path().join(".git");
    add_checkpoint(d.path(), A);
    chain::init_from_branch(&git_dir, "refs/heads/rift/checkpoints/v1", 1).unwrap();
    let tip_before = chain::tip(&git_dir).unwrap();

    let again = chain::init_from_branch(&git_dir, "refs/heads/rift/checkpoints/v1", 1);
    assert!(
        again.is_err(),
        "a second init is a rewrite wearing an initializer's name"
    );
    assert_eq!(
        chain::tip(&git_dir).unwrap(),
        tip_before,
        "the ref did not move"
    );

    // Appending is how it grows, and the parent linkage is the order.
    add_checkpoint(d.path(), B);
    chain::append(&git_dir, B, 2).unwrap();
    assert_eq!(
        chain::read_order(&git_dir).unwrap(),
        vec![A.to_string(), B.to_string()]
    );
}

/// A worker whose checkpoint landed and whose entry did not must be detectable,
/// so the next append can heal it instead of the gap becoming permanent.
#[test]
fn a_checkpoint_with_no_chain_entry_is_reported_as_missing() {
    let d = repo();
    let git_dir = d.path().join(".git");
    add_checkpoint(d.path(), A);
    add_checkpoint(d.path(), B);
    chain::append(&git_dir, A, 1).unwrap(); // B's entry never written

    let have = chain::branch_order(&git_dir, "refs/heads/rift/checkpoints/v1").unwrap();
    assert_eq!(have, vec![A.to_string(), B.to_string()]);
    assert_eq!(
        chain::missing_entries(&git_dir, &have).unwrap(),
        vec![B.to_string()]
    );

    chain::append(&git_dir, B, 2).unwrap();
    assert!(chain::missing_entries(&git_dir, &have).unwrap().is_empty());
}

/// The id must be recoverable from the COMMIT alone — no tree, no blob — or a
/// treeless consumer cannot read the order (sync-format's remote-read section).
#[test]
fn the_order_is_readable_without_fetching_trees_or_blobs() {
    let d = repo();
    let git_dir = d.path().join(".git");
    add_checkpoint(d.path(), A);
    chain::append(&git_dir, A, 1).unwrap();

    let tip = chain::tip(&git_dir).unwrap().unwrap();
    let body = git(
        d.path(),
        &[
            "--git-dir",
            git_dir.to_str().unwrap(),
            "log",
            "-1",
            "--format=%B",
            &tip,
        ],
    );
    assert!(body.contains(chain::CHECKPOINT_TRAILER), "{body}");
    assert!(
        body.contains(A),
        "the id is in the commit object itself: {body}"
    );
    // …and the entry's tree is empty, so there is nothing else to fetch.
    let tree = git(
        d.path(),
        &[
            "--git-dir",
            git_dir.to_str().unwrap(),
            "log",
            "-1",
            "--format=%T",
            &tip,
        ],
    );
    assert_eq!(tree.trim(), "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
}

#[test]
fn a_non_linear_branch_is_reported_rather_than_guessed_around() {
    let d = repo();
    let git_dir = d.path().join(".git");
    add_checkpoint(d.path(), A);
    git(d.path(), &["checkout", "-q", "-b", "side"]);
    add_checkpoint(d.path(), MID);
    git(d.path(), &["checkout", "-q", "rift/checkpoints/v1"]);
    add_checkpoint(d.path(), B);
    git(d.path(), &["merge", "-q", "--no-edit", "side"]);

    let err = chain::branch_order(&git_dir, "refs/heads/rift/checkpoints/v1");
    assert!(err.is_err(), "a merge in the graph must stop the backfill");
}

/// The other direction, which `missing_entries` cannot see: an entry naming a
/// checkpoint the branch does not carry. Not healable, so it is named.
#[test]
fn an_entry_naming_no_checkpoint_on_the_branch_is_reported_as_an_orphan() {
    let d = repo();
    let git_dir = d.path().join(".git");
    add_checkpoint(d.path(), A);
    chain::append(&git_dir, A, 1).unwrap();
    // B was never archived — its entry names nothing on the branch.
    chain::append(&git_dir, B, 2).unwrap();

    let have = chain::branch_order(&git_dir, "refs/heads/rift/checkpoints/v1").unwrap();
    assert_eq!(have, vec![A.to_string()]);
    assert_eq!(
        chain::orphan_entries(&git_dir, &have).unwrap(),
        vec![B.to_string()]
    );
    // …and the two directions do not see each other's finding.
    assert!(chain::missing_entries(&git_dir, &have).unwrap().is_empty());

    // Archiving B afterwards resolves it: the same entry now names a checkpoint.
    add_checkpoint(d.path(), B);
    let have = chain::branch_order(&git_dir, "refs/heads/rift/checkpoints/v1").unwrap();
    assert!(chain::orphan_entries(&git_dir, &have).unwrap().is_empty());
}
