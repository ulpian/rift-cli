//! The stale-session sweep: closing sessions no hook will close, rescuing the
//! agent jobs they left queued, and archiving what they captured. Separate
//! because it is driven from `rift doctor` and the session verbs, over every
//! session of a member, rather than by the one hook that is running.

use std::path::Path;

use rift_core::Result;
use rift_shadow::hooks::session_cache::SessionCache;

use super::archive::archive_now;
use super::git::{effective_config, strategy_kind};
use super::worker::{drain_jobs, LockWait};
use super::{now_ms, projection_db_at};

/// What one crashed-session sweep found and did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionSweep {
    /// Sessions still legitimately live (touched inside the idle window).
    pub(crate) live: Vec<String>,
    /// Sessions this pass finalized: `IdleTimeout` → condense → `Ended`.
    pub(crate) finalized: Vec<String>,
    /// Idle sessions this pass did not close, each with the one
    /// [`DeferCause`] that stopped it. They stay live, and either the worker
    /// holding the member's lock or **the next sweep** condenses them. Nothing
    /// else does: `Ending` has no `IdleTimeout` edge, so the machine will not
    /// re-request a condense, and `post-commit` only ever runs the strategy
    /// decision — it never picks up a pending `RequestCondense`.
    pub(crate) condense_deferred: Vec<Deferred>,
    /// The checkpoints this pass actually minted, one per finalized session.
    ///
    /// Carried rather than discarded because the caller has to WITNESS them:
    /// doctor's sweep is a real condensation, and the soak is counted by
    /// condensations. The ids used to die inside an `is_ok_and(|c|
    /// c.is_some())` — the same "a bool where an id belongs" spelling that
    /// `archive_now`'s own doc records as how the silent-loss defect was
    /// written.
    pub(crate) condensed_checkpoints: Vec<String>,
}

/// One idle session the sweep left owed, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Deferred {
    /// The session still owed a condense.
    pub(crate) session_id: String,
    /// The one point the sweep stopped at for it.
    pub(crate) cause: DeferCause,
}

/// Why a sweep left an idle session's condense owed: a closed vocabulary, so a
/// scripted caller can tell "a worker is finishing this" from "this failed".
///
/// One count with three possible causes was a report nobody could act on: a
/// fixture that should wait for a worker and a CI step that should flag a
/// failed archive read the same line. Each variant is a point in
/// [`finalize_idle_session`] or [`sweep_member_under_its_lock`] where the
/// sweep stops short, and only [`DeferCause::WorkerHoldsLock`] is somebody
/// else's to finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DeferCause {
    /// A capture worker holds the member's queue lock; its own tail sweep
    /// condenses the session under that lock.
    WorkerHoldsLock,
    /// Legacy ownership or unresolved run-state debt prevents mutation.
    OwnershipUnready,
    /// The machine answered the idle timeout without asking for a condense.
    NoIdleEdge,
    /// **Retired, read-only.** HEAD is unborn. No writer emits it since a
    /// condense stopped needing a commit (L-4: an anchorless checkpoint lands
    /// with the member absent); the variant stays so a journaled old token
    /// still parses as itself rather than as a corrupt document.
    #[allow(dead_code)] // retired: read back by `parse`, constructed by no writer
    NoCommitToAnchor,
    /// The archive ran against HEAD and minted no checkpoint.
    ArchiveProducedNothing,
    /// The checkpoint was minted, but recording the condense as done failed.
    DoneMarkFailed,
}

/// What every session of one sweep is finalized against, opened once by
/// [`open_sweep`] so the per-session steps read it rather than reopen it.
struct SweepCtx<'a> {
    /// The member being swept.
    repo: &'a rift_git::RepoPaths,
    /// Its capture projection.
    store: rift_store::SqliteStore,
    /// The runtime every bridge call is driven on.
    rt: tokio::runtime::Runtime,
    /// The commit every condense anchors to; `None` on an unborn HEAD, which
    /// archives with the anchor absent.
    head: Option<String>,
    /// The configured checkpoint strategy, recorded on each decision.
    strategy: rift_shadow::hooks::strategy::StrategyKind,
    /// The family id the last turn's usage is settled under.
    repo_id: String,
    /// The sweep's clock.
    now: i64,
}

