//! The detached capture worker: the queue drain under the family lock and the
//! jobs it runs (agent-queue jobs, token counts, basis snapshots, the document
//! and search-index passes, condensing a queued session). Separate because
//! nothing here runs inside a hook's latency budget; it is the work a hook
//! deferred.

mod boundary;
pub(in crate::hooks) mod linehash;
mod ownership;
mod reads;
mod recall_ingest;
mod repair_skip;

use std::path::Path;

use rift_core::Result;

use super::archive::{archive_now, bind_and_cache_outlines, condense_queued, level_projection};
use super::audit::{attribute_serve_job, serve_audit_job};
use super::git::{capture_guarded, effective_config, state_from_token, strategy_kind};
use super::recovery::hook_number;
use super::{flag, now_ms, projection_db_at};
pub(crate) use repair_skip::{leave_to_recovery, Historical, Leave};

use self::reads::count_tool_result_tokens_job;
pub(in crate::hooks) use self::reads::{snapshot_basis_job, BasisSnapshot};

/// `rift hooks worker --git-dir <dir>`: drain the capture queue. Diverges — the
/// worker is off git's critical path but it is still capture, so it keeps the
/// never-block guard: a panic here must not leave a held lock behind.
pub(super) fn run_capture_worker(args: &[String]) -> ! {
    let git_dir = flag(args, "--git-dir");
    capture_guarded(|| drain_queues(git_dir.as_deref(), LockWait::Brief).map(|_| ()));
}

/// How long a drain waits for the queue lock.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LockWait {
    /// Return at once if another worker holds it — the inline arms, which are
    /// inside a hook or a doctor pass that must not stall.
    None,
    /// Retry for a bounded while. The detached worker is off every critical
    /// path and can afford to wait, and it should: a hook that enqueued a job
    /// while the previous worker was on its last pass would otherwise leave
    /// that job to the next commit or the next `rift doctor`.
    Brief,
}

/// Which deliveries a drain runs (L-0A §4.2): the ordinary worker runs what
/// is due and unparked; the repair roads — the idle sweep's rescue, `rift
/// doctor --fix`, `rift session retry` — run everything, backoff and park
/// notwithstanding. Two listings, never a flag a caller passes as a literal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DrainPolicy {
    /// Due and unparked deliveries only.
    Due,
    /// Every readable delivery.
    Everything,
}

impl DrainPolicy {
    fn agent_jobs(
        self,
        git_dir: &Path,
        now: i64,
    ) -> Vec<(std::path::PathBuf, crate::agent_queue::AgentJob)> {
        match self {
            Self::Due => crate::agent_queue::jobs_due(git_dir, now),
            // A repair road still passes by the boundary tickets a `--fix`
            // run left to capture recovery.
            Self::Everything => crate::agent_queue::jobs(git_dir)
                .into_iter()
                .filter(|(path, job)| !repair_skip::left_to_recovery(path, job))
                .collect(),
        }
    }

    fn commits(
        self,
        git_dir: &Path,
        now: i64,
    ) -> Vec<(std::path::PathBuf, crate::capture_queue::QueueEntry)> {
        match self {
            Self::Due => crate::capture_queue::entries_due(git_dir, now),
            Self::Everything => crate::capture_queue::entries(git_dir),
        }
    }
}

/// What one agent-hook job left on the checkpoint branch.
#[derive(Debug, PartialEq, Eq)]
enum Landed {
    Nothing,
    /// A checkpoint that finalized no session: an interval archive, or a
    /// requested sweep that condensed.
    Checkpoint,
    /// A session-end condense: the session it finalized and its checkpoint.
    Condensed(String, String),
}

/// What one drain landed: the condenses its callers witness, and whether
/// anything at all wrote a checkpoint the projection has not seen.
#[derive(Default)]
struct Drained {
    condensed: Vec<(String, String)>,
    minted: bool,
    ready: bool,
}

/// How the levelling at a drain's tail ended. Every arm is a value: a drain
/// never fails on it.
#[derive(Debug, PartialEq, Eq)]
enum Levelled {
    Done,
    /// The projection is being replaced; the next drain that writes a
    /// checkpoint levels everything this one would have.
    Deferred,
    Failed,
}

/// The bound on [`LockWait::Brief`]: attempts × the pause between them.
const LOCK_WAIT_ATTEMPTS: u32 = 40;

const LOCK_WAIT_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

