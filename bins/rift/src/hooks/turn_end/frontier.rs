//! The turn-seal frontier sidecar (L-5 slice 5): what each turn-end
//! checkpoint sealed, so the lag between the live transcript and the last
//! seal can be said without opening a store or a shard (design Q9: the cut is
//! the last complete record the worker read, the frontier recorded, the lag
//! said).
//!
//! One file per session, `<sessions>/<session>.turn-seal`, under the
//! family-keyed sessions root, published whole by a same-directory rename so a
//! reader sees the previous record or the next, never a torn one. Only the
//! worker that minted the checkpoint writes it, after the shard landed; doctor
//! only reads it.

use std::path::Path;

#[cfg(test)]
pub(crate) use rift_shadow::hooks::turn_frontier::LAG_BOUND_BYTES;
pub(crate) use rift_shadow::hooks::turn_frontier::{write, CarriedEntry, SealFallback};
use rift_shadow::turn_seal::{ChainBreak, CompactCarry, TurnSeal};

/// The sidecar record as the writer knows it: with the compactor's carry.
pub(crate) type TurnFrontier = rift_shadow::hooks::turn_frontier::TurnFrontier<CompactCarry>;

/// One sidecar as doctor reads it.
pub(crate) type Recorded = rift_shadow::hooks::turn_frontier::Recorded<CompactCarry>;

/// What a seal says before its checkpoint id is known.
#[derive(Clone, Debug)]
pub(crate) struct SealRead {
    read_bytes: u64,
    sealed_lines: Option<u64>,
    fallback: Option<SealFallback>,
    pub(super) compact: Option<CompactCarry>,
    /// The subagent transcripts and originals the shard holds, for the next
    /// turn end's carry.
    pub(crate) carried: Vec<CarriedEntry>,
}

/// The fallback a broken chain names, by doctor's closed word.
fn fallback_of(broken: ChainBreak) -> SealFallback {
    match broken {
        ChainBreak::PacksChanged => SealFallback::PacksChanged,
        ChainBreak::SegmentMismatch => SealFallback::SegmentMismatch,
        ChainBreak::LineCount
        | ChainBreak::LeafMismatch
        | ChainBreak::Truncated
        | ChainBreak::OidMismatch => SealFallback::SegmentChainBroken,
    }
}

impl SealRead {
    pub(super) fn of(read: usize, seal: &TurnSeal) -> Self {
        let (sealed_lines, fallback) = match seal {
            TurnSeal::Appended { document_lines, .. } => (Some(*document_lines), None),
            TurnSeal::Whole { broken, .. } => (None, Some(fallback_of(*broken))),
            TurnSeal::Unchanged => (None, None),
        };
        Self {
            read_bytes: u64::try_from(read).unwrap_or(u64::MAX),
            sealed_lines,
            fallback,
            compact: None,
            carried: Vec::new(),
        }
    }
}

/// Record the frontier a landed turn-end checkpoint sealed. Loud, never
/// fatal: the shard is already in git and a lost sidecar costs only the lag.
pub(crate) fn record(git_dir: &Path, session: &str, checkpoint: &str, read: SealRead, now: i64) {
    let frontier = TurnFrontier {
        session_id: session.to_owned(),
        checkpoint: checkpoint.to_owned(),
        read_bytes: read.read_bytes,
        sealed_lines: read.sealed_lines,
        fallback: read.fallback,
        sealed_at_ms: now,
        compact: read.compact,
        carried: read.carried,
    };
    let sessions = crate::family::run_state::sessions_dir(git_dir);
    if let Err(e) = write(&sessions, &frontier) {
        tracing::warn!(error = %e, "turn-seal frontier not recorded");
    }
}

/// The compactor state the seal of `checkpoint` left for `session`; `None`
/// when the sidecar is missing, unreadable, or names another checkpoint.
pub(super) fn carry_after(git_dir: &Path, session: &str, checkpoint: &str) -> Option<CompactCarry> {
    let sessions = crate::family::run_state::sessions_dir(git_dir);
    match rift_shadow::hooks::turn_frontier::read_one(&sessions, session.to_owned()) {
        Recorded::Frontier(f) if f.checkpoint == checkpoint => f.compact,
        Recorded::Frontier(_) | Recorded::Unreadable(_) => None,
    }
}

/// Every session's sidecar, by session id; a missing root reads as none.
pub(crate) fn recorded(sessions: &Path) -> Vec<Recorded> {
    rift_shadow::hooks::turn_frontier::recorded(sessions)
}
