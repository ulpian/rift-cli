//! A regular anchor proves file kind, not that its bytes predate a commit.

use super::*;
use rift_shadow_attribution::{capture_commit, LineAttribution};
use rift_store::{
    latest_turn_boundary_snapshot, turn_boundary_counts, turn_boundary_file_kinds_for_snapshot,
    turn_boundary_files_for_snapshot, BoundaryFileKind,
};

#[tokio::test]
async fn old_mtimes_and_equal_timestamps_do_not_certify_historical_writers() {
    let h = history();
    stamp(&h.tmp.path().join("a.txt"), BEFORE_INSTANT);
    let s = store();
    let r = reader(h.tmp.path());
    // Equal millisecond readings do not establish a causal order. The file
    // has a preserved old mtime but contains the fixture's later bytes.
    crate::turn_boundary::capture_boundary_with_clock(
        &s,
        &h.repo,
        &r,
        Some(&h.parent),
        AGENT,
        NATIVE,
        1,
        Boundary::Start,
        COMMITTED_AT,
        || Ok(COMMITTED_AT),
    )
    .await
    .unwrap();
    assert_raw_anchor(
        &s,
        &r,
        COMMITTED_AT,
        rift_store::BoundaryCollectionInterval {
            started_at: COMMITTED_AT,
            finished_at: COMMITTED_AT,
        },
    )
    .await;
    assert!(hunk_paths(&s).await.is_empty());
    assert_commit_stays_unknown(&s, &r, &h.committed).await;
}

#[tokio::test]
async fn later_collections_and_new_sessions_preserve_regular_observations() {
    let h = history();
    let s = store();
    let r = reader(h.tmp.path());
    capture_boundary(
        &s,
        &h.repo,
        &r,
        Some(&h.parent),
        AGENT,
        NATIVE,
        1,
        Boundary::Start,
        INSTANT + 10_000,
    )
    .await
    .unwrap();
    let later = b"one\nLATER_REGULAR\n";
    std::fs::write(h.tmp.path().join("a.txt"), later).unwrap();
    stamp(&h.tmp.path().join("a.txt"), INSTANT + 35_000);
    for (native, ordinal, at) in [
        (NATIVE, 2, INSTANT + 40_000),
        ("new-session", 0, INSTANT + 50_000),
    ] {
        crate::turn_boundary::capture_boundary_with_clock(
            &s,
            &h.repo,
            &r,
            Some(&h.committed),
            AGENT,
            native,
            ordinal,
            Boundary::Start,
            at,
            || Ok(at + 1_000),
        )
        .await
        .unwrap();
        let session = ids::session_row_id(AGENT, native);
        let snapshot = latest_turn_boundary_snapshot(&s, &session)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.captured_at, at);
        let kinds = turn_boundary_file_kinds_for_snapshot(&s, &snapshot.snapshot_id)
            .await
            .unwrap();
        assert_eq!(kinds.get("a.txt"), Some(&BoundaryFileKind::Regular));
        let files = turn_boundary_files_for_snapshot(&s, &snapshot.snapshot_id)
            .await
            .unwrap();
        let file = files.iter().find(|file| file.path == "a.txt").unwrap();
        assert_eq!(r.read_blob(&file.blob_oid).unwrap().unwrap(), later);
        let mut capture = capture_commit(&r, &h.committed).unwrap();
        let before = format!("{capture:?}");
        crate::turn_boundary::enrich_attribution(&s, &r, AGENT, native, &mut capture)
            .await
            .unwrap();
        assert_eq!(format!("{capture:?}"), before);
        assert!(turn_diff_hunks_for_session(&s, &session)
            .await
            .unwrap()
            .is_empty());
    }
}

const COMMITTED_AT: i64 = INSTANT + 20_000;
const FUTURE_AT: i64 = INSTANT + 25_000;
const FUTURE_BYTES: &[u8] = b"one\nFUTURE_WORKTREE\n";

struct History {
    tmp: tempfile::TempDir,
    repo: RepoPaths,
    parent: String,
    committed: String,
}

fn commit_at(dir: &Path, time: i64, message: &str) -> String {
    let date = format!("@{} +0000", time / 1_000);
    let out = Command::new("git")
        .args(["commit", "-q", "-am", message])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out.stderr);
    git(dir, &["rev-parse", "HEAD"])
}