#[cfg(test)]
thread_local! {
    /// How many times this thread's drains levelled the projection.
    static LEVELS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Condense every queued commit, oldest first, under the drain lock.
///
/// An entry that fails is **kept** and marked, so the next worker retries it and
/// the next hook reports it; only a drained one is removed.
pub(crate) fn drain_capture_queue(git_dir: Option<&str>) -> Result<()> {
    let Some(git_dir) = git_dir else {
        return Ok(());
    };
    // `rift doctor --fix`'s road: the whole-family repair runs everything.
    drain_owned_passes(
        Path::new(git_dir),
        LockWait::None,
        true,
        DrainPolicy::Everything,
    );
    Ok(())
}

/// The retry verb's drain: every delivery, under a brief wait for the lock,
/// without the idle sweep at the tail (the operator named a session; a sweep
/// of the family is doctor's). The condenses that landed.
pub(crate) fn drain_everything(git_dir: &Path) -> Vec<(String, String)> {
    drain_owned_passes(git_dir, LockWait::Brief, false, DrainPolicy::Everything).condensed
}

/// Drain **both** journals under the one lock: the agent-hook jobs (token
/// counts, then session-end condenses) first, then the queued commits, and
/// then sweep the family's idle sessions.
///
/// One worker, two queues, and the order is load-bearing: a token count that
/// drained after the commit that archived its session's reads would be a
/// count the shard never carries. A pass that drained nothing new ends the
/// loop; a job that arrived during a pass is taken by the next.
///
/// The sweep at the tail is the placement that needs no trigger: a worker
/// already runs after every commit and every session end, so a session whose
/// agent went away without ending it is condensed at the family's idle
/// floor by whichever pass comes next, under the lock this pass holds.
///
/// A pass that wrote a checkpoint then levels the projection once, still
/// under the lock: a condense writes the shard and its chain entry, and
/// nothing else projects the shard's own rows, so without this the live
/// projection would hold none of the checkpoints a session end or a sweep
/// made. A queued commit is not counted, because its archive already levels
/// through the soak's verify.
pub(super) fn drain_queues(git_dir: Option<&str>, wait: LockWait) -> Result<Vec<(String, String)>> {
    let Some(git_dir) = git_dir else {
        return Ok(Vec::new());
    };
    let git_dir = Path::new(git_dir);
    // The ordinary worker: due and unparked only, so a not-due job costs no
    // attempt and a parked one waits for a hand (§4.2).
    Ok(drain_owned_passes(git_dir, wait, true, DrainPolicy::Due).condensed)
}

/// [`drain_queues`] without the idle sweep at its tail: the drain the
/// session sweep itself runs first, whose caller then sweeps exactly the
/// sessions it was asked to (a targeted `rift session stop` must never
/// finalize a neighbour).
///
/// Returns the `(session_id, checkpoint_id)` pairs the condense jobs landed,
/// so a caller that must witness them (the doctor sweep, which counts the
/// soak by condensations) can.
pub(super) fn drain_jobs(git_dir: Option<&str>, wait: LockWait) -> Vec<(String, String)> {
    let Some(git_dir) = git_dir.map(Path::new) else {
        return Vec::new();
    };
    // The sweep's rescue is a repair road: backoff and park are ignored.
    drain_owned_passes(git_dir, wait, false, DrainPolicy::Everything).condensed
}

/// Every job and queued commit, drained under the lock the caller holds.
///
/// Within a pass every journaled job runs before any queued commit, in the
/// journal's order. That is what keeps a turn's boundary snapshots ahead of
/// commit processing, and a turn's end ahead of the
/// next turn's start.
fn drain_locked(
    t: &crate::hooktime::HookTimer,
    git_dir: &Path,
    owner: &crate::capture_queue::QueueLock,
    attempted: &mut std::collections::BTreeSet<std::path::PathBuf>,
    policy: DrainPolicy,
) -> Drained {
    let mut drained = Drained::default();
    if crate::family::run_state::prepare_owned(owner).is_err() {
        return drained;
    }
    drained.ready = true;
    loop {
        let now = now_ms();
        // A path joins `attempted` only when its job runs, so a pass that
        // yields leaves the rest owed to whichever drain comes next.
        let agent_jobs: Vec<_> = policy
            .agent_jobs(git_dir, now)
            .into_iter()
            .filter(|(path, _)| !attempted.contains(path))
            .collect();
        let commits: Vec<_> = policy
            .commits(git_dir, now)
            .into_iter()
            .filter(|(path, _)| !attempted.contains(path))
            .collect();
        if agent_jobs.is_empty() && commits.is_empty() {
            return drained;
        }
        let pass = Pass { t, git_dir, owner };
        if pass.run_agent_jobs(agent_jobs, attempted, &mut drained) == Step::Yield {
            return drained;
        }
        if pass.run_commits(commits, attempted) == Step::Yield {
            return drained;
        }
        if recovery_requested(git_dir) {
            return drained;
        }
    }
}

/// What one pass runs under: the hook's clock, the git dir and the lock.
struct Pass<'a> {
    t: &'a crate::hooktime::HookTimer,
    git_dir: &'a Path,
    owner: &'a crate::capture_queue::QueueLock,
}

/// Whether a pass goes on to its next job or lets go of the queue.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Next,
    Yield,
}

impl Pass<'_> {
    /// Run the journaled jobs in order. A recovery request seen after a job
    /// stops the pass there: each job is atomic, so the boundary between two
    /// is safe, and one pass over a long queue would outlast `--fix`'s wait.
    fn run_agent_jobs(
        &self,
        jobs: Vec<(std::path::PathBuf, crate::agent_queue::AgentJob)>,
        attempted: &mut std::collections::BTreeSet<std::path::PathBuf>,
        drained: &mut Drained,
    ) -> Step {
        for (path, job) in jobs {
            attempted.insert(path.clone());
            match run_agent_job(self.t, self.git_dir, &path, &job, self.owner) {
                Landed::Nothing => (),
                Landed::Checkpoint => drained.minted = true,
                Landed::Condensed(session, checkpoint) => {
                    drained.minted = true;
                    drained.condensed.push((session, checkpoint));
                }
            }
            #[cfg(test)]
            run_state_tests::pause_after_job();
            if recovery_requested(self.git_dir) {
                return Step::Yield;
            }
        }
        Step::Next
    }

    /// Run the queued commits in order, yielding between them as the jobs do.
    fn run_commits(
        &self,
        commits: Vec<(std::path::PathBuf, crate::capture_queue::QueueEntry)>,
        attempted: &mut std::collections::BTreeSet<std::path::PathBuf>,
    ) -> Step {
        for (path, entry) in commits {
            attempted.insert(path.clone());
            let result = ownership::validate_capture(self.owner, &path, &entry)
                .and_then(|()| condense_queued(self.t, self.git_dir, &entry, &path));
            match result {
                Ok(()) => {
                    let _ = crate::capture_queue::remove_owned(&path, self.owner);
                }
                Err(e) if projection_busy(&e) || crate::capture_queue::is_deferred(&e) => (),
                Err(e) => {
                    let _ = crate::capture_queue::record_failure_owned(
                        &path,
                        &entry,
                        &e.to_string(),
                        self.owner,
                        now_ms(),
                    );
                }
            }
            if recovery_requested(self.git_dir) {
                return Step::Yield;
            }
        }
        Step::Next
    }
}

/// Whether `rift doctor --fix` waits for the queue: a worker then lets go at
/// the next job boundary, never inside a job. A stat of the marker only.
fn recovery_requested(git_dir: &Path) -> bool {
    crate::capture_queue::request::requested(git_dir, std::time::SystemTime::now())
}