impl DeferCause {
    /// The wire token, used verbatim in every JSON document that reports it.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::WorkerHoldsLock => "worker-holds-lock",
            Self::OwnershipUnready => "ownership-unready",
            Self::NoIdleEdge => "no-idle-edge",
            Self::NoCommitToAnchor => "no-commit-to-anchor",
            Self::ArchiveProducedNothing => "archive-produced-nothing",
            Self::DoneMarkFailed => "done-mark-failed",
        }
    }

    /// The cause a wire token names, so an old journal reads back as the
    /// vocabulary it was written in; `None` for a token this build never wrote.
    /// No production reader needs it yet; the vocabulary pin holds it.
    #[cfg(test)]
    pub(crate) fn parse(token: &str) -> Option<Self> {
        [
            Self::WorkerHoldsLock,
            Self::OwnershipUnready,
            Self::NoIdleEdge,
            Self::NoCommitToAnchor,
            Self::ArchiveProducedNothing,
            Self::DoneMarkFailed,
        ]
        .into_iter()
        .find(|cause| cause.as_str() == token)
    }

    /// Whether a worker that is already running finishes this condense, so
    /// the caller is told to wait rather than to retry or look.
    pub(crate) fn is_left_to_worker(self) -> bool {
        matches!(self, Self::WorkerHoldsLock)
    }

    /// Whether no writer in this build emits the cause any more: it is read
    /// from old journals and never defers a session today.
    #[cfg(test)]
    pub(crate) fn is_retired(self) -> bool {
        matches!(self, Self::NoCommitToAnchor)
    }

    /// What stopped the condense, in one clause.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::WorkerHoldsLock => "a capture worker holds the member's queue lock",
            Self::OwnershipUnready => "run-state ownership or migration remains unresolved",
            Self::NoIdleEdge => "the session machine did not ask for a condense",
            Self::NoCommitToAnchor => {
                "no commit to anchor to (retired cause: a condense no longer needs one)"
            }
            Self::ArchiveProducedNothing => "the archive ran and minted no checkpoint",
            Self::DoneMarkFailed => {
                "the checkpoint was written but the session could not be closed"
            }
        }
    }
}

impl SessionSweep {
    /// Record one session left owed, under the cause that stopped it.
    fn defer(&mut self, session_id: String, cause: DeferCause) {
        self.condense_deferred.push(Deferred { session_id, cause });
    }

    /// The ids of the sessions left owed, in the order they were deferred.
    pub(crate) fn deferred_ids(&self) -> Vec<&str> {
        self.condense_deferred
            .iter()
            .map(|d| d.session_id.as_str())
            .collect()
    }

    /// Each deferred session's cause token, keyed by session id: the shape a
    /// scripted caller reads to decide whether to wait or to look.
    pub(crate) fn deferred_causes_json(&self) -> serde_json::Value {
        self.condense_deferred
            .iter()
            .map(|d| (d.session_id.clone(), d.cause.as_str().into()))
            .collect::<serde_json::Map<_, _>>()
            .into()
    }
}