fn history() -> History {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "T"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    let path = dir.join("a.txt");
    std::fs::write(&path, b"one\nBASE\n").unwrap();
    stamp(&path, INSTANT);
    git(dir, &["add", "a.txt"]);
    let parent = commit_at(dir, INSTANT, "parent");
    std::fs::write(&path, b"one\nHUMAN_COMMITTED\n").unwrap();
    stamp(&path, INSTANT + 5_000);
    let committed = commit_at(dir, COMMITTED_AT, "human edit");
    std::fs::write(&path, FUTURE_BYTES).unwrap();
    stamp(&path, FUTURE_AT);
    let repo = rift_git::discover(dir).unwrap();
    History {
        tmp,
        repo,
        parent,
        committed,
    }
}

async fn assert_raw_anchor(
    s: &SqliteStore,
    r: &CountingReader,
    at: i64,
    interval: rift_store::BoundaryCollectionInterval,
) {
    let session = ids::session_row_id(AGENT, NATIVE);
    let snapshot = latest_turn_boundary_snapshot(s, &session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.captured_at, at);
    let audit = rift_store::turn_boundary_observation_for_snapshot(s, &snapshot.snapshot_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(audit.interval, interval);
    assert_eq!(turn_boundary_counts(s, &session).await.unwrap(), (1, 0));
    let kinds = turn_boundary_file_kinds_for_snapshot(s, &snapshot.snapshot_id)
        .await
        .unwrap();
    assert_eq!(kinds.get("a.txt"), Some(&BoundaryFileKind::Regular));
    let files = turn_boundary_files_for_snapshot(s, &snapshot.snapshot_id)
        .await
        .unwrap();
    let file = files.iter().find(|file| file.path == "a.txt").unwrap();
    let bytes = r.read_blob(&file.blob_oid).unwrap().unwrap();
    assert_eq!(bytes, FUTURE_BYTES);
}

async fn assert_commit_stays_unknown(s: &SqliteStore, r: &CountingReader, commit: &str) {
    let mut capture = capture_commit(r, commit).unwrap();
    assert_eq!(capture.files.len(), 1);
    let file = &capture.files[0];
    let baseline = (
        file.attribution,
        file.confidence.to_bits(),
        file.hunk_categories.clone(),
        file.method_version.clone(),
    );
    assert_eq!(
        baseline.0,
        LineAttribution {
            unknown_lines: 1,
            ..Default::default()
        }
    );
    assert!(baseline.2.is_empty());
    assert_eq!(baseline.3, "commit-diff@1");
    crate::turn_boundary::enrich_attribution(s, r, AGENT, NATIVE, &mut capture)
        .await
        .unwrap();
    let file = &capture.files[0];
    assert_eq!(
        (
            file.attribution,
            file.confidence.to_bits(),
            file.hunk_categories.clone(),
            file.method_version.clone()
        ),
        baseline,
        "later worktree evidence cannot identify the writer of this earlier commit"
    );
}

#[tokio::test]
async fn a_delayed_boundary_cannot_credit_future_bytes_to_an_older_commit() {
    let h = history();
    let s = store();
    let r = reader(h.tmp.path());
    let at = INSTANT + 10_000;
    assert!(at < COMMITTED_AT && COMMITTED_AT < FUTURE_AT);
    capture_boundary(
        &s,
        &h.repo,
        &r,
        Some(&h.parent),
        AGENT,
        NATIVE,
        1,
        Boundary::Start,
        at,
    )
    .await
    .unwrap();
    assert_raw_anchor(
        &s,
        &r,
        at,
        rift_store::BoundaryCollectionInterval {
            started_at: at + 60_000,
            finished_at: at + 60_001,
        },
    )
    .await;
    assert!(hunk_paths(&s).await.is_empty());
    assert_commit_stays_unknown(&s, &r, &h.committed).await;
}

#[tokio::test]
async fn a_genuinely_later_boundary_cannot_credit_an_older_commit() {
    let h = history();
    let s = store();
    let r = reader(h.tmp.path());
    let at = INSTANT + 30_000;
    assert!(COMMITTED_AT < FUTURE_AT && FUTURE_AT < at);
    capture_boundary(
        &s,
        &h.repo,
        &r,
        Some(&h.committed),
        AGENT,
        NATIVE,
        1,
        Boundary::Start,
        at,
    )
    .await
    .unwrap();
    assert_raw_anchor(
        &s,
        &r,
        at,
        rift_store::BoundaryCollectionInterval {
            started_at: at + 60_000,
            finished_at: at + 60_001,
        },
    )
    .await;
    assert!(hunk_paths(&s).await.is_empty());
    assert_commit_stays_unknown(&s, &r, &h.committed).await;
}
