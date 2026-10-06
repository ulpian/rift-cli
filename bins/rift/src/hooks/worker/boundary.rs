//! The turn-boundary snapshot a hook journaled, taken by the detached worker.
//!
//! Separate from the drain because it is the one job whose answer depends on
//! *when* it describes: the snapshot is held to the boundary's own instant and
//! `HEAD`, which the hook recorded, not to the moment the worker runs.

use rift_core::Result;

use crate::agent_queue::{AgentJob, BoundaryEdge, BoundarySnapshot};
use crate::hooks::projection_db_at;

/// Take one journaled boundary snapshot: the same capture the hook once ran
/// inline, given the hook's `HEAD` and instant.
///
/// A failure goes back to the drain, which keeps the job and counts the
/// attempt (or drops it on a refused row): the boundary is then missing from
/// the session, which the replay already reads as an unanchored window.
pub(super) fn boundary_snapshot_job(
    t: &crate::hooktime::HookTimer,
    job: &AgentJob,
    snapshot: &BoundarySnapshot,
) -> Result<()> {
    let repo = rift_git::discover(&job.work_tree)?;
    let reader = t.phase("gix_open", || {
        rift_git::GixBlobReader::open(&repo.work_tree)
    })?;
    let head = boundary_head(&reader, snapshot)?;
    let store = t.phase("open_projection", || {
        rift_store::open_capture_projection(projection_db_at(&repo.work_tree)?)
    })?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    t.phase(phase_name(snapshot.boundary), || {
        rt.block_on(rift_shadow::capture_boundary(
            &store,
            &repo,
            &reader,
            head.as_deref(),
            &snapshot.agent,
            &snapshot.session_id,
            snapshot.ordinal,
            snapshot.boundary.boundary(),
            snapshot.captured_at_ms,
        ))
    })
}

/// The commit the boundary's diffs start from: the one the hook read.
///
/// The hook reads `HEAD` from the ref files alone, and says nothing for an
/// unborn branch or a ref store those files do not hold. The worker's reader
/// tells the two apart, as of now: still unborn reads as nothing, and a ref
/// store only it understands names the commit the work tree is on.
fn boundary_head(
    reader: &rift_git::GixBlobReader,
    snapshot: &BoundarySnapshot,
) -> Result<Option<String>> {
    match &snapshot.head_oid {
        Some(oid) => Ok(Some(oid.clone())),
        None => reader.head_hex(),
    }
}

