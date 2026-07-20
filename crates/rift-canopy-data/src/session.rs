//! The live-session signal (M4-06): a **local, lenient, total** reader over
//! the capture hooks' per-session cache, `<git_dir>/rift-sessions/*.json`.
//!
//! The writer is `rift-shadow-hooks` (`session_cache.rs`, SCHEMA 1: `schema`,
//! `session_id`, `state`, `agent`, `current_turn_ordinal`, `turn_open`,
//! `compaction_boundary`, `updated_at_ms`). The contract is **mirrored
//! locally** — this crate deliberately takes no shadow-hooks dependency
//! (ADR-0009's dependency list) — and read leniently: unknown fields are
//! ignored, unknown states degrade to "not live", and only the two fields the
//! signal needs (`state`, `updated_at_ms`) are required. Parsing is **total**:
//! corrupt bytes yield [`SessionsRead::Corrupt`] (a distinct error status),
//! never a panic and never a silent "quiet"; files over [`MAX_CACHE_BYTES`]
//! are treated as corrupt rather than parsed.
//!
//! [`sessions_detail`] is the single active-session cache pass. It returns the
//! badge signal plus every live native identity so the atomic System feed can
//! resolve scope without selecting a misleading fallback.

use std::path::Path;

use rift_canopy::{LiveSessions, SessionsRead};

/// Directory (under the git dir) holding per-session cache files — the
/// mirrored half of the writer's contract.
const SESSIONS_DIR: &str = "rift-sessions";

/// A cache file larger than this is treated as corrupt (the writer emits
/// hundreds of bytes; anything huge is not ours).
const MAX_CACHE_BYTES: u64 = 64 * 1024;

/// The staleness window: a session whose cache has not been touched for this
/// long is treated as crash-stale and ignored (the cache is explicitly a
/// cache — a crash leaves the file behind, and the hooks reconcile on the
/// next event). Thirty minutes keeps a genuinely idle-but-open agent session
/// visible (hooks refresh the file on every event) while bounding how long a
/// crashed one can masquerade as live. An M4-06 decision — revisited against
/// real sessions at the M4 gate dogfood (see the map's fog note).
pub const ACTIVE_WINDOW_MS: i64 = 30 * 60 * 1000;

/// The cache states that count as live — mirrored from the writer's
/// `live_sessions` filter.
const LIVE_STATES: &[&str] = &["active", "compacting", "ending"];

/// The one-pass detail read over the session caches: the badge signal
/// ([`SessionsRead`], unchanged contract) plus the id of the **newest live**
/// session — greatest `updated_at_ms`, ties broken toward the greater id so
/// the pick is deterministic. The id anchors the session-scoped panel
/// metrics (ADR-0047); the badge also feeds the status truth table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionsDetail {
    /// The active-sessions badge signal.
    pub read: SessionsRead,
    /// The newest live session's id. `None` when nothing is live, on a
    /// corrupt read, or when the winning cache carries no id (leniency: an
    /// id-less cache still counts toward the badge but cannot anchor).
    pub newest_live: Option<String>,
    /// Every live cache that carries both identity halves, sorted
    /// deterministically by update time then identity.
    pub identities: Vec<LiveSessionIdentity>,
    /// Live caches that count toward the badge but cannot be resolved because
    /// either the native session id or adapter name is absent.
    pub unresolved: u16,
}

/// Identity facts retained from one live top-level capture-session cache.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LiveSessionIdentity {
    /// Adapter name (`claude-code`, ...).
    pub agent: String,
    /// Adapter-native session id.
    pub native_session_id: String,
    /// Cache update time in epoch milliseconds.
    pub updated_at_ms: i64,
}

/// **The** single active-sessions query: distinct live sessions for the repo
/// whose git dir is `git_dir`, judged at `now_ms` (injected — no wall clock).
///
/// A session counts iff its cache file parses, its `state` is live
/// (`active`/`compacting`/`ending`), and its `updated_at_ms` is within
/// [`ACTIVE_WINDOW_MS`] of `now_ms`. A missing sessions directory is
/// `Ok(None)`; any unparseable cache file is [`SessionsRead::Corrupt`] (the
/// distinct error status — never silently "quiet"). `since_ms` is the
/// earliest live session's last update, so "running since" reflects the
/// oldest current activity.
#[must_use]
pub fn active_sessions(git_dir: &Path, now_ms: i64) -> SessionsRead {
    sessions_detail(git_dir, now_ms).read
}

