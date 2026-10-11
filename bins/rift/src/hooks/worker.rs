//! The detached capture worker: the queue drain under the family lock and the
//! jobs it runs (agent-queue jobs, token counts, basis snapshots, the document
//! and search-index passes, condensing a queued session). Separate because
//! nothing here runs inside a hook's latency budget; it is the work a hook
//! deferred.

mod boundary;
mod reads;

use std::path::Path;

use rift_core::Result;

use super::archive::{archive_now, bind_and_cache_outlines, condense_queued, level_projection};
use super::audit::{attribute_serve_job, read_identified_bytes, serve_audit_job};
use super::git::{capture_guarded, effective_config, state_from_token, strategy_kind};
use super::recovery::hook_number;
use super::{flag, now_ms, projection_db_at};

use self::reads::count_tool_result_tokens_job;
pub(in crate::hooks) use self::reads::snapshot_basis_job;

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

/// Condense every queued commit, oldest first, under the drain lock.
///
/// An entry that fails is **kept** and marked, so the next worker retries it and
/// the next hook reports it; only a drained one is removed.
pub(crate) fn drain_capture_queue(git_dir: Option<&str>) -> Result<()> {
    drain_queues(git_dir, LockWait::None).map(|_| ())
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
    // Another worker is already draining: it will take these entries too, so a
    // second drain would only duplicate idempotent work and its git writes.
    let Some(_lock) = acquire_queue_lock(git_dir, wait) else {
        return Ok(Vec::new());
    };
    let t = crate::hooktime::HookTimer::from_env("capture-worker");
    let drained = drain_locked(&t, git_dir);
    let swept = super::sweep::sweep_idle_family(git_dir, now_ms(), None);
    if drained.minted || !swept.condensed_checkpoints.is_empty() {
        let levelled = level_after_condense(git_dir);
        tracing::debug!(?levelled, "projection levelled after a condense");
    }
    Ok(drained.condensed)
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
    match acquire_queue_lock(git_dir, wait) {
        Some(_lock) => {
            let t = crate::hooktime::HookTimer::from_env("capture-worker");
            drain_locked(&t, git_dir).condensed
        }
        None => Vec::new(),
    }
}