/// The crashed-session sweep, and the **driver** the
/// `IdleTimeout` trigger never had.
///
/// A session cache is written on every prompt, tool call and turn end, so a
/// cache that still claims a live phase after a full idle window means the
/// agent is gone: killed, rebooted out from under, terminal closed. Nothing
/// used to notice. The session stayed `active` forever, was never condensed,
/// and — because `live_sessions` returns most-recently-updated first — kept
/// being offered to every later archive as the session to attribute against.
///
/// The sweep asks the state machine, it does not decide for it: `IdleTimeout`
/// is the trigger, `Ending` + `RequestCondense` is what the machine answers,
/// and the archive that follows is the ordinary session-end condense — the same
/// `ArchiveReason::SessionEnd` an agent's own `SessionEnd` hook produces,
/// because it is the same effect from the same machine and inventing a second
/// reason token would widen an audit vocabulary for no new fact.
///
/// It is also the **retry** for a condense that was requested and never landed,
/// which is a state nothing else in the product can leave: `Ending` has no
/// `IdleTimeout` edge (that is what keeps a repeated sweep from asking for a
/// second condense), and `post-commit` runs the strategy decision, never a
/// pending `RequestCondense`. So a session left `ending` — by an earlier
/// deferral, or by a `SessionEnd` hook that could not reach the archive — would
/// otherwise be counted live by `live_sessions` forever and condensed by
/// nothing. This pass re-attempts it, and the idle window is what keeps that
/// safe: a session a hook is still closing was touched seconds ago, so it is
/// not idle and this pass does not see it.
///
/// A session is only ever closed on an archive that actually ran. A
/// repository with nothing to archive against closes nothing, because
/// `Ended` is left only by the session's own next hook, and an agent that is
/// gone sends none: closing there would spend the one condense it had, so
/// that case defers instead. The hook path holds the same
/// line at `SessionEnd` (see the note at `drive_post_ingest`), which is why this
/// pass finds `ending` sessions no agent will ever speak for again.
///
/// **A swept session that is then resumed is working again.** Its next hook
/// returns it to `active` (the machine's `Activity` and `Start` edges out of
/// `ending` and `ended`), and its next end, or the next sweep after it goes
/// quiet, condenses the bytes since the last checkpoint. Nothing is lost and
/// nothing double-condenses: an unchanged session re-condenses to a no-op.
///
/// One session's failure never stops the sweep: each is finalized on its own
/// and a failure leaves that session live for the next pass.
///
/// **Every condense here runs under the member's capture-queue lock**, the
/// one the detached worker drains under. A worker archiving a queued commit
/// and this pass condensing a session against the same HEAD are two archivers
/// of one commit, and two archivers of one commit can each read the chain
/// before either appends. The lock is taken twice, never waited on across
/// both: the rescue drain takes and releases it, then the member sweep takes
/// it again. When a worker holds it, this pass condenses nothing and reports
/// its idle sessions as `condense_deferred`, because that worker's own tail
/// sweep condenses them under the lock it already holds. `rift doctor`
/// therefore never waits; a targeted `rift session stop` waits briefly for
/// the rescue, since the operator named that session and a worker is usually
/// seconds from done.
///
/// # Errors
/// [`rift_core::RiftError`] only if the repository, its projection, or the
/// session directory cannot be opened at all.
pub(crate) fn sweep_stale_sessions(
    root: &Path,
    now: i64,
    timeout_ms: i64,
    only: Option<&str>,
) -> Result<SessionSweep> {
    let repo = rift_git::discover(root)?;
    let mut sweep = SessionSweep::default();
    let wait = match only {
        Some(_) => LockWait::Brief,
        None => LockWait::None,
    };
    rescue_agent_jobs(&repo.git_dir, only, wait, &mut sweep);
    // The caller's clock is the wall clock here (doctor and the stop verb
    // both read it), so it doubles as the lock's.
    sweep_member_under_its_lock(&repo, now, now, timeout_ms, only, &mut sweep)?;
    Ok(sweep)
}