/// Recheck after releasing ownership so a producer that saw the old owner
/// busy still gets a pass. Already attempted paths never trigger reacquisition.
fn drain_owned_passes(git_dir: &Path, wait: LockWait, sweep: bool, policy: DrainPolicy) -> Drained {
    let mut all = Drained::default();
    // A worker started under a request leaves the lock to recovery.
    if recovery_requested(git_dir) {
        return all;
    }
    let Some(mut owner) = acquire_queue_lock(git_dir, wait) else {
        return all;
    };
    let t = crate::hooktime::HookTimer::from_env("capture-worker");
    let mut attempted = std::collections::BTreeSet::new();
    loop {
        let pass = drain_locked(&t, git_dir, &owner, &mut attempted, policy);
        let mut minted = pass.minted;
        all.condensed.extend(pass.condensed);
        if !pass.ready {
            all.minted |= minted;
            return all;
        }
        all.ready = true;
        // A pass that yielded to recovery skips the sweep: recovery is waiting.
        if sweep && !recovery_requested(git_dir) {
            let swept = super::sweep::sweep_idle_family(&owner, now_ms(), None);
            minted |= !swept.condensed_checkpoints.is_empty();
        }
        all.minted |= minted;
        // Only a pass that wrote a checkpoint levels. Keying this on the
        // worker's whole life made every later pass re-walk the whole
        // checkpoint branch once anything had been written, and while agents
        // kept enqueueing jobs that kept the worker CPU-bound on the lock.
        if minted && crate::family::run_state::prepare_owned(&owner).is_ok() {
            let levelled = level_after_condense(git_dir);
            tracing::debug!(?levelled, "projection levelled under queue owner");
            #[cfg(test)]
            LEVELS.with(|levels| levels.set(levels.get() + 1));
            // The new checkpoint is recall's to ingest too. Asked for here and
            // run as a job, so the pending check below takes it in this
            // worker's next pass and many such passes owe one ingest.
            recall_ingest::owe_recall_ingest(git_dir);
        }
        #[cfg(test)]
        run_state_tests::pause_before_owner_release();
        drop(owner);
        if recovery_requested(git_dir) {
            return all;
        }
        // A not-due or parked job is not pending for a `Due` drain: a worker
        // never spins on a delivery its policy would not run.
        let now = now_ms();
        let pending = policy
            .agent_jobs(git_dir, now)
            .into_iter()
            .map(|(path, _)| path)
            .chain(
                policy
                    .commits(git_dir, now)
                    .into_iter()
                    .map(|(path, _)| path),
            )
            .any(|path| !attempted.contains(&path));
        if !pending {
            return all;
        }
        let Some(next) = acquire_queue_lock(git_dir, LockWait::None) else {
            return all;
        };
        owner = next;
    }
}

/// Take the drain lock, waiting per `wait`.
fn acquire_queue_lock(git_dir: &Path, wait: LockWait) -> Option<crate::capture_queue::QueueLock> {
    let attempts = match wait {
        LockWait::None => 1,
        LockWait::Brief => LOCK_WAIT_ATTEMPTS,
    };
    for i in 0..attempts {
        if let Some(lock) = crate::capture_queue::acquire_lock(git_dir, now_ms()) {
            return Some(lock);
        }
        if i + 1 < attempts {
            std::thread::sleep(LOCK_WAIT_PAUSE);
        }
    }
    None
}

/// Level the projection with the checkpoint branch after a drain that wrote
/// to it: the same walk `rift rebuild` runs, through the guarded projection
/// open every writer uses. The walk reads every shard on the branch (the
/// watermark is recorded, not yet used to skip), and each row is
/// insert-or-ignore, so a shard already projected costs a read and no write.
///
/// Best-effort and never retried here. A replacement in progress is not a
/// failure and costs no job an attempt, because no job owes this: the next
/// drain that writes a checkpoint levels everything this one would have.
fn level_after_condense(git_dir: &Path) -> Levelled {
    let Some(work_tree) = rift_shadow::hooks::session_cache::work_tree_of(git_dir) else {
        return Levelled::Failed;
    };
    let levelled =
        rift_git::discover(&work_tree).and_then(|repo| level_projection(&repo.work_tree));
    match levelled {
        Ok(()) => Levelled::Done,
        Err(e) if projection_busy(&e) => Levelled::Deferred,
        Err(e) => {
            tracing::warn!(error = %e, "projection not levelled after a condense");
            Levelled::Failed
        }
    }
}

/// Run one journaled agent-hook job and settle its file: removed when done,
/// kept-and-marked (bounded) when it failed, dropped-and-marked when the
/// schema refused its row, which no retry can change. Says what the job left
/// on the checkpoint branch, naming the session and checkpoint of a condense.
fn run_agent_job(
    t: &crate::hooktime::HookTimer,
    git_dir: &Path,
    path: &Path,
    job: &crate::agent_queue::AgentJob,
    owner: &crate::capture_queue::QueueLock,
) -> Landed {
    let result = ownership::validate_job(owner, path, job)
        .and_then(|context| run_job_kind(t, git_dir, job, owner, &context));
    match result {
        Ok(landed) => {
            let _ = crate::capture_queue::remove_owned(path, owner);
            crate::capture_queue::forget_reason(git_dir, &crate::agent_queue::note_key(job));
            landed
        }
        Err(e) if crate::capture_queue::is_deferred(&e) => {
            if let Some(reason) = crate::capture_queue::defer_reason(&e) {
                crate::capture_queue::mark_note(
                    git_dir,
                    &crate::agent_queue::note_key(job),
                    &format!("{}: {reason}", crate::agent_queue::note_key(job)),
                );
            }
            Landed::Nothing
        }
        Err(e) if projection_busy(&e) => Landed::Nothing,
        Err(e @ rift_core::RiftError::ConstraintRefused { .. }) => {
            let key = crate::agent_queue::note_key(job);
            crate::capture_queue::mark_note(git_dir, &key, &format!("{e} (dropped)"));
            let _ = crate::capture_queue::remove_owned(path, owner);
            Landed::Nothing
        }
        Err(e) => {
            let _ = crate::agent_queue::record_failure_owned(
                path,
                job,
                &e.to_string(),
                owner,
                now_ms(),
            );
            Landed::Nothing
        }
    }
}