/// The same single pass as [`active_sessions`], also surfacing the newest
/// live session's id (see [`SessionsDetail`]). One `read_dir` walk serves
/// both consumers, so the badge and the metrics anchor can never disagree
/// about which sessions are live.
#[must_use]
pub fn sessions_detail(git_dir: &Path, now_ms: i64) -> SessionsDetail {
    let corrupt = SessionsDetail {
        read: SessionsRead::Corrupt,
        newest_live: None,
        identities: Vec::new(),
        unresolved: 0,
    };
    let dir = git_dir.join(SESSIONS_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return SessionsDetail {
                read: SessionsRead::Ok(None),
                newest_live: None,
                identities: Vec::new(),
                unresolved: 0,
            }
        }
        Err(_) => return corrupt, // exists but unlistable
    };
    let mut live: u16 = 0;
    let mut since_ms = i64::MAX;
    // The newest live entry seen so far: (updated_at_ms, session_id).
    let mut newest: Option<(i64, String)> = None;
    let mut identities = Vec::new();
    let mut unresolved = 0u16;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue; // tmp files / strays are not cache entries
        }
        if entry.metadata().is_ok_and(|m| m.len() > MAX_CACHE_BYTES) {
            return corrupt; // size cap: not plausibly ours
        }
        let Ok(bytes) = std::fs::read(&path) else {
            return corrupt; // exists but unreadable
        };
        match parse_cache(&bytes) {
            Some(cache) => {
                let fresh = now_ms.saturating_sub(cache.updated_at_ms) < ACTIVE_WINDOW_MS
                    && cache.updated_at_ms <= now_ms.saturating_add(ACTIVE_WINDOW_MS);
                if cache.live && fresh {
                    live = live.saturating_add(1);
                    since_ms = since_ms.min(cache.updated_at_ms);
                    if let (Some(id), Some(agent)) = (cache.session_id, cache.agent) {
                        let candidate = (cache.updated_at_ms, id.clone());
                        if newest.as_ref().is_none_or(|best| candidate > *best) {
                            newest = Some(candidate);
                        }
                        identities.push(LiveSessionIdentity {
                            agent,
                            native_session_id: id,
                            updated_at_ms: cache.updated_at_ms,
                        });
                    } else {
                        unresolved = unresolved.saturating_add(1);
                    }
                }
            }
            None => return corrupt,
        }
    }
    if live == 0 {
        return SessionsDetail {
            read: SessionsRead::Ok(None),
            newest_live: None,
            identities: Vec::new(),
            unresolved: 0,
        };
    }
    identities.sort_by(|a, b| {
        (a.updated_at_ms, &a.agent, &a.native_session_id).cmp(&(
            b.updated_at_ms,
            &b.agent,
            &b.native_session_id,
        ))
    });
    SessionsDetail {
        read: SessionsRead::Ok(Some(LiveSessions {
            active_sessions: live,
            since_ms,
        })),
        newest_live: newest.map(|(_, id)| id),
        identities,
        unresolved,
    }
}

/// The facts the signal needs from one cache file.
struct CacheFacts {
    live: bool,
    updated_at_ms: i64,
    /// The cache's `session_id`, when present — the metrics anchor. Optional
    /// by leniency: only `state` + `updated_at_ms` are *required*.
    session_id: Option<String>,
    /// Adapter name, optional under the lenient cache contract.
    agent: Option<String>,
}