/// The automatic idle sweep: every member of the family `git_dir` belongs
/// to, at the family's floor (`[capture] idle_sweep_minutes`, read at the
/// main checkout) unless `floor_ms` names one.
///
/// This is what ends a session whose agent went away without a
/// `SessionEnd` — an editor panel closed, a process killed, an agent that
/// publishes no end at all — minutes after it went quiet rather than on the
/// next `rift doctor --fix`. It runs where work already runs, at every
/// `SessionStart` and at the tail of every worker pass, never on a timer.
/// A member whose checkout is gone is skipped, as doctor's sweep skips it,
/// and one member that cannot be swept never stops the others.
/// Unlike [`sweep_stale_sessions`] it rescues no queued jobs: its caller is
/// the worker that is draining them.
///
/// The caller holds the queue lock of `git_dir`'s member only, and the locks
/// are per member. So its own member is swept under that lock, never by
/// taking it again (a second take would find it held and defer everything),
/// and every other member is swept only under that member's own lock: a
/// sibling whose worker holds it may be archiving the very commit this pass
/// would condense against, and that worker's tail sweep reaches the
/// sibling's idle sessions anyway, so the sibling is left to it.
pub(crate) fn sweep_idle_family(
    owner: &crate::capture_queue::QueueLock,
    now: i64,
    floor_ms: Option<i64>,
) -> SessionSweep {
    let git_dir = owner.git_dir();
    let mut sweep = SessionSweep::default();
    let Some(work_tree) = rift_shadow::hooks::session_cache::work_tree_of(git_dir) else {
        return sweep;
    };
    let Ok(paths) = rift_git::discover(&work_tree) else {
        return sweep;
    };
    let journal = crate::session_verbs::family_journal(&paths);
    let floor = floor_ms.unwrap_or_else(|| family_sweep_floor(&journal, &paths));
    let held = canonical_git_dir(git_dir);
    for member in journal.iter().map(|j| &j.member) {
        if member.prunable {
            continue;
        }
        let Ok(repo) = rift_git::discover(&member.work_tree) else {
            continue;
        };
        let _ = if canonical_git_dir(&repo.git_dir) == held {
            if crate::family::run_state::prepare_owned(owner).is_ok() {
                sweep_member(&repo, now, floor, None, &mut sweep)
            } else {
                defer_unready_member(&repo, now, floor, None, &mut sweep)
            }
        } else {
            // A sibling requires its own open-file owner, regardless of the
            // decision clock or the age of any legacy marker.
            sweep_member_under_its_lock(&repo, now, now_ms(), floor, None, &mut sweep)
        };
    }
    let ttl = super::worker::linehash::LINEHASH_TTL_MS;
    super::worker::linehash::sweep_linehash_cache(&paths.common_dir, now, ttl);
    // The graft indexes a killed worker left: the sweep is the one remover,
    // never a hook, and it takes only what nobody holds and a day has passed on.
    for member in journal.iter().map(|j| &j.member) {
        if let Ok(repo) = rift_git::discover(&member.work_tree) {
            let _ = rift_git::sweep_stale_graft_indexes(&repo.git_dir, now);
        }
    }
    sweep
}

/// The family's sweep floor, read at its main checkout: one family, one
/// floor, whichever member's worker happens to run the pass.
pub(crate) fn family_sweep_floor(
    journal: &[crate::session_verbs::MemberJournal],
    paths: &rift_git::RepoPaths,
) -> i64 {
    let main = journal
        .iter()
        .find(|j| j.member.is_main)
        .map_or(paths.work_tree.as_path(), |j| j.member.work_tree.as_path());
    rift_config::idle_sweep_ms(&main.join(".rift"))
}

/// A git dir spelled one way whichever path reached it, so the caller's own
/// member is recognised however its `--git-dir` was written.
fn canonical_git_dir(git_dir: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf())
}

/// [`sweep_member`] for a caller that holds no lock: take the member's
/// capture-queue lock first, and when a worker holds it, condense nothing
/// and report the idle sessions deferred to that worker's tail sweep.
///
/// The lock is taken only once some session is idle, because the automatic
/// sweep runs this for every sibling on every worker pass, and a held lock
/// is a sibling's hook drain that finds the queue busy. The sessions are
/// read again under the lock: the worker that held it may have ended them.
/// `lock_now` is passed through for compatibility; ownership is an open-file
/// lock, and a legacy marker blocks regardless of age.
fn sweep_member_under_its_lock(
    repo: &rift_git::RepoPaths,
    now: i64,
    _lock_now: i64,
    timeout_ms: i64,
    only: Option<&str>,
    sweep: &mut SessionSweep,
) -> Result<()> {
    let (idle, live) = idle_and_live(repo, now, timeout_ms, only)?;
    if idle.is_empty() {
        sweep.live.extend(live.into_iter().map(|c| c.session_id));
        return Ok(());
    }
    let owner = match crate::capture_queue::acquire_owner(&repo.git_dir) {
        Ok(owner) => owner,
        Err(block) => {
            let cause = if block == crate::capture_queue::OwnerBlock::ActiveOwner {
                DeferCause::WorkerHoldsLock
            } else {
                DeferCause::OwnershipUnready
            };
            sweep
                .live
                .extend(live.into_iter().map(|cache| cache.session_id));
            for cache in idle {
                sweep.defer(cache.session_id, cause);
            }
            return Ok(());
        }
    };
    if crate::family::run_state::prepare_owned(&owner).is_err() {
        return defer_unready_member(repo, now, timeout_ms, only, sweep);
    }
    sweep_member(repo, now, timeout_ms, only, sweep)
}