/// Record a basis snapshot that completed `unavailable` before the drain
/// removes its job file, so a completion that found the bytes gone is never
/// mistaken for one that kept them.
fn basis_landed(
    owner: &crate::capture_queue::QueueLock,
    job: &crate::agent_queue::AgentJob,
    outcome: &BasisSnapshot,
) -> Landed {
    if *outcome == BasisSnapshot::Unavailable {
        crate::agent_queue::record_unavailable(owner.git_dir(), job);
    }
    Landed::Nothing
}

/// The move a hook asked for. The drain already moved before it listed this
/// job; moving again takes whatever an older binary wrote since, and costs a
/// `stat` otherwise.
fn move_run_state_job(owner: &crate::capture_queue::QueueLock) -> Result<Landed> {
    crate::family::run_state::prepare_owned(owner)?;
    Ok(Landed::Nothing)
}

fn landed_condense(pair: Option<(String, String)>) -> Landed {
    pair.map_or(Landed::Nothing, |(session, checkpoint)| {
        Landed::Condensed(session, checkpoint)
    })
}

/// The work of one journaled job, by its kind.
fn run_job_kind(
    t: &crate::hooktime::HookTimer,
    _git_dir: &Path,
    job: &crate::agent_queue::AgentJob,
    owner: &crate::capture_queue::QueueLock,
    context: &serde_json::Value,
) -> Result<Landed> {
    use crate::agent_queue::JobKind;

    match &job.kind {
        JobKind::Condense {
            session_id,
            agent,
            repo_id,
        } => condense_agent_job(t, job, session_id, agent, repo_id, owner, context)
            .map(landed_condense),
        JobKind::Archive(archive) => interval_archive_job(t, job, archive).map(landed_if),
        JobKind::SealTurn(seal) => super::turn_end::turn_end_job(t, job, seal).map(landed_if),
        JobKind::ReadTokens {
            tool_call_id,
            rel_path,
            content_sha256,
            blob_oid,
            lines,
            schema_version,
        } => count_read_tokens_job(
            t,
            job,
            tool_call_id,
            rel_path,
            content_sha256,
            blob_oid,
            *lines,
            *schema_version,
        )
        .map(|()| Landed::Nothing),
        JobKind::ServeAudit(audit) => serve_audit_job(t, job, audit).map(|()| Landed::Nothing),
        JobKind::AttributeServe(claim) => {
            attribute_serve_job(t, job, claim).map(|()| Landed::Nothing)
        }
        JobKind::SearchIndex => rebuild_search_index_job(t, job).map(|()| Landed::Nothing),
        JobKind::DocConvert {
            rel_path,
            content_sha256,
        } => convert_document_job(t, job, rel_path, content_sha256).map(|()| Landed::Nothing),
        JobKind::DocSweep => sweep_documents_job(t, job).map(|()| Landed::Nothing),
        JobKind::SweepIdle { floor_ms } => Ok(sweep_idle_job(t, owner, job, *floor_ms)),
        JobKind::MoveRunState => move_run_state_job(owner),
        JobKind::RecallIngest => recall_ingest::recall_ingest_job(job).map(|()| Landed::Nothing),
        JobKind::BoundarySnapshot(snapshot) => {
            ownership::require_new_boundary(job, snapshot)?;
            boundary::boundary_snapshot_job(t, job, snapshot).map(|()| Landed::Nothing)
        }
        JobKind::BasisSnapshot {
            rel_path,
            content_sha256,
            blob_oid,
        } => snapshot_basis_job(t, job, rel_path, content_sha256, blob_oid)
            .map(|outcome| basis_landed(owner, job, &outcome)),
        JobKind::ToolResultTokens {
            tool_result_id,
            session_id,
            tool_name,
            channel,
            search,
            tree_key,
        } => count_tool_result_tokens_job(
            t,
            job,
            tool_result_id,
            session_id,
            tool_name,
            channel,
            search,
            tree_key.as_ref(),
        )
        .map(|()| Landed::Nothing),
    }
}

/// Context wrappers preserve typed causes; Display text never classifies a
/// deferred job. A busy attempt leaves its existing retry budget untouched.
fn projection_busy(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut cause = Some(error);
    while let Some(error) = cause {
        if matches!(
            error.downcast_ref::<rift_core::RiftError>(),
            Some(rift_core::RiftError::ProjectionBusy { .. })
        ) {
            return true;
        }
        cause = error.source();
    }
    false
}

/// The sweep a session start asked for, on the requesting hook's clock and
/// floor. Never a failure: a member that cannot be swept is left for the
/// next pass, which is what the tail sweep of every drain is.
fn sweep_idle_job(
    t: &crate::hooktime::HookTimer,
    owner: &crate::capture_queue::QueueLock,
    job: &crate::agent_queue::AgentJob,
    floor_ms: i64,
) -> Landed {
    let swept = t.phase("sweep_idle", || {
        super::sweep::sweep_idle_family(owner, job.queued_at_ms, Some(floor_ms))
    });
    landed_if(!swept.condensed_checkpoints.is_empty())
}

/// [`Landed::Checkpoint`] when a job that finalizes no session wrote one.
fn landed_if(minted: bool) -> Landed {
    if minted {
        Landed::Checkpoint
    } else {
        Landed::Nothing
    }
}

