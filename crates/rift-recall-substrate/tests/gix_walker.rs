//! End-to-end tests for the gix-backed substrate reader against a real on-disk
//! repository built with the `git` CLI (dev-only fixtures).
//!
//! Covers: shard enumeration, artifact reads, the read-only invariant, the
//! traversal guard, and the headline metric (`tokens_avoided > 0`).
#![cfg(feature = "gix")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use rift_recall_ledger::{ingest, RecallMetrics};
use rift_recall_substrate::{worktree, Dialect, GixSubstrate, ShardWalker, SubstrateReader};

/// Run `git` in `dir` with a fixed identity / no signing (deterministic).
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "rift-test")
        .env("GIT_AUTHOR_EMAIL", "test@rift.local")
        .env("GIT_COMMITTER_NAME", "rift-test")
        .env("GIT_COMMITTER_EMAIL", "test@rift.local")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .expect("spawn git");
    assert!(
        status.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&status.stderr)
    );
}

/// Check files out with the bytes they were committed with, whatever the
/// machine's global `core.autocrlf`: the fixture precomputes content hashes,
/// and a CRLF rewrite on checkout would make every one of them stale.
fn pin_line_endings(dir: &Path) {
    git(dir, &["config", "core.autocrlf", "false"]);
    git(dir, &["config", "core.eol", "lf"]);
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

const A_RS: &str = "fn main() {}\n";

/// Build a repo with a real `main` branch carrying the worktree file `a.rs`, plus
/// an orphan `rift/checkpoints/v1` branch holding two sharded checkpoints. The
/// committed read facts use the *real* SHA-256 of `a.rs` (so that read is
/// "unchanged vs worktree") and a distinct hash for `b.rs`.
fn build_fixture(dir: &Path) {
    let hash_a = worktree::sha256_hex(A_RS.as_bytes());
    let hash_b = worktree::sha256_hex(b"other contents\n");

    // A real working branch with a.rs committed.
    git(dir, &["init", "-q", "-b", "main"]);
    pin_line_endings(dir);
    write(&dir.join("a.rs"), A_RS);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "worktree"]);

    // The orphan checkpoint branch: clear the worktree, lay down sharded artifacts.
    git(dir, &["checkout", "-q", "--orphan", "rift/checkpoints/v1"]);
    git(dir, &["rm", "-rfq", "."]);

    // Checkpoint "ab12": a Read of a.rs@hash_a (matches worktree) and a repeated
    // Read of b.rs@hash_b. token_estimates supplies the input tokens.
    write(
        &dir.join("ab/12/tool_calls.jsonl"),
        &format!(
            concat!(
                r#"{{"id":"t1","name":"Read","file_path":"a.rs","sha256":"{a}"}}"#,
                "\n",
                r#"{{"id":"t2","name":"Read","file_path":"b.rs","sha256":"{b}"}}"#,
                "\n",
                r#"{{"id":"t3","name":"Read","file_path":"b.rs","sha256":"{b}"}}"#,
                "\n",
            ),
            a = hash_a,
            b = hash_b,
        ),
    );
    write(
        &dir.join("ab/12/token_estimates.json"),
        concat!(
            r#"{"id":"t1","input":100}"#,
            "\n",
            r#"{"id":"t2","input":100}"#,
            "\n",
            r#"{"id":"t3","input":100}"#,
            "\n",
        ),
    );
    write(
        &dir.join("ab/12/metadata.json"),
        r#"{"strategy":"manual-commit"}"#,
    );
    // Checkpoint "cd34": minimal, just to prove multi-shard enumeration.
    write(&dir.join("cd/34/tool_calls.jsonl"), "");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "checkpoints"]);

    // Back to the working branch for the metric comparison.
    git(dir, &["checkout", "-q", "main"]);
}

#[test]
fn walks_shards_and_reads_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());

    let walker = ShardWalker::open(dir.path()).unwrap();
    assert_eq!(walker.present_dialects(), vec![Dialect::Rift]);

    let ids: Vec<String> = walker.shard_ids(Dialect::Rift).unwrap();
    assert_eq!(ids, vec!["ab12".to_string(), "cd34".to_string()]);

    // Artifact read by tree-path lookup inside the checkpoint subtree.
    let meta = walker
        .read_artifact_in(Dialect::Rift, "ab12", "metadata.json")
        .unwrap()
        .expect("metadata present");
    assert!(String::from_utf8_lossy(&meta).contains("manual-commit"));
    assert!(walker
        .read_artifact_in(Dialect::Rift, "ab12", "missing.json")
        .unwrap()
        .is_none());
}

