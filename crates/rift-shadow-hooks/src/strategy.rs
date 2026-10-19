//! Checkpoint strategy decision logic (pure).
//!
//! [`decide`] is a **pure** function of its [`TriggerCtx`]: same context → same
//! [`ArchiveDecision`], no side effects. The four capture strategies
//! (manual-commit default, on-pr, scheduled, policy-driven) answer one question
//! — *archive now?* — and a `true` answer becomes a
//! `RequestArchive`/`RequestCondense` effect run by the executor (in
//! `rift-shadow`). A fifth token, the **import marker** ([`StrategyKind::Import`]),
//! is not a strategy at all: `rift import` stamps it on the read-only
//! checkpoints it lands from an agent's own store, so provenance can tell a
//! bulk-imported transcript from a checkpoint a live hook archived. It never
//! answers *archive now?* with `true`.
//!
//! **Composition:** policy-driven overlays the base — a change touching a
//! sensitive path archives even under manual-commit. Sensitive-path matching is
//! done upstream (the policy lives in `rift-config`); this layer takes the already
//! computed `sensitive_path_touched` boolean so the decision stays pure and
//! dependency-free.

use crate::SessionState;

/// The four checkpoint strategies, plus the import marker.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StrategyKind {
    /// Archive at `git commit` while a session is/was active. Default; never
    /// pollutes the active branch.
    #[default]
    ManualCommit,
    /// Archive once per PR boundary (a push that maps to a PR).
    OnPr,
    /// Archive when the injected clock crosses an interval while active.
    Scheduled,
    /// Archive immediately on any change touching a sensitive path.
    PolicyDriven,
    /// The import marker: a checkpoint
    /// `rift import` landed from an agent's own session store — a real
    /// position on the chain, read-only, never a live decision. Deliberately
    /// absent from the config vocabulary (`rift_config::CHECKPOINT_STRATEGIES`):
    /// nothing can be configured to import. Distinct from the v13
    /// `imported_checkpoint` relation, which is `--from-legacy`'s
    /// claim-without-position.
    Import,
}

impl StrategyKind {
    /// The canonical lowercase token used in the `archive_decision` audit row.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ManualCommit => "manual-commit",
            Self::OnPr => "on-pr",
            Self::Scheduled => "scheduled",
            Self::PolicyDriven => "policy-driven",
            Self::Import => "import",
        }
    }
}

/// Why a strategy decided to archive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArchiveReason {
    /// A human `git commit` (post-commit).
    Commit,
    /// A PR-open/update signal.
    Pr,
    /// A scheduled interval elapsed.
    Schedule,
    /// A sensitive path was touched (composition overlay).
    SensitivePath,
    /// The session ended (the state machine's `RequestCondense` effect).
    SessionEnd,
    /// A turn ended (the agent's `Stop`): one checkpoint per turn end (L-5).
    TurnEnd,
}

impl ArchiveReason {
    /// The canonical lowercase token used in the `archive_decision` audit row.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Pr => "pr",
            Self::Schedule => "schedule",
            Self::SensitivePath => "sensitive-path",
            Self::SessionEnd => "session-end",
            Self::TurnEnd => "turn-end",
        }
    }
}

/// A git lifecycle event that can drive a checkpoint decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitEvent {
    /// `post-commit` — a commit object now exists.
    PostCommit,
    /// `pre-push` — refs about to leave.
    PrePush,
    /// A PR-open/update signal (host/agent specific).
    PrSignal,
}

/// The decision the executor acts on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ArchiveDecision {
    /// Whether to archive (condense) now.
    pub archive: bool,
    /// The reason carried into the audit row.
    pub reason: ArchiveReason,
    /// The strategy that produced the decision (policy-driven when composed).
    pub strategy: StrategyKind,
}

/// The pure decision context. All fields are caller-supplied so the decision is a
/// total function of its inputs (clock injected; sensitivity pre-computed).
#[derive(Clone, Copy, Debug)]
pub struct TriggerCtx {
    /// The git event, if any drove this evaluation.
    pub git_event: Option<GitEvent>,
    /// The session's current phase.
    pub session_state: SessionState,
    /// Whether the pending change touched a policy-sensitive path.
    pub sensitive_path_touched: bool,
    /// The injected clock (epoch ms).
    pub clock_ms: i64,
    /// When the last archive happened (epoch ms), if ever.
    pub last_archive_ms: Option<i64>,
    /// The scheduled-strategy interval (ms).
    pub schedule_interval_ms: i64,
}

/// Whether the session is live enough to archive (active, compacting, or ending).
/// Idle/Ended never archive.
fn is_live(state: SessionState) -> bool {
    matches!(
        state,
        SessionState::Active | SessionState::Compacting | SessionState::Ending
    )
}

/// The pure checkpoint decision. `base` is the configured strategy; policy-driven
/// composes over it (a sensitive-path touch always archives).
#[must_use]
pub fn decide(base: StrategyKind, ctx: &TriggerCtx) -> ArchiveDecision {
    let live = is_live(ctx.session_state);

    // Composition overlay: a sensitive-path touch forces an immediate checkpoint
    // regardless of the base strategy (so sensitive work is captured + redacted
    // promptly even under manual-commit).
    if live && ctx.sensitive_path_touched {
        return ArchiveDecision {
            archive: true,
            reason: ArchiveReason::SensitivePath,
            strategy: StrategyKind::PolicyDriven,
        };
    }

    let (archive, reason) = match base {
        StrategyKind::ManualCommit => (
            live && ctx.git_event == Some(GitEvent::PostCommit),
            ArchiveReason::Commit,
        ),
        StrategyKind::OnPr => (
            live && ctx.git_event == Some(GitEvent::PrSignal),
            ArchiveReason::Pr,
        ),
        StrategyKind::Scheduled => (live && schedule_elapsed(ctx), ArchiveReason::Schedule),
        // As a base, policy-driven only archives on a sensitive path (handled
        // above); otherwise it is a no-op.
        StrategyKind::PolicyDriven => (false, ArchiveReason::SensitivePath),
        // The import marker is never a live decision: an imported checkpoint
        // is landed by `rift import` on an operator's terminal, and no hook
        // can be configured to archive under it.
        StrategyKind::Import => (false, ArchiveReason::Commit),
    };

    ArchiveDecision {
        archive,
        reason,
        strategy: base,
    }
}