/// Every job and queued commit, drained under the lock the caller holds.
///
/// Within a pass every journaled job runs before any queued commit, in the
/// journal's order. That is what keeps a turn's boundary snapshots ahead of
/// the commit whose attribution replays them, and a turn's end ahead of the
/// next turn's start.
fn drain_locked(t: &crate::hooktime::HookTimer, git_dir: &Path) -> Drained {
    let mut drained = Drained::default();
    let now = now_ms();
    // Under the lock, on the same clock reading the lock decision used: a temp
    // file older than the lock-steal age is the corpse of a write that died
    // before its rename, and nothing else in the queue will ever remove it.
    let swept = crate::capture_queue::sweep_stale_tmps(git_dir, now);
    if swept > 0 {
        tracing::debug!(swept, "removed stale capture-queue temp files");
    }
    // A job attempted once in this drain is not attempted again by it: a
    // failed job is re-journaled with its count raised, and re-finding it on
    // the next pass would spend every retry it has inside one worker — the
    // retry is the NEXT worker's or the doctor's, after whatever failed has
    // had a chance to change.
    let mut attempted = std::collections::BTreeSet::new();
    loop {
        let agent_jobs: Vec<_> = crate::agent_queue::jobs(git_dir)
            .into_iter()
            .filter(|(path, _)| attempted.insert(path.clone()))
            .collect();
        let commits: Vec<_> = crate::capture_queue::entries(git_dir)
            .into_iter()
            .filter(|(path, _)| attempted.insert(path.clone()))
            .collect();
        if agent_jobs.is_empty() && commits.is_empty() {
            return drained;
        }
        for (path, job) in agent_jobs {
            match run_agent_job(t, git_dir, &path, &job) {
                Landed::Nothing => (),
                Landed::Checkpoint => drained.minted = true,
                Landed::Condensed(session, checkpoint) => {
                    drained.minted = true;
                    drained.condensed.push((session, checkpoint));
                }
            }
        }
        for (path, entry) in commits {
            match condense_queued(t, git_dir, &entry, &path) {
                Ok(()) => crate::capture_queue::remove(&path),
                Err(e) if projection_busy(&e) => (),
                Err(e) => {
                    crate::capture_queue::record_failure(&path, &entry, git_dir, &e.to_string());
                }
            }
        }
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
) -> Landed {
    match run_job_kind(t, git_dir, job) {
        Ok(landed) => {
            crate::agent_queue::remove(path);
            landed
        }
        Err(e) if projection_busy(&e) => Landed::Nothing,
        Err(e @ rift_core::RiftError::ConstraintRefused { .. }) => {
            let key = crate::agent_queue::note_key(job);
            crate::capture_queue::mark_note(git_dir, &key, &format!("{e} (dropped)"));
            crate::agent_queue::remove(path);
            Landed::Nothing
        }
        Err(e) => {
            crate::agent_queue::record_failure(path, job, git_dir, &e.to_string());
            Landed::Nothing
        }
    }
}

/// The work of one journaled job, by its kind.
fn run_job_kind(
    t: &crate::hooktime::HookTimer,
    git_dir: &Path,
    job: &crate::agent_queue::AgentJob,
) -> Result<Landed> {
    use crate::agent_queue::JobKind;

    match &job.kind {
        JobKind::Condense {
            session_id,
            agent,
            repo_id,
        } => condense_agent_job(t, job, session_id, agent, repo_id).map(|condensed| {
            condensed.map_or(Landed::Nothing, |(session, checkpoint)| {
                Landed::Condensed(session, checkpoint)
            })
        }),
        JobKind::Archive(archive) => interval_archive_job(t, job, archive).map(landed_if),
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
        JobKind::SweepIdle { floor_ms } => Ok(sweep_idle_job(t, git_dir, job, *floor_ms)),
        JobKind::BoundarySnapshot(snapshot) => {
            boundary::boundary_snapshot_job(t, job, snapshot).map(|()| Landed::Nothing)
        }
        JobKind::BasisSnapshot {
            rel_path,
            content_sha256,
            blob_oid,
        } => {
            snapshot_basis_job(t, job, rel_path, content_sha256, blob_oid).map(|()| Landed::Nothing)
        }
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
    git_dir: &Path,
    job: &crate::agent_queue::AgentJob,
    floor_ms: i64,
) -> Landed {
    let swept = t.phase("sweep_idle", || {
        super::sweep::sweep_idle_family(git_dir, job.queued_at_ms, Some(floor_ms))
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
/// archive against HEAD under `ArchiveReason::SessionEnd`, and drive
/// Ending → Ended on the checkpoint that exists. This is verbatim the work
/// that used to run inside the hook itself; only *when* it runs
/// changed, and the clock it reads is the job's queue time.
///
/// Idempotent by two doors: the journal's done-marker (a redelivered
/// `SessionEnd`, or a worker killed between the archive and its `remove`)
/// and the cache phase (a session already `ended` owes nothing). A missing
/// HEAD is an error, so the job is kept and marked: the sweep is the retry.
///
/// Returns `(session_id, checkpoint_id)` when it condensed.
fn condense_agent_job(
    t: &crate::hooktime::HookTimer,
    job: &crate::agent_queue::AgentJob,
    session: &str,
    agent: &str,
    repo_id: &str,
) -> Result<Option<(String, String)>> {
    use rift_shadow::hooks::session_cache::read_cache;
    use rift_shadow::hooks::strategy::{ArchiveDecision, ArchiveReason};
    use rift_shadow::hooks::SessionState;

    let repo = rift_git::discover(&job.work_tree)?;
    if crate::agent_queue::is_condensed(&repo.git_dir, session) {
        return Ok(None);
    }
    if read_cache(&repo.git_dir, session)?.is_some_and(|c| c.state == SessionState::Ended.as_str())
    {
        return Ok(None);
    }
    let reader = t.phase("gix_open", || {
        rift_git::GixBlobReader::open(&repo.work_tree)
    })?;
    let Some(commit) = t.phase("head_hex", || reader.head_hex())? else {
        return Err(rift_core::RiftError::validity(
            "session-end condense: no commit to anchor a checkpoint to",
        ));
    };
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
            &repo.git_dir,
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
            &commit,
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
    let _ = rt.block_on(rift_shadow::bridge::mark_condense_done(
        &store,
        &repo.git_dir,
        session,
        agent,
        now,
    ));
    crate::agent_queue::mark_condensed(&repo.git_dir, session);
    Ok(Some((session.to_owned(), checkpoint.as_str().to_owned())))
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
        if matches!(e, rift_core::RiftError::ProjectionBusy { .. }) {
            return e;
        }
        rift_core::RiftError::Other(format!(
            "interval archive of session {}: {e}",
            archive.session_id
        ))
    })
}

/// [`interval_archive_job`]'s work: the commit to anchor to, the decision, and
/// the archive the session-end condense and the sweep also run. `true` when
/// it wrote a checkpoint.
fn archive_if_still_owed(
    t: &crate::hooktime::HookTimer,
    job: &crate::agent_queue::AgentJob,
    archive: &crate::agent_queue::IntervalArchive,
) -> Result<bool> {
    let repo = rift_git::discover(&job.work_tree)?;
    let reader = t.phase("gix_open", || {
        rift_git::GixBlobReader::open(&repo.work_tree)
    })?;
    // No commit to anchor a checkpoint to defers silently, as the hook always
    // did: the next due hook asks again.
    let Some(commit) = t.phase("head_hex", || reader.head_hex())? else {
        return Ok(false);
    };
    let store = t.phase("open_projection", || {
        rift_store::open_capture_projection(projection_db_at(&repo.work_tree)?)
    })?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let Some(decision) = owed_interval_decision(&rt, &store, &repo, job, archive)? else {
        return Ok(false);
    };
    // A deferred interval archive changes no lifecycle: the session stays
    // live, and the checkpoint moves the next interval's start.
    let checkpoint = t.phase("archive", || {
        archive_now(
            &rt,
            &store,
            &repo.work_tree,
            &commit,
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

    let Some(cache) = read_cache(&repo.git_dir, &archive.session_id)? else {
        return Ok(None);
    };
    let cfg = effective_config(&repo.work_tree);
    let last = rt.block_on(rift_store::latest_archive_ms(store, &archive.session_id))?;
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

/// The deferred BPE count of one read, moved off the hook path in 2026-08 to
/// stay inside the git-latency budget: count the bytes whose identity the job names — the
/// working file if its SHA-256 still matches, else the git blob by oid — and
/// append the `file_read_tokens` row at the read's own `tool_call` id.
/// Insert-or-ignore, so a redelivered job writes no second row. Bytes that
/// match neither source are not counted under a guessed identity: the read
/// stays unmeasured ("measured or absent"), said once.
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
    let Some(bytes) = t.phase("read_bytes", || {
        read_identified_bytes(&repo.work_tree, rel_path, content_sha256, blob_oid)
    }) else {
        return Err(rift_core::RiftError::validity(format!(
            "read tokens: the bytes read at {rel_path} are no longer available to count"
        )));
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    // The outline of what was just read, bound now rather than at condense so
    // the family's next first read of these bytes can be answered with it.
    // Its own counts and refusals; it never fails the count below.
    let read = [(rel_path.to_owned(), content_sha256.to_owned())];
    t.phase("bind_outline", || {
        bind_and_cache_outlines(&rt, &repo.work_tree, &read)
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
    let store = t.phase("open_projection", || {
        rift_store::open_capture_projection(projection_db_at(&repo.work_tree)?)
    })?;
    rt.block_on(rift_store::record_file_read_tokens(
        &store,
        tool_call_id,
        tokens,
        rift_tokens::TOKENIZER,
        schema_version,
    ))?;
    Ok(())
}

#[cfg(test)]
mod refused_job_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_queue::{AgentJob, IntervalArchive, IntervalReason, JobKind};

    /// ★ The job that counts a read binds the outline of what was read and
    /// caches it for the read gate, before anything about the count can fail.
    #[test]
    fn the_read_count_job_binds_and_caches_the_outline_of_what_was_read() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        rift_git::test_support::fake_repo(root).unwrap();
        let body = "fn alpha() {\n    1\n}\n\nfn beta() {\n    2\n}\n";
        std::fs::write(root.join("lib.rs"), body).unwrap();
        let sha = rift_recall_substrate::worktree::sha256_hex(body.as_bytes());
        let job = AgentJob {
            schema_version: 1,
            work_tree: root.to_path_buf(),
            queued_at_ms: 1_700_000_000_000,
            attempts: 0,
            kind: JobKind::ReadTokens {
                tool_call_id: "0".repeat(64),
                rel_path: "lib.rs".to_owned(),
                content_sha256: sha.clone(),
                blob_oid: rift_recall_substrate::worktree::git_blob_oid(body.as_bytes()),
                lines: None,
                schema_version: 1,
            },
        };
        // Whatever becomes of the count row, the outline is already bound.
        let _ = run_job_kind(
            &crate::hooktime::HookTimer::from_env("test"),
            &root.join(".git"),
            &job,
        );
        let common_dir = rift_git::discover(root).unwrap().common_dir;
        let cached = crate::hooks::gate::outline_cache_dir(&common_dir)
            .join(format!("{sha}-e3p1r1.outline"));
        let text = std::fs::read_to_string(&cached).expect("the outline is cached");
        assert!(
            text.starts_with("rift-outline 1\nclass code\nshape 7 "),
            "{text}"
        );
        assert!(
            text.contains(" alpha\n") && text.contains(" beta\n"),
            "{text}"
        );
    }

    #[test]
    fn a_session_condense_without_a_recall_projection_archives_once() {
        let checkout = tempfile::tempdir().unwrap();
        let (repo, path) = queue_session_end(checkout.path(), "s-absent");
        let recall = crate::family::recall_db(checkout.path()).unwrap();
        let git_dir = repo.git_dir.to_str().unwrap();
        let condensed = drain_jobs(Some(git_dir), LockWait::None);
        assert_eq!(condensed.len(), 1);
        assert_eq!(condensed[0].0, "s-absent");
        assert!(!path.exists());
        assert!(crate::capture_queue::peek_errors(&repo.git_dir).is_empty());
        assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
        assert!(!recall.exists());
        let mut marker = recall.as_os_str().to_owned();
        marker.push(".lock");
        assert!(!Path::new(&marker).exists());
    }

    #[test]
    fn a_busy_interval_archive_preserves_its_typed_cause_and_existing_retry_budget() {
        let checkout = tempfile::tempdir().unwrap();
        fixture_git(checkout.path(), &["init", "--quiet"]);
        fixture_git(
            checkout.path(),
            &["commit", "--quiet", "--allow-empty", "-m", "fixture"],
        );
        let repo = rift_git::discover(checkout.path()).unwrap();
        let db = crate::family::projection_db(checkout.path()).unwrap();
        let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
        let archive = IntervalArchive {
            session_id: "s-interval".into(),
            agent: "claude-code".into(),
            reason: IntervalReason::Schedule,
        };
        let job = AgentJob {
            schema_version: 1,
            work_tree: checkout.path().to_owned(),
            queued_at_ms: 1,
            attempts: 0,
            kind: JobKind::Archive(archive.clone()),
        };
        let timer = crate::hooktime::HookTimer::new("test", false);
        assert!(matches!(
            interval_archive_job(&timer, &job, &archive),
            Err(rift_core::RiftError::ProjectionBusy { .. })
        ));
        let path = crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap();
        let before = std::fs::read(&path).unwrap();
        let git_dir = repo.git_dir.to_str().unwrap();
        for _ in 0..crate::capture_queue::MAX_ATTEMPTS + 2 {
            assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(crate::capture_queue::peek_errors(&repo.git_dir).is_empty());
        }
        drop(guard);
        assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn busy_attribution_keeps_the_existing_job_even_after_the_retry_budget() {
        let checkout = tempfile::tempdir().unwrap();
        let initialized = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(checkout.path())
            .status()
            .unwrap();
        assert!(initialized.success());
        let repo = rift_git::discover(checkout.path()).unwrap();
        let db = crate::family::recall_db(checkout.path()).unwrap();
        let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
        let git_dir = repo.git_dir.to_str().unwrap();
        for attempts in [0, crate::capture_queue::MAX_ATTEMPTS, 7] {
            let job = AgentJob {
                schema_version: 1,
                work_tree: checkout.path().to_owned(),
                queued_at_ms: 1,
                attempts,
                kind: JobKind::AttributeServe(crate::agent_queue::ServeClaim {
                    session: "s-1".into(),
                    window: None,
                    call_id: "a".repeat(32),
                    audit_id: "b".repeat(64),
                }),
            };
            let path = crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap();
            let before = std::fs::read(&path).unwrap();
            for _ in 0..crate::capture_queue::MAX_ATTEMPTS + 2 {
                assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
                assert_eq!(std::fs::read(&path).unwrap(), before);
            }
            assert!(crate::capture_queue::peek_errors(&repo.git_dir).is_empty());
            assert!(
                !db.exists(),
                "a busy no-source gap must not mint a database"
            );
        }
        drop(guard);
        let (path, _) = crate::agent_queue::jobs(&repo.git_dir).pop().unwrap();
        assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
        assert!(!path.exists(), "the next worker can complete the same job");
        assert!(!db.exists(), "legitimate absence remains an omission");
        assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
        assert!(crate::agent_queue::jobs(&repo.git_dir).is_empty());
    }

    #[test]
    fn busy_is_classified_by_its_typed_cause_through_context() {
        #[derive(Debug)]
        struct Context(rift_core::RiftError);
        impl std::fmt::Display for Context {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "capture job failed: {}", self.0)
            }
        }
        impl std::error::Error for Context {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let busy = Context(rift_core::RiftError::ProjectionBusy {
            projection: "projection.db".into(),
            reason: "replacement owns its marker".into(),
        });
        assert!(projection_busy(&busy));
        assert!(!projection_busy(&rift_core::RiftError::Other(
            busy.to_string()
        )));
    }

    #[test]
    fn repeated_busy_commit_drains_keep_the_job_then_record_its_facts_once() {
        let checkout = tempfile::tempdir().unwrap();
        fixture_git(checkout.path(), &["init", "--quiet"]);
        std::fs::write(checkout.path().join("file.txt"), b"captured line\n").unwrap();
        fixture_git(checkout.path(), &["add", "file.txt"]);
        fixture_git(checkout.path(), &["commit", "--quiet", "-m", "fixture"]);
        let commit = fixture_git(checkout.path(), &["rev-parse", "HEAD"]);
        let repo = rift_git::discover(checkout.path()).unwrap();
        let db = crate::family::projection_db(checkout.path()).unwrap();
        let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
        let entry = crate::capture_queue::QueueEntry {
            schema_version: 1,
            commit: commit.trim().into(),
            work_tree: checkout.path().to_owned(),
            session_id: None,
            agent: None,
            session_state: None,
            queued_at_ms: 1,
            attempts: 0,
        };
        let path = crate::capture_queue::enqueue(&repo.git_dir, &entry).unwrap();
        let before = std::fs::read(&path).unwrap();
        let git_dir = repo.git_dir.to_str().unwrap();
        for _ in 0..crate::capture_queue::MAX_ATTEMPTS + 2 {
            assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(crate::capture_queue::peek_errors(&repo.git_dir).is_empty());
        }
        drop(guard);
        assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
        assert!(!path.exists());
        assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
        let store = rift_store::SqliteStore::open_read_only(&db).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert_eq!(
            rt.block_on(rift_store::CanonicalStore::count(&store, "file_revision"))
                .unwrap(),
            1
        );
        assert!(crate::capture_queue::entries(&repo.git_dir).is_empty());
    }

    #[test]
    fn a_drained_session_end_condense_leaves_its_checkpoint_in_the_projection() {
        let checkout = tempfile::tempdir().unwrap();
        let (repo, path) = queue_session_end(checkout.path(), "s-level");
        let git_dir = repo.git_dir.to_str().unwrap();
        let condensed = drain_queues(Some(git_dir), LockWait::None).unwrap();
        assert_eq!(condensed.len(), 1);
        assert!(!path.exists());
        let checkpoint = condensed[0].1.clone();
        let db = crate::family::projection_db(checkout.path()).unwrap();
        let store = rift_store::SqliteStore::open_read_only(&db).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let rows = |sql: &str| {
            rt.block_on(rift_store::CanonicalStore::query_opt_i64(
                &store,
                sql,
                &[rift_store::Value::text(checkpoint.clone())],
            ))
            .unwrap()
        };
        assert_eq!(
            rows("SELECT COUNT(*) FROM checkpoint WHERE checkpoint_id = ?1"),
            Some(1)
        );
        assert_eq!(
            rows("SELECT COUNT(*) FROM session_in_checkpoint WHERE checkpoint_id = ?1"),
            Some(1)
        );
        let watermarks = rt
            .block_on(rift_store::CanonicalStore::count(
                &store,
                "ingest_watermark",
            ))
            .unwrap();
        assert!(watermarks >= 1, "the levelling records where it got to");
    }

    #[test]
    fn a_busy_projection_defers_the_condense_and_its_levelling_without_spending_an_attempt() {
        let checkout = tempfile::tempdir().unwrap();
        let (repo, path) = queue_session_end(checkout.path(), "s-busy");
        let db = crate::family::projection_db(checkout.path()).unwrap();
        let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
        let before = std::fs::read(&path).unwrap();
        let git_dir = repo.git_dir.to_str().unwrap();
        for _ in 0..crate::capture_queue::MAX_ATTEMPTS + 2 {
            assert!(drain_queues(Some(git_dir), LockWait::None)
                .unwrap()
                .is_empty());
            assert_eq!(level_after_condense(&repo.git_dir), Levelled::Deferred);
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert_eq!(crate::agent_queue::jobs(&repo.git_dir).len(), 1);
            assert!(crate::capture_queue::peek_errors(&repo.git_dir).is_empty());
        }
        drop(guard);
        let condensed = drain_queues(Some(git_dir), LockWait::None).unwrap();
        assert_eq!(condensed.len(), 1);
        assert!(!path.exists());
        let store = rift_store::SqliteStore::open_read_only(&db).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert_eq!(
            rt.block_on(rift_store::CanonicalStore::count(&store, "checkpoint"))
                .unwrap(),
            1
        );
    }

    /// A committed checkout with one session in `ending` and its session-end
    /// condense journaled, as the `SessionEnd` hook leaves them.
    fn queue_session_end(root: &Path, session: &str) -> (rift_git::RepoPaths, std::path::PathBuf) {
        fixture_git(root, &["init", "--quiet"]);
        fixture_git(
            root,
            &["commit", "--quiet", "--allow-empty", "-m", "fixture"],
        );
        let repo = rift_git::discover(root).unwrap();
        let family = crate::family::resolve_at(root).unwrap();
        let cache = rift_shadow::hooks::SessionCache::new(session, "claude-code", "ending", 1);
        rift_shadow::hooks::write_cache(&repo.git_dir, &cache).unwrap();
        let job = AgentJob {
            schema_version: 1,
            work_tree: root.to_owned(),
            queued_at_ms: 1,
            attempts: 0,
            kind: JobKind::Condense {
                session_id: session.into(),
                agent: "claude-code".into(),
                repo_id: family.family_id.to_string(),
            },
        };
        let path = crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap();
        (repo, path)
    }

    fn fixture_git(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=Fixture", "-c", "user.email=fixture-id"])
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_DATE", "2026-10-01T12:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-10-01T12:00:00Z")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// An interval archive that cannot run is kept for the next worker with
    /// its attempt counted, and the note it leaves for the doctor names whose
    /// interval archive it was.
    #[test]
    fn a_failed_interval_archive_is_kept_and_its_note_names_the_session() {
        let git_dir = tempfile::tempdir().unwrap();
        let job = AgentJob {
            schema_version: 1,
            work_tree: git_dir.path().join("gone"),
            queued_at_ms: 1,
            attempts: 0,
            kind: JobKind::Archive(IntervalArchive {
                session_id: "s-1".into(),
                agent: "claude-code".into(),
                reason: IntervalReason::Schedule,
            }),
        };
        let path = crate::agent_queue::enqueue(git_dir.path(), &job).unwrap();
        let t = crate::hooktime::HookTimer::new("test", false);
        assert_eq!(
            run_agent_job(&t, git_dir.path(), &path, &job),
            Landed::Nothing
        );
        let (_, kept) = crate::agent_queue::jobs(git_dir.path()).pop().unwrap();
        assert_eq!(kept.attempts, 1, "kept, with the attempt counted");
        let notes = crate::capture_queue::peek_errors(git_dir.path());
        assert!(
            notes
                .iter()
                .any(|n| n.contains("interval archive of session s-1")),
            "{notes:?}"
        );
    }
}
