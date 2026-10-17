//! The subagent claim marker: what an agent's `SubagentStop` said about where
//! the subagent's own transcript is, kept until the archive reads it.
//!
//! Run state, not a record (Q37.3): one small file per subagent at
//! `<sessions>/<session>.subagents/<agent_id>.json` in the member's run state, holding an
//! address and a type and no fact the sealed shard will not hold. One file per
//! subagent rather than a member of the session cache because subagents stop
//! concurrently, and the cache is rewritten whole by every hook: two stops
//! racing on one file would drop a claim.
//!
//! The marker is a hint, not the source of truth. The archive also lists the
//! session's `subagents/` directory, so a stop hook that never fired loses
//! nothing; the marker adds the address the agent stated, which survives a
//! client that moves the file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The marker's schema version.
const SCHEMA_VERSION: u32 = 1;
/// The suffix of a session's marker directory under the sessions directory.
/// Not `.json`, so no reader of session caches enumerates it.
const CLAIMS_SUFFIX: &str = "subagents";
/// The longest agent id or type a marker keeps. Claude Code's ids are 17
/// characters; anything far past that is not an id.
const MAX_ID_BYTES: usize = 128;
/// The longest transcript address a marker keeps.
const MAX_PATH_BYTES: usize = 4096;

/// One subagent's claim, as its stop hook stated it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SubagentClaim {
    /// The marker's schema version.
    pub(super) schema_version: u32,
    /// The client's id for the subagent.
    pub(super) agent_id: String,
    /// The dispatched subagent type, when the hook named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) agent_type: Option<String>,
    /// Where the client keeps the subagent's transcript.
    pub(crate) transcript_path: String,
}

/// Record the claim a hook payload carries, if it carries one: an
/// `agent_id` and an `agent_transcript_path`. Best-effort, like every write on
/// the hook path; a claim not recorded is found again by the archive's listing.
pub(super) fn record_claim(git_dir: &Path, raw: &rift_shadow::hooks::RawHook) {
    let Some(claim) = claim_of(&raw.payload) else {
        return;
    };
    let Some(dir) = claims_dir(git_dir, &raw.session) else {
        return;
    };
    if let Err(e) = write_claim(&dir, &claim) {
        tracing::warn!(error = %e, "subagent claim not recorded");
    }
}

/// Every claim recorded for `session`, keyed by agent id. Unreadable or
/// malformed markers are skipped: the listing still finds their files.
pub(crate) fn read_claims(
    git_dir: &Path,
    session: &str,
) -> std::collections::BTreeMap<String, SubagentClaim> {
    let mut out = std::collections::BTreeMap::new();
    let Some(dir) = claims_dir(git_dir, session) else {
        return out;
    };
    for path in rift_shadow::hooks::run_state::list_entries(&dir) {
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let parsed = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<SubagentClaim>(&b).ok());
        let usable = |c: &SubagentClaim| {
            is_safe_id(&c.agent_id) && is_transcript_address(&c.transcript_path)
        };
        if let Some(claim) = parsed.filter(usable) {
            out.insert(claim.agent_id.clone(), claim);
        }
    }
    out
}

/// The claim a payload carries: both members present, the id a safe file
/// name and the path bounded.
fn claim_of(payload: &serde_json::Value) -> Option<SubagentClaim> {
    let text = |key: &str| payload.get(key).and_then(serde_json::Value::as_str);
    let agent_id = text("agent_id").filter(|id| is_safe_id(id))?;
    let transcript_path = text("agent_transcript_path").filter(|p| is_transcript_address(p))?;
    let agent_type = text("agent_type")
        .filter(|t| !t.is_empty() && t.len() <= MAX_ID_BYTES)
        .map(str::to_owned);
    Some(SubagentClaim {
        schema_version: SCHEMA_VERSION,
        agent_id: agent_id.to_owned(),
        agent_type,
        transcript_path: transcript_path.to_owned(),
    })
}