/// The conversion budget one document gets in the detached worker.
///
/// Deliberately not the gate path's 250 ms. That deadline is a ceiling on
/// what a developer waits with an agent stalled in front of them; here nobody
/// is waiting, the work has already been moved off the path that measured it,
/// and cutting a 15 MiB workbook off at a quarter of a second would simply
/// mean the document is never converted at all. What the budget is still for
/// is a parser that has stopped making progress: past it the child is killed,
/// the pass moves on, and the next pass tries again.
#[cfg(feature = "doc")]
const WORKER_CONVERT_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// The worker's converter: this binary, re-spawned on the hidden `hooks doc`
/// route, under the worker's budget.
///
/// `None` only when the running executable cannot name its own path, which
/// is treated like every other refusal: the document is left unconverted.
/// The route is the seam's one decision about what to spawn
/// (`crate::doc_helper`), not a second copy of it here.
#[cfg(feature = "doc")]
fn document_converter() -> Option<rift_recall_doc::ChildProcessConverter> {
    crate::doc_helper::self_exec_converter(WORKER_CONVERT_BUDGET)
}

/// Convert one journaled document into the family's sidecar store.
///
/// Everything this can fail at is a value rather than an error: no family, no
/// helper, a policy that is off, a document that has moved on since the job
/// was written. The job is drained either way — a retry would meet the same
/// answer, and the proactive pass converts whatever is still owed.
#[cfg(feature = "doc")]
fn convert_document_job(
    t: &crate::hooktime::HookTimer,
    job: &crate::agent_queue::AgentJob,
    rel_path: &str,
    content_sha256: &str,
) -> Result<()> {
    let repo = rift_git::discover(&job.work_tree)?;
    let Some(family) = crate::family::try_at(&repo.work_tree) else {
        return Ok(());
    };
    // The repository's own `[doc]` table, not a family's resolved levers:
    // proactive conversion is not a decision about any agent — every family
    // reads the same sidecars, through a rewrite, a redirect or the verb — so
    // the per-family caps that bound a *decline* have nothing to bound here.
    // An unreadable policy fails closed to every lever off, and this with it.
    let policy = rift_recall_policy::ServePolicy::load(&repo.work_tree.join(".rift"));
    if policy.doc.mode == rift_recall_policy::Mode::Off {
        return Ok(());
    }
    if !crate::doc_store::is_convertible_path(&effective_config(&repo.work_tree), &policy, rel_path)
    {
        return Ok(());
    }
    let (Some(path), Some(converter)) = (
        crate::doc_store::document_path(&repo.work_tree, rel_path),
        document_converter(),
    ) else {
        return Ok(());
    };
    let store = crate::doc_store::DocStore::for_member(&family.common_dir, &repo.work_tree);
    let outcome = t.phase("doc_convert", || {
        crate::doc_store::convert_into_store(
            &store,
            &converter,
            &path,
            &policy.doc,
            Some(content_sha256),
        )
    });
    tracing::debug!(rel_path, ?outcome, "document conversion");
    Ok(())
}

/// The same job in a build without the document seam: journaled, drained, and
/// honestly not done.
#[cfg(not(feature = "doc"))]
fn convert_document_job(
    _t: &crate::hooktime::HookTimer,
    _job: &crate::agent_queue::AgentJob,
    rel_path: &str,
    _content_sha256: &str,
) -> Result<()> {
    tracing::debug!(rel_path, "document conversion requested; not in this build");
    Ok(())
}

/// One proactive pass over this work tree's tracked documents.
///
/// Ahead of anybody asking, and off every path anybody is waiting on: the
/// documents a repository holds change when somebody commits and are unknown
/// when a session starts, and both of those moments are followed by a worker
/// that has nothing else to do. What it buys is the difference between an
/// agent's first document read costing a parse and costing a `stat` — and,
/// because the answers land in the family store, one member's pass is every
/// member's.
#[cfg(feature = "doc")]
fn sweep_documents_job(
    t: &crate::hooktime::HookTimer,
    job: &crate::agent_queue::AgentJob,
) -> Result<()> {
    let repo = rift_git::discover(&job.work_tree)?;
    let Some(family) = crate::family::try_at(&repo.work_tree) else {
        return Ok(());
    };
    let policy = rift_recall_policy::ServePolicy::load(&repo.work_tree.join(".rift"));
    if policy.doc.mode == rift_recall_policy::Mode::Off {
        return Ok(());
    }
    let Some(converter) = document_converter() else {
        return Ok(());
    };
    let store = crate::doc_store::DocStore::for_member(&family.common_dir, &repo.work_tree);
    let report = t.phase("doc_sweep", || {
        crate::doc_store::convert_tracked_documents(
            &repo,
            &store,
            &converter,
            &effective_config(&repo.work_tree),
            &policy,
            crate::doc_store::SWEEP_LIMIT,
        )
    });
    tracing::debug!(?report, "document pass");
    Ok(())
}

/// The same pass in a build without the document seam.
#[cfg(not(feature = "doc"))]
fn sweep_documents_job(
    _t: &crate::hooktime::HookTimer,
    _job: &crate::agent_queue::AgentJob,
) -> Result<()> {
    tracing::debug!("document pass requested; not in this build");
    Ok(())
}

/// Rebuild this work tree's path index, out of band.
///
/// The walk is the expensive half of search and nothing is waiting on it here,
/// so the worker takes it whole: no deadline, the sensitive globs the
/// repository's own policy declares, and a file written at the end that the
/// next caller loads in place of walking anything.
///
/// Everything it can fail at is a value rather than an error — a work tree
/// that has gone, a policy that is off, a tree over the path cap, an index
/// that could not be written. The job is drained either way: a retry would
/// meet the same answer, and the next session start asks again.
#[cfg(feature = "search")]
pub(super) fn rebuild_search_index_job(
    t: &crate::hooktime::HookTimer,
    job: &crate::agent_queue::AgentJob,
) -> Result<()> {
    let repo = rift_git::discover(&job.work_tree)?;
    // The repository's own `[search]` table, not a family's resolved levers:
    // an index is not a decision about any agent, and an unreadable policy
    // fails closed to every lever off, this with it.
    let policy = rift_recall_policy::ServePolicy::load(&repo.work_tree.join(".rift"));
    if policy.search.mode == rift_recall_policy::Mode::Off {
        return Ok(());
    }
    let config = effective_config(&repo.work_tree);
    let head = rift_recall_search::head_oid(&repo.work_tree);
    let built = t.phase("search_index", || {
        rift_recall_search::rebuild(&repo.work_tree, &head, config.sensitive_paths())
    });
    match built {
        Ok(index) => tracing::debug!(paths = index.entries().len(), "search index"),
        Err(reason) => tracing::debug!(%reason, "search index not rebuilt"),
    }
    Ok(())
}

