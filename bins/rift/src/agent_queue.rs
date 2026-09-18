//! The agent-hook job journal (settled 2026-08-30): the
//! durable record of the work an agent hook owes but must not do inline.
//!
//! The agent-hook budget is p50 < 100 ms / p95 < 500 ms
//! *including ingest*. Two pieces of hook-path work measured past it: the
//! `SessionEnd` condense (redact + shard + chain append, p50 647.80 ms with a
//! transcript) and the BPE token count of a read (the tokenizer's own
//! initialisation is 234 ms on this machine, before a single byte is counted).
//! Neither fits and neither can be made to fit, so both are **journaled here
//! and performed by the detached one-shot worker** — the same spawn, the same
//! lock and the same said-once marker channel as the post-commit capture queue
//! (`capture_queue`). There is no daemon, by standing decision: each hook that
//! enqueues spawns one worker, which drains what it finds and exits.
//!
//! **A job carries identifiers only.** A condense job names the session; a
//! token job names the read's own `tool_call` row and the content identity
//! (`content_sha256`, `blob_oid`) of the bytes it must count, so the worker
//! re-reads *those* bytes or none — a modified working file is never counted
//! under a stale identity. No capture bytes of either kind are journaled, for
//! the reason `capture_queue` gives.
//!
//! **The file name is the job's identity**, so a redelivered hook overwrites
//! its own job rather than queueing a second one, and a worker killed after
//! the work but before the `remove` finds the done-marker and skips.

use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};

/// The journal directory under `<git_dir>` (sibling of `rift-capture-queue`).
const QUEUE_DIR: &str = "rift-agent-queue";

/// One journaled job: what the worker needs to do it later.
///
/// Unknown fields are ignored on read, for the reason `capture_queue`'s entry
/// gives: a newer writer's job must still be drainable by an older worker.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct AgentJob {
    /// Contract schema version.
    pub schema_version: u32,
    /// The working tree the job's facts are keyed by.
    pub work_tree: PathBuf,
    /// Transaction time; the condense worker's archive clock, so a deferred
    /// run neither backdates nor advances the decision.
    pub queued_at_ms: i64,
    /// How many times a worker has already failed on this job.
    #[serde(default)]
    pub attempts: u32,
    /// What is owed.
    pub kind: JobKind,
}

