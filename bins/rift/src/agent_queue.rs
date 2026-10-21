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
//! Filenames bind the complete context except the retry state (`attempts`,
//! `next_due_ms`, `parked`, read through `delivery::retry_state`). Identical
//! publication keeps the retained retry state; distinct context remains a
//! separate delivery. Completion requires the worker's per-kind proof, not the
//! filename alone.

pub(crate) mod names;
mod unavailable;

pub(crate) use self::unavailable::{record_unavailable, unavailable_count};

use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};

/// One journaled job: what the worker needs to do it later.
///
/// Typed execution reads supported fields. The full validated JSON value,
/// including unknown fields, remains bound to delivery identity and retries.
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
    /// The interval archive an agent hook's strategy decision asked for:
    /// redact the session's transcript, write the shard, append to the chain,
    /// and change no lifecycle. Journaled because the redaction alone outlasts
    /// a client's hook timeout on a multi-megabyte transcript.
    Archive(IntervalArchive),
    /// The checkpoint one turn end owes (L-5 SealTurn ruling): seal the turn's
    /// new transcript lines onto the earlier segments and archive.
    SealTurn(SealTurn),
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
    /// Attribute one serve the skill CLI already booked to the session whose
    /// tool result named its keys. The CLI process is a person's or an
    /// agent's, and only the hook that ingests its answer knows which; the
    /// hook opens no store, so the row is the worker's to write.
    AttributeServe(ServeClaim),
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
    /// Sweep the family's idle sessions: condense and end every live session
    /// untouched for longer than `floor_ms`.
    ///
    /// Asked for at every session start, because that is the moment another
    /// agent's abandoned session is most likely to be sitting there, and the
    /// worker a start dispatches has nothing else to wait for. Every worker
    /// pass also sweeps at its tail; the job is what keeps the request when
    /// the worker that was dispatched could not take the lock.
    SweepIdle {
        /// The idle floor the requesting hook read from the family's config.
        floor_ms: i64,
    },
    /// Queued boundary metadata and a request for later current-byte collection.
    /// The hook records HEAD and event time, not immutable event-time bytes.
    BoundarySnapshot(BoundarySnapshot),
    /// The member's run state still has files in its git dir's old tree, and
    /// a hook found them. A hook never moves anything — the move takes a lock
    /// and renames, which is no hook's to pay for — so it asks the worker,
    /// which moves before it drains.
    MoveRunState,
    /// Ingest the checkpoint branch into the family's recall store, after a
    /// worker pass that wrote a checkpoint. It names nothing but the family
    /// whose queue holds it, so every pass that asks while one is pending is
    /// answered by that one.
    RecallIngest,
}

/// One turn boundary a hook closed and the worker owes a snapshot of.
///
/// The hook records HEAD and event time within its latency budget. The worker
/// later collects current bounded bytes. Event time does not prove when those
/// bytes were written or who wrote them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct BoundarySnapshot {
    /// The agent-supplied session id.
    pub session_id: String,
    /// The session's *owner* agent, not the hook's.
    pub agent: String,
    /// The turn the boundary opens or closes.
    pub ordinal: u32,
    /// Whether it opens the turn or closes it.
    pub boundary: BoundaryEdge,
    /// The commit `HEAD` named when the hook ran; `None` on an unborn branch
    /// or a ref store the hook's reader does not understand.
    #[serde(default)]
    pub head_oid: Option<String>,
    /// The boundary's own instant, epoch milliseconds.
    pub captured_at_ms: i64,
}

/// Which edge of a turn a [`BoundarySnapshot`] is, spelled as the boundary
/// rows spell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BoundaryEdge {
    /// The turn opens: closes the human's window.
    Start,
    /// The turn ends: closes the agent's window.
    End,
}

/// One interval archive a hook decided and the worker owes.
///
/// It names the session and why, never the transcript: the worker reads the
/// transcript at drain time through the address the session cache keeps, as
/// the session-end condense does, and asks the strategy again under the lock.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct IntervalArchive {
    /// The agent-supplied session id.
    pub session_id: String,
    /// The session's *owner* agent, not the hook's.
    pub agent: String,
    /// Why the hook decided the session had earned a checkpoint.
    pub reason: IntervalReason,
    /// Which interval the checkpoint closes: the session's last archive
    /// instant the hook read when it found the interval due, `None` before
    /// the session's first. Part of the delivery's identity, so every hook
    /// inside one due interval asks for the same checkpoint and a hook in the
    /// next interval asks for another.
    #[serde(default)]
    pub interval_start_ms: Option<i64>,
}

