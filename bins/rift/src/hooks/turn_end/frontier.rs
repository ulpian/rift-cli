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

use std::path::{Path, PathBuf};

use rift_shadow::turn_seal::{ChainBreak, CompactCarry, TurnSeal};
use serde::{Deserialize, Serialize};

/// Bytes the live transcript may run past the last seal before doctor names
/// the session. A live session between two Stops always lags a little; this
/// names one whose seals stopped landing (about one long turn of output).
pub(crate) const LAG_BOUND_BYTES: u64 = 1 << 20;

/// The sidecar's file suffix beside the session cache.
const SUFFIX: &str = "turn-seal";

/// How a turn's seal left the segment chain, by doctor's closed word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SealFallback {
    /// An earlier part failed its read-back, so the file was resealed whole.
    SegmentChainBroken,
    /// The rule packs changed since the earlier parts, so the file was
    /// resealed whole.
    PacksChanged,
}

/// What one turn's seal covered: the frontier the worker read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TurnFrontier {
    pub(crate) session_id: String,
    /// The turn-end checkpoint the seal landed in.
    pub(crate) checkpoint: String,
    /// The transcript's length when the worker read it.
    pub(crate) read_bytes: u64,
    /// Raw lines the segments cover; `None` when the file was resealed whole
    /// or nothing new was admitted.
    pub(crate) sealed_lines: Option<u64>,
    pub(crate) fallback: Option<SealFallback>,
    pub(crate) sealed_at_ms: i64,
    /// The compactor's state at this seal, so the next turn compacts only
    /// its new lines; absent from a record an older binary wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) compact: Option<CompactCarry>,
}

/// What a seal says before its checkpoint id is known.
#[derive(Clone, Debug)]
pub(crate) struct SealRead {
    read_bytes: u64,
    sealed_lines: Option<u64>,
    fallback: Option<SealFallback>,
    pub(super) compact: Option<CompactCarry>,
}

/// One sidecar as doctor reads it.
#[derive(Debug)]
pub(crate) enum Recorded {
    Frontier(TurnFrontier),
    /// The file name's session, whose record does not parse.
    Unreadable(String),
}

impl SealFallback {
    fn of(broken: ChainBreak) -> Self {
        match broken {
            ChainBreak::PacksChanged => Self::PacksChanged,
            ChainBreak::LineCount
            | ChainBreak::LeafMismatch
            | ChainBreak::Truncated
            | ChainBreak::OidMismatch => Self::SegmentChainBroken,
        }
    }

    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::SegmentChainBroken => "segment-chain-broken",
            Self::PacksChanged => "packs-changed",
        }
    }
}

impl SealRead {
    pub(super) fn of(read: usize, seal: &TurnSeal) -> Self {
        let (sealed_lines, fallback) = match seal {
            TurnSeal::Appended { document_lines, .. } => (Some(*document_lines), None),
            TurnSeal::Whole { broken, .. } => (None, Some(SealFallback::of(*broken))),
            TurnSeal::Unchanged => (None, None),
        };
        Self {
            read_bytes: u64::try_from(read).unwrap_or(u64::MAX),
            sealed_lines,
            fallback,
            compact: None,
        }
    }
}

impl TurnFrontier {
    /// Bytes the live transcript has grown past this seal's read; `None`
    /// with no transcript on record.
    pub(crate) fn lag_bytes(&self, git_dir: &Path) -> Option<u64> {
        let live = super::transcript_frontier(git_dir, &self.session_id)?;
        Some(live.saturating_sub(self.read_bytes))
    }

    /// Doctor's closed word for this session, `None` when all is well.
    pub(crate) fn finding(&self, lag: Option<u64>) -> Option<&'static str> {
        if let Some(fallback) = self.fallback {
            return Some(fallback.word());
        }
        lag.filter(|l| *l > LAG_BOUND_BYTES).map(|_| "lagging")
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
    match read_one(&sessions, session.to_owned()) {
        Recorded::Frontier(f) if f.checkpoint == checkpoint => f.compact,
        Recorded::Frontier(_) | Recorded::Unreadable(_) => None,
    }
}

/// Publish `frontier` whole: a temp of this writer's own, then a rename.
pub(crate) fn write(sessions: &Path, frontier: &TurnFrontier) -> std::io::Result<()> {
    let Some(path) = path_for(sessions, &frontier.session_id) else {
        return Err(std::io::Error::other(
            "turn-seal sidecar: unsafe session id",
        ));
    };
    let json = serde_json::to_vec(frontier).map_err(std::io::Error::other)?;
    let tmp = sessions.join(format!(
        ".{}.{SUFFIX}.tmp-{}",
        frontier.session_id,
        std::process::id()
    ));
    let written = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Every session's sidecar, by session id; a missing root reads as none.
pub(crate) fn recorded(sessions: &Path) -> Vec<Recorded> {
    let Ok(entries) = std::fs::read_dir(sessions) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .filter_map(|n| n.strip_suffix(&format!(".{SUFFIX}")).map(str::to_owned))
        .collect();
    names.sort();
    names.into_iter().map(|s| read_one(sessions, s)).collect()
}

fn read_one(sessions: &Path, session: String) -> Recorded {
    let parsed = path_for(sessions, &session)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<TurnFrontier>(&b).ok())
        .filter(|f| f.session_id == session);
    match parsed {
        Some(f) => Recorded::Frontier(f),
        None => Recorded::Unreadable(session),
    }
}

fn path_for(sessions: &Path, session: &str) -> Option<PathBuf> {
    rift_shadow::hooks::is_safe_session_id(session)
        .then(|| sessions.join(format!("{session}.{SUFFIX}")))
}
