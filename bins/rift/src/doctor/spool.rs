//! The family's spool of rewritten results' originals: what is still there,
//! and which of it belongs to a session no member has live state for.
//!
//! A spooled original stays until it is sealed and its session's final
//! archive has landed — every checkpoint re-seals the whole session, so an
//! earlier removal would lose it from the next shard. Removal at that final
//! archive is the session lifecycle's to do; until it does, a spool whose
//! session left no state behind is exactly what nothing else will mention.
//! Finding only: this leg reads and never removes.

use std::path::Path;

use crate::hooks::{spooled_sessions, SpooledSession};

/// The spool leg. Silent when the family spooled nothing.
pub(super) fn doctor_original_spool(repo: &Path) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let spooled = spooled_sessions(&paths.common_dir);
    let leftover: Vec<&SpooledSession> = spooled
        .iter()
        .filter(|s| !has_live_state(&paths.common_dir, &s.session_id))
        .collect();
    let records: usize = spooled.iter().map(|s| s.records).sum();
    let mut human = String::new();
    if !spooled.is_empty() {
        human.push_str(&format!(
            "\n  original spool: {records} record(s) across {} session(s)",
            spooled.len()
        ));
    }
    for s in &leftover {
        human.push_str(&format!(
            "\n    leftover: session {} has {} spooled original(s) and no live session state \
             (reported, not removed)",
            s.session_id, s.records
        ));
    }
    let json = serde_json::json!({
        "sessions": spooled.len(),
        "records": records,
        "leftover": leftover
            .iter()
            .map(|s| serde_json::json!({ "session": s.session_id, "records": s.records }))
            .collect::<Vec<_>>(),
    });
    (human, json)
}

/// Whether any member of the family still holds a session cache for
/// `session_id`: the common dir's own, or a linked worktree's.
fn has_live_state(common_dir: &Path, session_id: &str) -> bool {
    let members = std::fs::read_dir(common_dir.join("worktrees"))
        .map(|entries| entries.flatten().map(|e| e.path()).collect::<Vec<_>>())
        .unwrap_or_default();
    std::iter::once(common_dir.to_path_buf())
        .chain(members)
        .any(|git_dir| {
            rift_shadow::hooks::read_cache(&git_dir, session_id).is_ok_and(|c| c.is_some())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A spool whose session still has a cache is counted; one whose session
    /// left none is named as leftover; nothing is removed.
    #[test]
    fn a_spool_with_no_live_session_is_named_and_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo)
            .status()
            .unwrap();
        let common = repo.join(".git");
        assert_eq!(doctor_original_spool(repo).0, "");
        for session in ["live-1", "gone-1"] {
            let dir = common.join("rift-family").join("spool").join(session);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("toolu_a.original"), b"x").unwrap();
        }
        let cache = rift_shadow::hooks::session_cache::cache_path(&common, "live-1").unwrap();
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        let live = serde_json::json!({
            "schema": rift_shadow::hooks::session_cache::SCHEMA, "session_id": "live-1",
            "state": "active", "agent": "claude-code", "current_turn_ordinal": 0,
            "updated_at_ms": 0,
        });
        std::fs::write(&cache, live.to_string()).unwrap();
        let (human, json) = doctor_original_spool(repo);
        assert!(human.contains("2 record(s) across 2 session(s)"), "{human}");
        assert!(human.contains("session gone-1"), "{human}");
        assert!(!human.contains("session live-1"), "{human}");
        assert_eq!(json["leftover"][0]["session"], "gone-1");
        assert!(common
            .join("rift-family/spool/gone-1/toolu_a.original")
            .exists());
    }
}