/// One turn end a `Stop` hook closed and the worker owes a checkpoint for.
///
/// Its identity is (session, frontier), round 28's interval-identity rule
/// applied to turns: a redelivered `Stop` names the same frontier and
/// coalesces, and the next turn's end names a later one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SealTurn {
    /// The agent-supplied session id.
    pub session_id: String,
    /// The session's *owner* agent, not the hook's.
    pub agent: String,
    /// The transcript's length in bytes when the hook ran: the end of the
    /// last record the agent had written (a stat, never a read).
    pub frontier: u64,
}

/// Why an agent hook archives a live session between its commits: the two
/// reasons a decision with no git event can carry, spelled as the audit row
/// spells them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum IntervalReason {
    /// The configured interval elapsed.
    Schedule,
    /// A sensitive path was touched: the overlay that archives under any base.
    SensitivePath,
}

impl BoundaryEdge {
    /// The journal's spelling of a boundary the state machine closed.
    pub(crate) fn of(boundary: rift_shadow::Boundary) -> Self {
        match boundary {
            rift_shadow::Boundary::Start => Self::Start,
            rift_shadow::Boundary::End => Self::End,
        }
    }

    /// The boundary the capture takes.
    pub(crate) fn boundary(self) -> rift_shadow::Boundary {
        match self {
            Self::Start => rift_shadow::Boundary::Start,
            Self::End => rift_shadow::Boundary::End,
        }
    }

    /// The edge's place within its turn, for the journal's file name: a start
    /// before the end of the same turn.
    fn position(self) -> &'static str {
        match self {
            Self::Start => "0-start",
            Self::End => "1-end",
        }
    }
}

impl IntervalReason {
    /// The journal's reason for a decision's, or `None` for a reason only a
    /// git event or a session end can produce.
    pub(crate) fn of(reason: rift_shadow::hooks::strategy::ArchiveReason) -> Option<Self> {
        use rift_shadow::hooks::strategy::ArchiveReason;

        match reason {
            ArchiveReason::Schedule => Some(Self::Schedule),
            ArchiveReason::SensitivePath => Some(Self::SensitivePath),
            ArchiveReason::Commit
            | ArchiveReason::Pr
            | ArchiveReason::SessionEnd
            | ArchiveReason::TurnEnd => None,
        }
    }
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
    /// The session whose hook decided, so the worker can write the row that
    /// attributes the decision to it and the session's checkpoint freezes it.
    /// `None` on a job an older binary left behind, whose rows stay
    /// unattributed and are frozen into no shard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The window journal's key of the context window that decided, where the
    /// hook had it in hand without a second read of its payload; `None`
    /// elsewhere, and the attribution then names the session alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
}