/// The work a job names.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum JobKind {
    /// The session-end condense the state machine requested
    /// (`Effect::RequestCondense`): settle the last turn, archive, close.
    Condense {
        /// The agent-supplied session id.
        session_id: String,
        /// The session's *owner* agent, not the hook's.
        agent: String,
        /// The repository id the settle records cost under.
        repo_id: String,
    },
    /// The BPE token count of one read, recorded as a late `file_read_tokens`
    /// row at the read's own `tool_call` id (insert-or-ignore: idempotent).
    ReadTokens {
        /// The read's content-addressed `tool_call` row id (64 hex).
        tool_call_id: String,
        /// Repo-relative path of the bytes read.
        rel_path: String,
        /// SHA-256 (hex) of the bytes at read time — what the worker verifies.
        content_sha256: String,
        /// Git blob oid of the same bytes — the fallback source when the
        /// working file has moved on.
        blob_oid: String,
        /// The 1-based inclusive line range the call named, when it named one
        /// — a shell read spells its own range and is priced over that slice
        /// of the identified bytes. Absent for a structured read tool, which
        /// is priced over the whole file exactly as it always was, and absent
        /// on a job an older writer left behind.
        #[serde(default)]
        lines: Option<(u32, u32)>,
        /// The event schema version the read fact was recorded under.
        schema_version: i64,
    },
    /// One accounting row a hook decided but must not write: the hook opens no
    /// store and initialises no tokenizer, so the decision is journaled here
    /// and the worker counts the tokens and appends the row to **both**
    /// serving relations.
    ServeAudit(ServeAudit),
    /// Rebuild the file-search index for this work tree. The arm is a no-op
    /// until the search engine exists; the kind is declared now so the hook
    /// that wants the rebuild and the worker that performs it are written
    /// against one contract rather than two.
    SearchIndex,
    /// Convert one document to its Markdown sidecar out of band, after a
    /// conversion missed its deadline on the hook path or a boundary hook
    /// asked for it in advance.
    DocConvert {
        /// Repo-relative path of the document.
        rel_path: String,
        /// SHA-256 (hex) of its bytes — the sidecar's key, and the identity
        /// the worker verifies before converting anything.
        content_sha256: String,
    },
    /// Keep the read-time bytes of one revision, so a delta against it
    /// survives the edit that follows.
    ///
    /// The identifiers are the read's own — the same three a
    /// [`JobKind::ReadTokens`] job carries, and resolved by the same ladder:
    /// the working file while its hash still matches, else the git blob, else
    /// nothing at all. Nothing of the bytes is journaled here, for the reason
    /// every other kind gives, and nothing durable is written without passing
    /// the redaction gate, which is the worker's to do because it costs more
    /// than a hook has.
    BasisSnapshot {
        /// Repo-relative path of the bytes read, in the `/` spelling the
        /// window journal keys by.
        rel_path: String,
        /// SHA-256 (hex) of the bytes at read time — the snapshot's key, and
        /// what the worker verifies before it keeps anything.
        content_sha256: String,
        /// Git blob oid of the same bytes — the fallback source when the
        /// working file has moved on.
        blob_oid: String,
    },
    /// Measure what one **search** put in front of the model, by running it
    /// again read-only and counting the output with the real tokenizer.
    ///
    /// The identifiers are the search itself
    /// ([`crate::search_measure::MeasuredSearch`] is a description of a call,
    /// never a result) and the tree state it ran against, so a measurement
    /// taken after the tree moved is discarded rather than attributed to a
    /// call that saw something else. Nothing of what the search found is
    /// journaled, exactly as no capture bytes are.
    ToolResultTokens {
        /// The row's content-addressed id
        /// (`rift_shadow::ids::tool_result_row_id`, 64 hex).
        tool_result_id: String,
        /// The `session` row the result belongs to — the only per-family axis
        /// the projection has.
        session_id: String,
        /// The family's own spelling of the tool, for the row's `tool_name`.
        tool_name: String,
        /// Which channel it arrived on
        /// (`rift_store::schema::TOOL_RESULT_CHANNELS`).
        channel: String,
        /// What to run, and how.
        search: crate::search_measure::MeasuredSearch,
        /// `HEAD` oid and newest directory mtime as the member's path index
        /// last recorded them — the tree the search ran against. `None` where
        /// the index was cold, in which case the worker brackets its own run
        /// and has nothing earlier to compare with.
        #[serde(default)]
        tree_key: Option<TreeKey>,
    },
    /// Convert the documents this work tree tracks, ahead of anyone asking.
    ///
    /// Requested where a session begins and after a commit, because those are
    /// the two moments the set of documents can have changed and nobody is
    /// waiting on an answer. The walk itself is bounded and lives in the
    /// worker; this is only the request.
    DocSweep,
}

