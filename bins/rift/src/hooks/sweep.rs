//! The stale-session sweep: closing sessions no hook will close, rescuing the
//! agent jobs they left queued, and archiving what they captured. Separate
//! because it is driven from `rift doctor` and the session verbs, over every
//! session of a member, rather than by the one hook that is running.

use std::path::Path;

use rift_core::Result;

use super::archive::archive_now;
use super::git::{effective_config, strategy_kind};
use super::projection_db_at;
use super::worker::{drain_queues, LockWait};

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
/// `Wrapper`-routed repository with no provisioned backend archives nothing, and
/// closing the session there would spend the one chance it had — `Ended` has no
/// outgoing edges — so that case defers instead. The hook path holds the same
/// line at `SessionEnd` (see the note at `drive_post_ingest`), which is why this
/// pass finds `ending` sessions no agent will ever speak for again.
///
/// **A swept session that is then resumed stays `ending` for the rest of its
/// life, and that is not a defect.** The machine has no edge out of `Ending`
/// except `CondenseDone`, so a returning agent's hooks — which are legal, and
/// captured in full: receipts, read facts, turn boundaries — leave the phase
/// where it is while re-stamping the last-touch time. The session is therefore
/// no longer idle, so this pass leaves it alone while the developer works, and
/// condenses it on the first sweep after they stop. Nothing is lost and nothing
/// double-condenses; the only cost is that the phase reads `ending` during a
/// resumed session's second life, which is why it is written down here.
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
    use rift_shadow::hooks::session_cache::{is_idle_past, live_sessions_for};
    use rift_shadow::hooks::strategy::{ArchiveDecision, ArchiveReason};
    use rift_shadow::hooks::{Effect, SessionState};

    let repo = rift_git::discover(root)?;
    let mut sweep = SessionSweep::default();
    rescue_agent_jobs(&repo.git_dir, only, &mut sweep);
    let mut sessions = live_sessions_for(&repo.git_dir, &repo.common_dir, &repo.work_tree)?;
    // `rift session stop ID` finalizes one session; the others are not even
    // looked at, so a targeted stop can never sweep a neighbour.
    if let Some(target) = only {
        sessions.retain(|s| s.session_id == target);
    }
    if sessions.is_empty() {
        return Ok(sweep);
    }
    let store = rift_store::open_capture_projection(projection_db_at(&repo.work_tree)?)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    // Resolved once: every finalized session condenses against the current
    // HEAD, exactly as a session-end hook would.
    let head = rift_git::GixBlobReader::open(&repo.work_tree)
        .ok()
        .and_then(|r| r.head_hex().ok().flatten());
    let strategy = strategy_kind(effective_config(&repo.work_tree).checkpoint_strategy());
    let repo_id = crate::family::try_at(&repo.work_tree)
        .map(|f| f.family_id.to_string())
        .unwrap_or_default();

    for cache in sessions {
        let id = cache.session_id.clone();
        if !is_idle_past(&cache, now, timeout_ms) {
            sweep.live.push(id);
            continue;
        }
        // A session already in `ending` owes a condense from an earlier pass (or
        // from a hook that could not reach the archive), and asking the machine
        // again would answer nothing — `Ending` has no `IdleTimeout` edge. That
        // refusal is what makes a repeated sweep idempotent, and it is also what
        // would strand the session forever if this pass simply reported it. So
        // the condense is re-attempted directly instead: the transition it needs
        // has already happened, only the effect is outstanding.
        if cache.state != SessionState::Ending.as_str() {
            let timed_out = rt.block_on(rift_shadow::bridge::timeout_idle_session(
                &store,
                &repo.git_dir,
                &id,
                now,
            ));
            let Ok((_, effects)) = timed_out else {
                sweep.live.push(id);
                continue;
            };
            if !effects.contains(&Effect::RequestCondense) {
                // The machine refused a state with no idle edge. Unreachable
                // through `live_sessions` today (it yields only active /
                // compacting / ending), so nothing is owed and nothing is done.
                sweep.condense_deferred.push(id);
                continue;
            }
        }
        // The session's last turn is one no hook will ever settle — this pass
        // is the backstop (Decision 1): record it on its last event before
        // the condense reads the projection. Best-effort, like everything
        // else in the sweep.
        let _ = rt.block_on(rift_shadow::bridge::settle_closed_turn_usage(
            &store,
            &repo.git_dir,
            &id,
            &cache.agent,
            &repo_id,
            rift_shadow::bridge::SettleMode::Final,
            now,
        ));
        let minted = head.as_ref().and_then(|commit| {
            archive_now(
                &rt,
                &store,
                &repo.work_tree,
                commit,
                &id,
                &cache.agent,
                ArchiveDecision {
                    archive: true,
                    reason: ArchiveReason::SessionEnd,
                    strategy,
                },
                now,
            )
            // An archive that was skipped for want of a backend is not a
            // condense, however successfully it returned: only a minted
            // checkpoint id counts.
            .ok()
            .flatten()
        });
        let condensed = minted.is_some();
        if let Some(checkpoint) = minted {
            sweep.condensed_checkpoints.push(checkpoint);
        }
        if condensed
            && rt
                .block_on(rift_shadow::bridge::mark_condense_done(
                    &store,
                    &repo.git_dir,
                    &id,
                    &cache.agent,
                    now,
                ))
                .is_ok()
        {
            sweep.finalized.push(id);
        } else {
            sweep.condense_deferred.push(id);
        }
    }
    Ok(sweep)
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
    for (session, checkpoint) in drain_queues(git_dir.to_str(), LockWait::None).unwrap_or_default()
    {
        if only.is_none_or(|t| t == session) {
            sweep.finalized.push(session);
            sweep.condensed_checkpoints.push(checkpoint);
        }
    }
}
