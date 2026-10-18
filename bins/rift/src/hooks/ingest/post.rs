//! What follows a successful ingest: the working-tree snapshots the turn
//! boundaries imply, and the archive work the state machine asked for.
//!
//! Separate because both steps are best-effort and neither may fail the
//! agent's hook — and because every snapshot and every archive is journaled
//! for the detached worker rather than performed here.

use std::path::Path;

use rift_core::Result;

use super::{dispatch_agent_worker, CapturedHook};
use crate::agent_queue::{
    AgentJob, BoundaryEdge, BoundarySnapshot, IntervalArchive, IntervalReason, JobKind,
};
use crate::hooks::git::{effective_config, strategy_kind};
use crate::hooks::now_ms;

/// Journal the working-tree snapshot each boundary this hook closed owes,
/// for the detached worker. Returns whether any is owed — the caller then
/// sees the worker dispatched, once, with whatever else this hook journaled.
///
/// The hook records the queued HEAD and boundary event time. The worker later
/// collects current bounded working-tree bytes; these are observations, not
/// proof of bytes at the event time or of who wrote them. Identical complete
/// deliveries keep their retained attempts. Changed context stays separate.
///
/// Takes the RESOLVED work tree (never the process cwd: a hook's cwd is not
/// guaranteed to be the repository). A journaling failure is **said once**
/// rather than swallowed (cause E of `turn_boundary`'s register): it lands
/// on the capture queue's note channel under `boundary-<sha16(session)>`, so
/// re-noticing overwrites instead of accumulating, and `rift doctor` reports
/// it without consuming it.
pub(super) fn snapshot_boundaries(
    run: &CapturedHook<'_>,
    agent: &str,
    boundaries: &[(u32, rift_shadow::Boundary)],
) -> bool {
    if boundaries.is_empty() {
        return false;
    }
    let session = run.raw.session.as_str();
    let head_oid = rift_git::head_state(run.repo).oid;
    let captured_at_ms = now_ms();
    for (ordinal, boundary) in boundaries {
        let kind = JobKind::BoundarySnapshot(BoundarySnapshot {
            session_id: session.to_owned(),
            agent: agent.to_owned(),
            ordinal: *ordinal,
            boundary: BoundaryEdge::of(*boundary),
            head_oid: head_oid.clone(),
            captured_at_ms,
        });
        if let Err(e) = journal_once(run.repo, kind, captured_at_ms) {
            note_boundary_failure(&run.repo.git_dir, session, &e);
        }
    }
    true
}

/// Publish the complete delivery context without replacing a pending context.
fn journal_once(repo: &rift_git::RepoPaths, kind: JobKind, queued_at_ms: i64) -> Result<()> {
    crate::agent_queue::enqueue(
        &repo.git_dir,
        &AgentJob {
            schema_version: 1,
            work_tree: repo.work_tree.clone(),
            queued_at_ms,
            attempts: 0,
            kind,
        },
    )?;
    Ok(())
}

/// Say a failed boundary snapshot once, on the capture queue's note channel.
///
/// Keyed by the session so re-noticing overwrites instead of accumulating, and
/// left for `rift doctor` to report without consuming it.
fn note_boundary_failure(git_dir: &Path, session: &str, e: &rift_core::RiftError) {
    let digest = rift_shadow::sha256_hex(&[session.as_bytes()]);
    let key = format!("boundary-{}", digest.get(..16).unwrap_or_default());
    crate::capture_queue::mark_note(
        git_dir,
        &key,
        &format!("turn-boundary collection could not be journaled ({e})"),
    );
}

/// Journal the archive work an agent hook may have triggered for the detached
/// worker: the state machine's `RequestCondense` effect (session end) and the
/// `scheduled` strategy's interval archive. Returns whether the worker is now
/// dispatched — for either of those, or for the boundary snapshots
/// [`snapshot_boundaries`] journaled (`snapshots_owed`) — so the caller need
/// not spawn a second.
pub(super) fn drive_post_ingest(
    run: &CapturedHook<'_>,
    agent: &str,
    state: rift_shadow::hooks::SessionState,
    effects: &[rift_shadow::hooks::Effect],
    snapshots_owed: bool,
) -> Result<bool> {
    let archived = drive_archives(run, agent, state, effects);
    if snapshots_owed && !matches!(archived, Ok(true)) {
        dispatch_agent_worker(&run.repo.git_dir);
        return Ok(true);
    }
    archived
}

/// The session-end condense or the interval archive, whichever this hook
/// owes; `true` when one was journaled and its worker dispatched.
fn drive_archives(
    run: &CapturedHook<'_>,
    agent: &str,
    state: rift_shadow::hooks::SessionState,
    effects: &[rift_shadow::hooks::Effect],
) -> Result<bool> {
    use rift_shadow::hooks::Effect;

    let session = run.raw.session.as_str();
    if effects.contains(&Effect::RequestCondense) {
        queue_session_condense(&run.repo.git_dir, run.repo_id, session, agent)?;
        return Ok(true);
    }
    drive_scheduled_archive(run, agent, state)
}