/// The decision one journaled accounting row is about.
///
/// It carries **identifiers and Rift's own text**, never file content: the
/// bytes whose tokens are the avoided figure are named by path and content
/// hash so the worker re-reads exactly those bytes or counts none, exactly as
/// [`JobKind::ReadTokens`] does.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ServeAudit {
    /// This decision's own identity: the `serve_call_event` call id, the key
    /// the audit row is addressed by, and this job's file name. Fresh per
    /// decision, so two identical decisions are two rows and a redelivered
    /// job is one.
    pub call_id: String,
    /// Which decision it was. Named `decision` and not `kind` because the
    /// job envelope's own tag is `kind`: two `kind` members in one object is
    /// a document that does not round-trip.
    pub decision: ServeAuditKind,
    /// The family whose hook decided, which is the only per-family axis the
    /// existing relations have (it goes to `serve_call_event.tool_name`
    /// beside the step that decided).
    pub agent: String,
    /// The bytes whose tokens the decision avoided, when it named some.
    #[serde(default)]
    pub basis: Option<AuditBasis>,
    /// Rift's own composed text put in front of the model — a reason frame, a
    /// recovery map, an answer. Counted by the worker with the real
    /// tokenizer; empty when the decision returned nothing.
    #[serde(default)]
    pub frame: String,
    /// A neutral tag for the row's reason column, when the decision has one
    /// of its own. The worker overrides it when it cannot measure.
    #[serde(default)]
    pub tag: Option<String>,
    /// A count the decision produced instead of an avoided figure (a capped
    /// search's reported total).
    #[serde(default)]
    pub total: Option<u32>,
    /// An avoided figure the decision **declares** rather than measures, for
    /// the classes whose native cost cannot be counted twice: a document read
    /// that never happened, and a decline whose slice another row of the same
    /// window already booked, which declares a zero. Always paired with the
    /// tag that says so.
    #[serde(default)]
    pub declared_avoided: Option<u32>,
    /// Which native call an answered name search stood in for, which is the
    /// ceiling its paths are priced at. `None` on every other decision, and on
    /// a job an older binary left behind — where the worker takes the smaller
    /// of the two ceilings rather than guessing the larger one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<rift_recall_serve::ListingShape>,
}

/// Which accounting row a [`ServeAudit`] becomes. One variant per hook-side
/// serving verb; the two pull-side verbs are written by the CLI, which has a
/// store already and journals nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ServeAuditKind {
    /// A read was declined once, with a reason the model sees.
    ReadIntercepted,
    /// The same decision, recorded and not acted on.
    ReadRepeatObserved,
    /// A read Rift had already made unnecessary was never made: the window
    /// acted on a file Rift told it that it holds, without reading it again.
    ReadPrevented,
    /// The same decision, recorded and not acted on.
    ReadPreventedObserved,
    /// A compacted context was handed the map of what it stopped holding.
    RecoveryInjected,
    /// A listing-shaped search was answered instead of run.
    SearchAnswered,
    /// A content search was given the result cap it lacked.
    GrepCapped,
    /// A document read was pointed at its Markdown sidecar.
    DocRewritten,
    /// A document read ran untouched; the conversion is owed.
    DocDeferred,
}

/// The bytes an avoided figure is the token count of.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct AuditBasis {
    /// Repo-relative path, in the spelling the window journal keys by.
    pub rel_path: String,
    /// SHA-256 (hex) of the bytes at decision time.
    pub content_sha256: String,
    /// Git blob oid of the same bytes — the fallback source when the working
    /// file has moved on.
    pub blob_oid: String,
    /// The run of lines the decision was about, when it was about one.
    #[serde(default)]
    pub lines: Option<AuditLines>,
}

/// The tree a search ran against, as the path index already keys one.
///
/// `(HEAD oid, newest directory mtime)` — the same pair
/// [`rift_recall_search::IndexKey`] is, spelled here because the journal has
/// to round-trip it and that type is not this crate's to serialize. A
/// measurement is the call's measurement only while both halves still hold.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct TreeKey {
    /// `HEAD`'s object id, or the empty string where there is not one.
    pub head_oid: String,
    /// The newest modification time of any directory the walk entered, ms.
    pub newest_dir_mtime_ms: u64,
}

impl TreeKey {
    /// The journal's spelling of one the search crate computed.
    ///
    /// Only a build with the search seam has one to convert: the key is the
    /// path index's, and a build without the index keeps no index and
    /// therefore no key.
    #[cfg(feature = "search")]
    pub(crate) fn of(key: &rift_recall_search::IndexKey) -> Self {
        Self {
            head_oid: key.head_oid.clone(),
            newest_dir_mtime_ms: key.newest_dir_mtime_ms,
        }
    }
}

/// A run of lines, one-based and inclusive at both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct AuditLines {
    /// The first line.
    pub lo: u32,
    /// The last line.
    pub hi: u32,
}

