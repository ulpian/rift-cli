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