/// Parse one cache file's bytes, leniently and totally: unknown fields are
/// ignored, an unknown `state` reads as not-live, and `None` means the bytes
/// are not a cache file at all (corrupt). Never panics on any input.
fn parse_cache(bytes: &[u8]) -> Option<CacheFacts> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let obj = value.as_object()?;
    let state = obj.get("state")?.as_str()?;
    let updated_at_ms = obj.get("updated_at_ms")?.as_i64()?;
    let session_id = obj
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let agent = obj.get("agent").and_then(|v| v.as_str()).map(str::to_owned);
    Some(CacheFacts {
        live: LIVE_STATES.contains(&state),
        updated_at_ms,
        session_id,
        agent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn write_cache(dir: &Path, name: &str, body: &str) {
        let d = dir.join(SESSIONS_DIR);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(format!("{name}.json")), body).unwrap();
    }

    fn cache_json(state: &str, updated_at_ms: i64) -> String {
        format!(
            r#"{{"schema":1,"session_id":"s","state":"{state}","agent":"claude-code",
                 "current_turn_ordinal":3,"turn_open":true,"compaction_boundary":null,
                 "updated_at_ms":{updated_at_ms},"native_id_map":{{}}}}"#
        )
    }

    #[test]
    fn no_sessions_dir_reads_quiet() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(active_sessions(tmp.path(), 1_000), SessionsRead::Ok(None));
    }

    #[test]
    fn live_fresh_sessions_are_counted_distinctly() {
        let tmp = tempfile::tempdir().unwrap();
        write_cache(tmp.path(), "a", &cache_json("active", 900));
        write_cache(tmp.path(), "b", &cache_json("compacting", 800));
        write_cache(tmp.path(), "c", &cache_json("ended", 950)); // not live
        assert_eq!(
            active_sessions(tmp.path(), 1_000),
            SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 2,
                since_ms: 800, // the earliest live activity
            }))
        );
    }

    #[test]
    fn crash_stale_sessions_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let now = 100 * ACTIVE_WINDOW_MS;
        write_cache(
            tmp.path(),
            "stale",
            &cache_json("active", now - ACTIVE_WINDOW_MS - 1),
        );
        write_cache(tmp.path(), "fresh", &cache_json("active", now - 5));
        assert_eq!(
            active_sessions(tmp.path(), now),
            SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 1,
                since_ms: now - 5,
            }))
        );
    }

    #[test]
    fn sessions_detail_picks_the_newest_live_id() {
        // The metrics anchor is the live session touched most recently — the
        // one being typed in — not the earliest (`since_ms` keeps that role).
        let tmp = tempfile::tempdir().unwrap();
        write_cache(tmp.path(), "a", &cache_json_id("a-id", "active", 900));
        write_cache(tmp.path(), "b", &cache_json_id("b-id", "compacting", 800));
        write_cache(tmp.path(), "c", &cache_json_id("c-id", "ended", 950)); // not live
        let detail = sessions_detail(tmp.path(), 1_000);
        assert_eq!(
            detail.read,
            SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 2,
                since_ms: 800,
            }))
        );
        assert_eq!(detail.newest_live.as_deref(), Some("a-id"));
        assert_eq!(detail.identities.len(), 2);
        assert_eq!(detail.unresolved, 0);
    }

    #[test]
    fn an_id_less_cache_counts_toward_the_badge_but_never_anchors() {
        // Leniency: only `state` + `updated_at_ms` are required — a cache
        // without a `session_id` still counts as live, it just cannot be the
        // metrics anchor.
        let tmp = tempfile::tempdir().unwrap();
        write_cache(
            tmp.path(),
            "bare",
            r#"{"state":"active","updated_at_ms":990}"#,
        );
        let detail = sessions_detail(tmp.path(), 1_000);
        assert_eq!(
            detail.read,
            SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 1,
                since_ms: 990,
            }))
        );
        assert_eq!(detail.newest_live, None);
        assert!(detail.identities.is_empty());
        assert_eq!(detail.unresolved, 1);
    }

    #[test]
    fn newest_live_ties_break_deterministically() {
        let tmp = tempfile::tempdir().unwrap();
        write_cache(tmp.path(), "x", &cache_json_id("aa", "active", 900));
        write_cache(tmp.path(), "y", &cache_json_id("zz", "active", 900));
        let detail = sessions_detail(tmp.path(), 1_000);
        assert_eq!(
            detail.newest_live.as_deref(),
            Some("zz"),
            "equal freshness breaks to the greater id, deterministically"
        );
    }

    fn cache_json_id(id: &str, state: &str, updated_at_ms: i64) -> String {
        format!(
            r#"{{"schema":1,"session_id":"{id}","state":"{state}","agent":"claude-code",
                 "current_turn_ordinal":3,"turn_open":true,"compaction_boundary":null,
                 "updated_at_ms":{updated_at_ms},"native_id_map":{{}}}}"#
        )
    }

    #[test]
    fn unknown_fields_and_unknown_states_degrade_gracefully() {
        // Leniency (fog: schema drift): extra fields ignored; a new state the
        // writer might grow reads as not-live rather than corrupt.
        let tmp = tempfile::tempdir().unwrap();
        write_cache(
            tmp.path(),
            "future",
            r#"{"schema":2,"session_id":"s","state":"hibernating","updated_at_ms":990,
                "brand_new_field":{"nested":true}}"#,
        );
        assert_eq!(active_sessions(tmp.path(), 1_000), SessionsRead::Ok(None));
    }

    #[test]
    fn corrupt_bytes_are_a_distinct_error_never_quiet() {
        let tmp = tempfile::tempdir().unwrap();
        write_cache(tmp.path(), "bad", "not json at all");
        assert_eq!(active_sessions(tmp.path(), 1_000), SessionsRead::Corrupt);

        // Valid JSON that is not a cache shape is corrupt too.
        let tmp = tempfile::tempdir().unwrap();
        write_cache(tmp.path(), "shape", "[1, 2, 3]");
        assert_eq!(active_sessions(tmp.path(), 1_000), SessionsRead::Corrupt);
    }

    #[test]
    fn oversized_cache_file_is_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let huge = format!(
            r#"{{"state":"active","updated_at_ms":1,"pad":"{}"}}"#,
            "x".repeat(usize::try_from(MAX_CACHE_BYTES).unwrap())
        );
        write_cache(tmp.path(), "huge", &huge);
        assert_eq!(active_sessions(tmp.path(), 1_000), SessionsRead::Corrupt);
    }

    #[test]
    fn non_json_extensions_are_ignored() {
        // The writer's own temp files (`.<id>.tmp`) must not read as corrupt.
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join(SESSIONS_DIR);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(".s1.tmp"), b"\xff\xfe partial write").unwrap();
        write_cache(tmp.path(), "ok", &cache_json("active", 990));
        assert_eq!(
            active_sessions(tmp.path(), 1_000),
            SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 1,
                since_ms: 990,
            }))
        );
    }

    proptest! {
        /// The parser is total over arbitrary bytes: Some or None, never a panic.
        #[test]
        fn parser_is_total_over_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let _ = parse_cache(&bytes);
        }

        /// The full query is total over one arbitrary cache file: it yields a
        /// SessionsRead, never a panic, for any file contents.
        #[test]
        fn query_is_total_over_arbitrary_file(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let tmp = tempfile::tempdir().unwrap();
            let d = tmp.path().join(SESSIONS_DIR);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("x.json"), &bytes).unwrap();
            let _ = active_sessions(tmp.path(), 1_000);
        }
    }
}
