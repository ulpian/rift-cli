//! **First-writer-wins session ownership**.
//!
//! One conversation can fire hooks as two agents. The motivating case is
//! Cursor: the IDE runs `cursor-agent` *and* forwards the same session to
//! Claude Code's hook system, so Rift receives `rift hooks cursor …` and
//! `rift hooks claude-code …` carrying one session id. Nothing tied them
//! together, and because a session row is derived from `(agent, session_id)`
//! the one conversation split into two sessions — turns divided between them,
//! spend counted under both, neither history complete.
//!
//! The fix is an exclusive marker file, `<git_dir>/rift-sessions/<id>.agent`,
//! published whole and never clobbered (see `publish_marker`): whichever
//! agent's hook lands first writes its label and every later hook for that
//! session reads it back. The winner does not
//! have to be the agent that fired the current hook — that is the point. The
//! *firing* agent still decides which adapter maps the payload (only it knows
//! its own schema, and the idempotency key stays keyed to it, so redelivery is
//! still a no-op); the *owner* decides which session the events belong to.
//!
//! **Ownership is not announcement.** The legacy backend shipped this hint and then had to
//! add a separate `.banner` marker, because the agent that won ownership was
//! not always the one able to print the session banner — a claim on one thing
//! silently suppressed the other. So this module answers exactly one question,
//! "whose session is this", and nothing here may be reused to decide whether a
//! user-visible message has already been shown. Rift's equivalent of that
//! second question is session-pointer injection dedup, which keeps its own
//! marker under `.rift/state/injection/`.
//!
//! Degrades like every capture path: an unwritable git dir, an unsafe session
//! id, or a corrupt marker simply yields "no owner", and the caller falls back
//! to the firing agent — never an error that fails the agent.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use rift_core::AgentId;

use crate::session_cache::sessions_dir;
use crate::session_id::is_safe_session_id;

/// Marker suffix for the ownership file.
const CLAIM_SUFFIX: &str = ".agent";
/// Longest marker we will read back. The framing plus the longest plausible
/// agent slug is well under this; anything larger is not a marker, and the
/// bound keeps a hostile file from being buffered.
const MAX_CLAIM_BYTES: usize = 128;

/// Schema version of the ownership marker's contents.
///
/// The marker is a serialized artifact that outlives the process that wrote
/// it and is read by every later hook of the session, so it carries its own
/// version: without one, a future change to the contents is indistinguishable
/// from corruption, and the only safe reaction to corruption — "unclaimed" —
/// is exactly the wrong reaction to a newer writer.
///
/// A marker whose version this build does not know reads as **unclaimed**,
/// which degrades the same way a corrupt one does: the firing agent owns the
/// session for want of a better answer, and no hook fails.
pub const CLAIM_SCHEMA_VERSION: u32 = 1;

/// The marker's contents. One line of JSON, matching the session cache beside
/// it; the file is published whole (see [`publish_marker`]) so a reader never
/// sees half of it.
#[derive(serde::Serialize, serde::Deserialize)]
struct Marker {
    schema_version: u32,
    agent: String,
}

/// The outcome of a claim attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claim {
    /// This agent created the marker: the session is now attributed to it.
    Won,
    /// Another agent claimed the session first; its label is the owner.
    Held(String),
    /// The session could not be claimed at all (unsafe id, unwritable git
    /// dir). The caller attributes to the firing agent, as before.
    Unavailable,
}

impl Claim {
    /// The agent label the session belongs to, given the agent that fired the
    /// current hook.
    #[must_use]
    pub fn owner<'a>(&'a self, firing_agent: &'a str) -> &'a str {
        match self {
            Self::Held(owner) => owner.as_str(),
            Self::Won | Self::Unavailable => firing_agent,
        }
    }
}

/// The ownership marker's path, or `None` when the session id may not name a
/// file (see [`is_safe_session_id`]).
#[must_use]
pub fn claim_path(git_dir: &Path, session_id: &str) -> Option<PathBuf> {
    is_safe_session_id(session_id)
        .then(|| sessions_dir(git_dir).join(format!("{session_id}{CLAIM_SUFFIX}")))
}

/// Claim `session_id` for `agent`, first writer wins.
///
/// Never fails: every filesystem error degrades to [`Claim::Unavailable`], and
/// a marker that does not hold a well-formed agent token is treated as absent
/// (this agent then claims it by overwriting nothing — it simply owns the
/// session for want of a better answer).
#[must_use]
pub fn claim_session_agent(git_dir: &Path, session_id: &str, agent: &str) -> Claim {
    let Some(path) = claim_path(git_dir, session_id) else {
        return Claim::Unavailable;
    };
    if !AgentId::new(agent).is_well_formed() {
        return Claim::Unavailable;
    }
    // The common case is a session already claimed, so look before leaping.
    if let Some(owner) = read_owner(&path) {
        return if owner == agent {
            Claim::Won
        } else {
            Claim::Held(owner)
        };
    }
    let dir = sessions_dir(git_dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return Claim::Unavailable;
    }
    match publish_marker(&dir, &path, agent) {
        Some(true) => Claim::Won,
        // Another hook got there first — and because the marker is published
        // whole, the loser reads a *complete* label, never a half-written one.
        Some(false) => match read_owner(&path) {
            Some(owner) if owner != agent => Claim::Held(owner),
            Some(_) => Claim::Won,
            None => Claim::Unavailable,
        },
        None => Claim::Unavailable,
    }
}