/// The journal directory for `git_dir`.
pub(crate) fn queue_dir(git_dir: &Path) -> PathBuf {
    git_dir.join(QUEUE_DIR)
}

/// Whether `s` is lowercase hex of exactly `len` characters — the only shape
/// that becomes a path component here.
fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The hex SHA-256 of `session_id` — a session id is agent-supplied text, so
/// it never becomes a path component itself.
fn session_key(session_id: &str) -> String {
    rift_shadow::sha256_hex(&[session_id.as_bytes()])
}

impl JobKind {
    /// The file stem this job is journaled under: its identity, so the same
    /// work enqueued twice is one file.
    fn stem(&self) -> Option<String> {
        match self {
            Self::Condense { session_id, .. } => {
                Some(format!("condense-{}", session_key(session_id)))
            }
            Self::ReadTokens { tool_call_id, .. } => {
                is_lower_hex(tool_call_id, 64).then(|| format!("tokens-{tool_call_id}"))
            }
            // The decision's own id, which this process minted, so the file
            // name is one decision and a redelivered job overwrites itself.
            Self::ServeAudit(audit) => {
                is_lower_hex(&audit.call_id, 32).then(|| format!("audit-{}", audit.call_id))
            }
            // One rebuild is owed at a time: a second request while one is
            // pending is the same request.
            Self::SearchIndex => Some("search-index".to_owned()),
            // Keyed by what is to be converted, not by who asked: two hooks
            // that meet the same document owe one conversion.
            Self::DocConvert { content_sha256, .. } => {
                is_lower_hex(content_sha256, 64).then(|| format!("doc-{content_sha256}"))
            }
            // Keyed by the revision, not by the read: ten reads of one
            // revision owe one snapshot, and the second hook to meet it
            // overwrites the first hook's job rather than queueing a second.
            Self::BasisSnapshot { content_sha256, .. } => {
                is_lower_hex(content_sha256, 64).then(|| format!("basis-{content_sha256}"))
            }
            // The result's own content-addressed id, which this process
            // computed from the fact: two identical searches in one turn are
            // one call and one row.
            Self::ToolResultTokens { tool_result_id, .. } => {
                is_lower_hex(tool_result_id, 64).then(|| format!("result-{tool_result_id}"))
            }
            // One pass is owed at a time, as with the index: three hooks that
            // fire while the worker is busy have asked for the same walk.
            Self::DocSweep => Some("doc-sweep".to_owned()),
        }
    }

    /// Token jobs drain before condense jobs, so the shard a condense writes
    /// carries every count the session's reads were owed.
    fn rank(&self) -> u8 {
        match self {
            Self::ReadTokens { .. }
            | Self::ServeAudit(_)
            | Self::SearchIndex
            | Self::DocConvert { .. }
            | Self::BasisSnapshot { .. }
            | Self::ToolResultTokens { .. }
            | Self::DocSweep => 0,
            Self::Condense { .. } => 1,
        }
    }
}

/// Journal one job (atomically: temp file + rename, owner-only). Enqueueing
/// the same identity again overwrites — one file per job identity.
///
/// # Errors
/// [`RiftError::Validity`] if the job's identity is not path-safe;
/// [`RiftError::Io`] on a filesystem failure.
pub(crate) fn enqueue(git_dir: &Path, job: &AgentJob) -> Result<PathBuf> {
    let Some(stem) = job.kind.stem() else {
        return Err(RiftError::validity(
            "agent queue: job identity is not path-safe",
        ));
    };
    let dir = queue_dir(git_dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{stem}.json"));
    let tmp = dir.join(format!("{stem}.json.tmp"));
    let body = serde_json::to_vec(job)
        .map_err(|e| RiftError::validity(format!("agent queue: encode: {e}")))?;
    std::fs::write(&tmp, body)?;
    restrict(&tmp)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Owner-only permissions on a journal file (unix).
fn restrict(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Every readable job: token counts first, then condenses, oldest first
/// within a kind. A file this reader cannot understand — or whose name
/// disagrees with its content — is skipped and left in place, for the reason
/// `capture_queue::entries` gives.
pub(crate) fn jobs(git_dir: &Path) -> Vec<(PathBuf, AgentJob)> {
    let Ok(rd) = std::fs::read_dir(queue_dir(git_dir)) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, AgentJob)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?.to_owned();
            let bytes = std::fs::read(&p).ok()?;
            let job: AgentJob = serde_json::from_slice(&bytes).ok()?;
            (job.kind.stem().as_deref() == Some(stem.as_str())).then_some((p, job))
        })
        .collect();
    out.sort_by(|a, b| {
        a.1.kind
            .rank()
            .cmp(&b.1.kind.rank())
            .then(a.1.queued_at_ms.cmp(&b.1.queued_at_ms))
            .then(a.0.cmp(&b.0))
    });
    out
}