#[test]
fn empty_repo_yields_no_shards() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let walker = ShardWalker::open(dir.path()).unwrap();
    assert!(walker.shard_ids(Dialect::Rift).unwrap().is_empty());
    assert!(walker.present_dialects().is_empty());
}

#[test]
fn read_artifact_rejects_traversal() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());
    let walker = ShardWalker::open(dir.path()).unwrap();
    // A traversal component just fails the tree lookup; no filesystem access.
    assert!(walker
        .read_artifact_in(Dialect::Rift, "ab12", "../../../etc/passwd")
        .unwrap()
        .is_none());
}

#[test]
fn walk_issues_no_writes() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());

    let refs_before = list_refs(dir.path());
    let objects_before = count_objects(dir.path());

    let walker = ShardWalker::open(dir.path()).unwrap();
    let _ = walker.shards_all().unwrap();
    let _ = walker
        .read_artifact_in(Dialect::Rift, "ab12", "metadata.json")
        .unwrap();

    assert_eq!(
        refs_before,
        list_refs(dir.path()),
        "walk must not change refs"
    );
    assert_eq!(
        objects_before,
        count_objects(dir.path()),
        "walk must not write objects"
    );
}

#[test]
fn metrics_report_tokens_avoided_gt_zero() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());

    assert_worktree_holds_the_committed_bytes(dir.path(), "a.rs");

    let sub = GixSubstrate::open(dir.path()).unwrap();
    let (reads, issues) = ingest::read_facts_all(&sub).unwrap();
    assert!(
        issues.is_empty(),
        "fixture artifacts parse cleanly: {issues:?}"
    );
    // 3 reads: a.rs (once), b.rs (twice).
    assert_eq!(reads.len(), 3);

    let paths: BTreeSet<String> = ingest::read_paths(&reads);
    let wt = worktree::hash_paths(dir.path(), &paths);

    let m = RecallMetrics::compute(&reads, &wt);
    // a.rs (100, unchanged vs worktree) + b.rs second read (100, repeated) = 200.
    assert_eq!(m.total_input_tokens, 300);
    assert_eq!(m.tokens_avoided, 200, "a.rs unchanged + b.rs reread");
    assert!(
        m.tokens_avoided > 0,
        "definition-of-done: tokens_avoided > 0"
    );
}

/// The working file's raw bytes hash to the blob `HEAD` commits for it:
/// `--no-filters` keeps a line-ending conversion from hiding a rewrite.
fn assert_worktree_holds_the_committed_bytes(dir: &Path, path: &str) {
    let worktree = git_out(dir, &["hash-object", "--no-filters", path]);
    let committed = git_out(dir, &["rev-parse", &format!("HEAD:{path}")]);
    assert_eq!(worktree, committed, "{path} was rewritten on checkout");
}

