//! The stale-session sweep: closing sessions no hook will close, rescuing the
//! agent jobs they left queued, and archiving what they captured. Separate
//! because it is driven from `rift doctor` and the session verbs, over every
//! session of a member, rather than by the one hook that is running.

use std::path::Path;

use rift_core::Result;
use rift_shadow::hooks::session_cache::SessionCache;

use super::archive::archive_now;
use super::git::{effective_config, strategy_kind};
use super::projection_db_at;
use super::worker::{drain_jobs, LockWait};

/// What one crashed-session sweep found and did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionSweep {
    /// Sessions still legitimately live (touched inside the idle window).
    pub(crate) live: Vec<String>,
    /// Sessions this pass finalized: `IdleTimeout` → condense → `Ended`.
    pub(crate) finalized: Vec<String>,
    /// Sessions this pass moved to `ending` — or found already there, left by an
    /// earlier pass or by a `SessionEnd` hook whose archive did not land — and
    /// could not condense: no commit to anchor a checkpoint to, or no archive
    /// backend to write one with. They stay live, and **the next sweep retries
    /// them**. Nothing else does: `Ending` has no `IdleTimeout` edge, so the
    /// machine will not re-request a condense, and `post-commit` only ever runs
    /// the strategy decision — it never picks up a pending `RequestCondense`.
    pub(crate) condense_deferred: Vec<String>,
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

/// What every session of one sweep is finalized against, opened once by
/// [`open_sweep`] so the per-session steps read it rather than reopen it.
struct SweepCtx<'a> {
    /// The member being swept.
    repo: &'a rift_git::RepoPaths,
    /// Its capture projection.
    store: rift_store::SqliteStore,
    /// The runtime every bridge call is driven on.
    rt: tokio::runtime::Runtime,
    /// The commit every condense anchors to; `None` on an unborn HEAD.
    head: Option<String>,
    /// The configured checkpoint strategy, recorded on each decision.
    strategy: rift_shadow::hooks::strategy::StrategyKind,
    /// The family id the last turn's usage is settled under.
    repo_id: String,
    /// The sweep's clock.
    now: i64,
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
    rescue_agent_jobs(&repo.git_dir, only, &mut sweep);
    sweep_member(&repo, now, timeout_ms, only, &mut sweep)?;
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
/// `SessionStart` and at the tail of every worker pass, never on a timer: the
/// caller holds the queue lock, so it never races a condense of the same
/// session. A member whose checkout is gone is skipped, as doctor's sweep
/// skips it, and one member that cannot be swept never stops the others.
/// Unlike [`sweep_stale_sessions`] it rescues no queued jobs: its caller is
/// the worker that is draining them.
pub(crate) fn sweep_idle_family(git_dir: &Path, now: i64, floor_ms: Option<i64>) -> SessionSweep {
    let mut sweep = SessionSweep::default();
    let Some(work_tree) = rift_shadow::hooks::session_cache::work_tree_of(git_dir) else {
        return sweep;
    };
    let Ok(paths) = rift_git::discover(&work_tree) else {
        return sweep;
    };
    let journal = crate::session_verbs::family_journal(&paths);
    let floor = floor_ms.unwrap_or_else(|| family_sweep_floor(&journal, &paths));
    for member in journal.iter().map(|j| &j.member) {
        if member.prunable {
            continue;
        }
        if let Ok(repo) = rift_git::discover(&member.work_tree) {
            let _ = sweep_member(&repo, now, floor, None, &mut sweep);
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

/// Sweep one member's live sessions: those inside the idle window are
/// reported live, the rest are finalized. Nothing is opened unless some
/// session is idle, because this runs on every worker pass.
fn sweep_member(
    repo: &rift_git::RepoPaths,
    now: i64,
    timeout_ms: i64,
    only: Option<&str>,
    sweep: &mut SessionSweep,
) -> Result<()> {
    use rift_shadow::hooks::session_cache::{is_idle_past, live_sessions_for};

    let mut sessions = live_sessions_for(&repo.git_dir, &repo.common_dir, &repo.work_tree)?;
    // `rift session stop ID` finalizes one session; the others are not even
    // looked at, so a targeted stop can never sweep a neighbour.
    if let Some(target) = only {
        sessions.retain(|s| s.session_id == target);
    }
    let (idle, live): (Vec<_>, Vec<_>) = sessions
        .into_iter()
        .partition(|cache| is_idle_past(cache, now, timeout_ms));
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
            &ctx.repo.git_dir,
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
            sweep.condense_deferred.push(id);
            return;
        }
    }
    settle_last_turn(ctx, cache);
    let minted = condense_swept_session(ctx, cache);
    let condensed = minted.is_some();
    if let Some(checkpoint) = minted {
        sweep.condensed_checkpoints.push(checkpoint);
    }
    if condensed && mark_swept_condensed(ctx, cache) {
        sweep.finalized.push(id);
    } else {
        sweep.condense_deferred.push(id);
    }
}

/// The first thing the sweep does: drain the jobs a detached agent-hook
/// worker left undone — killed, never started, or still owed after its
/// bounded retries — whatever the sessions' idle state. A session whose
/// `SessionEnd` hook journaled a condense that no worker finished is exactly
/// what the sweep exists to rescue, and it was touched seconds before the
/// crash, so the idle window alone would never reach it. Inline and never
/// waiting: a worker that holds the lock is doing this work already. The
/// condenses that landed are witnessed on the sweep like its own.
fn rescue_agent_jobs(git_dir: &Path, only: Option<&str>, sweep: &mut SessionSweep) {
    for (session, checkpoint) in drain_jobs(git_dir.to_str(), LockWait::None) {
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
            &ctx.repo.git_dir,
            &cache.session_id,
            &cache.agent,
            &ctx.repo_id,
            rift_shadow::bridge::SettleMode::Final,
            ctx.now,
        ));
}

/// The ordinary session-end condense, against HEAD, and the checkpoint it
/// minted.
///
/// An archive that was skipped for want of a backend is not a condense,
/// however successfully it returned: only a minted checkpoint id counts, so a
/// repository with no commit to anchor to, or no backend to write with,
/// answers `None`.
fn condense_swept_session(
    ctx: &SweepCtx<'_>,
    cache: &SessionCache,
) -> Option<rift_shadow::CheckpointId> {
    use rift_shadow::hooks::strategy::{ArchiveDecision, ArchiveReason};

    let commit = ctx.head.as_ref()?;
    archive_now(
        &ctx.rt,
        &ctx.store,
        &ctx.repo.work_tree,
        commit,
        &cache.session_id,
        &cache.agent,
        ArchiveDecision {
            archive: true,
            reason: ArchiveReason::SessionEnd,
            strategy: ctx.strategy,
        },
        ctx.now,
    )
    .ok()
    .flatten()
}

/// Tell the machine the condense landed, closing the session; `false` leaves
/// it for the next sweep to retry.
fn mark_swept_condensed(ctx: &SweepCtx<'_>, cache: &SessionCache) -> bool {
    ctx.rt
        .block_on(rift_shadow::bridge::mark_condense_done(
            &ctx.store,
            &ctx.repo.git_dir,
            &cache.session_id,
            &cache.agent,
            ctx.now,
        ))
        .is_ok()
}