/// The timer's name for the capture, one per edge, so a drain's report says
/// which boundaries it took and in what order.
fn phase_name(edge: BoundaryEdge) -> &'static str {
    match edge {
        BoundaryEdge::Start => "boundary_start",
        BoundaryEdge::End => "boundary_end",
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::agent_queue::JobKind;
    use crate::hooks::worker::{drain_locked, run_agent_job, Landed};

    const SESSION: &str = "s-boundary";
    const AGENT: &str = "claude-code";
    /// An instant after every edit these fixtures make, fixed so no verdict
    /// reads the clock: 2100-01-01.
    const AFTER_THE_EDITS: i64 = 4_102_444_800_000;

    /// A committed checkout whose family is minted, with `a.txt` edited and
    /// `new.txt` created since the commit, and its `HEAD`.
    fn edited_checkout(root: &Path) -> (rift_git::RepoPaths, String) {
        git(root, &["init", "--quiet"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        git(root, &["add", "a.txt"]);
        git(root, &["commit", "--quiet", "-m", "fixture"]);
        crate::family::resolve_at(root).unwrap();
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(root.join("new.txt"), "alpha\n").unwrap();
        let head = git(root, &["rev-parse", "HEAD"]).trim().to_owned();
        (rift_git::discover(root).unwrap(), head)
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=Fixture", "-c", "user.email=fixture-id"])
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn snapshot_job(
        root: &Path,
        head: &str,
        ordinal: u32,
        edge: BoundaryEdge,
        at: i64,
    ) -> AgentJob {
        AgentJob {
            schema_version: 1,
            work_tree: root.to_owned(),
            queued_at_ms: at,
            attempts: 0,
            kind: JobKind::BoundarySnapshot(BoundarySnapshot {
                session_id: SESSION.into(),
                agent: AGENT.into(),
                ordinal,
                boundary: edge,
                head_oid: Some(head.to_owned()),
                captured_at_ms: at,
            }),
        }
    }

    /// Every boundary row a store holds, one line per row, in a fixed order.
    fn boundary_rows(store: &impl rift_store::CanonicalStore) -> Vec<String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let queries = [
            "SELECT snapshot_id || '|' || session_id || '|' || turn_ordinal || '|' || boundary \
             || '|' || actor || '|' || worktree_hash || '|' || captured_at \
             FROM turn_boundary_snapshot ORDER BY 1",
            "SELECT snapshot_id || '|' || path || '|' || blob_oid || '|' || content_sha256 \
             FROM turn_boundary_file ORDER BY 1",
            "SELECT hunk_id || '|' || edit_kind || '|' || path || '|' || old_start || '|' \
             || old_len || '|' || new_start || '|' || new_len FROM turn_diff_hunk ORDER BY 1",
        ];
        let mut lines = Vec::new();
        for sql in queries {
            for row in rt.block_on(store.query(sql, &[])).unwrap() {
                lines.push(row.get_text(0).unwrap());
            }
        }
        lines
    }

    /// The worker's snapshot of a journaled boundary is the snapshot the hook
    /// once took inline over the same tree, row for row.
    #[test]
    fn the_worker_records_the_rows_an_inline_snapshot_of_the_same_tree_records() {
        let checkout = tempfile::tempdir().unwrap();
        let (repo, head) = edited_checkout(checkout.path());
        let job = snapshot_job(
            checkout.path(),
            &head,
            1,
            BoundaryEdge::Start,
            AFTER_THE_EDITS,
        );
        let JobKind::BoundarySnapshot(snapshot) = &job.kind else {
            panic!("a boundary job");
        };
        let t = crate::hooktime::HookTimer::new("test", false);
        boundary_snapshot_job(&t, &job, snapshot).unwrap();
        let db = crate::family::projection_db(checkout.path()).unwrap();
        let worker = boundary_rows(&rift_store::SqliteStore::open_read_only(&db).unwrap());

        let inline = rift_store::SqliteStore::open_in_memory().unwrap();
        inline.create_canonical_schema().unwrap();
        let reader = rift_git::GixBlobReader::open(checkout.path()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(rift_shadow::capture_boundary(
            &inline,
            &repo,
            &reader,
            Some(&head),
            AGENT,
            SESSION,
            1,
            rift_shadow::Boundary::Start,
            AFTER_THE_EDITS,
        ))
        .unwrap();

        assert_eq!(worker, boundary_rows(&inline));
        assert_eq!(
            worker.len(),
            5,
            "one snapshot, two files, two hunks: {worker:?}"
        );
    }

    /// With a turn's end, the next turn's start and a commit all queued, one
    /// drain takes the end, then the start, and only then replays the commit
    /// — whatever order the files were written in.
    #[test]
    fn a_drain_takes_a_turns_end_then_the_next_start_and_both_before_the_commit() {
        let checkout = tempfile::tempdir().unwrap();
        let (repo, head) = edited_checkout(checkout.path());
        let entry = crate::capture_queue::QueueEntry {
            schema_version: 1,
            commit: head.clone(),
            work_tree: checkout.path().to_owned(),
            session_id: Some(SESSION.into()),
            agent: Some(AGENT.into()),
            session_state: Some("active".into()),
            queued_at_ms: AFTER_THE_EDITS - 10,
            attempts: 0,
        };
        crate::capture_queue::enqueue(&repo.git_dir, &entry).unwrap();
        let start = snapshot_job(
            checkout.path(),
            &head,
            2,
            BoundaryEdge::Start,
            AFTER_THE_EDITS - 20,
        );
        let end = snapshot_job(
            checkout.path(),
            &head,
            1,
            BoundaryEdge::End,
            AFTER_THE_EDITS - 30,
        );
        crate::agent_queue::enqueue(&repo.git_dir, &start).unwrap();
        crate::agent_queue::enqueue(&repo.git_dir, &end).unwrap();

        let t = crate::hooktime::HookTimer::new("test", true);
        drain_locked(&t, &repo.git_dir);

        let order: Vec<&str> = t
            .records()
            .into_iter()
            .map(|(phase, _)| phase)
            .filter(|phase| {
                ["boundary_start", "boundary_end", "enrich_attribution"].contains(phase)
            })
            .collect();
        assert_eq!(
            order,
            ["boundary_end", "boundary_start", "enrich_attribution"]
        );
    }

    /// A journaled boundary the worker cannot take (its checkout is gone) is
    /// kept with the attempt counted, like every other job.
    #[test]
    fn a_boundary_job_that_fails_is_kept_with_its_attempt_counted() {
        let git_dir = tempfile::tempdir().unwrap();
        let gone = git_dir.path().join("gone");
        let job = snapshot_job(&gone, "0".repeat(40).as_str(), 1, BoundaryEdge::End, 1);
        let path = crate::agent_queue::enqueue(git_dir.path(), &job).unwrap();
        let t = crate::hooktime::HookTimer::new("test", false);

        assert_eq!(
            run_agent_job(&t, git_dir.path(), &path, &job),
            Landed::Nothing
        );
        let (_, kept) = crate::agent_queue::jobs(git_dir.path()).pop().unwrap();
        assert_eq!(kept.attempts, 1);
    }
}