fn list_refs(dir: &Path) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["show-ref"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn count_objects(dir: &Path) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["count-objects", "-v"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run `git` and return trimmed stdout (for `rev-parse`).
fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "rift-test")
        .env("GIT_AUTHOR_EMAIL", "test@rift.local")
        .env("GIT_COMMITTER_NAME", "rift-test")
        .env("GIT_COMMITTER_EMAIL", "test@rift.local")
        .output()
        .expect("spawn git");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The git-graph surface: `integration_tips`, `branch_tips`,
/// `commit_meta`, and reachability against a **real** merged/unmerged history.
#[test]
fn gix_graph_surface_reflects_real_history() {
    use rift_recall_substrate::{is_reachable, SubstrateReader, MAX_REACHABILITY_WALK};

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();

    // main: c0 → c1
    git(p, &["init", "-q", "-b", "main"]);
    write(&p.join("a.txt"), "0\n");
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "c0"]);
    let c0 = git_out(p, &["rev-parse", "HEAD"]);
    write(&p.join("a.txt"), "1\n");
    git(p, &["commit", "-qam", "c1"]);

    // feature off c0, merged back into main with a merge commit
    git(p, &["checkout", "-q", "-b", "feature", &c0]);
    write(&p.join("b.txt"), "feat\n");
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "feature work"]);
    let feat = git_out(p, &["rev-parse", "HEAD"]);
    git(p, &["checkout", "-q", "main"]);
    git(
        p,
        &["merge", "-q", "--no-ff", "-m", "merge feature", "feature"],
    );
    let main_tip = git_out(p, &["rev-parse", "HEAD"]);

    // an orphan branch that is never merged
    git(p, &["checkout", "-q", "-b", "orphan", &c0]);
    write(&p.join("c.txt"), "orphan\n");
    git(p, &["add", "-A"]);
    git(p, &["commit", "-q", "-m", "orphan work"]);
    let orphan = git_out(p, &["rev-parse", "HEAD"]);
    git(p, &["checkout", "-q", "main"]);

    let sub = GixSubstrate::open(p).unwrap();

    // integration tip is main's HEAD (a merge commit with two parents)
    assert_eq!(sub.integration_tips().unwrap(), vec![main_tip.clone()]);
    let merge_meta = sub.commit_meta(&main_tip).unwrap().expect("merge commit");
    assert_eq!(merge_meta.parents.len(), 2, "no-ff merge has two parents");
    assert_eq!(merge_meta.summary, "merge feature");
    assert!(merge_meta.committed_ms > 0);

    // branch tips include working branches, never a rift/legacy namespace
    let branches: BTreeSet<String> = sub
        .branch_tips()
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(
        branches.contains("main") && branches.contains("feature") && branches.contains("orphan")
    );
    assert!(!branches
        .iter()
        .any(|b| b.starts_with("rift/") || b.starts_with(concat!("ent", "ire", "/"))));

    // reachability: the merged feature commit is reachable from main; the orphan is not
    assert!(is_reachable(&sub, &feat, &main_tip, MAX_REACHABILITY_WALK).unwrap());
    assert!(!is_reachable(&sub, &orphan, &main_tip, MAX_REACHABILITY_WALK).unwrap());
}

/// The ULID that names the per-checkpoint ref and its shard tree. A real ULID
/// (Crockford base32, 26 chars) so the fixture exercises the id spelling that
/// made this layout invisible: `shard_ids` requires hex, and a ULID is not hex.
const LEGACY_ULID: &str = "01JQZ8T9WQKX3M7VN2P4RB6CDE";

/// Build a repo whose ONLY legacy artifact is a per-checkpoint ref — no
/// the retired shared-checkpoint branch anywhere.
///
/// This is the shape that read as "no checkpoints" before the walk existed:
/// `branch_tips` excludes the retired namespace by contract, `shard_ids` only
/// reads a dialect's single checkpoint branch, and nothing else looked at
/// the retired per-checkpoint refs at all.
fn build_only_refs_fixture(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    write(&dir.join("a.rs"), A_RS);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "worktree"]);

    // Lay the shard tree down on an orphan commit, then point ONLY a
    // per-checkpoint ref at it and delete the branch — so the artifacts are
    // reachable exclusively through the retired per-checkpoint namespace.
    git(dir, &["checkout", "-q", "--orphan", "legacy-staging"]);
    git(dir, &["rm", "-rfq", "."]);
    let (hi, rest) = LEGACY_ULID.split_at(2);
    write(
        &dir.join(hi).join(rest).join("metadata.json"),
        r#"{"strategy":"manual-commit","session_ids":["01JLEGACYSESSIONX"]}"#,
    );
    write(
        &dir.join(hi).join(rest).join("full.jsonl"),
        "{\"session_id\":\"01JLEGACYSESSIONX\",\"turn\":0,\"event\":0,\"kind\":\"session.start\"}\n",
    );
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "legacy checkpoint"]);
    let oid = String::from_utf8(
        Command::new("git")
            .current_dir(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    git(
        dir,
        &[
            "update-ref",
            &format!(
                "refs/{}/checkpoints/{hi}/{LEGACY_ULID}",
                concat!("ent", "ire")
            ),
            &oid,
        ],
    );
    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["branch", "-qD", "legacy-staging"]);
}