/// The two keys one skill-CLI serve printed, and the window whose tool result
/// carried them.
///
/// Both keys, because the schema-1 key is a salted hash the CLI computed and
/// nothing recomputes it; the worker writes a row only for a key its relation
/// already holds.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ServeClaim {
    /// The agent's own session id.
    pub session: String,
    /// The window journal's key of the window that ran the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// The schema-4 key (`serve_call_event.call_id`), 32 lowercase hex.
    pub call_id: String,
    /// The schema-1 key (`memd_audit_event.audit_id`), 64 lowercase hex.
    pub audit_id: String,
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
    /// A read that ran of bytes a decline of the same window had already
    /// been credited with sparing — a follow-up the decline offered, or any
    /// other read of them since. What it delivered is that decline's cost,
    /// booked as its own row and never as a change to the decline's.
    ReadFollowUp,
    /// A read Rift had already made unnecessary was never made: the window
    /// acted on a file Rift told it that it holds, without reading it again.
    ReadPrevented,
    /// The same decision, recorded and not acted on.
    ReadPreventedObserved,
    /// A terminal result carried lines the window provably holds, and the
    /// lever that would stand a frame in for them was only observing. The
    /// bytes the frame would have spared ride in `total`.
    ToolOutputHeld,
    /// The same held stretch, stood in for by its frame in the result the
    /// model read because the lever enforces. Booked apart from the observed
    /// row so an acted figure and a would-be figure never mix.
    ToolOutputSubstituted,
    /// A terminal result carried runs of alike lines a fold line could stand
    /// in for, and the lever that would fold them was only observing. The
    /// bytes the folds would have spared ride in `total`.
    ToolOutputFolded,
    /// The same runs, folded in the result the model read because the lever
    /// enforces. Booked apart so an acted figure and a would-be one never mix.
    ToolOutputFoldApplied,
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
///
/// The member's run directory under the family's cache directory, resolved
/// (and moved out of the git dir on the first resolution) by
/// [`crate::family::run_state`]; every path in this module derives from it.
pub(crate) fn queue_dir(git_dir: &Path) -> PathBuf {
    crate::family::run_state::agent_queue_dir(git_dir)
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
    /// The session a job belongs to, when its kind names one — what the retry
    /// verb and the doctor's parked lines are keyed by.
    pub(crate) fn session_id(&self) -> Option<&str> {
        match self {
            Self::Condense { session_id, .. } | Self::ToolResultTokens { session_id, .. } => {
                Some(session_id)
            }
            Self::Archive(archive) => Some(&archive.session_id),
            Self::SealTurn(seal) => Some(&seal.session_id),
            Self::BoundarySnapshot(snapshot) => Some(&snapshot.session_id),
            Self::ReadTokens { .. }
            | Self::ServeAudit(_)
            | Self::AttributeServe(_)
            | Self::SearchIndex
            | Self::DocConvert { .. }
            | Self::BasisSnapshot { .. }
            | Self::DocSweep
            | Self::SweepIdle { .. }
            | Self::MoveRunState
            | Self::RecallIngest => None,
        }
    }

    /// The file stem this job is journaled under: its identity, so the same
    /// work enqueued twice is one file.
    fn stem(&self) -> Option<String> {
        match self {
            Self::Condense { session_id, .. } => {
                Some(format!("condense-{}", session_key(session_id)))
            }
            // One archive is owed per session at a time: every hook inside
            // one due interval asks for the same checkpoint, and the note a
            // failure leaves is one note per session.
            Self::Archive(archive) => Some(format!("archive-{}", session_key(&archive.session_id))),
            Self::SealTurn(seal) => Some(format!(
                "turn-end-{}-{}",
                session_key(&seal.session_id),
                seal.frontier
            )),
            Self::ReadTokens { tool_call_id, .. } => {
                is_lower_hex(tool_call_id, 64).then(|| format!("tokens-{tool_call_id}"))
            }
            // The decision's own id, which this process minted, so the file
            // name is one decision and a redelivered job overwrites itself.
            Self::ServeAudit(audit) => {
                is_lower_hex(&audit.call_id, 32).then(|| format!("audit-{}", audit.call_id))
            }
            // The serve's own call id: a result ingested twice owes one row.
            Self::AttributeServe(claim) => (is_lower_hex(&claim.call_id, 32)
                && is_lower_hex(&claim.audit_id, 64))
            .then(|| format!("attribute-{}", claim.call_id)),
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
            // One sweep is owed at a time: every start while one is pending
            // asks for the same pass, so the name (and the note a failure
            // leaves) is constant.
            Self::SweepIdle { .. } => Some("sweep-idle".to_owned()),
            // One snapshot per boundary: a redelivered hook owes the one its
            // first delivery journaled. The ordinal is zero-padded so the
            // names of one session sort in turn order, start before end,
            // which is the order the drain breaks a tie of instants in.
            Self::BoundarySnapshot(snapshot) => Some(format!(
                "boundary-{}-{:010}-{}",
                session_key(&snapshot.session_id),
                snapshot.ordinal,
                snapshot.boundary.position()
            )),
            // One move is owed per member: every hook that finds the old
            // tree asks for the same move.
            Self::MoveRunState => Some("move-run-state".to_owned()),
            // One ingest is owed per family at a time: it reads the whole
            // branch from its watermark, so a second ask is the same work.
            Self::RecallIngest => Some("recall-ingest".to_owned()),
        }
    }

    /// Whether two deliveries of this request that differ only in their
    /// instant may stand side by side without conflict: running either is
    /// running the other, so a move need not hold one back for the other.
    pub(crate) fn is_repeatable(&self) -> bool {
        matches!(
            self,
            Self::SearchIndex
                | Self::DocSweep
                | Self::SweepIdle { .. }
                | Self::MoveRunState
                | Self::RecallIngest
        )
    }

    /// Whether a second ask for this work while one is pending is answered by
    /// the pending one: a request that names nothing but its work tree (the
    /// index rebuild, the run-state move) or the interval it closes (an
    /// interval archive), so one delivery answers every caller that asks
    /// before the worker takes it, on the clock of the first. Every other kind
    /// keeps the delivery a changed instant makes.
    fn coalesces_while_pending(&self) -> bool {
        matches!(
            self,
            Self::SearchIndex
                | Self::MoveRunState
                | Self::RecallIngest
                | Self::Archive(_)
                | Self::SealTurn { .. }
        )
    }

    /// Token jobs drain before condense and archive jobs, so the shard either
    /// writes carries every count the session's reads were owed; the sweep drains
    /// last, so a session whose own end is queued is condensed by that end
    /// rather than timed out beside it. Boundary snapshots drain with the
    /// first rank, in the order of their instants: each diffs against the
    /// boundary before it, so a turn's end must be observed before the next
    /// turn's start, and both before the condense that follows them.
    fn rank(&self) -> u8 {
        match self {
            Self::ReadTokens { .. }
            | Self::ServeAudit(_)
            | Self::AttributeServe(_)
            | Self::SearchIndex
            | Self::DocConvert { .. }
            | Self::BasisSnapshot { .. }
            | Self::ToolResultTokens { .. }
            | Self::DocSweep
            | Self::BoundarySnapshot(_)
            | Self::MoveRunState => 0,
            Self::Condense { .. } | Self::Archive(_) | Self::SealTurn(_) => 1,
            // After the checkpoint writers, so the ingest a pass asks for
            // sees every checkpoint that pass wrote.
            Self::SweepIdle { .. } | Self::RecallIngest => 2,
        }
    }
}

