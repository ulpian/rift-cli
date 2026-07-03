//! P3 S3 — the out-of-tree transient session cache.
//!
//! A small JSON file at `<git_dir>/rift-sessions/<session_id>.json` holds the
//! session's **current** phase so Canopy and the strategy layer can read it
//! without a DB round-trip. It is a *cache*: the append-only `session_transition`
//! rows are the source of truth (a crash leaves a stale file, reconciled on the
//! next event). It lives under the git dir — outside the working tree, so it never
//! shows up as a dirty file — and carries **no raw content**: only ids, the phase,
//! ordinals, and the native→Rift id map.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};
use serde::{Deserialize, Serialize};

/// Directory (under the git dir) holding per-session cache files.
const SESSIONS_DIR: &str = "rift-sessions";
/// Cache file schema version.
pub const SCHEMA: u32 = 1;

/// The transient per-session state. Structural only — no transcript/tool bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCache {
    /// Contract schema version.
    pub schema: u32,
    /// The session id.
    pub session_id: String,
    /// The current phase (`SessionState::as_str`).
    pub state: String,
    /// The agent driving the session.
    pub agent: String,
    /// The current turn ordinal.
    pub current_turn_ordinal: u32,
    /// The compaction boundary ordinal, if mid-compaction.
    pub compaction_boundary: Option<u32>,
    /// Transaction-time of the last update (epoch ms).
    pub updated_at_ms: i64,
    /// Native id → Rift id reconciliation map (ids only).
    #[serde(default)]
    pub native_id_map: BTreeMap<String, String>,
}

impl SessionCache {
    /// A fresh cache for a session entering `state`.
    #[must_use]
    pub fn new(
        session_id: impl Into<String>,
        agent: impl Into<String>,
        state: &str,
        now_ms: i64,
    ) -> Self {
        Self {
            schema: SCHEMA,
            session_id: session_id.into(),
            state: state.to_owned(),
            agent: agent.into(),
            current_turn_ordinal: 0,
            compaction_boundary: None,
            updated_at_ms: now_ms,
            native_id_map: BTreeMap::new(),
        }
    }
}

/// The directory holding session cache files for `git_dir`.
#[must_use]
pub fn sessions_dir(git_dir: &Path) -> PathBuf {
    git_dir.join(SESSIONS_DIR)
}

/// The cache file path for `session_id`.
#[must_use]
pub fn cache_path(git_dir: &Path, session_id: &str) -> PathBuf {
    sessions_dir(git_dir).join(format!("{session_id}.json"))
}

/// Atomically write the cache (temp file + rename) so a reader never sees a torn
/// write.
///
/// # Errors
/// [`RiftError::Io`] on a filesystem failure, [`RiftError::Validity`] if the
/// value cannot be serialized.
pub fn write_cache(git_dir: &Path, c: &SessionCache) -> Result<()> {
    let dir = sessions_dir(git_dir);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_vec_pretty(c)
        .map_err(|e| RiftError::validity(format!("session cache serialize: {e}")))?;
    let tmp = dir.join(format!(".{}.tmp", c.session_id));
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, cache_path(git_dir, &c.session_id))?;
    Ok(())
}

/// Read the cache for `session_id`, or `None` if there is no file yet.
///
/// # Errors
/// [`RiftError::Io`] on a filesystem failure, [`RiftError::Validity`] if the file
/// is not valid cache JSON.
pub fn read_cache(git_dir: &Path, session_id: &str) -> Result<Option<SessionCache>> {
    match std::fs::read(cache_path(git_dir, session_id)) {
        Ok(bytes) => {
            let c = serde_json::from_slice(&bytes)
                .map_err(|e| RiftError::validity(format!("session cache parse: {e}")))?;
            Ok(Some(c))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Read every session cache under `git_dir` whose phase is **live** (active,
/// compacting, or ending) — the sessions a post-commit archive should condense.
/// A missing/empty sessions dir yields an empty list; an unreadable individual
/// file is skipped (never fatal — capture must not abort a git op).
///
/// # Errors
/// [`RiftError::Io`] only if the sessions directory exists but cannot be listed.
pub fn live_sessions(git_dir: &Path) -> Result<Vec<SessionCache>> {
    const LIVE: &[&str] = &["active", "compacting", "ending"];
    let dir = sessions_dir(git_dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(stem) = name
            .to_string_lossy()
            .strip_suffix(".json")
            .map(str::to_owned)
        else {
            continue; // skip temp files / non-cache entries
        };
        if let Ok(Some(cache)) = read_cache(git_dir, &stem) {
            if LIVE.contains(&cache.state.as_str()) {
                out.push(cache);
            }
        }
    }
    // Deterministic order (most-recently-updated first), so a single archive
    // target is stable across runs.
    out.sort_by(|a, b| {
        b.updated_at_ms
            .cmp(&a.updated_at_ms)
            .then(a.session_id.cmp(&b.session_id))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = SessionCache::new("01JSESSION", "claude-code", "active", 42);
        c.current_turn_ordinal = 7;
        c.native_id_map.insert("native-1".into(), "01JRIFT".into());
        write_cache(tmp.path(), &c).unwrap();
        let read = read_cache(tmp.path(), "01JSESSION").unwrap().unwrap();
        assert_eq!(read, c);
    }

    #[test]
    fn missing_cache_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_cache(tmp.path(), "nope").unwrap().is_none());
    }

    #[test]
    fn overwrite_is_atomic_and_latest_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let c1 = SessionCache::new("s", "claude-code", "active", 1);
        write_cache(tmp.path(), &c1).unwrap();
        let c2 = SessionCache::new("s", "claude-code", "ending", 2);
        write_cache(tmp.path(), &c2).unwrap();
        assert_eq!(
            read_cache(tmp.path(), "s").unwrap().unwrap().state,
            "ending"
        );
        // no leftover temp file
        let leftovers: Vec<_> = std::fs::read_dir(sessions_dir(tmp.path()))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file must be renamed away");
    }

    #[test]
    fn live_sessions_filters_by_phase_and_orders_recent_first() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path();
        assert!(live_sessions(gd).unwrap().is_empty(), "no dir → empty");
        write_cache(gd, &SessionCache::new("s-idle", "claude-code", "idle", 1)).unwrap();
        write_cache(gd, &SessionCache::new("s-ended", "claude-code", "ended", 2)).unwrap();
        write_cache(
            gd,
            &SessionCache::new("s-active", "claude-code", "active", 5),
        )
        .unwrap();
        write_cache(
            gd,
            &SessionCache::new("s-ending", "claude-code", "ending", 9),
        )
        .unwrap();
        let live = live_sessions(gd).unwrap();
        let ids: Vec<&str> = live.iter().map(|c| c.session_id.as_str()).collect();
        // only active+ending, most-recent first
        assert_eq!(ids, vec!["s-ending", "s-active"]);
    }

    #[test]
    fn cache_carries_no_raw_content_fields() {
        // Guard: the struct is structural-only. If someone adds a `payload`/`text`
        // field, this test (and review) should force a rethink.
        let c = SessionCache::new("s", "claude-code", "active", 0);
        let json = serde_json::to_string(&c).unwrap();
        for forbidden in ["payload", "transcript", "prompt", "content", "text"] {
            assert!(!json.contains(forbidden), "cache must stay structural-only");
        }
    }
}