/// The rejection this fixture exists for: a repository whose only legacy
/// artifact is a per-checkpoint ref used to enumerate as **nothing at all**, and
/// the import verb reported "no pre-Rift checkpoints found" with exit 0 — a
/// confident silent false negative.
#[test]
fn a_repo_whose_only_legacy_artifact_is_a_per_checkpoint_ref_is_not_invisible() {
    let dir = tempfile::tempdir().unwrap();
    build_only_refs_fixture(dir.path());
    let walker = ShardWalker::open(dir.path()).unwrap();

    // There is no legacy BRANCH — which is the whole point of the fixture.
    assert!(
        !walker.present_dialects().contains(&Dialect::Legacy),
        "no legacy checkpoint branch exists in this repo"
    );
    assert!(walker.shard_ids(Dialect::Legacy).unwrap().is_empty());

    // …and the ref walk finds it anyway.
    let refs = walker.legacy_checkpoint_refs().unwrap();
    assert_eq!(refs.len(), 1, "{refs:?}");
    assert_eq!(
        refs[0].0,
        format!(
            "refs/{}/checkpoints/01/{LEGACY_ULID}",
            concat!("ent", "ire")
        )
    );
    let ids = walker.legacy_ref_shard_ids().unwrap();
    assert_eq!(ids.len(), 1, "{ids:?}");
    assert_eq!(ids[0].0, LEGACY_ULID);

    // The union that every SubstrateReader consumer actually calls.
    let shards = walker.shards_all().unwrap();
    assert_eq!(shards.len(), 1, "{shards:?}");
    assert_eq!(shards[0].checkpoint_id, LEGACY_ULID);
    assert_eq!(shards[0].dialect, Dialect::Legacy);

    // Enumerating without being able to READ would be a worse failure than not
    // enumerating: the caller would see a checkpoint with no artifacts and
    // conclude the shard was malformed.
    let meta = walker
        .read_artifact_any(LEGACY_ULID, "metadata.json")
        .unwrap()
        .expect("metadata reachable through the ref");
    assert!(String::from_utf8_lossy(&meta).contains("manual-commit"));
    let full = walker
        .read_artifact_any(LEGACY_ULID, "full.jsonl")
        .unwrap()
        .expect("event stream reachable through the ref");
    assert!(String::from_utf8_lossy(&full).contains("01JLEGACYSESSIONX"));
    assert!(walker
        .read_artifact_any(LEGACY_ULID, "absent.json")
        .unwrap()
        .is_none());
}

/// The frozen literal: the namespace is a fact about other
/// people's repositories. Renaming it would not rename their refs — it would
/// stop Rift finding them, which is the failure this whole layer exists to fix.
#[test]
fn the_legacy_per_checkpoint_namespace_is_frozen() {
    let dir = tempfile::tempdir().unwrap();
    build_only_refs_fixture(dir.path());
    let walker = ShardWalker::open(dir.path()).unwrap();
    let refs = walker.legacy_checkpoint_refs().unwrap();
    assert!(
        refs[0]
            .0
            .starts_with(concat!("refs/", "ent", "ire", "/checkpoints/")),
        "the on-disk prefix moved: {refs:?}"
    );
}

/// Both layouts in one repository enumerate together, and the branch layout is
/// untouched by the new walk — a regression guard on the loosened id rule, which
/// applies to ref trees ONLY.
#[test]
fn both_legacy_layouts_enumerate_together_without_disturbing_the_branch_walk() {
    let dir = tempfile::tempdir().unwrap();
    build_only_refs_fixture(dir.path());
    // Add the branch layout beside it, with a hex id.
    git(
        dir.path(),
        &[
            "checkout",
            "-q",
            "--orphan",
            concat!("ent", "ire", "/checkpoints/v1"),
        ],
    );
    git(dir.path(), &["rm", "-rfq", "."]);
    write(
        &dir.path().join("ab/34cd56ef78/metadata.json"),
        r#"{"strategy":"manual-commit"}"#,
    );
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "legacy branch shard"]);
    git(dir.path(), &["checkout", "-q", "main"]);

    let walker = ShardWalker::open(dir.path()).unwrap();
    let ids: BTreeSet<String> = walker
        .shards_all()
        .unwrap()
        .into_iter()
        .map(|s| s.checkpoint_id)
        .collect();
    assert!(ids.contains(LEGACY_ULID), "refs layout: {ids:?}");
    assert!(ids.contains("ab34cd56ef78"), "branch layout: {ids:?}");
    // The branch walk itself is unchanged: still hex-only, still just the branch.
    assert_eq!(
        walker.shard_ids(Dialect::Legacy).unwrap(),
        vec!["ab34cd56ef78".to_string()]
    );
}