/// The same pass in a build without the search seam.
#[cfg(not(feature = "search"))]
pub(super) fn rebuild_search_index_job(
    _t: &crate::hooktime::HookTimer,
    _job: &crate::agent_queue::AgentJob,
) -> Result<()> {
    tracing::debug!("search index rebuild requested; not in this build");
    Ok(())
}

/// Ask for a rebuild of this work tree's path index.
///
/// A request, not the work, and one file however many callers ask: the two
/// moments worth asking at are the two moments the set of paths can have
/// changed — a session beginning and a commit landing — which are the same two
/// moments the document pass is asked for, and for the same reason.
///
/// Reachable from the serving plane as well as from the hooks, and for the
/// third moment: the search verb met a checkout with no usable index and could
/// not afford to walk it. One asking site rather than two is what keeps
/// "however many callers ask, one file" true.
pub(crate) fn owe_search_index(git_dir: &Path, work_tree: &Path) -> bool {
    crate::agent_queue::enqueue(
        git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: work_tree.to_path_buf(),
            queued_at_ms: now_ms(),
            attempts: 0,
            kind: crate::agent_queue::JobKind::SearchIndex,
        },
    )
    .is_ok()
}

/// Ask for a proactive pass over this work tree's documents.
///
/// A request, not the work: the file is ~200 bytes and one rename, and the
/// same request from three hooks is one file. Whether there is anything to do
/// — a policy that is on, a helper that exists, a document that has changed —
/// is decided by the worker, where deciding it is free.
pub(super) fn owe_document_sweep(git_dir: &Path, work_tree: &Path) -> bool {
    crate::agent_queue::enqueue(
        git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: work_tree.to_path_buf(),
            queued_at_ms: now_ms(),
            attempts: 0,
            kind: crate::agent_queue::JobKind::DocSweep,
        },
    )
    .is_ok()
}

/// The proactive passes a beginning session is worth asking for, and whether
/// either of them was journaled.
///
/// A session start is the one moment when nothing has been asked for yet and
/// the worker this hook is about to dispatch has the rest of the session to
/// answer in. Both requests are cheap and unconditional — whether there is a
/// document to convert or a path set to index is the worker's to decide — and
/// both are best effort, because a request that cannot be journaled is a
/// saving not taken rather than a hook that failed.
pub(super) fn owe_session_start_passes(
    t: &crate::hooktime::HookTimer,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
) -> bool {
    if !session_start_hook(&raw.agent, &raw.verb, &raw.payload) {
        return false;
    }
    let documents = t.phase("owe_doc_sweep", || owe_document_sweep(git_dir, work_tree));
    let index = t.phase("owe_search_index", || owe_search_index(git_dir, work_tree));
    let sweep = t.phase("owe_idle_sweep", || owe_idle_sweep(git_dir, work_tree));
    documents || index || sweep
}

/// Whether this hook is the family's "a session is beginning" boundary.
///
/// Two of the three families name it outright. Antigravity names no session
/// start at all; what it publishes is an invocation counter, and its zeroth
/// invocation is the one execution boundary it has — the same reading the
/// recovery map already takes of it, rather than a second interpretation of
/// the same field.
pub(super) fn session_start_hook(
    agent: &rift_core::AgentId,
    verb: &str,
    payload: &serde_json::Value,
) -> bool {
    if *agent == rift_core::AgentId::ANTIGRAVITY {
        return verb == "PreInvocation" && hook_number(payload, "invocationNum") == Some(0);
    }
    verb == "SessionStart"
}

/// Ask for the idle sweep at the family's floor.
///
/// A session starting is when another agent's abandoned session is most
/// likely to be found, and the worker this hook dispatches has nothing else
/// to wait for: the hook pays one journal write, the worker pays the sweep.
fn owe_idle_sweep(git_dir: &Path, work_tree: &Path) -> bool {
    let floor_ms = rift_config::idle_sweep_ms(&work_tree.join(".rift"));
    crate::agent_queue::enqueue(
        git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: work_tree.to_path_buf(),
            queued_at_ms: now_ms(),
            attempts: 0,
            kind: crate::agent_queue::JobKind::SweepIdle { floor_ms },
        },
    )
    .is_ok()
}

