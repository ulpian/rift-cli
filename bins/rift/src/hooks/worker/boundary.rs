//! The turn-boundary snapshot a hook journaled, taken by the detached worker.
//!
//! The hook's HEAD/time identify the event; the collection interval audits
//! later current bytes and cannot establish event-time content or writers.
//! Queue ownership controls mutation, not historical authorship.

use rift_core::Result;

use crate::agent_queue::{AgentJob, BoundaryEdge, BoundarySnapshot};
use crate::hooks::projection_db_at;

/// Collect bounded current bytes for queued boundary metadata under the owner.
/// The event HEAD/time remain separate from the later collection audit.
/// Execution failures return to the bounded retry path; unavailable admission
/// evidence stays owed without spending execution attempts. Missing or incomplete
/// observations stay missing; neither collection nor retry certifies authorship.
pub(super) fn boundary_snapshot_job(
    t: &crate::hooktime::HookTimer,
    job: &AgentJob,
    snapshot: &BoundarySnapshot,
) -> Result<()> {
    boundary_snapshot_job_with_clock(t, job, snapshot, rift_shadow::boundary_collection_time)
}

fn boundary_snapshot_job_with_clock(
    t: &crate::hooktime::HookTimer,
    job: &AgentJob,
    snapshot: &BoundarySnapshot,
    clock: impl FnMut() -> Result<i64>,
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
        rt.block_on(rift_shadow::capture_boundary_with_clock(
            &store,
            &repo,
            &reader,
            head.as_deref(),
            &snapshot.agent,
            &snapshot.session_id,
            snapshot.ordinal,
            snapshot.boundary.boundary(),
            snapshot.captured_at_ms,
            clock,
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
    /// Fixed event time for fixtures. Direct collection tests inject separate
    /// read intervals; drain tests use the production clock without comparing
    /// it to this synthetic event time: 2100-01-01.
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
            "SELECT snapshot_id || '|' || worktree_hash || '|' || collection_started_at || '|' \
             || collection_finished_at FROM turn_boundary_observation ORDER BY 1",
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
        let mut worker_tick = AFTER_THE_EDITS + 60_000;
        boundary_snapshot_job_with_clock(&t, &job, snapshot, || {
            let at = worker_tick;
            worker_tick += 1;
            Ok(at)
        })
        .unwrap();
        let db = crate::family::projection_db(checkout.path()).unwrap();
        let worker = boundary_rows(&rift_store::SqliteStore::open_read_only(&db).unwrap());

        let inline = rift_store::SqliteStore::open_in_memory().unwrap();
        inline.create_canonical_schema().unwrap();
        let reader = rift_git::GixBlobReader::open(checkout.path()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mut inline_tick = AFTER_THE_EDITS + 60_000;
        rt.block_on(rift_shadow::capture_boundary_with_clock(
            &inline,
            &repo,
            &reader,
            Some(&head),
            AGENT,
            SESSION,
            1,
            rift_shadow::Boundary::Start,
            AFTER_THE_EDITS,
            || {
                let at = inline_tick;
                inline_tick += 1;
                Ok(at)
            },
        ))
        .unwrap();

        assert_eq!(worker, boundary_rows(&inline));
        assert_eq!(
            worker.len(),
            4,
            "one event, two current files, one collection audit, no actor hunks: {worker:?}"
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
        let owner = crate::capture_queue::acquire_owner(&repo.git_dir).unwrap();
        drain_locked(
            &t,
            &repo.git_dir,
            &owner,
            &mut std::collections::BTreeSet::new(),
            crate::hooks::worker::DrainPolicy::Due,
        );

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
        // Drain uses the checked production clock. Validate the audit shape,
        // without comparing real collection time to the synthetic event time.
        let store = rift_store::open_capture_projection(
            crate::family::projection_db(checkout.path()).unwrap(),
        )
        .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let id = rift_shadow::ids::session_row_id(AGENT, SESSION);
        let last = rt
            .block_on(rift_store::latest_turn_boundary_snapshot(&store, &id))
            .unwrap()
            .unwrap();
        let audit = rt
            .block_on(rift_store::turn_boundary_observation_for_snapshot(
                &store,
                &last.snapshot_id,
            ))
            .unwrap()
            .unwrap();
        assert!(audit.interval.started_at >= 0);
        assert!(audit.interval.finished_at >= audit.interval.started_at);
    }

    #[test]
    fn collection_clock_failure_or_reversal_does_not_persist_an_event_or_audit() {
        use rift_store::CanonicalStore as _;
        let checkout = tempfile::tempdir().unwrap();
        let (_, head) = edited_checkout(checkout.path());
        let job = snapshot_job(
            checkout.path(),
            &head,
            1,
            BoundaryEdge::Start,
            AFTER_THE_EDITS,
        );
        let JobKind::BoundarySnapshot(snapshot) = &job.kind else {
            panic!("boundary fixture");
        };
        let t = crate::hooktime::HookTimer::new("test", false);
        assert!(boundary_snapshot_job_with_clock(&t, &job, snapshot, || Err(
            rift_core::RiftError::storage("fixture collection clock unavailable")
        ),)
        .is_err());
        let mut at = 30;
        assert!(boundary_snapshot_job_with_clock(&t, &job, snapshot, || {
            let current = at;
            at -= 1;
            Ok(current)
        })
        .is_err());
        let store = rift_store::open_capture_projection(
            crate::family::projection_db(checkout.path()).unwrap(),
        )
        .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert_eq!(
            rt.block_on(store.count("turn_boundary_snapshot")).unwrap(),
            0
        );
        assert_eq!(
            rt.block_on(store.count("turn_boundary_observation"))
                .unwrap(),
            0
        );
    }

    /// A missing checkout leaves the original boundary delivery unattempted.
    #[test]
    fn a_gone_boundary_checkout_stays_owed_without_spending_an_attempt() {
        let git_dir = tempfile::tempdir().unwrap();
        let gone = git_dir.path().join("gone");
        let job = snapshot_job(&gone, "0".repeat(40).as_str(), 1, BoundaryEdge::End, 1);
        let path = crate::agent_queue::enqueue(git_dir.path(), &job).unwrap();
        let before = std::fs::read(&path).unwrap();
        let t = crate::hooktime::HookTimer::new("test", false);

        assert_eq!(
            run_agent_job(
                &t,
                git_dir.path(),
                &path,
                &job,
                &crate::capture_queue::acquire_owner(git_dir.path()).unwrap()
            ),
            Landed::Nothing
        );
        let (_, kept) = crate::agent_queue::jobs(git_dir.path()).pop().unwrap();
        assert_eq!(kept.attempts, 0);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    #[test]
    fn an_admitted_boundary_blob_write_failure_spends_exactly_three_attempts() {
        use rift_store::CanonicalStore as _;
        let tmp = tempfile::tempdir().unwrap();
        let (repo, head) = edited_checkout(tmp.path());
        let mut obstruction = None;
        for candidate in 0..256 {
            let bytes = format!("fresh boundary payload {candidate}\n");
            let oid = rift_recall_substrate::worktree::git_blob_oid(bytes.as_bytes());
            let fanout = repo.common_dir.join("objects").join(&oid[..2]);
            if !fanout.exists() {
                std::fs::write(tmp.path().join("new.txt"), bytes).unwrap();
                std::fs::write(&fanout, b"blob-write-obstruction").unwrap();
                obstruction = Some(fanout);
                break;
            }
        }
        let obstruction = obstruction.expect("unused fanout in bounded fixture");
        let job = snapshot_job(tmp.path(), &head, 1, BoundaryEdge::Start, 1);
        let path = crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap();
        let crate::agent_queue::JobKind::BoundarySnapshot(snapshot) = &job.kind else {
            panic!("boundary fixture")
        };
        crate::hooks::worker::ownership::require_new_boundary(&job, snapshot).unwrap();
        let db = projection_db_at(tmp.path()).unwrap();
        let store = rift_store::open_capture_projection(&db).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let facts = || {
            rt.block_on(store.query(
                "SELECT * FROM turn_boundary_snapshot ORDER BY snapshot_id",
                &[],
            ))
            .unwrap()
        };
        let before = facts();
        // L-0A Q5 / design §4.1: three failures park the job — present AND
        // parked with a backoff — where they used to remove it.
        for attempt in 1..=crate::capture_queue::PARK_AFTER {
            crate::hooks::worker::drain_jobs(
                repo.git_dir.to_str(),
                crate::hooks::worker::LockWait::None,
            );
            let (_, _, state) = crate::agent_queue::retry_states(&repo.git_dir).remove(0);
            assert_eq!(state.attempts, attempt);
            assert_eq!(state.parked, attempt == crate::capture_queue::PARK_AFTER);
            assert!(state.next_due_ms > 0 && path.exists());
            assert_eq!(
                std::fs::read(&obstruction).unwrap(),
                b"blob-write-obstruction"
            );
            assert_eq!(facts(), before);
        }
    }
}