const CP_A: &str = "4c5c0a053e73b924663009e1602882b6073d3fdd10ad33aa92c988a643e040f3";
const CP_B: &str = "b970a014c1a3dd6508c5382679051a37f7cc1cd238b66543ecb1472a746d23c7";
const CP_MID: &str = "93ae336492a162b429cf27a5952d8d8f0a7a878eca243585d5bc33893d7d0d9a";

/// Write one chain entry (trailer-carried id) onto `refs/rift/chain/v1`.
fn chain_append(dir: &Path, id: &str) {
    let parent = String::from_utf8(
        Command::new("git")
            .current_dir(dir)
            .args(["rev-parse", "--verify", "--quiet", "refs/rift/chain/v1"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    Command::new("git")
        .current_dir(dir)
        .arg("mktree")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let mut args: Vec<String> = vec![
        "commit-tree".into(),
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
    ];
    if !parent.is_empty() {
        args.push("-p".into());
        args.push(parent.clone());
    }
    args.push("-m".into());
    args.push(format!("rift-chain\n\nRift-Chain-Checkpoint: {id}\n"));
    let out = Command::new("git")
        .current_dir(dir)
        .args(&args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "commit-tree: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let entry = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    git(dir, &["update-ref", "refs/rift/chain/v1", &entry, &parent]);
}

fn add_shard(dir: &Path, id: &str) {
    let (hi, rest) = id.split_at(2);
    write(
        &dir.join(hi).join(rest).join("metadata.json"),
        r#"{"strategy":"manual-commit"}"#,
    );
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "checkpoint"]);
}

/// PART 2, at the seam every consumer reads: `shards_all` takes its order from
/// `refs/rift/chain/v1`, not from a sort of content hashes.
///
/// The fixture is the fork shape: CP_A < CP_MID < CP_B by hash, written A, B,
/// MID. Sorted order puts MID second and hands it B's predecessor. Chain order
/// puts it third, where it was written.
#[test]
fn shards_all_takes_its_order_from_the_chain_ref_not_from_sorted_hashes() {
    assert!(
        CP_A < CP_MID && CP_MID < CP_B,
        "the fixture must carry the sort hazard"
    );
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "rift/checkpoints/v1"]);
    for id in [CP_A, CP_B, CP_MID] {
        add_shard(dir.path(), id);
        chain_append(dir.path(), id);
    }

    let walker = ShardWalker::open(dir.path()).unwrap();
    assert_eq!(
        walker.chain_order().unwrap(),
        vec![CP_A.to_string(), CP_B.to_string(), CP_MID.to_string()]
    );

    let shards = walker.shards_all().unwrap();
    let ids: Vec<&str> = shards.iter().map(|s| s.checkpoint_id.as_str()).collect();
    assert_eq!(ids, vec![CP_A, CP_B, CP_MID], "write order, not hash order");

    // The linkage every downstream reader consumes: no two shards may claim the
    // same predecessor, which is precisely what sorted order produced.
    assert_eq!(shards[0].prev, None, "exactly one genesis");
    assert_eq!(shards[1].prev.as_deref(), Some(CP_A));
    assert_eq!(shards[2].prev.as_deref(), Some(CP_B));
    let mut prevs: Vec<_> = shards.iter().filter_map(|s| s.prev.clone()).collect();
    let before = prevs.len();
    prevs.sort();
    prevs.dedup();
    assert_eq!(prevs.len(), before, "a predecessor may be claimed once");
}

/// A repository that has not run `chain init` has NO order, and the walker says
/// so by leaving the chain empty rather than manufacturing one.
#[test]
fn without_the_chain_ref_there_is_no_order_to_report() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "rift/checkpoints/v1"]);
    add_shard(dir.path(), CP_A);
    let walker = ShardWalker::open(dir.path()).unwrap();
    assert!(walker.chain_order().unwrap().is_empty());
    // The shard still enumerates — it exists — it simply has no linkage yet.
    let shards = walker.shards_all().unwrap();
    assert_eq!(shards.len(), 1);
    assert_eq!(shards[0].prev, None);
}