/// Journal the session-end condense for the detached worker, and hand it over.
///
/// The condense itself — redact, shard, chain append, then Ending → Ended —
/// measured p50 647.80 ms with a transcript against a 100 ms agent-hook
/// budget, so it is JOURNALED here and performed by the detached worker
/// (`condense_agent_job`), exactly as `post-commit` defers its own. The job is
/// durable before the spawn: a worker that never starts leaves it for the next
/// worker or `rift doctor`.
///
/// Everything the old inline branch said about closure still holds in the
/// worker: `ended` is claimed only by a checkpoint that exists, and a skipped
/// or failed archive leaves the session `ending` for `sweep_stale_sessions` to
/// retry. The done-marker an earlier end left is cleared first: this is a new
/// end of a session that re-opened, and it owes its own condense.
fn queue_session_condense(git_dir: &Path, repo_id: &str, session: &str, agent: &str) -> Result<()> {
    let cwd = std::env::current_dir()?;
    crate::agent_queue::clear_condensed(git_dir, session);
    crate::agent_queue::enqueue(
        git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: rift_git::discover(&cwd)?.work_tree,
            queued_at_ms: now_ms(),
            attempts: 0,
            kind: crate::agent_queue::JobKind::Condense {
                session_id: session.to_owned(),
                agent: agent.to_owned(),
                repo_id: repo_id.to_owned(),
            },
        },
    )?;
    dispatch_agent_worker(git_dir);
    Ok(())
}

/// Ask the `scheduled` strategy whether this hook's activity has earned a
/// checkpoint, and journal the archive for the worker if it has. Returns
/// whether a job was journaled — its worker is then already dispatched.
///
/// The archive redacts the session's whole transcript, which on a long
/// session is megabytes: performed here, it outlasted the client's hook
/// timeout, the hook was killed inside it, nothing after it ran, and the next
/// hook began it again, so an interval checkpoint was never minted at all.
/// The hook therefore decides and the worker archives (`interval_archive_job`),
/// as with the session-end condense. The decision reads the store this hook
/// already holds; the commit the checkpoint anchors to is the worker's to
/// read, at drain time, and a missing one defers silently there.
fn drive_scheduled_archive(
    run: &CapturedHook<'_>,
    agent: &str,
    state: rift_shadow::hooks::SessionState,
) -> Result<bool> {
    use rift_shadow::hooks::strategy::{decide, TriggerCtx};

    let session = run.raw.session.as_str();
    let cfg = effective_config(&run.repo.work_tree);
    let now = now_ms();
    // The scheduled strategy fires from agent activity too — a long session
    // without commits still checkpoints on its interval.
    let last = run
        .rt
        .block_on(rift_store::latest_archive_ms(run.store, session))?;
    let decision = decide(
        strategy_kind(cfg.checkpoint_strategy()),
        &TriggerCtx {
            git_event: None,
            session_state: state,
            // The sensitive-path overlay is decided where a change is known
            // whole: the worker's condense of a queued commit, from its diff.
            sensitive_path_touched: false,
            clock_ms: now,
            last_archive_ms: last,
            schedule_interval_ms: cfg.schedule_interval_ms(),
        },
    );
    if !decision.archive {
        return Ok(false);
    }
    let Some(reason) = IntervalReason::of(decision.reason) else {
        return Ok(false);
    };
    let archive = IntervalArchive {
        session_id: session.to_owned(),
        agent: agent.to_owned(),
        reason,
        interval_start_ms: last,
    };
    queue_interval_archive(run.repo, archive, now)?;
    Ok(true)
}

/// Journal one interval archive and hand it over.
///
/// One archive is owed per interval: the interval it closes is part of the
/// job's identity, so a job already pending for it answers every later hook
/// inside the interval, on the clock of the hook that first found it due. The
/// worker is dispatched either way, because the one a pending job's hook
/// started may have gone. A deferred interval archive changes no lifecycle:
/// the session stays live and the next activity asks again.
fn queue_interval_archive(
    repo: &rift_git::RepoPaths,
    archive: IntervalArchive,
    now: i64,
) -> Result<()> {
    journal_once(repo, JobKind::Archive(archive), now)?;
    dispatch_agent_worker(&repo.git_dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A boundary journaled twice keeps the job and the instant of its first
    /// delivery: the redelivery owes the same snapshot, of the same moment.
    #[test]
    fn identical_boundary_context_keeps_attempts_and_changed_instants_remain_owed() {
        let checkout = tempfile::tempdir().unwrap();
        let initialized = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(checkout.path())
            .status()
            .unwrap();
        assert!(initialized.success());
        let repo = rift_git::discover(checkout.path()).unwrap();
        let kind = |at: i64| {
            JobKind::BoundarySnapshot(BoundarySnapshot {
                session_id: "s-1".into(),
                agent: "claude-code".into(),
                ordinal: 1,
                boundary: BoundaryEdge::End,
                head_oid: None,
                captured_at_ms: at,
            })
        };

        journal_once(&repo, kind(100), 100).unwrap();
        let initial = crate::agent_queue::jobs(&repo.git_dir);
        assert_eq!(initial.len(), 1);
        let owner = crate::capture_queue::acquire_owner(&repo.git_dir).unwrap();
        let state = crate::agent_queue::record_failure_owned(
            &initial[0].0,
            &initial[0].1,
            "fixture execution failure",
            &owner,
            1,
        )
        .unwrap();
        assert!(!state.parked && state.attempts == 1, "kept, due later");
        let retained = std::fs::read(&initial[0].0).unwrap();
        journal_once(&repo, kind(100), 100).unwrap();
        assert_eq!(
            std::fs::read(&initial[0].0).unwrap(),
            retained,
            "identical full context never resets attempts"
        );
        assert_eq!(
            crate::agent_queue::read_job(&initial[0].0)
                .unwrap()
                .1
                .attempts,
            1
        );
        journal_once(&repo, kind(200), 200).unwrap();
        let jobs = crate::agent_queue::jobs(&repo.git_dir);
        assert_eq!(
            jobs.len(),
            2,
            "changed event and queue instants are separate owed contexts"
        );
        assert!(jobs
            .iter()
            .any(|(_, job)| job.kind == kind(100) && job.queued_at_ms == 100 && job.attempts == 1));
        assert!(jobs
            .iter()
            .any(|(_, job)| job.kind == kind(200) && job.queued_at_ms == 200 && job.attempts == 0));
    }
}