/// Whether the scheduled interval has elapsed since the last archive (or this is
/// the first archive of a live session).
fn schedule_elapsed(ctx: &TriggerCtx) -> bool {
    match ctx.last_archive_ms {
        None => true,
        Some(last) => ctx.clock_ms.saturating_sub(last) >= ctx.schedule_interval_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> TriggerCtx {
        TriggerCtx {
            git_event: None,
            session_state: SessionState::Active,
            sensitive_path_touched: false,
            clock_ms: 1_000,
            last_archive_ms: None,
            schedule_interval_ms: 600_000,
        }
    }

    #[test]
    fn manual_commit_archives_on_post_commit_when_active() {
        let c = TriggerCtx {
            git_event: Some(GitEvent::PostCommit),
            ..ctx()
        };
        let d = decide(StrategyKind::ManualCommit, &c);
        assert!(d.archive);
        assert_eq!(d.reason, ArchiveReason::Commit);
        assert_eq!(d.strategy, StrategyKind::ManualCommit);
        // a non-commit event does not archive under manual-commit
        let d2 = decide(StrategyKind::ManualCommit, &ctx());
        assert!(!d2.archive);
        // idle session never archives even on a commit
        let idle = TriggerCtx {
            git_event: Some(GitEvent::PostCommit),
            session_state: SessionState::Idle,
            ..ctx()
        };
        assert!(!decide(StrategyKind::ManualCommit, &idle).archive);
    }

    #[test]
    fn on_pr_fires_on_pr_signal_only() {
        let pr = TriggerCtx {
            git_event: Some(GitEvent::PrSignal),
            ..ctx()
        };
        assert!(decide(StrategyKind::OnPr, &pr).archive);
        let commit = TriggerCtx {
            git_event: Some(GitEvent::PostCommit),
            ..ctx()
        };
        assert!(!decide(StrategyKind::OnPr, &commit).archive);
    }

    #[test]
    fn scheduled_fires_once_interval_elapses() {
        // first archive of a live session fires
        assert!(decide(StrategyKind::Scheduled, &ctx()).archive);
        // before the interval, no archive
        let before = TriggerCtx {
            last_archive_ms: Some(900),
            clock_ms: 1_000,
            schedule_interval_ms: 600_000,
            ..ctx()
        };
        assert!(!decide(StrategyKind::Scheduled, &before).archive);
        // after the interval, archive
        let after = TriggerCtx {
            last_archive_ms: Some(0),
            clock_ms: 600_001,
            ..before
        };
        assert!(decide(StrategyKind::Scheduled, &after).archive);
    }

    #[test]
    fn policy_driven_composes_over_base() {
        // a sensitive-path touch archives even under manual-commit, with no commit
        let c = TriggerCtx {
            sensitive_path_touched: true,
            git_event: None,
            ..ctx()
        };
        let d = decide(StrategyKind::ManualCommit, &c);
        assert!(d.archive);
        assert_eq!(d.reason, ArchiveReason::SensitivePath);
        assert_eq!(d.strategy, StrategyKind::PolicyDriven, "overlay wins");
    }

    #[test]
    fn decide_is_pure() {
        let c = TriggerCtx {
            git_event: Some(GitEvent::PostCommit),
            ..ctx()
        };
        assert_eq!(
            decide(StrategyKind::ManualCommit, &c),
            decide(StrategyKind::ManualCommit, &c)
        );
    }

    #[test]
    fn audit_tokens_are_canonical() {
        assert_eq!(StrategyKind::ManualCommit.as_str(), "manual-commit");
        assert_eq!(StrategyKind::PolicyDriven.as_str(), "policy-driven");
        assert_eq!(StrategyKind::Import.as_str(), "import");
        assert_eq!(ArchiveReason::SensitivePath.as_str(), "sensitive-path");
        assert_eq!(ArchiveReason::Schedule.as_str(), "schedule");
    }

    /// The import marker never archives, whatever the trigger — even the
    /// post-commit of a live session, and even with the sensitive-path
    /// overlay off. (The overlay itself still fires under any base, which is
    /// its contract, so it is asserted separately below.)
    #[test]
    fn import_is_never_a_live_decision() {
        for state in [
            SessionState::Idle,
            SessionState::Active,
            SessionState::Compacting,
            SessionState::Ending,
        ] {
            for event in [None, Some(GitEvent::PostCommit), Some(GitEvent::PrSignal)] {
                let c = TriggerCtx {
                    git_event: event,
                    session_state: state,
                    last_archive_ms: None,
                    ..ctx()
                };
                let d = decide(StrategyKind::Import, &c);
                assert!(!d.archive, "{state:?} {event:?}");
                assert_eq!(d.strategy, StrategyKind::Import);
            }
        }
        let overlay = TriggerCtx {
            sensitive_path_touched: true,
            ..ctx()
        };
        assert_eq!(
            decide(StrategyKind::Import, &overlay).strategy,
            StrategyKind::PolicyDriven,
            "the sensitive-path overlay composes over every base"
        );
    }
}