/// Publish through an exclusive private stage and no-clobber install.
/// Identical context keeps retained attempts; distinct context stays separate.
///
/// # Errors
/// [`RiftError::Validity`] if the job's identity is not path-safe;
/// [`RiftError::Io`] on a filesystem failure.
pub(crate) fn enqueue(git_dir: &Path, job: &AgentJob) -> Result<PathBuf> {
    enqueue_in(&queue_dir(git_dir), job)
}

/// [`enqueue`] into an explicit journal directory.
///
/// The resolution of a member's run state journals the move job through
/// this, with the directory it has just resolved: going through
/// [`queue_dir`] would resolve the member again from inside its own
/// resolution.
///
/// # Errors
/// As [`enqueue`].
pub(crate) fn enqueue_in(dir: &Path, job: &AgentJob) -> Result<PathBuf> {
    let Some(stem) = job.kind.stem() else {
        return Err(RiftError::validity(
            "agent queue: job identity is not path-safe",
        ));
    };
    let value =
        serde_json::to_value(job).map_err(|_| RiftError::validity("agent queue: encode failed"))?;
    if job.kind.coalesces_while_pending() {
        if let Some(pending) = pending_repeat(dir, &stem, &value) {
            return Ok(pending);
        }
    }
    crate::capture_queue::delivery::publish(dir, &stem, &value)
}

/// Whether two deliveries of one repeatable request ask for the same work:
/// equal in everything but the instant each caller stamped and the attempts a
/// worker spent.
pub(crate) fn same_request(first: &serde_json::Value, second: &serde_json::Value) -> bool {
    let request = |value: &serde_json::Value| {
        let mut value = value.clone();
        if let Some(fields) = value.as_object_mut() {
            fields.remove("queued_at_ms");
            fields.remove("attempts");
        }
        value
    };
    request(first) == request(second)
}