/// The deferred half of `SessionEnd`: settle the session's last turn (no
/// later hook will — `SettleMode::Final`, exactly as the doctor sweep does),
/// archive — against HEAD when there is one, anchorless on an unborn HEAD
/// (Q78: a condense never needs a commit) — under `ArchiveReason::SessionEnd`,
/// and drive Ending → Ended on the checkpoint that exists. This is verbatim
/// the work that used to run inside the hook itself; only *when* it runs
/// changed, and the clock it reads is the job's queue time.
///
/// Completion requires an exact-context marker and an existing checkpoint.
/// A legacy done marker or ended cache cannot prove that this delivery ran;
/// those deliveries stay owed. Missing input proof does not spend retries.
///
/// Returns `(session_id, checkpoint_id)` when it condensed.
fn condense_agent_job(
    t: &crate::hooktime::HookTimer,
    job: &crate::agent_queue::AgentJob,
    session: &str,
    agent: &str,
    repo_id: &str,
    owner: &crate::capture_queue::QueueLock,
    context: &serde_json::Value,
) -> Result<Option<(String, String)>> {
    use rift_shadow::hooks::strategy::{ArchiveDecision, ArchiveReason};

    let repo = rift_git::discover(&job.work_tree)?;
    if ownership::completed_condense(owner, context, &repo.work_tree)? {
        return Ok(None);
    }
    let sessions = crate::family::run_state::sessions_dir(&repo.git_dir);
    ownership::require_condense_input(&repo.git_dir, &sessions, session)?;
    let reader = t.phase("gix_open", || {
        rift_git::GixBlobReader::open(&repo.work_tree)
    })?;
    let commit = t.phase("head_hex", || reader.head_hex())?;
    let store = t.phase("open_projection", || {
        rift_store::open_capture_projection(projection_db_at(&repo.work_tree)?)
    })?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let now = job.queued_at_ms;
    // Best-effort, like the sweep's: the settle never decides the condense.
    let _ = t.phase("settle_usage", || {
        rt.block_on(rift_shadow::bridge::settle_closed_turn_usage(
            &store,
            &sessions,
            session,
            agent,
            repo_id,
            rift_shadow::bridge::SettleMode::Final,
            now,
        ))
    });
    let strategy = strategy_kind(effective_config(&repo.work_tree).checkpoint_strategy());
    let checkpoint = t.phase("archive", || {
        archive_now(
            &rt,
            &store,
            &repo.work_tree,
            commit.as_deref(),
            session,
            agent,
            ArchiveDecision {
                archive: true,
                reason: ArchiveReason::SessionEnd,
                strategy,
            },
            now,
        )
    })?;
    // `ended` is claimed only by a checkpoint that exists (see the note at
    // `drive_post_ingest`'s old inline branch, now the sweep's doc).
    let Some(checkpoint) = checkpoint else {
        return Ok(None);
    };
    let state = rt.block_on(rift_shadow::bridge::mark_condense_done(
        &store, &sessions, session, agent, now,
    ))?;
    ownership::record_condense(owner, context, &repo.work_tree, checkpoint.as_str())?;
    crate::agent_queue::mark_condensed(&repo.git_dir, session);
    finish_condensed_session(&repo, &sessions, session, state);
    linehash::cache_window_line_hashes(&repo.work_tree, &repo.common_dir, session);
    Ok(Some((session.to_owned(), checkpoint.as_str().to_owned())))
}

/// The session-end cleanup (§4.3), run once after the final archive landed
/// **and** the machine answered `Ended`. Any other answer is the CFX-3
/// re-activation window — a stale completion closing a session that is
/// active again — and then the spool, claims and settled `.usage` stay, said
/// once; `.usage` goes only after `SettleMode::Final` settled, which the
/// `Ended` answer is the proof of.
fn finish_condensed_session(
    repo: &rift_git::RepoPaths,
    sessions: &Path,
    session: &str,
    state: rift_shadow::hooks::SessionState,
) {
    if state != rift_shadow::hooks::SessionState::Ended {
        crate::capture_queue::mark_note(
            &repo.git_dir,
            &format!("cleanup-{}", crate::agent_queue::note_key_for_session(session)),
            &format!(
                "session {} condensed but reads {}: its spool and claims are kept, cleanup left owed",
                session,
                state.as_str()
            ),
        );
        return;
    }
    let _ = crate::family::run_state::cleanup_session(
        sessions,
        &super::family_state::family_state_dir(&repo.common_dir),
        session,
    );
}

/// The interval archive a hook journaled: asked again under the lock, and
/// archived if it is still owed.
///
/// Asked again because the hook's answer can be overtaken while the job
/// waits: a commit's condense, or a job an earlier hook left, may have
/// archived the session since, and a second checkpoint for one interval is a
/// checkpoint the strategy never asked for. The clock is the job's, as every
/// deferred archive's is, so a late drain neither backdates nor advances the
/// decision. A failure names the session, so the note a kept job leaves says
/// whose interval archive it was.
fn interval_archive_job(
    t: &crate::hooktime::HookTimer,
    job: &crate::agent_queue::AgentJob,
    archive: &crate::agent_queue::IntervalArchive,
) -> Result<bool> {
    archive_if_still_owed(t, job, archive).map_err(|e| {
        if matches!(e, rift_core::RiftError::ProjectionBusy { .. })
            || crate::capture_queue::is_deferred(&e)
        {
            return e;
        }
        rift_core::RiftError::Other(format!(
            "interval archive of session {}: {e}",
            archive.session_id
        ))
    })
}

/// [`interval_archive_job`]'s work: the commit to anchor to (none on an
/// unborn HEAD — the interval lands anchorless, Q10), the decision, and the
/// archive the session-end condense and the sweep also run. `true` when it
/// wrote a checkpoint, and only then: never a no-HEAD skip.
fn archive_if_still_owed(
    t: &crate::hooktime::HookTimer,
    job: &crate::agent_queue::AgentJob,
    archive: &crate::agent_queue::IntervalArchive,
) -> Result<bool> {
    let repo = rift_git::discover(&job.work_tree)?;
    let reader = t.phase("gix_open", || {
        rift_git::GixBlobReader::open(&repo.work_tree)
    })?;
    let commit = t.phase("head_hex", || reader.head_hex())?;
    let store = t
        .phase("open_projection", || {
            rift_store::open_capture_projection(projection_db_at(&repo.work_tree)?)
        })
        .map_err(|error| ownership::proof_read(error, "interval history is unavailable"))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let Some(decision) = owed_interval_decision(&rt, &store, &repo, job, archive)? else {
        return Err(crate::capture_queue::deferred(
            "interval completion lacks exact delivery proof",
        ));
    };
    // A deferred interval archive changes no lifecycle: the session stays
    // live, and the checkpoint moves the next interval's start.
    let checkpoint = t.phase("archive", || {
        archive_now(
            &rt,
            &store,
            &repo.work_tree,
            commit.as_deref(),
            &archive.session_id,
            &archive.agent,
            decision,
            job.queued_at_ms,
        )
    })?;
    Ok(checkpoint.is_some())
}