fn defer_unready_member(
    repo: &rift_git::RepoPaths,
    now: i64,
    timeout_ms: i64,
    only: Option<&str>,
    sweep: &mut SessionSweep,
) -> Result<()> {
    let (idle, live) = idle_and_live(repo, now, timeout_ms, only)?;
    sweep
        .live
        .extend(live.into_iter().map(|cache| cache.session_id));
    for cache in idle {
        sweep.defer(cache.session_id, DeferCause::OwnershipUnready);
    }
    Ok(())
}

/// Sweep one member's live sessions under the member's capture-queue lock,
/// which the caller holds: those inside the idle window are reported live,
/// the rest are finalized. Nothing is opened unless some session is idle,
/// because this runs on every worker pass.
fn sweep_member(
    repo: &rift_git::RepoPaths,
    now: i64,
    timeout_ms: i64,
    only: Option<&str>,
    sweep: &mut SessionSweep,
) -> Result<()> {
    let (idle, live) = idle_and_live(repo, now, timeout_ms, only)?;
    sweep
        .live
        .extend(live.into_iter().map(|cache| cache.session_id));
    if idle.is_empty() {
        return Ok(());
    }
    let ctx = open_sweep(repo, now)?;
    for cache in idle {
        finalize_idle_session(&ctx, &cache, sweep);
    }
    Ok(())
}

/// One member's live sessions, split into those idle past `timeout_ms` and
/// those still inside it.
fn idle_and_live(
    repo: &rift_git::RepoPaths,
    now: i64,
    timeout_ms: i64,
    only: Option<&str>,
) -> Result<(Vec<SessionCache>, Vec<SessionCache>)> {
    use rift_shadow::hooks::session_cache::{is_idle_past, live_sessions_for};

    let mut sessions = live_sessions_for(
        &crate::family::run_state::sessions_dir(&repo.git_dir),
        &crate::family::run_state::sessions_dir(&repo.common_dir),
        &repo.work_tree,
    )?;
    // `rift session stop ID` finalizes one session; the others are not even
    // looked at, so a targeted stop can never sweep a neighbour.
    if let Some(target) = only {
        sessions.retain(|s| s.session_id == target);
    }
    Ok(sessions
        .into_iter()
        .partition(|cache| is_idle_past(cache, now, timeout_ms)))
}

/// Time one idle session out, condense it, and close it; what happened is
/// recorded on `sweep`.
fn finalize_idle_session(ctx: &SweepCtx<'_>, cache: &SessionCache, sweep: &mut SessionSweep) {
    use rift_shadow::hooks::{Effect, SessionState};

    let id = cache.session_id.clone();
    // A session already in `ending` owes a condense from an earlier pass (or
    // from a hook that could not reach the archive), and asking the machine
    // again would answer nothing — `Ending` has no `IdleTimeout` edge. That
    // refusal is what makes a repeated sweep idempotent, and it is also what
    // would strand the session forever if this pass simply reported it. So
    // the condense is re-attempted directly instead: the transition it needs
    // has already happened, only the effect is outstanding.
    if cache.state != SessionState::Ending.as_str() {
        let timed_out = ctx.rt.block_on(rift_shadow::bridge::timeout_idle_session(
            &ctx.store,
            &crate::family::run_state::sessions_dir(&ctx.repo.git_dir),
            &id,
            ctx.now,
        ));
        let Ok((_, effects)) = timed_out else {
            sweep.live.push(id);
            return;
        };
        if !effects.contains(&Effect::RequestCondense) {
            // The machine refused a state with no idle edge. Unreachable
            // through `live_sessions` today (it yields only active /
            // compacting / ending), so nothing is owed and nothing is done.
            sweep.defer(id, DeferCause::NoIdleEdge);
            return;
        }
    }
    settle_last_turn(ctx, cache);
    let checkpoint = match condense_swept_session(ctx, cache) {
        Ok(checkpoint) => checkpoint,
        Err(cause) => return sweep.defer(id, cause),
    };
    sweep.condensed_checkpoints.push(checkpoint);
    if mark_swept_condensed(ctx, cache) {
        sweep.finalized.push(id);
    } else {
        sweep.defer(id, DeferCause::DoneMarkFailed);
    }
}