/// Whether `id` may name a file and a shard subtree: ASCII letters, digits,
/// `_` and `-`, bounded. Shared with the archive, whose subtree names the same
/// rule guards.
pub(crate) fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether `path` may be a subagent transcript address: bounded, NUL-free and
/// naming a `.jsonl` file, the only kind the client writes there. Anything else
/// is not read, whatever the hook said.
fn is_transcript_address(path: &str) -> bool {
    path.len() <= MAX_PATH_BYTES
        && !path.contains('\0')
        && Path::new(path).extension().is_some_and(|e| e == "jsonl")
        && Path::new(path).file_stem().is_some_and(|s| !s.is_empty())
}

/// The session's marker directory, `None` for a session id that cannot name a
/// file.
fn claims_dir(git_dir: &Path, session: &str) -> Option<PathBuf> {
    if !rift_shadow::hooks::is_safe_session_id(session) {
        return None;
    }
    let sessions = crate::family::run_state::sessions_dir(git_dir);
    Some(sessions.join(format!("{session}.{CLAIMS_SUFFIX}")))
}

/// Write one marker whole: a temporary sibling renamed over the target, so a
/// reader never sees half a claim.
fn write_claim(dir: &Path, claim: &SubagentClaim) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let target = dir.join(format!("{}.json", claim.agent_id));
    let tmp = dir.join(format!(
        "{}.json.tmp-{}",
        claim.agent_id,
        std::process::id()
    ));
    let bytes = serde_json::to_vec(claim).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(session: &str, payload: serde_json::Value) -> rift_shadow::hooks::RawHook {
        let body =
            serde_json::json!({ "schema": 1, "session": session, "turn": 1, "payload": payload });
        rift_shadow::hooks::build_raw_hook(
            "claude-code",
            "SubagentStop",
            body.to_string().as_bytes(),
            1,
        )
        .unwrap()
    }

    #[test]
    fn a_subagent_stop_leaves_one_claim_the_archive_reads_back() {
        let git_dir = tempfile::tempdir().unwrap();
        let payload = serde_json::json!({
            "agent_id": "a1", "agent_type": "Explore",
            "agent_transcript_path": "/x/s/subagents/agent-a1.jsonl",
        });
        record_claim(git_dir.path(), &raw("s-1", payload));
        let claims = read_claims(git_dir.path(), "s-1");
        assert_eq!(claims.len(), 1);
        let claim = &claims["a1"];
        assert_eq!(claim.agent_type.as_deref(), Some("Explore"));
        assert_eq!(claim.transcript_path, "/x/s/subagents/agent-a1.jsonl");
        assert!(
            read_claims(git_dir.path(), "s-2").is_empty(),
            "keyed by session"
        );
    }

    #[test]
    fn a_payload_without_both_members_or_with_an_unsafe_id_claims_nothing() {
        let git_dir = tempfile::tempdir().unwrap();
        for payload in [
            serde_json::json!({ "agent_id": "a1" }),
            serde_json::json!({ "agent_transcript_path": "/x.jsonl" }),
            serde_json::json!({ "agent_id": "../a1", "agent_transcript_path": "/x.jsonl" }),
            serde_json::json!({ "agent_id": "a1", "agent_transcript_path": "" }),
        ] {
            record_claim(git_dir.path(), &raw("s-1", payload));
        }
        assert!(read_claims(git_dir.path(), "s-1").is_empty());
        assert!(claims_dir(git_dir.path(), "../s").is_none());
    }

    #[test]
    fn a_claim_whose_address_is_not_a_jsonl_file_is_not_recorded() {
        let git_dir = tempfile::tempdir().unwrap();
        let payload =
            serde_json::json!({ "agent_id": "a1", "agent_transcript_path": "/x/notes.txt" });
        record_claim(git_dir.path(), &raw("s-1", payload));
        assert!(read_claims(git_dir.path(), "s-1").is_empty());
    }

    #[test]
    fn a_malformed_marker_is_skipped_rather_than_failing_the_read() {
        let git_dir = tempfile::tempdir().unwrap();
        let dir = claims_dir(git_dir.path(), "s-1").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a1.json"), b"{not json").unwrap();
        std::fs::write(dir.join("a2.txt"), b"{}").unwrap();
        assert!(read_claims(git_dir.path(), "s-1").is_empty());
    }
}