/// The strategy's decision for a journaled interval archive, on the job's
/// clock and the archive history as it stands under the lock; `None` when the
/// archive is no longer owed — another road archived the interval, the
/// session is no longer live, or the repository stopped running `scheduled`.
fn owed_interval_decision(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    job: &crate::agent_queue::AgentJob,
    archive: &crate::agent_queue::IntervalArchive,
) -> Result<Option<rift_shadow::hooks::strategy::ArchiveDecision>> {
    use rift_shadow::hooks::session_cache::read_cache;
    use rift_shadow::hooks::strategy::{decide, TriggerCtx};

    let Some(cache) = read_cache(
        &crate::family::run_state::sessions_dir(&repo.git_dir),
        &archive.session_id,
    )
    .map_err(|error| ownership::proof_read(error, "interval session input is unreadable"))?
    else {
        return Ok(None);
    };
    let cfg = effective_config(&repo.work_tree);
    let last = rt
        .block_on(rift_store::latest_archive_ms(store, &archive.session_id))
        .map_err(|error| ownership::proof_read(error, "interval archive history is unreadable"))?;
    let decision = decide(
        strategy_kind(cfg.checkpoint_strategy()),
        &TriggerCtx {
            git_event: None,
            session_state: state_from_token(&cache.state),
            sensitive_path_touched: archive.reason
                == crate::agent_queue::IntervalReason::SensitivePath,
            clock_ms: job.queued_at_ms,
            last_archive_ms: last,
            schedule_interval_ms: cfg.schedule_interval_ms(),
        },
    );
    if !decision.archive {
        return Ok(None);
    }
    Ok(Some(decision))
}

/// The 1-based inclusive line range `lo..=hi` of `text`, as the file itself
/// spells it.
///
/// This is the whole of what "priced at the requested range" means. A shell
/// read names its own range — `sed -n '40,80p'` asked for forty-one lines and
/// no more — and pricing it at the whole file would put tokens in the
/// denominator that never reached a context window. The bytes counted are the
/// file's own at the identity the fact recorded, never the client's rendering
/// of them: a terminal that truncated its display still cost what the file
/// costs over those lines.
///
/// A range that starts past the end of the file yields nothing, and nothing
/// counts as nothing — a read of no lines is not rounded up to one.
pub(super) fn line_slice(text: &str, lo: u32, hi: u32) -> String {
    let skip = usize::try_from(lo.saturating_sub(1)).unwrap_or(usize::MAX);
    let take = usize::try_from(hi.saturating_sub(lo).saturating_add(1)).unwrap_or(usize::MAX);
    let mut out = String::new();
    for line in text.split_inclusive('\n').skip(skip).take(take) {
        out.push_str(line);
    }
    out
}

/// The deferred BPE count of one read, moved off the hook path to stay inside
/// the git-latency budget: count the bytes of the revision the
/// read recorded — the working file while its SHA-256 still matches, else the
/// git blob by oid, else the family's basis snapshot, each verified against
/// the recorded hash — and append the `file_read_tokens` row at the read's own
/// `tool_call` id. A redelivery whose key's row is provably its own is
/// discharged with a payload-free note, and writes no second row.
///
/// Where no source holds those bytes any more the job is done, not failed:
/// the read stays unmeasured ("measured or absent"), and it is counted as
/// unmeasured once because that count is the read's own `tool_reads_file` row
/// with no `file_read_tokens` row beside it — nothing is written, so a
/// redelivery cannot count it twice. Retrying would only re-ask sources that
/// already said the revision is gone, and a hook has nothing to tell the
/// developer about it, so the worker's own log is the one place it is said.
fn count_read_tokens_job(
    t: &crate::hooktime::HookTimer,
    job: &crate::agent_queue::AgentJob,
    tool_call_id: &str,
    rel_path: &str,
    content_sha256: &str,
    blob_oid: &str,
    lines: Option<(u32, u32)>,
    schema_version: i64,
) -> Result<()> {
    let repo = rift_git::discover(&job.work_tree)?;
    let store = t
        .phase("open_projection", || {
            rift_store::open_capture_projection(projection_db_at(&repo.work_tree)?)
        })
        .map_err(|error| ownership::proof_read(error, "token prior-key proof is unavailable"))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let key = ownership::read_token_key(&store, &rt, tool_call_id, rel_path, content_sha256)?;
    if key == ownership::ReadTokenKey::Recorded {
        let note = crate::agent_queue::note_key(job);
        let said = format!("{note}: redelivery discharged; the row at its key counts its bytes");
        crate::capture_queue::mark_note(&repo.git_dir, &note, &said);
        return Ok(());
    }
    let Some(bytes) = t.phase("read_bytes", || {
        reads::recorded_revision_bytes(&repo.work_tree, rel_path, content_sha256, blob_oid)
    }) else {
        tracing::debug!(
            path = rel_path,
            "read tokens: the recorded revision is gone; the read stays unmeasured"
        );
        return Ok(());
    };
    // The outline of what was just read, bound now rather than at condense so
    // the family's next first read of these bytes can be answered with it.
    // Its own counts and refusals; it never fails the count below.
    let read = [(rel_path.to_owned(), content_sha256.to_owned())];
    t.phase("bind_outline", || {
        bind_and_cache_outlines(&rt, &repo.work_tree, &read)
    });
    let _ = t.phase("cache_line_hashes", || {
        linehash::cache_line_hashes(&repo.common_dir, content_sha256, &bytes)
    });
    if rift_tokens::provenance() != rift_tokens::TOKENIZER {
        // Only real BPE figures persist; the chars/4 fallback never lands.
        return Ok(());
    }
    let text = String::from_utf8_lossy(&bytes);
    let text = match lines {
        None => text,
        Some((lo, hi)) => std::borrow::Cow::Owned(line_slice(&text, lo, hi)),
    };
    let tokens = t.phase("count", || rift_tokens::count(&text));
    let inserted = rt.block_on(rift_store::record_file_read_tokens(
        &store,
        tool_call_id,
        tokens,
        rift_tokens::TOKENIZER,
        schema_version,
    ))?;
    if !inserted {
        return Err(crate::capture_queue::deferred(
            "token key was concurrently recorded",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod refused_job_tests;

#[cfg(test)]
mod run_state_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod retry_tests;

#[cfg(test)]
mod recovery_tests;