/// The pending delivery of `stem` that already asks for `value`'s work, if one
/// is journaled. Check-then-publish: two callers racing past the check leave
/// two deliveries, which is what every changed instant left before this
/// check, so a lost race costs the coalescing and never loses a delivery.
fn pending_repeat(dir: &Path, stem: &str, value: &serde_json::Value) -> Option<PathBuf> {
    let versioned = format!("{stem}--");
    rift_shadow::hooks::run_state::list_entries(dir)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|x| x == "json"))
        .filter(|path| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == stem || name.starts_with(&versioned))
        })
        .find(|path| read_job(path).is_ok_and(|(pending, _)| same_request(&pending, value)))
}

/// Every readable job: token counts first, then condenses, oldest first
/// within a kind. A file this reader cannot understand — or whose name
/// disagrees with its content — is skipped and left in place, for the reason
/// `capture_queue::entries` gives.
pub(crate) fn jobs(git_dir: &Path) -> Vec<(PathBuf, AgentJob)> {
    let mut out: Vec<(PathBuf, AgentJob)> =
        rift_shadow::hooks::run_state::list_entries(&queue_dir(git_dir))
            .into_iter()
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter_map(|p| {
                let (_, job) = read_job(&p).ok()?;
                Some((p, job))
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

pub(crate) fn read_job(path: &Path) -> Result<(serde_json::Value, AgentJob)> {
    let value = crate::capture_queue::delivery::decode(&std::fs::read(path)?)?;
    let job: AgentJob = serde_json::from_value(value.clone())
        .map_err(|_| RiftError::validity("agent queue: unsupported job"))?;
    let Some(stem) = job.kind.stem() else {
        return Err(RiftError::validity("agent queue: invalid logical identity"));
    };
    if !crate::capture_queue::delivery::matches_name(path, &stem, &value)? {
        return Err(RiftError::validity(
            "agent queue: name and context disagree",
        ));
    }
    Ok((value, job))
}

pub(crate) fn logical_stem(job: &AgentJob) -> Option<String> {
    job.kind.stem()
}

/// The jobs an ordinary pass runs at `now_ms`: due and unparked
/// (`DrainPolicy::Due`), in [`jobs`]' order. A repair road lists [`jobs`].
pub(crate) fn jobs_due(git_dir: &Path, now_ms: i64) -> Vec<(PathBuf, AgentJob)> {
    retry_states(git_dir)
        .into_iter()
        .filter(|(_, _, state)| state.is_due(now_ms))
        .map(|(path, job, _)| (path, job))
        .collect()
}

/// Every readable job with its retry state, in [`jobs`]' order: the doctor's,
/// the retry verb's and the drain policy's one reading of the two sibling
/// members (L-0A §4.1 as amended 2026-10-17).
pub(crate) fn retry_states(
    git_dir: &Path,
) -> Vec<(
    PathBuf,
    AgentJob,
    crate::capture_queue::delivery::RetryState,
)> {
    let mut out: Vec<_> = rift_shadow::hooks::run_state::list_entries(&queue_dir(git_dir))
        .into_iter()
        .filter(|p| is_ticket(p))
        .filter_map(|p| {
            let (value, job) = read_job(&p).ok()?;
            let state = crate::capture_queue::delivery::retry_state(&value).ok()?;
            Some((p, job, state))
        })
        .collect();
    out.sort_by(|a, b| {
        a.1.kind
            .rank()
            .cmp(&b.1.kind.rank())
            .then_with(|| a.1.queued_at_ms.cmp(&b.1.queued_at_ms))
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

/// Reset a parked job to due-now under the member owner, keeping its
/// `attempts`: the retry verb's one write.
pub(crate) fn unpark_owned(
    path: &Path,
    job: &AgentJob,
    owner: &crate::capture_queue::QueueLock,
) -> Result<()> {
    owner.check_path(path)?;
    let (value, actual) = read_job(path)?;
    if actual != *job {
        return Err(crate::capture_queue::deferred(
            "agent delivery changed before retry",
        ));
    }
    let stem = logical_stem(job)
        .ok_or_else(|| crate::capture_queue::deferred("agent identity unavailable"))?;
    let next = crate::capture_queue::delivery::retry_state(&value)?.unparked();
    crate::capture_queue::delivery::replace_retry_state(path, &stem, &value, next, owner)
}

/// How many job FILES the journal holds, parseable or not (the doctor count;
/// see `capture_queue::entry_file_count` for why it is not [`jobs`]`.len()`).
pub(crate) fn job_file_count(git_dir: &Path) -> usize {
    rift_shadow::hooks::run_state::list_entries(&queue_dir(git_dir))
        .iter()
        .filter(|path| is_ticket(path))
        .count()
}

/// Record one execution failure at `failed_at_ms` (the worker's clock): the
/// job is **kept** with its retry state advanced per §4.1 — the third failure
/// parks it, nothing removes it — and the reason lands on the said-once
/// channel as `capture_queue::failure_note` spells it, rewritten past the park
/// only when the reason changed (one note per new reason). Returns the new
/// state.
pub(crate) fn record_failure_owned(
    path: &Path,
    job: &AgentJob,
    reason: &str,
    owner: &crate::capture_queue::QueueLock,
    failed_at_ms: i64,
) -> Result<crate::capture_queue::delivery::RetryState> {
    owner.check_path(path)?;
    let (value, actual) = read_job(path)?;
    if actual != *job {
        return Err(crate::capture_queue::deferred(
            "agent delivery changed before retry",
        ));
    }
    let stem = logical_stem(job)
        .ok_or_else(|| crate::capture_queue::deferred("agent identity unavailable"))?;
    let next = crate::capture_queue::delivery::retry_state(&value)?.after_failure(failed_at_ms);
    crate::capture_queue::delivery::replace_retry_state(path, &stem, &value, next, owner)?;
    let key = note_key(job);
    let note = crate::capture_queue::failure_note(reason, next, job.kind.session_id());
    // Past the park the note is said once per new reason; the reason record
    // outlives the note, which the next hook consumes.
    let changed = crate::capture_queue::record_reason(owner.git_dir(), &key, reason);
    if changed || next.attempts <= crate::capture_queue::PARK_AFTER {
        crate::capture_queue::mark_note(owner.git_dir(), &key, &note);
    }
    Ok(next)
}

/// Whether `path` names a job ticket in the queue's one name grammar.
fn is_ticket(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| names::classify(name) == names::QueueName::Ticket)
}

/// The marker a finished condense leaves, keyed by session: the worker that
/// finds it does not condense again, however the job came back.
fn done_marker(git_dir: &Path, session_id: &str) -> PathBuf {
    queue_dir(git_dir).join(names::condensed(&session_key(session_id)))
}

/// Record that `session_id`'s condense landed.
pub(crate) fn mark_condensed(git_dir: &Path, session_id: &str) {
    let dir = queue_dir(git_dir);
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(done_marker(git_dir, session_id), b"");
    }
}

/// Forget that `session_id`'s condense landed, because a new end asked for
/// another. A session re-opened after its end condenses again at its next
/// end, and the marker the first condense left answers for that end only;
/// a redelivered end asks for nothing (the state machine ignores an end for
/// a session already ending or ended), so it never reaches here.
pub(crate) fn clear_condensed(git_dir: &Path, session_id: &str) {
    let _ = std::fs::remove_file(done_marker(git_dir, session_id));
}

/// Whether `session_id`'s condense already landed.
pub(crate) fn is_condensed(git_dir: &Path, session_id: &str) -> bool {
    let name = names::condensed(&session_key(session_id));
    rift_shadow::hooks::run_state::read_path(&queue_dir(git_dir), &name).exists()
}

/// A short, path-safe key for the said-once note a failing job leaves on the
/// capture queue's marker channel (`capture_queue::mark_note`), so the next
/// commit hook and `rift doctor` say it — the worker's stderr is `/dev/null`.
pub(crate) fn note_key(job: &AgentJob) -> String {
    let stem = job.kind.stem().unwrap_or_else(|| "agent-job".to_owned());
    let short: String = stem.chars().take(40).collect();
    format!("agent-{short}")
}

/// A path-safe key for a said-once note about `session_id` that no one job
/// owns (the cleanup a stale completion left owed, §4.3).
pub(crate) fn note_key_for_session(session_id: &str) -> String {
    session_key(session_id).chars().take(16).collect()
}

#[cfg(test)]
mod tests;