/// How many job FILES the journal holds, parseable or not (the doctor count;
/// see `capture_queue::entry_file_count` for why it is not [`jobs`]`.len()`).
pub(crate) fn job_file_count(git_dir: &Path) -> usize {
    std::fs::read_dir(queue_dir(git_dir)).map_or(0, |rd| {
        rd.flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .count()
    })
}

/// Drop one drained job.
pub(crate) fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// The marker a finished condense leaves, keyed by session: the worker that
/// finds it does not condense again, however the job came back.
fn done_marker(git_dir: &Path, session_id: &str) -> PathBuf {
    queue_dir(git_dir).join(format!("condensed-{}", session_key(session_id)))
}

/// Record that `session_id`'s condense landed.
pub(crate) fn mark_condensed(git_dir: &Path, session_id: &str) {
    let dir = queue_dir(git_dir);
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(done_marker(git_dir, session_id), b"");
    }
}

/// Whether `session_id`'s condense already landed.
pub(crate) fn is_condensed(git_dir: &Path, session_id: &str) -> bool {
    done_marker(git_dir, session_id).exists()
}

/// A short, path-safe key for the said-once note a failing job leaves on the
/// capture queue's marker channel (`capture_queue::mark_note`), so the next
/// commit hook and `rift doctor` say it — the worker's stderr is `/dev/null`.
pub(crate) fn note_key(job: &AgentJob) -> String {
    let stem = job.kind.stem().unwrap_or_else(|| "agent-job".to_owned());
    let short: String = stem.chars().take(40).collect();
    format!("agent-{short}")
}