/// The candidate set a basis is proved from, over a real history: two commits
/// of one file give two distinct blobs, newest first, and the bytes behind
/// each hash to what was committed. This is what lets a revision an agent read
/// three commits ago be reached without Rift having kept a copy of it.
#[test]
fn a_paths_blobs_come_back_newest_first_over_the_bounded_history() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    let first = "one\ntwo\n";
    let second = "one\nTWO\nthree\n";
    write(&dir.path().join("src/a.rs"), first);
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "first"]);
    write(&dir.path().join("src/a.rs"), second);
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "second"]);

    let sub = GixSubstrate::open(dir.path()).unwrap();
    let paths: BTreeSet<String> = ["src/a.rs".to_string(), "src/absent.rs".to_string()]
        .into_iter()
        .collect();
    let map = sub.path_blob_oids(&paths, 8).unwrap();
    assert!(!map.contains_key("src/absent.rs"), "a path git never had");
    let oids = &map["src/a.rs"];
    assert_eq!(oids.len(), 2, "one blob per distinct revision: {oids:?}");

    // Newest first, and each blob's bytes hash to the revision that was
    // committed — the equality the bridge decides on.
    let bytes_of = |oid: &str| sub.blob(oid).unwrap().unwrap();
    assert_eq!(bytes_of(&oids[0]), second.as_bytes());
    assert_eq!(bytes_of(&oids[1]), first.as_bytes());
    assert_eq!(
        worktree::sha256_hex(&bytes_of(&oids[1])),
        worktree::sha256_hex(first.as_bytes())
    );

    // The bound is a bound: one commit of history sees only the newest blob.
    assert_eq!(sub.path_blob_oids(&paths, 1).unwrap()["src/a.rs"].len(), 1);
    assert!(sub.path_blob_oids(&paths, 0).unwrap().is_empty());

    // And the reader says where the files are, so a caller can ask what the
    // file is NOW rather than what a checkpoint recorded.
    assert_eq!(
        sub.worktree_root()
            .map(|p| std::fs::canonicalize(p).unwrap()),
        Some(std::fs::canonicalize(dir.path()).unwrap())
    );
}

/// Lay down one segmented turn-end shard on the checked-out checkpoint
/// branch and commit it there. Nothing touches the working branch.
fn add_turn_end_shard(dir: &Path, id: &str, parts: &[&str]) {
    let (hi, rest) = id.split_at(2);
    let shard = dir.join(hi).join(rest);
    let lines = vec!["1"; parts.len()].join(",");
    write(
        &shard.join("metadata.json"),
        &format!(
            r#"{{"strategy":"turn-end","created_at":"2026-01-01T00:00:00Z","transcript_segments":{},"segment_raw_lines":[{lines}]}}"#,
            parts.len()
        ),
    );
    for (n, part) in parts.iter().enumerate() {
        write(&shard.join(format!("transcript.segments/{n:06}")), part);
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "turn-end"]);
}

/// Before the first commit on the working branch, each turn-end seal lands
/// only on `refs/heads/rift/checkpoints/v1`. The next catalogue build lists
/// it, anchorless and dated by its own record, and reads its joined parts:
/// no commit on the working branch is needed for a seal to be seen.
#[test]
fn a_turn_end_seal_on_the_checkpoint_branch_is_catalogued_before_any_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    pin_line_endings(dir);
    git(dir, &["checkout", "-q", "--orphan", "rift/checkpoints/v1"]);
    add_turn_end_shard(dir, "ab12", &["a\n"]);
    git(dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let reader = GixSubstrate::open(dir).unwrap();
    let first = rift_recall_substrate::build_checkpoint_catalogue(&reader).unwrap();
    assert_eq!(first.len(), 1);

    git(
        dir,
        &["symbolic-ref", "HEAD", "refs/heads/rift/checkpoints/v1"],
    );
    add_turn_end_shard(dir, "cd34", &["a\n", "b\n"]);
    git(dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let reader = GixSubstrate::open(dir).unwrap();
    let next = rift_recall_substrate::build_checkpoint_catalogue(&reader).unwrap();
    let ids: Vec<&str> = next.iter().map(|e| e.checkpoint_id.as_str()).collect();
    assert_eq!(ids, ["ab12", "cd34"]);
    for entry in &next {
        assert!(entry.anchor_commit.is_none(), "{}", entry.checkpoint_id);
        assert!(entry.recorded_at_ms.is_some(), "{}", entry.checkpoint_id);
        assert!(entry.issues.is_empty(), "{:?}", entry.issues);
    }
    let joined = reader.read_artifact("cd34", "transcript.jsonl").unwrap();
    assert_eq!(joined.as_deref(), Some(&b"a\nb\n"[..]));
}