/// The first thing the sweep does: drain the jobs a detached agent-hook
/// worker left undone — killed, never started, or still owed after its
/// bounded retries — whatever the sessions' idle state. A session whose
/// `SessionEnd` hook journaled a condense that no worker finished is exactly
/// what the sweep exists to rescue, and it was touched seconds before the
/// crash, so the idle window alone would never reach it. Inline, and waiting
/// at most `wait`: a worker that holds the lock is doing this work already.
/// The condenses that landed are witnessed on the sweep like its own.
fn rescue_agent_jobs(git_dir: &Path, only: Option<&str>, wait: LockWait, sweep: &mut SessionSweep) {
    for (session, checkpoint) in drain_jobs(git_dir.to_str(), wait) {
        if only.is_none_or(|t| t == session) {
            sweep.finalized.push(session);
            sweep.condensed_checkpoints.push(checkpoint);
        }
    }
}

/// Open what every session of one sweep is finalized against, once: the
/// projection, a runtime to drive it, and the three facts a session-end
/// condense reads.
///
/// `head` is resolved here rather than per session because every finalized
/// session condenses against the current HEAD, exactly as a session-end hook
/// would.
fn open_sweep(repo: &rift_git::RepoPaths, now: i64) -> Result<SweepCtx<'_>> {
    let store = rift_store::open_capture_projection(projection_db_at(&repo.work_tree)?)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let head = rift_git::GixBlobReader::open(&repo.work_tree)
        .ok()
        .and_then(|r| r.head_hex().ok().flatten());
    let strategy = strategy_kind(effective_config(&repo.work_tree).checkpoint_strategy());
    let repo_id = crate::family::try_at(&repo.work_tree)
        .map(|f| f.family_id.to_string())
        .unwrap_or_default();
    Ok(SweepCtx {
        repo,
        store,
        rt,
        head,
        strategy,
        repo_id,
        now,
    })
}

/// Settle the session's last turn, which no hook will ever settle — this pass
/// is the backstop (Decision 1): record it on its last event before the
/// condense reads the projection. Best-effort, like everything else in the
/// sweep.
fn settle_last_turn(ctx: &SweepCtx<'_>, cache: &SessionCache) {
    let _ = ctx
        .rt
        .block_on(rift_shadow::bridge::settle_closed_turn_usage(
            &ctx.store,
            &crate::family::run_state::sessions_dir(&ctx.repo.git_dir),
            &cache.session_id,
            &cache.agent,
            &ctx.repo_id,
            rift_shadow::bridge::SettleMode::Final,
            ctx.now,
        ));
}

/// The ordinary session-end condense, against HEAD when there is one, and
/// the checkpoint it minted.
///
/// An unborn HEAD is not a deferral: the archive names no anchor and lands
/// (Q78 — a condense never needs a commit). An archive that returned without
/// minting is not a condense, however successfully it returned: only a minted
/// checkpoint id counts, and anything else answers the cause the session is
/// deferred under.
fn condense_swept_session(
    ctx: &SweepCtx<'_>,
    cache: &SessionCache,
) -> std::result::Result<rift_shadow::CheckpointId, DeferCause> {
    use rift_shadow::hooks::strategy::{ArchiveDecision, ArchiveReason};

    let archived = archive_now(
        &ctx.rt,
        &ctx.store,
        &ctx.repo.work_tree,
        ctx.head.as_deref(),
        &cache.session_id,
        &cache.agent,
        ArchiveDecision {
            archive: true,
            reason: ArchiveReason::SessionEnd,
            strategy: ctx.strategy,
        },
        ctx.now,
    );
    match archived {
        Ok(Some(checkpoint)) => Ok(checkpoint),
        Ok(None) | Err(_) => Err(DeferCause::ArchiveProducedNothing),
    }
}