/// Record a failed attempt: keep the job with its count raised, or — once it
/// has spent `capture_queue::MAX_ATTEMPTS` — drop it. Either way the reason
/// lands on the said-once channel. Returns whether the job survives.
pub(crate) fn record_failure(path: &Path, job: &AgentJob, git_dir: &Path, reason: &str) -> bool {
    let attempts = job.attempts.saturating_add(1);
    let key = note_key(job);
    if attempts >= crate::capture_queue::MAX_ATTEMPTS {
        crate::capture_queue::mark_note(
            git_dir,
            &key,
            &format!("{reason} (gave up after {attempts} attempts)"),
        );
        remove(path);
        return false;
    }
    crate::capture_queue::mark_note(git_dir, &key, reason);
    let _ = enqueue(
        git_dir,
        &AgentJob {
            attempts,
            ..job.clone()
        },
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALL: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn condense(session: &str, at: i64) -> AgentJob {
        AgentJob {
            schema_version: 1,
            work_tree: PathBuf::from("/w"),
            queued_at_ms: at,
            attempts: 0,
            kind: JobKind::Condense {
                session_id: session.to_owned(),
                agent: "claude-code".into(),
                repo_id: "repo".into(),
            },
        }
    }

    fn tokens(call: &str, at: i64) -> AgentJob {
        AgentJob {
            schema_version: 1,
            work_tree: PathBuf::from("/w"),
            queued_at_ms: at,
            attempts: 0,
            kind: JobKind::ReadTokens {
                tool_call_id: call.to_owned(),
                rel_path: "a.txt".into(),
                content_sha256: "ff".repeat(32),
                blob_oid: "ee".repeat(20),
                lines: None,
                schema_version: 1,
            },
        }
    }

    /// The same work enqueued twice is one file, and a session id — agent
    /// text — never becomes a path component itself.
    #[test]
    fn a_redelivered_job_overwrites_its_own_file() {
        let tmp = tempfile::tempdir().unwrap();
        let a = enqueue(tmp.path(), &condense("../evil session", 1)).unwrap();
        let b = enqueue(tmp.path(), &condense("../evil session", 2)).unwrap();
        assert_eq!(a, b);
        assert!(a
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("condense-"));
        assert!(!a.to_string_lossy().contains("evil"));
        let listed = jobs(tmp.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1.queued_at_ms, 2, "the later delivery's clock");
    }

    /// Token counts drain before the condense that would freeze them into a
    /// shard, whatever order they were queued in.
    #[test]
    fn token_jobs_list_before_condense_jobs() {
        let tmp = tempfile::tempdir().unwrap();
        enqueue(tmp.path(), &condense("s", 1)).unwrap();
        enqueue(tmp.path(), &tokens(CALL, 5)).unwrap();
        let kinds: Vec<u8> = jobs(tmp.path())
            .iter()
            .map(|(_, j)| j.kind.rank())
            .collect();
        assert_eq!(kinds, vec![0, 1]);
        assert_eq!(job_file_count(tmp.path()), 2);
    }

    /// A token job whose call id is not 64 lowercase hex is refused, never
    /// written under a name it did not earn.
    #[test]
    fn a_token_job_needs_a_hex_call_id() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(enqueue(tmp.path(), &tokens("../x", 1)).is_err());
        assert!(enqueue(tmp.path(), &tokens(&CALL.to_uppercase(), 1)).is_err());
        assert!(jobs(tmp.path()).is_empty());
    }

    /// A file whose name disagrees with its content is skipped in place and
    /// counted — the two halves of the doctor's report.
    #[test]
    fn a_renamed_job_is_skipped_but_counted() {
        let tmp = tempfile::tempdir().unwrap();
        let path = enqueue(tmp.path(), &condense("s", 1)).unwrap();
        let moved = path.with_file_name(format!("tokens-{CALL}.json"));
        std::fs::rename(&path, &moved).unwrap();
        assert!(jobs(tmp.path()).is_empty());
        assert_eq!(job_file_count(tmp.path()), 1);
        assert!(moved.exists(), "left for the doctor, not deleted");
    }

    /// Failure is loud, bounded and idempotent: the note lands on the shared
    /// said-once channel, the job survives until the bound, then it goes.
    #[test]
    fn a_failing_job_is_said_once_and_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let job = condense("s", 1);
        let path = enqueue(tmp.path(), &job).unwrap();
        assert!(record_failure(&path, &job, tmp.path(), "no head"));
        let notes = crate::capture_queue::take_errors(tmp.path());
        assert_eq!(notes, vec!["no head"]);
        let (_, retried) = jobs(tmp.path()).pop().unwrap();
        assert_eq!(retried.attempts, 1);
        let mut j = retried;
        j.attempts = crate::capture_queue::MAX_ATTEMPTS - 1;
        assert!(!record_failure(&path, &j, tmp.path(), "no head"));
        assert!(jobs(tmp.path()).is_empty(), "spent its attempts");
        assert!(crate::capture_queue::take_errors(tmp.path())[0].contains("gave up"));
    }

    fn audit(call_id: &str, at: i64) -> AgentJob {
        AgentJob {
            schema_version: 1,
            work_tree: PathBuf::from("/w"),
            queued_at_ms: at,
            attempts: 0,
            kind: JobKind::ServeAudit(ServeAudit {
                call_id: call_id.to_owned(),
                decision: ServeAuditKind::ReadIntercepted,
                agent: "claude-code".into(),
                basis: None,
                frame: "a reason".into(),
                tag: None,
                total: None,
                declared_avoided: None,
                shape: None,
            }),
        }
    }

    /// Each new kind is journaled under an identity of its own making: the
    /// decision's id, the document's content hash, or the single name a
    /// pending index rebuild has. Nothing agent-supplied becomes a path
    /// component, and an identity that is not path-safe is refused rather
    /// than written under a name it did not earn.
    #[test]
    fn the_new_kinds_are_journaled_under_identities_they_earned() {
        let tmp = tempfile::tempdir().unwrap();
        let call = "0123456789abcdef0123456789abcdef";
        let a = enqueue(tmp.path(), &audit(call, 1)).unwrap();
        let b = enqueue(tmp.path(), &audit(call, 2)).unwrap();
        assert_eq!(a, b, "one decision is one file");
        assert!(a.ends_with(format!("audit-{call}.json")));
        assert!(enqueue(tmp.path(), &audit("../x", 1)).is_err());
        assert!(enqueue(tmp.path(), &audit(&call.to_uppercase(), 1)).is_err());

        let index = AgentJob {
            kind: JobKind::SearchIndex,
            ..audit(call, 3)
        };
        assert!(enqueue(tmp.path(), &index)
            .unwrap()
            .ends_with("search-index.json"));
        let doc = |sha: &str| AgentJob {
            kind: JobKind::DocConvert {
                rel_path: "docs/spec.docx".into(),
                content_sha256: sha.to_owned(),
            },
            ..audit(call, 4)
        };
        let sha = "ab".repeat(32);
        assert!(enqueue(tmp.path(), &doc(&sha))
            .unwrap()
            .ends_with(format!("doc-{sha}.json")));
        assert!(enqueue(tmp.path(), &doc("../evil")).is_err());
        let sweep = AgentJob {
            kind: JobKind::DocSweep,
            ..audit(call, 5)
        };
        assert!(enqueue(tmp.path(), &sweep)
            .unwrap()
            .ends_with("doc-sweep.json"));
        assert_eq!(
            enqueue(tmp.path(), &sweep).unwrap(),
            enqueue(tmp.path(), &sweep).unwrap(),
            "three hooks asking for a pass have asked for one pass"
        );
        assert_eq!(
            jobs(tmp.path()).len(),
            4,
            "one file per identity, nothing else"
        );
    }

    /// Everything that is not a condense drains before the condense that
    /// would freeze a session's shard, and a decision journaled first is
    /// written first.
    #[test]
    fn accounting_and_index_work_drain_before_a_condense() {
        let tmp = tempfile::tempdir().unwrap();
        enqueue(tmp.path(), &condense("s", 1)).unwrap();
        enqueue(tmp.path(), &audit("0123456789abcdef0123456789abcdef", 5)).unwrap();
        enqueue(
            tmp.path(),
            &AgentJob {
                kind: JobKind::SearchIndex,
                ..condense("s", 2)
            },
        )
        .unwrap();
        let ranks: Vec<u8> = jobs(tmp.path())
            .iter()
            .map(|(_, j)| j.kind.rank())
            .collect();
        assert_eq!(ranks, vec![0, 0, 1]);
    }

    /// The done-marker is what makes a redelivered or re-found condense a
    /// no-op.
    #[test]
    fn the_done_marker_is_per_session() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_condensed(tmp.path(), "a"));
        mark_condensed(tmp.path(), "a");
        assert!(is_condensed(tmp.path(), "a"));
        assert!(!is_condensed(tmp.path(), "b"));
    }

    /// Unknown fields are ignored on read: a newer writer's job still drains.
    #[test]
    fn a_newer_writers_job_still_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let path = enqueue(tmp.path(), &condense("s", 1)).unwrap();
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["future"] = serde_json::json!(true);
        std::fs::write(&path, v.to_string()).unwrap();
        assert_eq!(jobs(tmp.path()).len(), 1);
    }
}
