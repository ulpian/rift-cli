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
//! and Canopy only read it.
//!
//! The format lives here, beside the session cache it sits next to, because
//! the hook binary writes it and the dashboard's data plane reads it, and the
//! dashboard cannot import the binary. The compactor's carry is a type of the
//! capture crate this one cannot see, so the record is generic over it: the
//! writer names the real type, and a reader that only wants the frontier
//! skips the field with [`serde::de::IgnoredAny`].

use std::path::{Path, PathBuf};

use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};

use crate::{is_safe_session_id, read_cache};

/// Bytes the live transcript may run past the last seal before doctor names
/// the session. A live session between two Stops always lags a little; this
/// names one whose seals stopped landing (about one long turn of output).
pub const LAG_BOUND_BYTES: u64 = 1 << 20;

/// The sidecar's file suffix beside the session cache.
const SUFFIX: &str = "turn-seal";

/// How a turn's seal left the segment chain, by doctor's closed word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SealFallback {
    /// An earlier part failed its read-back, so the file was resealed whole.
    SegmentChainBroken,
    /// The rule packs changed since the earlier parts, so the file was
    /// resealed whole.
    PacksChanged,
    /// At a session end, the whole file sealed once differed from the turn
    /// parts, so the whole seal was written in their place (design §3.4).
    SegmentMismatch,
}

/// What one turn's seal covered: the frontier the worker read. `C` is the
/// compactor's carry; a reader that does not need it keeps the default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnFrontier<C = IgnoredAny> {
    /// The session the seal belongs to; the file name says it too.
    pub session_id: String,
    /// The turn-end checkpoint the seal landed in.
    pub checkpoint: String,
    /// The transcript's length when the worker read it.
    pub read_bytes: u64,
    /// Raw lines the segments cover; `None` when the file was resealed whole
    /// or nothing new was admitted.
    pub sealed_lines: Option<u64>,
    /// Why the seal fell back to a whole reseal, when it did.
    pub fallback: Option<SealFallback>,
    /// When the worker recorded the seal, in Unix milliseconds.
    pub sealed_at_ms: i64,
    /// The compactor's state at this seal, so the next turn compacts only
    /// its new lines; absent from a record an older binary wrote.
    #[serde(default = "none", skip_serializing_if = "Option::is_none")]
    pub compact: Option<C>,
    /// The subagent transcripts and spooled originals this checkpoint holds,
    /// so the next turn end writes an unchanged one by its oid instead of
    /// redacting it again; absent from a record an older binary wrote.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried: Vec<CarriedEntry>,
    /// What this turn end's scans of the main transcript found, so the next
    /// turn end scans only the bytes past `scanned_bytes`; absent from a
    /// record an older binary wrote.
    #[serde(default = "none", skip_serializing_if = "Option::is_none")]
    pub scan: Option<ScanCarry>,
}

/// The spill references and claimed originals found in the main transcript
/// up to a line boundary. The next turn end trusts it only when the file
/// still holds the same last bytes before that boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanCarry {
    /// The byte just past the last complete line scanned.
    pub scanned_bytes: u64,
    /// Complete lines before `scanned_bytes`.
    pub scanned_lines: u64,
    /// SHA-256 of the last bytes before `scanned_bytes`, so a rewritten file
    /// is noticed without hashing all of it.
    pub tail_sha256: String,
    /// Tool-use id and the spill address its result names, first naming
    /// winning.
    pub spills: Vec<(String, String)>,
    /// The spooled tool-use ids the scan looked for; a record spooled later
    /// was not looked for, so its arrival sends the next scan back to whole.
    pub wanted: Vec<String>,
    /// Tool-use id and the first line recording its result.
    pub claimed: Vec<(String, u64)>,
}

/// One subagent transcript or spooled original a turn-end shard holds, with
/// the raw bytes it was sealed from named by their hash: equal raw bytes at
/// the next turn end mean the shard's blobs already stand for them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarriedEntry {
    /// The subagent's agent id, or the spooled original's tool-use id.
    pub id: String,
    /// Whether `id` names a subagent transcript or a spooled original.
    pub kind: CarriedKind,
    /// SHA-256 over the rule packs' fingerprint and the raw file the blobs
    /// were sealed from, so changed packs carry nothing.
    pub raw_sha256: String,
    /// Each blob's path inside the shard and its git blob id: the full then
    /// the compact transcript for a subagent, the one body for an original.
    pub blobs: Vec<(String, String)>,
    /// A subagent pair's redaction fallback counts, `[lines, placeholders]`.
    #[serde(default)]
    pub fallback: [u32; 2],
}

/// What a [`CarriedEntry`] holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CarriedKind {
    /// A subagent transcript pair under `tasks/<name>/`.
    Subagent,
    /// A spooled original under a `tool-originals/` tree.
    Original,
}

/// One sidecar as a reader finds it.
#[derive(Debug)]
pub enum Recorded<C = IgnoredAny> {
    /// The record parsed and names its own session.
    Frontier(TurnFrontier<C>),
    /// The file name's session, whose record does not parse.
    Unreadable(String),
}

impl SealFallback {
    /// Doctor's closed word for this fallback.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::SegmentChainBroken => "segment-chain-broken",
            Self::PacksChanged => "packs-changed",
            Self::SegmentMismatch => "segment-mismatch",
        }
    }
}

impl<C> TurnFrontier<C> {
    /// Bytes the live transcript has grown past this seal's read; `None`
    /// with no transcript on record in the session cache under `sessions`.
    #[must_use]
    pub fn lag_bytes(&self, sessions: &Path) -> Option<u64> {
        let path = read_cache(sessions, &self.session_id)
            .ok()
            .flatten()?
            .transcript_path?;
        let live = std::fs::metadata(path).ok()?.len();
        Some(live.saturating_sub(self.read_bytes))
    }

    /// Doctor's closed word for this session, `None` when all is well.
    #[must_use]
    pub fn finding(&self, lag: Option<u64>) -> Option<&'static str> {
        if let Some(fallback) = self.fallback {
            return Some(fallback.word());
        }
        lag.filter(|l| *l > LAG_BOUND_BYTES).map(|_| "lagging")
    }
}

/// Publish `frontier` whole: a temp of this writer's own, then a rename.
///
/// # Errors
/// An unsafe session id, or the write or rename failing.
pub fn write<C: Serialize>(sessions: &Path, frontier: &TurnFrontier<C>) -> std::io::Result<()> {
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
#[must_use]
pub fn recorded<C: DeserializeOwned>(sessions: &Path) -> Vec<Recorded<C>> {
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

/// One session's sidecar; missing or unparsable reads as unreadable.
#[must_use]
pub fn read_one<C: DeserializeOwned>(sessions: &Path, session: String) -> Recorded<C> {
    let parsed = path_for(sessions, &session)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<TurnFrontier<C>>(&b).ok())
        .filter(|f| f.session_id == session);
    match parsed {
        Some(f) => Recorded::Frontier(f),
        None => Recorded::Unreadable(session),
    }
}

/// Serde's default for the carry, spelled out so it asks nothing of `C`.
fn none<C>() -> Option<C> {
    None
}

fn path_for(sessions: &Path, session: &str) -> Option<PathBuf> {
    is_safe_session_id(session).then(|| sessions.join(format!("{session}.{SUFFIX}")))
}