/// Tell the machine the condense landed, closing the session; `false` leaves
/// it for the next sweep to retry.
fn mark_swept_condensed(ctx: &SweepCtx<'_>, cache: &SessionCache) -> bool {
    ctx.rt
        .block_on(rift_shadow::bridge::mark_condense_done(
            &ctx.store,
            &crate::family::run_state::sessions_dir(&ctx.repo.git_dir),
            &cache.session_id,
            &cache.agent,
            ctx.now,
        ))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The injected decision clock; ownership does not depend on age.
    const NOW: i64 = 1_790_000_000_000;

    /// An `active` session cache last touched at `updated_at_ms`.
    fn seed_session(git_dir: &Path, id: &str, updated_at_ms: i64) {
        let sessions = crate::family::run_state::sessions_dir(git_dir);
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join(format!("{id}.json")),
            format!(
                r#"{{"schema":1,"session_id":"{id}","state":"active","agent":"claude-code",
"current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,
"updated_at_ms":{updated_at_ms},"native_id_map":{{}}}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn an_idle_session_beside_a_held_queue_lock_is_deferred_to_the_worker_alone() {
        let tmp = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(tmp.path()).unwrap();
        let repo = rift_git::discover(tmp.path()).unwrap();
        seed_session(&repo.git_dir, "idle-1", 0);
        let held = crate::capture_queue::acquire_lock(&repo.git_dir, NOW).unwrap();

        let mut sweep = SessionSweep::default();
        sweep_member_under_its_lock(&repo, NOW, NOW, 60_000, None, &mut sweep).unwrap();

        let deferred = Deferred {
            session_id: "idle-1".to_owned(),
            cause: DeferCause::WorkerHoldsLock,
        };
        assert_eq!(sweep.condense_deferred, vec![deferred]);
        assert!(sweep.finalized.is_empty() && sweep.condensed_checkpoints.is_empty());
        assert_eq!(
            sweep.deferred_causes_json(),
            serde_json::json!({ "idle-1": "worker-holds-lock" })
        );
        drop(held);
    }

    #[test]
    fn a_legacy_barrier_defers_an_idle_session_without_claiming_an_active_owner() {
        let tmp = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(tmp.path()).unwrap();
        let repo = rift_git::discover(tmp.path()).unwrap();
        seed_session(&repo.git_dir, "idle-legacy", 0);
        let cache = crate::family::run_state::sessions_dir(&repo.git_dir).join("idle-legacy.json");
        let before = std::fs::read(&cache).unwrap();
        let legacy = repo.git_dir.join("rift-capture-queue/.lock");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"unknown-legacy-liveness").unwrap();
        let mut sweep = SessionSweep::default();
        sweep_member_under_its_lock(&repo, NOW, NOW, 60_000, None, &mut sweep).unwrap();
        assert_eq!(
            sweep.condense_deferred,
            vec![Deferred {
                session_id: "idle-legacy".into(),
                cause: DeferCause::OwnershipUnready
            }]
        );
        assert!(sweep.finalized.is_empty() && sweep.condensed_checkpoints.is_empty());
        assert_eq!(std::fs::read(&cache).unwrap(), before);
        assert_eq!(std::fs::read(&legacy).unwrap(), b"unknown-legacy-liveness");
    }

    /// Six tokens, each distinct and each parsing back to itself; only the
    /// held lock is a worker's to finish; and exactly one token is retired —
    /// `no-commit-to-anchor` still reads from an old journal, and no writer in
    /// this file names it (L-4: a condense never needs a commit).
    #[test]
    fn every_defer_cause_has_its_own_token_and_only_the_lock_is_left_to_a_worker() {
        let all = [
            DeferCause::WorkerHoldsLock,
            DeferCause::OwnershipUnready,
            DeferCause::NoIdleEdge,
            DeferCause::NoCommitToAnchor,
            DeferCause::ArchiveProducedNothing,
            DeferCause::DoneMarkFailed,
        ];
        let mut tokens: Vec<&str> = all.iter().map(|c| c.as_str()).collect();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), all.len());
        for cause in all {
            assert_eq!(DeferCause::parse(cause.as_str()), Some(cause));
        }
        assert_eq!(
            DeferCause::parse("no-commit-to-anchor"),
            Some(DeferCause::NoCommitToAnchor)
        );
        assert_eq!(DeferCause::parse("not-a-cause"), None);
        let left: Vec<DeferCause> = all.into_iter().filter(|c| c.is_left_to_worker()).collect();
        assert_eq!(left, vec![DeferCause::WorkerHoldsLock]);
        let retired: Vec<DeferCause> = all.into_iter().filter(|c| c.is_retired()).collect();
        assert_eq!(retired, vec![DeferCause::NoCommitToAnchor]);
        let source = include_str!("sweep.rs");
        let writers = source
            .lines()
            .filter(|line| line.contains("Err(DeferCause::NoCommitToAnchor)"))
            .count();
        assert_eq!(writers, 0, "the retired cause has no writer left");
    }
}