/// Publish the marker atomically: `Some(true)` if this call created it,
/// `Some(false)` if it already existed, `None` if the filesystem refused.
///
/// Creating a file and *then* writing the label leaves a window in which the
/// marker exists but is empty — a concurrent hook reads nothing, concludes the
/// session is unclaimed, and attributes to itself, which is precisely the split
/// this module exists to prevent (a five-way race reproduced it on the first
/// run). Writing the label into a temp file and hard-linking it into place
/// closes the window: `link(2)` publishes a name that already has its content
/// and fails with `EEXIST` rather than clobbering, so first-writer-wins holds
/// with no window at all.
fn publish_marker(dir: &Path, path: &Path, agent: &str) -> Option<bool> {
    // Unique per process *and* per call: two threads of one process claim
    // concurrently in tests, and a pid alone would make them collide on the
    // temp file rather than on the marker.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = dir.join(format!(
        ".{}.{}.{}.claim.tmp",
        path.file_stem()?.to_str()?,
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let contents = marker_bytes(agent)?;
    let _ = std::fs::remove_file(&tmp); // a crashed predecessor's leftover
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .ok()?;
    let written = file.write_all(&contents).and_then(|()| file.sync_all());
    drop(file);
    let published = match (written, std::fs::hard_link(&tmp, path)) {
        (Ok(()), Ok(())) => Some(true),
        (Ok(()), Err(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => Some(false),
        // A filesystem without hard links (some Windows volumes) still gets
        // first-writer-wins, just with the create-then-write window back.
        (Ok(()), Err(_)) => match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut marker) => marker.write_all(&contents).ok().map(|()| true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Some(false),
            Err(_) => None,
        },
        (Err(_), _) => None,
    };
    let _ = std::fs::remove_file(&tmp);
    published
}

/// The agent that owns `session_id`, if the session has been claimed.
#[must_use]
pub fn session_owner(git_dir: &Path, session_id: &str) -> Option<String> {
    read_owner(&claim_path(git_dir, session_id)?)
}

/// The bytes a marker holds for `agent`, at the current schema version.
fn marker_bytes(agent: &str) -> Option<Vec<u8>> {
    serde_json::to_vec(&Marker {
        schema_version: CLAIM_SCHEMA_VERSION,
        agent: agent.to_owned(),
    })
    .ok()
}

/// Read a marker back, accepting only a well-formed agent token — a truncated,
/// empty, oversized, future-versioned or otherwise corrupt marker reads as
/// "unclaimed" rather than as an agent nobody can dispatch to.
///
/// A **bare agent token** is also accepted. That is the marker format that
/// predates versioning, and a session that was live across the upgrade still
/// has one on
/// disk; refusing it would silently re-attribute a running conversation to
/// whichever agent's hook fired next, which is the exact split the marker
/// exists to prevent. Nothing writes that form any more, so it ages out with
/// the sessions that hold it.
fn read_owner(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_CLAIM_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() > MAX_CLAIM_BYTES {
        return None;
    }
    let text = String::from_utf8(buf).ok()?;
    let text = text.trim();
    let label = if text.starts_with('{') {
        let marker: Marker = serde_json::from_str(text).ok()?;
        if marker.schema_version != CLAIM_SCHEMA_VERSION {
            return None;
        }
        marker.agent
    } else {
        text.to_owned()
    };
    AgentId::new(label.clone())
        .is_well_formed()
        .then_some(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "f5927747-7be6-4b5e-b379-579c7193520a";

    #[test]
    fn the_first_agent_to_fire_owns_the_session() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        assert_eq!(
            claim_session_agent(git_dir, SESSION, "cursor"),
            Claim::Won,
            "an unclaimed session goes to the first writer"
        );
        // The Cursor double-config case: the same conversation also reaches
        // Claude Code's hook system. It must not open a second session.
        let second = claim_session_agent(git_dir, SESSION, "claude-code");
        assert_eq!(second, Claim::Held("cursor".to_owned()));
        assert_eq!(second.owner("claude-code"), "cursor");
        // The winner keeps winning, and asking is stable.
        assert_eq!(claim_session_agent(git_dir, SESSION, "cursor"), Claim::Won);
        assert_eq!(session_owner(git_dir, SESSION).unwrap(), "cursor");
    }

    #[test]
    fn an_unclaimed_session_reports_no_owner() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(session_owner(tmp.path(), SESSION).is_none());
        assert_eq!(Claim::Unavailable.owner("codex"), "codex");
        assert_eq!(Claim::Won.owner("codex"), "codex");
    }

    #[test]
    fn a_session_id_that_could_escape_the_directory_is_never_claimed() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(claim_path(tmp.path(), "../evil").is_none());
        assert_eq!(
            claim_session_agent(tmp.path(), "../evil", "claude-code"),
            Claim::Unavailable
        );
        assert!(session_owner(tmp.path(), "../evil").is_none());
        assert!(!tmp.path().parent().unwrap().join("evil.agent").exists());
    }

    #[test]
    fn a_corrupt_marker_reads_as_unclaimed_rather_than_as_an_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        std::fs::create_dir_all(sessions_dir(git_dir)).unwrap();
        let path = claim_path(git_dir, SESSION).unwrap();
        for corrupt in [
            String::new(),
            "  ".to_owned(),
            "not an agent token".to_owned(),
            "x".repeat(MAX_CLAIM_BYTES + 1),
        ] {
            std::fs::write(&path, corrupt.as_bytes()).unwrap();
            assert!(
                session_owner(git_dir, SESSION).is_none(),
                "corrupt marker {corrupt:?} must not name an owner"
            );
        }
        // …and a hook arriving on top of one is attributed to itself.
        assert_eq!(
            claim_session_agent(git_dir, SESSION, "claude-code").owner("claude-code"),
            "claude-code"
        );
    }

    /// The marker is a serialized artifact that outlives its writer,
    /// so it states its own version: without one, tomorrow's format change is
    /// indistinguishable from corruption, and the only safe reaction to
    /// corruption is the wrong reaction to a newer writer.
    #[test]
    fn the_marker_states_its_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        assert_eq!(claim_session_agent(git_dir, SESSION, "cursor"), Claim::Won);
        let raw = std::fs::read_to_string(claim_path(git_dir, SESSION).unwrap()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["schema_version"], CLAIM_SCHEMA_VERSION);
        assert_eq!(parsed["agent"], "cursor");
        assert_eq!(session_owner(git_dir, SESSION).unwrap(), "cursor");
    }

    /// A marker from a build that does not exist yet reads as unclaimed, which
    /// degrades exactly like a corrupt one: the firing agent owns the session
    /// for want of a better answer, and no hook fails.
    #[test]
    fn a_marker_from_a_future_version_reads_as_unclaimed() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        std::fs::create_dir_all(sessions_dir(git_dir)).unwrap();
        let path = claim_path(git_dir, SESSION).unwrap();
        std::fs::write(
            &path,
            br#"{"schema_version":99,"agent":"cursor","future":"field"}"#,
        )
        .unwrap();
        assert!(session_owner(git_dir, SESSION).is_none());
        assert_eq!(
            claim_session_agent(git_dir, SESSION, "codex").owner("codex"),
            "codex"
        );
    }

    /// A session that was live across the upgrade still has a bare-token
    /// marker on disk. Refusing it would silently re-attribute a running
    /// conversation to whichever agent's hook fired next — the exact split the
    /// marker exists to prevent — so the legacy form is still read.
    #[test]
    fn a_pre_versioning_marker_still_names_its_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        std::fs::create_dir_all(sessions_dir(git_dir)).unwrap();
        std::fs::write(claim_path(git_dir, SESSION).unwrap(), b"cursor").unwrap();
        assert_eq!(session_owner(git_dir, SESSION).unwrap(), "cursor");
        assert_eq!(
            claim_session_agent(git_dir, SESSION, "claude-code"),
            Claim::Held("cursor".to_owned()),
            "an in-flight session keeps its owner across the upgrade"
        );
    }

    #[test]
    fn only_one_of_many_concurrent_claimants_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(sessions_dir(&git_dir)).unwrap();
        let agents = ["cursor", "claude-code", "codex", "gemini", "copilot"];
        let claims: Vec<(&str, Claim)> = std::thread::scope(|scope| {
            let handles: Vec<_> = agents
                .iter()
                .map(|agent| {
                    let dir = git_dir.clone();
                    scope.spawn(move || (*agent, claim_session_agent(&dir, SESSION, agent)))
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        assert_eq!(claims.len(), agents.len());
        assert_eq!(
            claims.iter().filter(|(_, c)| *c == Claim::Won).count(),
            1,
            "O_EXCL admits exactly one creator: {claims:?}"
        );
        let owner = session_owner(&git_dir, SESSION).unwrap();
        for (agent, claim) in &claims {
            assert_eq!(
                claim.owner(agent),
                owner,
                "every hook agrees the session is {owner}'s"
            );
        }
    }
}
