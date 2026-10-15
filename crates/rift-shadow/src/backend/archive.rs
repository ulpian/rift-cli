//! The archived types: what a checkpoint shard carries, as the write side
//! assembles it — the archive itself, its events, read facts, transcript pair,
//! subagent tasks, redaction facts and the worktree label. Separate from the
//! writer so the shape a shard holds is readable without the git plumbing
//! that commits it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rift_core::RedactedBytes;

use super::artifacts::{canonical_json, insert_compact_boundary};
use super::originals::ArchivedOriginal;
use super::spills::ArchivedSpill;
use super::tasks::TaskProvenance;

/// One archived canonical event, carrying exactly the fields a `full.jsonl` line
/// records (content addresses, never re-derived). Mirrors
/// [`rift_recall_types::checkpoint::FullEventRecord`] on the write side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedEvent {
    /// The agent's native session id.
    pub session: String,
    /// Turn ordinal within the session.
    pub turn_ordinal: u32,
    /// Event ordinal within the turn.
    pub event_ordinal: u32,
    /// The canonical event-kind token, in the ONE spelling Rift writes:
    /// [`rift_core::EventKind::as_str`]'s snake_case (`session_start`, …).
    ///
    /// This said `session.start` until 2026-08-12 and had never been true of
    /// anything the native archive writes. The dot spelling is the ARCHIVED
    /// dialect the read-compat fixtures carry, which the artifact bridge
    /// accepts and normalises on the way in ([`rift_core::EventKind::from_wire_token`]).
    pub kind: String,
    /// The native hook verb that produced the event (idempotency-key input).
    pub verb: String,
    /// The agent token (`claude-code` | …).
    pub agent: String,
    /// Transaction time (epoch ms).
    pub occurred_at_ms: i64,
    /// Content address of the redacted payload (hex).
    pub payload_sha256: String,
    /// The **already-redacted** payload, as it will be archived into `full.jsonl`.
    /// INVARIANT: this must have passed the redaction gate before it reaches here —
    /// `full.jsonl` is written verbatim via `write_structural_blob` (the entropy
    /// redactor would corrupt its hex `payload_sha256`), so a caller that populates
    /// `events` must redact each payload first. In v0.1 no per-turn stream is
    /// threaded (`events` is always empty), so no un-redacted byte can reach here yet.
    pub payload: serde_json::Value,
}

/// **The worktree a checkpoint shard was archived in** — the absent-when-unknown
/// `worktree` member of `metadata.json`.
///
/// Name, token, linked-ness and branch. **No absolute path and no username**:
/// this member crosses machines inside a transfer bundle, and a checkout's path
/// is a local address, not content. The path lives in the projection-only
/// `session_worktree` relation instead, which never enters a shard — the same
/// split the session cache already makes between `work_tree` (an address) and
/// the facts it produces.
///
/// **It names the ARCHIVING worktree, and specifically the worktree of the
/// FIRST archive that minted the shard.** A checkpoint id is bound to the
/// shard's Merkle root, and `metadata.json` is exempt from the re-archive
/// idempotence comparison (`rift_git`'s `ANCHOR_DEPENDENT_SHARD_ENTRIES`), so a
/// later re-archive of byte-identical capture from a *different* worktree is a
/// no-op that keeps the first label — silently, by design. "Which worktree did
/// this session work in" is answered by `session_worktree`'s observation
/// history, never by this member, which is why `rift checkpoint explain` prints
/// it as `archived_in` rather than "worked in".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeLabel {
    /// The linked member's administrative name, or `main`.
    pub name: String,
    /// `RepoPaths::worktree_token()` — 12 lowercase hex.
    pub token: String,
    /// Whether the archiving checkout is a linked worktree.
    pub linked: bool,
    /// The branch it was on, `None` when detached or unreadable.
    pub branch: Option<String>,
}

impl WorktreeLabel {
    /// The label for the checkout at `work_tree`, or `None` when it is not in a
    /// repository this process can read.
    ///
    /// `None` is UNKNOWN and stays unknown: the member is then absent from
    /// `metadata.json` and the projection records no row, which is exactly what
    /// every shard written before the member existed looks like — never `main`.
    #[must_use]
    pub fn of(work_tree: &Path) -> Option<Self> {
        let paths = rift_git::discover(work_tree).ok()?;
        Some(Self {
            name: paths.member_name(),
            token: paths.worktree_token(),
            linked: paths.is_linked(),
            branch: rift_git::head_state(&paths).branch,
        })
    }

    /// The member's JSON object, in the shape `docs/sync-format.md` declares.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("name".into(), self.name.clone().into());
        obj.insert("token".into(), self.token.clone().into());
        obj.insert("linked".into(), self.linked.into());
        // Absent rather than null when there is no branch: a detached archive
        // records the same bytes a pre-branch writer would, and every other
        // member of this document is absent-when-unknown too.
        if let Some(branch) = &self.branch {
            obj.insert("branch".into(), branch.clone().into());
        }
        serde_json::Value::Object(obj)
    }

    /// Read the `worktree` member out of a shard's `metadata.json` bytes,
    /// screened. `None` when the document has no member, cannot be parsed, or
    /// carries one this side will not believe.
    ///
    /// The bytes come off an untrusted artifact — a shard can arrive in a
    /// bundle from another machine — and land in an append-only table nothing
    /// can correct, so the values are screened before the row is written rather
    /// than after ([`Self::screened`]).
    #[must_use]
    pub fn from_metadata_json(bytes: &[u8]) -> Option<Self> {
        let doc: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        Self::from_member(doc.get("worktree")?)
    }

    /// One `worktree`-shaped JSON object → a screened label.
    #[must_use]
    pub fn from_member(member: &serde_json::Value) -> Option<Self> {
        let text = |key: &str| member.get(key).and_then(serde_json::Value::as_str);
        Self {
            name: text("name")?.to_owned(),
            token: text("token")?.to_owned(),
            linked: member.get("linked").and_then(serde_json::Value::as_bool)?,
            branch: text("branch").map(str::to_owned),
        }
        .screened()
    }

    /// The label, or `None` if any field is outside what this writer can
    /// produce: the token is exactly 12 lowercase hex (`worktree_token`), the
    /// name is one non-empty path component, and neither the name nor the
    /// branch carries a control character or runs past a bound a reader would
    /// have to truncate.
    ///
    /// Refusing the whole member rather than sanitising a field: a half-trusted
    /// label is a label a reader cannot reason about, and "unknown" is already
    /// a value this design carries everywhere.
    #[must_use]
    pub fn screened(self) -> Option<Self> {
        let printable = |s: &str, max: usize| {
            !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
        };
        let token_ok = self.token.len() == 12
            && self
                .token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        let name_ok = printable(&self.name, 128) && !self.name.contains('/');
        let branch_ok = self.branch.as_deref().is_none_or(|b| printable(b, 255));
        (token_ok && name_ok && branch_ok).then_some(self)
    }
}

/// The content a checkpoint archive writes: the shard, assembled from the
/// structural fields plus the redacted `content`.
///
/// The redaction gate is honored two ways: `content` is [`RedactedBytes`] (proof
/// the raw capture passed the gate — written durably via
/// [`NativeGitWrite::write_durable_blob`](rift_git::NativeGitWrite::write_durable_blob)), and the structural artifacts are
/// Rift-authored from counts/ids/content-addresses that never touched a secret.
///
/// No `Debug` derive: `content` is [`RedactedBytes`], which withholds `Debug` on
/// purpose so redacted bytes never reach a log line.
#[derive(Clone)]
pub struct CheckpointArchive {
    /// Frozen checked recovery audit, absent on ordinary archives.
    pub boundary_gaps: Option<super::FrozenBoundaryGaps>,
    /// The repo root the archive belongs to.
    pub repo: PathBuf,
    /// The anchoring working-tree commit oid (hex).
    pub commit: String,
    /// The content-addressed checkpoint id (64-hex).
    pub checkpoint_id: String,
    /// The native session ids this checkpoint covers.
    pub sessions: Vec<String>,
    /// The archival strategy token (`manual-commit` | …).
    pub strategy: String,
    /// The prior checkpoint id in the chain (hex), `None` at genesis.
    pub prev_hash: Option<String>,
    /// The archived canonical events (empty ⇒ no `full.jsonl`).
    pub events: Vec<ArchivedEvent>,
    /// The session's captured read facts (empty ⇒ no `tool_calls.jsonl`) —
    /// what the recall plane ingests as `n_file_read`.
    pub read_facts: Vec<ArchivedReadFact>,
    /// The session's frozen serve envelopes, the bytes of `serve_audit.jsonl`
    /// (`None` ⇒ the artifact is not written). The caller hands them over
    /// already canonical, validated, de-duplicated and sorted, because the
    /// prediction and the archive must fold the same bytes and only one
    /// function may decide what those bytes are.
    pub serve_audit: Option<Vec<u8>>,
    /// The session's settled model calls with their usage (empty ⇒ no
    /// `usage.jsonl`) — what a rebuild re-derives usage and cost from.
    pub usage: Vec<ArchivedUsageFact>,
    /// The agent each archived session was captured from, keyed by native
    /// session id (empty ⇒ no `sessions.jsonl`) — what lets a rebuild key a
    /// session under its own agent rather than one default. A sibling artifact
    /// rather than a `metadata.json` member, because `metadata.json` is in
    /// `content_hash.txt`'s preimage and a new member there would move it.
    pub session_agents: BTreeMap<String, String>,
    /// Every row the session's context windows' journals held at archive time
    /// (empty ⇒ no `window_journal.jsonl`) — the reads, declines, edits and
    /// evictions the savings figures are derived from, frozen where a rebuild
    /// can reach them.
    pub window_journal: Vec<super::ArchivedWindowFact>,
    /// Every observation of the member the session was seen in (empty ⇒ no
    /// `session_worktrees.jsonl`) — frozen without the checkout's path or the
    /// head, one line per distinct state. Not the `session_worktrees` label
    /// map, which names only a session archived from another member and
    /// carries no subagent, basis or time.
    pub worktree_observations: Vec<super::ArchivedWorktreeObservation>,
    /// Every measured tool result the session's capture recorded (empty ⇒ no
    /// `tool_results.jsonl`) — the search half of the savings denominator,
    /// which no read fact carries because a search resolves to no file.
    pub tool_results: Vec<super::ArchivedToolResult>,
    /// The redacted raw capture (durable-blob proof of the gate).
    pub content: RedactedBytes,
    /// The redaction report for `redaction_report.json` (must be valid).
    pub report: RedactionReportView,
    /// The session transcript pair, when one reached the archive
    /// (`None` ⇒ neither transcript artifact is written). One field rather
    /// than three because a boundary without a transcript is an index into
    /// nothing.
    pub transcript: Option<ArchivedTranscript>,
    /// The subagent tasks archived under this checkpoint as
    /// `tasks/<tool-use-id>/` subtrees. Empty ⇒ no `tasks/`
    /// tree is written and the shard is byte-identical to a pre-task shard.
    pub tasks: Vec<ArchivedTask>,
    /// The capture event's own recorded time (epoch ms), or `None` when the
    /// caller has none to record. `Some` is written into `metadata.json` as
    /// `created_at` (the RFC3339 field the bridge's lenient view parses back
    /// into `checkpoint.created_at`) and stamps the shard commit's date;
    /// `None` omits the key and dates the commit at epoch — which keeps every
    /// earlier shard's metadata, and the committed anchors/goldens that freeze
    /// it, byte-identical. The live condense path always passes `Some`
    /// (the hook's own event time, never a projection-time clock).
    pub created_at_ms: Option<i64>,
    /// The worktree this archive was written FROM, or `None`
    /// when it cannot be determined. `Some` writes the `worktree` member into
    /// `metadata.json`; `None` omits the key, which keeps every committed
    /// anchor and golden — none of which is built inside a repository —
    /// byte-identical, exactly as `created_at_ms` does.
    pub worktree: Option<WorktreeLabel>,
    /// Per-session labels for a MULTI-session shard: the sessions whose last
    /// observation was in a different worktree from the archiving one. Empty
    /// for every archive this binary writes today, because `condense` archives
    /// exactly one session; the member exists so a multi-session archiver
    /// cannot land without it, and an empty map writes no key at all.
    pub session_worktrees: BTreeMap<String, WorktreeLabel>,
}

/// A checkpoint's transcript artifacts, both already through
/// the redaction gate: the compaction runs on the RAW side
/// ([`crate::transcript::compact_jsonl`], `SecretBytes` in and out) and each
/// result passes `rift-shadow-redact` like any other capture, so this struct
/// is constructible only from bytes the gate has sealed.
///
/// No `Debug`: both fields are [`RedactedBytes`], which withholds it.
#[derive(Clone)]
pub struct ArchivedTranscript {
    /// `transcript.jsonl` — the redacted full transcript, self-contained.
    pub full: RedactedBytes,
    /// `transcript.compact.jsonl` — the redacted deterministic compaction,
    /// also self-contained (a reader with one shard needs no other shard).
    pub compact: RedactedBytes,
    /// The 0-based compact-line index where this checkpoint's slice begins
    /// ([`crate::transcript::CompactTranscript::compact_transcript_start`]);
    /// rounds toward inclusion, recorded in `metadata.json`.
    pub compact_start: usize,
    /// The tool results this transcript names as spilled to a file of their
    /// own, each sealed beside it under `tool-results/` or accounted for by a
    /// closed word ([`super::SpillState`]). Empty when it names none, which keeps
    /// the shard every spill-less transcript always wrote.
    pub spills: Vec<ArchivedSpill>,
    /// The originals of the rewritten results this transcript records, each
    /// sealed beside it under `tool-originals/` or accounted for by a closed
    /// word ([`super::OriginalState`]). Empty when nothing was rewritten,
    /// which keeps the shard every unrewritten transcript always wrote.
    pub originals: Vec<ArchivedOriginal>,
    /// The lines of this pair the redaction gate sealed by its per-line
    /// fallback, summed over both artifacts; recorded on the metadata
    /// document beside the pair only when non-zero.
    pub redaction_fallback: RedactionFallback,
}

/// What the redaction gate's per-line fallback cost one transcript pair: the
/// lines it sealed flat or replaced because a fixpoint did not converge, and
/// how many of those it replaced with a placeholder record. Counts only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RedactionFallback {
    /// `redaction_flat_fallback`: every line the fallback sealed.
    pub lines: u32,
    /// `redaction_fallback_placeholders`: the subset replaced by a record.
    pub placeholders: u32,
}

impl RedactionFallback {
    /// The two reports' counts, summed: one per artifact of the pair.
    #[must_use]
    pub fn of(reports: &[&rift_shadow_redact::RedactionReport]) -> Self {
        reports.iter().fold(Self::default(), |acc, r| Self {
            lines: acc.lines.saturating_add(r.redaction_flat_fallback),
            placeholders: acc
                .placeholders
                .saturating_add(r.redaction_fallback_placeholders),
        })
    }
}

/// One subagent task archived **inside** the parent checkpoint: a
/// `tasks/<tool-use-id>/` subtree carrying the task's own
/// `metadata.json` and — when the subagent's transcript reached the archive —
/// its own transcript pair, every byte of it a Merkle leaf and therefore bound
/// by the parent's root-derived id.
///
/// Nesting is the deliberate departure from the consulted behavior: the legacy backend
/// wrote a subagent as a SEPARATE checkpoint (`is_task` + `tool_use_id` in
/// `api/checkpoint/metadata.go`, `WriteEphemeralTaskOptions` in
/// `checkpoint/checkpoint.go`, pinned tag, read-only), so a task and the
/// commit it served could drift apart and rollup was a join. Here the task
/// rides inside the parent shard: one path binds both, and rollup is a sum
/// over fields the same tree already carries.
///
/// The timestamps are **capture content** (when the subagent ran), never the
/// archive clock — an anchor-dependent field here would mint a new shard per
/// re-archive of unchanged capture and break the dedup no-op.
///
/// No `Debug`: `transcript` carries [`RedactedBytes`] artifacts, which
/// withhold it on purpose.
#[derive(Clone)]
pub struct ArchivedTask {
    /// The dispatch tool-use id — names the `tasks/<tool-use-id>/` subtree, so
    /// it must be a safe single tree component (checked at archive time by
    /// `NativeBackend::write_shard`; an unsafe id is a `Validity` refusal
    /// before any object is written).
    pub tool_use_id: String,
    /// The subagent identifier (the dispatched agent type/name; may be empty
    /// when the dispatch did not carry one — written either way so the
    /// metadata shape is stable).
    pub agent_id: String,
    /// When the subagent started, epoch ms — **from capture**. May predate the
    /// parent checkpoint's boundary: a task that began before the boundary
    /// still belongs to the checkpoint that saw it finish (the rollup is
    /// deliberately not window-gated; see `condense::tests`).
    pub started_at_ms: i64,
    /// When the subagent finished, epoch ms — from capture.
    pub ended_at_ms: i64,
    /// Input tokens as reported by the agent's own usage stream, when captured
    /// (an estimate carried as a fact, like [`ArchivedReadFact::input_tokens`] —
    /// not a `rift-tokens` measurement). `None` omits the field.
    pub input_tokens: Option<u64>,
    /// Output tokens as reported by the agent's usage stream, when captured.
    pub output_tokens: Option<u64>,
    /// The subagent's transcript pair, when one reached the archive — both
    /// artifacts already through the redaction gate, same proof-by-type as the
    /// parent's [`CheckpointArchive::transcript`].
    pub transcript: Option<ArchivedTranscript>,
    /// Where the transcript came from, when the archive collected the task
    /// from the agent's own files. `None` writes none of its members, so a
    /// task built without it keeps the metadata it always had.
    pub provenance: Option<TaskProvenance>,
}

impl ArchivedTask {
    /// The task's canonical `tasks/<tool-use-id>/metadata.json` bytes
    /// (deterministic key order). A NEW serialized artifact, so it carries
    /// `schema_version` (program-wide requirement 4).
    ///
    /// Every field is capture content: identifiers, capture-time stamps and
    /// agent-reported counts. Nothing here may drift with the anchoring commit
    /// or the archive clock — this document is a Merkle leaf, and an
    /// anchor-dependent byte in a leaf would move the id on every re-archive.
    #[must_use]
    pub fn metadata_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert("schema_version".into(), 1u32.into());
        obj.insert("tool_use_id".into(), self.tool_use_id.clone().into());
        obj.insert("agent_id".into(), self.agent_id.clone().into());
        obj.insert("started_at_ms".into(), self.started_at_ms.into());
        obj.insert("ended_at_ms".into(), self.ended_at_ms.into());
        if let Some(t) = self.input_tokens {
            obj.insert("input_tokens".into(), t.into());
        }
        if let Some(t) = self.output_tokens {
            obj.insert("output_tokens".into(), t.into());
        }
        // The compact name + boundary, exactly as the parent records its own
        // pair (present only when the artifacts are).
        if let Some(t) = &self.transcript {
            insert_compact_boundary(&mut obj, t);
        }
        if let Some(p) = &self.provenance {
            p.insert(&mut obj, self.transcript.as_ref());
        }
        canonical_json(&serde_json::Value::Object(obj))
    }
}

/// One read fact frozen into the shard's `tool_calls.jsonl`: identifiers +
/// hashes only, matching the recall parser's `ToolCallRecord` field aliases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedReadFact {
    /// The tool-call id (`ToolCallRecord`'s `id` alias).
    pub tool_use_id: String,
    /// The tool name (`Read`-shaped).
    pub tool_name: String,
    /// Repo-relative path.
    pub path: String,
    /// SHA-256 (hex) of the bytes at read time.
    pub content_sha256: String,
    /// Token estimate over the read-time bytes, when one was captured
    /// (`ToolCallRecord`'s inline `input_tokens`). `None` omits the field, so
    /// pre-estimate facts freeze byte-identically to their old lines.
    pub input_tokens: Option<u32>,
    /// When the read's tool call was made (epoch ms), the instant the savings
    /// denominator windows a read by. `None` omits the member.
    pub at: Option<i64>,
    /// The ordinal of the turn the read belongs to. `None` omits the member.
    pub turn: Option<i64>,
    /// The git blob oid of the revision read, half of the file revision's
    /// key. Frozen only when it is a 40- or 64-digit lowercase hex oid.
    pub blob_oid: Option<String>,
    /// The tokenizer that counted [`Self::input_tokens`], from the closed
    /// list of tokenizers the projection records.
    pub tokenizer: Option<String>,
}

/// The tokenizer tags a read's token count can carry: `rift-tokens`'s
/// `TOKENIZER` and its `FALLBACK` estimate. Spelled here rather than imported
/// because this crate does not depend on the tokenizer, and the archive needs
/// only the names.
pub(super) const READ_TOKENIZERS: [&str; 2] = ["cl100k_base", "chars/4+1"];

/// Serialize read facts exactly as the shard's `tool_calls.jsonl` freezes them
/// (`None` when empty — the artifact is omitted, never written empty). Shared
/// by the shard writer and the checkpoint-id derivation, which folds these
/// bytes into the id so a session whose fact set GREW mints a NEW checkpoint
/// instead of colliding with its earlier shard.
#[must_use]
pub fn tool_calls_jsonl_bytes(read_facts: &[ArchivedReadFact]) -> Option<Vec<u8>> {
    if read_facts.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for (i, f) in read_facts.iter().enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        let mut line = serde_json::json!({
            "id": f.tool_use_id,
            "tool": f.tool_name,
            "path": f.path,
            "sha256": f.content_sha256,
        });
        // Present only when captured: omission keeps pre-estimate
        // fact sets byte-identical to the lines they froze before.
        if let Some(obj) = line.as_object_mut() {
            insert_read_members(obj, f);
        }
        out.extend_from_slice(&canonical_json(&line));
    }
    out.push(b'\n');
    Some(out)
}

/// The optional members of one `tool_calls.jsonl` line, each written only
/// when the fact carries it, so a fact set without them freezes the lines it
/// froze before they existed. The blob oid and the tokenizer come from
/// projection rows rather than Rift's own grammar, so each is frozen only in
/// its declared shape and is [`WITHHELD`] otherwise.
fn insert_read_members(obj: &mut serde_json::Map<String, serde_json::Value>, f: &ArchivedReadFact) {
    if let Some(tokens) = f.input_tokens {
        obj.insert("input_tokens".to_owned(), serde_json::json!(tokens));
    }
    if let Some(at) = f.at {
        obj.insert("at".to_owned(), serde_json::json!(at));
    }
    if let Some(turn) = f.turn {
        obj.insert("turn".to_owned(), serde_json::json!(turn));
    }
    if let Some(oid) = &f.blob_oid {
        obj.insert(
            "blob_oid".to_owned(),
            serde_json::json!(oid_or_withheld(oid)),
        );
    }
    if let Some(tokenizer) = &f.tokenizer {
        let known = READ_TOKENIZERS.contains(&tokenizer.as_str());
        let frozen = if known { tokenizer.as_str() } else { WITHHELD };
        obj.insert("tokenizer".to_owned(), serde_json::json!(frozen));
    }
}

/// `oid` when it is a git object id (40 or 64 lowercase hex digits), else
/// [`WITHHELD`].
fn oid_or_withheld(oid: &str) -> &str {
    let hex = oid
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if hex && matches!(oid.len(), 40 | 64) {
        return oid;
    }
    WITHHELD
}

/// The line format version every `usage.jsonl` line declares as
/// `schema_version`.
pub const USAGE_SCHEMA_VERSION: u32 = 1;

/// What a `usage.jsonl` member that could carry text other than an
/// identifier is frozen as.
pub(super) const WITHHELD: &str = "withheld";

/// The longest identifier a `usage.jsonl` member freezes as written.
const MAX_IDENTIFIER_LEN: usize = 128;

/// One settled model call frozen into the shard's `usage.jsonl`: the four
/// provider-reported token classes with the counting method, the model and
/// the call's own key. No price and no dollar figure: both are derived at
/// read time from the price catalog, which moves independently of usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedUsageFact {
    /// The agent's native session id, as `metadata.json`'s `sessions` names it.
    pub session: String,
    /// The ordinal of the turn the call belongs to.
    pub turn: i64,
    /// The `model_call` key, frozen so a rebuild re-inserts under it rather
    /// than re-minting it from coordinates the projection does not keep.
    pub call_id: String,
    /// The model the provider reported for the call.
    pub model: String,
    /// When the call's anchoring event occurred (epoch ms).
    pub at: i64,
    /// Uncached input tokens.
    pub uncached_input: i64,
    /// Cached input tokens.
    pub cached_input: i64,
    /// Cache-creation tokens.
    pub cache_creation: i64,
    /// Output tokens.
    pub output: i64,
    /// How the counts were derived (`provider_reported`, …).
    pub method: String,
}

/// Serialize settled usage exactly as the shard's `usage.jsonl` freezes it:
/// one canonical object per line, sorted by `at` then `call_id` whatever the
/// order handed in, so the prediction and the archive fold identical bytes.
/// `None` when empty — the artifact is omitted, never written empty, and a
/// shard without usage keeps the leaf sequence it had before the artifact.
#[must_use]
pub fn usage_jsonl_bytes(usage: &[ArchivedUsageFact]) -> Option<Vec<u8>> {
    if usage.is_empty() {
        return None;
    }
    let mut sorted: Vec<&ArchivedUsageFact> = usage.iter().collect();
    sorted.sort_by(|a, b| (a.at, &a.call_id).cmp(&(b.at, &b.call_id)));
    let mut out = Vec::new();
    for fact in sorted {
        out.extend_from_slice(&canonical_json(&usage_line(fact)));
        out.push(b'\n');
    }
    Some(out)
}

/// One `usage.jsonl` line. The model, the method and the call key come from
/// provider fields and projection rows rather than from Rift's own grammar,
/// so each is frozen only when it reads as an identifier; anything else is
/// [`WITHHELD`], and no member can carry typed text into the shard.
fn usage_line(fact: &ArchivedUsageFact) -> serde_json::Value {
    serde_json::json!({
        "session": fact.session,
        "turn": fact.turn,
        "call_id": identifier_or_withheld(&fact.call_id),
        "model": identifier_or_withheld(&fact.model),
        "at": fact.at,
        "uncached_input": fact.uncached_input,
        "cached_input": fact.cached_input,
        "cache_creation": fact.cache_creation,
        "output": fact.output,
        "method": identifier_or_withheld(&fact.method),
        "schema_version": USAGE_SCHEMA_VERSION,
    })
}

/// `value` when it is a bounded identifier (ASCII letters, digits and the
/// punctuation model ids and counting methods use) or one closed-vocabulary
/// literal, else [`WITHHELD`].
pub(super) fn identifier_or_withheld(value: &str) -> &str {
    // Claude Code reports this model for a synthetic assistant message. It is
    // admitted as the exact literal, not by widening the character class:
    // `<` and `>` stay outside the grammar, so no other bracketed text can
    // reach a shard, and the call's tokens are frozen under its own name
    // instead of a placeholder the rebuild has to skip.
    const SYNTHETIC_MODEL: &str = "<synthetic>";
    if value == SYNTHETIC_MODEL || is_identifier(value) {
        return value;
    }
    WITHHELD
}

/// Whether `value` is a bounded identifier: ASCII letters, digits and the
/// punctuation model ids and counting methods use, at most
/// [`MAX_IDENTIFIER_LEN`] bytes. The empty string is one, vacuously; a caller
/// that needs a name says so itself.
pub(super) fn is_identifier(value: &str) -> bool {
    let is_identifier_char = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(c, '-' | '.' | '_' | ':' | '/' | '@' | '+' | '[' | ']')
    };
    value.len() <= MAX_IDENTIFIER_LEN && value.chars().all(is_identifier_char)
}

/// The line format version every `sessions.jsonl` line declares as
/// `schema_version`.
pub const SESSIONS_SCHEMA_VERSION: u32 = 1;

/// Serialize the archived sessions' agents exactly as the shard's
/// `sessions.jsonl` freezes them: one canonical line per session, in session
/// order, each naming the native session id and its agent's token. `None`
/// when empty — the artifact is omitted, never written empty, and a shard
/// without it keeps the leaf sequence it had before the artifact.
#[must_use]
pub fn sessions_jsonl_bytes(session_agents: &BTreeMap<String, String>) -> Option<Vec<u8>> {
    if session_agents.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for (session, agent) in session_agents {
        let line = serde_json::json!({
            "agent": agent_token_or_withheld(agent),
            "schema_version": SESSIONS_SCHEMA_VERSION,
            "session": session,
        });
        out.extend_from_slice(&canonical_json(&line));
        out.push(b'\n');
    }
    Some(out)
}

/// `agent` when it is one of the first-party agent tokens, else [`WITHHELD`].
/// A closed vocabulary rather than the token's shape: an agent id is an open
/// newtype, and a third-party token is text this shard has no reason to carry.
fn agent_token_or_withheld(agent: &str) -> &str {
    let known = rift_core::AgentId::FIRST_PARTY
        .iter()
        .any(|id| id.as_str() == agent);
    if known {
        return agent;
    }
    WITHHELD
}

/// The counts-only redaction facts the native archive freezes into
/// `redaction_report.json`. A structural mirror of
/// [`rift_shadow_redact::RedactionReport`] carrying what the strict bridge
/// gate needs — it is `is_valid()` iff a rule pack ran and the input address is
/// set — plus the per-rule counts the reader is owed: which
/// detector and which class masked how many spans. The reader side
/// (`RedactionReportFacts`) has always accepted both maps; until this view
/// carried them the archive dropped them on the floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionReportView {
    /// Total spans masked.
    pub total_spans: u32,
    /// Aggregate masked byte length.
    pub bytes_masked: u64,
    /// Spans deliberately kept via the allowlist.
    pub allowlisted: u32,
    /// Exactly which rule packs/versions ran.
    pub rule_pack_versions: Vec<String>,
    /// SHA-256 (hex) of the raw input.
    pub input_sha256: String,
    /// Spans masked per detector id (`vendor.aws.access_key_id`, `pii.email`,
    /// a pack's `<pack>.<rule>`), sorted by id.
    pub by_detector: BTreeMap<String, u32>,
    /// Spans masked per detector class, keyed by the class's own serde
    /// spelling ([`Self::class_key`]) so this view cannot drift from the
    /// report's serialization.
    pub by_class: BTreeMap<String, u32>,
}

impl RedactionReportView {
    /// Project a redaction-pipeline report into the frozen archive view.
    #[must_use]
    pub fn from_redact(report: &rift_shadow_redact::RedactionReport) -> Self {
        Self {
            total_spans: report.total_spans,
            bytes_masked: report.bytes_masked,
            allowlisted: report.allowlisted,
            rule_pack_versions: report.rule_pack_versions.clone(),
            input_sha256: report.input_sha256.to_hex(),
            by_detector: report.by_detector.clone(),
            by_class: report
                .by_class
                .iter()
                .map(|(class, n)| (Self::class_key(*class), *n))
                .collect(),
        }
    }

    /// The `by_class` key for a detector class: exactly the string
    /// `serde_json` serializes the enum as, so the archived spelling is the
    /// report's own and a renamed variant moves both at once.
    #[must_use]
    pub fn class_key(class: rift_shadow_redact::DetectorClass) -> String {
        serde_json::to_value(class)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("{class:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::archive;
    use super::*;

    /// A fact with a token estimate freezes it inline; one without
    /// freezes byte-identically to the pre-estimate line shape (no field).
    #[test]
    fn tool_calls_lines_carry_tokens_only_when_captured() {
        let plain = ArchivedReadFact {
            tool_use_id: "t1".into(),
            tool_name: "Read".into(),
            path: "a.rs".into(),
            content_sha256: "aa".into(),
            input_tokens: None,
            at: None,
            turn: None,
            blob_oid: None,
            tokenizer: None,
        };
        let with_tokens = ArchivedReadFact {
            input_tokens: Some(42),
            ..plain.clone()
        };
        let old = String::from_utf8(tool_calls_jsonl_bytes(&[plain]).unwrap()).unwrap();
        assert_eq!(
            old, "{\"id\":\"t1\",\"path\":\"a.rs\",\"sha256\":\"aa\",\"tool\":\"Read\"}\n",
            "a fact without an estimate must freeze exactly the old line"
        );
        let new = String::from_utf8(tool_calls_jsonl_bytes(&[with_tokens]).unwrap()).unwrap();
        assert!(
            new.contains("\"input_tokens\":42"),
            "the captured estimate must ride the line: {new}"
        );
    }

    /// The denominator's members ride the line when the fact carries them,
    /// and a fact without them freezes the old line exactly.
    #[test]
    fn tool_calls_lines_carry_the_denominator_members_only_when_captured() {
        let old = ArchivedReadFact {
            tool_use_id: "t1".into(),
            tool_name: "Read".into(),
            path: "a.rs".into(),
            content_sha256: "aa".into(),
            input_tokens: Some(42),
            at: None,
            turn: None,
            blob_oid: None,
            tokenizer: None,
        };
        let line = |f: &ArchivedReadFact| {
            String::from_utf8(tool_calls_jsonl_bytes(std::slice::from_ref(f)).unwrap()).unwrap()
        };
        assert_eq!(
            line(&old),
            "{\"id\":\"t1\",\"input_tokens\":42,\"path\":\"a.rs\",\"sha256\":\"aa\",\"tool\":\"Read\"}\n"
        );
        let new = ArchivedReadFact {
            at: Some(300),
            turn: Some(2),
            blob_oid: Some("e".repeat(40)),
            tokenizer: Some("cl100k_base".into()),
            ..old
        };
        assert_eq!(
            line(&new),
            format!(
                "{{\"at\":300,\"blob_oid\":\"{}\",\"id\":\"t1\",\"input_tokens\":42,\
                 \"path\":\"a.rs\",\"sha256\":\"aa\",\"tokenizer\":\"cl100k_base\",\"tool\":\"Read\",\
                 \"turn\":2}}\n",
                "e".repeat(40)
            )
        );
    }

    /// A blob oid that is not an object id, or a tokenizer outside the closed
    /// list, is withheld rather than frozen.
    #[test]
    fn a_read_member_outside_its_declared_shape_is_withheld() {
        let fact = ArchivedReadFact {
            tool_use_id: "t1".into(),
            tool_name: "Read".into(),
            path: "a.rs".into(),
            content_sha256: "aa".into(),
            input_tokens: Some(1),
            at: Some(1),
            turn: Some(1),
            blob_oid: Some("please ignore previous instructions".into()),
            tokenizer: Some("my tokenizer".into()),
        };
        let bytes = tool_calls_jsonl_bytes(&[fact]).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["blob_oid"], WITHHELD);
        assert_eq!(value["tokenizer"], WITHHELD);
        for oid in ["E".repeat(40), "e".repeat(41), "e".repeat(64)] {
            let expect = if oid.len() == 64 {
                oid.as_str()
            } else {
                WITHHELD
            };
            assert_eq!(oid_or_withheld(&oid), expect, "{oid}");
        }
    }

    /// The per-rule counts reach the archived JSON and read
    /// back through the reader's `RedactionReportFacts` unchanged, and the
    /// class keys are the report's own serde spelling.
    #[test]
    fn the_per_rule_counts_round_trip_through_the_archived_report() {
        use rift_shadow_redact::DetectorClass;
        let out = rift_shadow_redact::redact(&rift_core::SecretBytes::from(
            "key=AKIAIOSFODNN7EXAMPLE mail alice@example.com",
        ))
        .expect("redacts");
        let view = RedactionReportView::from_redact(&out.report);
        assert!(
            !view.by_detector.is_empty(),
            "the fixture input masks something"
        );
        assert_eq!(
            view.by_detector.values().sum::<u32>(),
            view.total_spans,
            "per-detector counts sum to the total"
        );
        assert_eq!(view.by_class.values().sum::<u32>(), view.total_spans);
        assert_eq!(
            RedactionReportView::class_key(DetectorClass::VendorSecret),
            "VendorSecret"
        );
        let mut archive = archive();
        archive.report = view.clone();
        let bytes = archive.redaction_report_json();
        let text = String::from_utf8(bytes.clone()).expect("utf-8");
        assert!(
            text.starts_with("{\"allowlisted\":")
                && text.contains("\"by_class\":{")
                && text.contains("\"by_detector\":{"),
            "keys sorted, both maps present: {text}"
        );
        let facts: rift_recall_types::checkpoint::RedactionReportFacts =
            serde_json::from_slice(&bytes).expect("the reader parses the writer's bytes");
        assert_eq!(facts.by_detector, view.by_detector);
        assert_eq!(facts.by_class, view.by_class);
        assert_eq!(facts.total_spans, view.total_spans);
        assert!(facts.is_valid());
    }

    /// A label exactly as this writer produces one: linked, on a branch.
    fn good_label() -> WorktreeLabel {
        WorktreeLabel {
            name: "swt-a".to_owned(),
            token: "0123456789ab".to_owned(),
            linked: true,
            branch: Some("swt/a-foundations".to_owned()),
        }
    }

    /// A `metadata.json` carrying exactly this label as its `worktree` member.
    fn metadata_with(label: &WorktreeLabel) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "worktree": label.to_json() })).unwrap()
    }

    /// The member arrives on untrusted bytes — a shard can come from another
    /// machine in a bundle — and lands in an append-only table nothing can
    /// correct. The writer's own bytes read back whole; a shard that names no
    /// worktree is UNKNOWN, not an error — every shard written before
    /// worktree provenance existed looks exactly like this.
    #[test]
    fn the_writers_own_worktree_member_round_trips_and_an_absent_one_is_unknown() {
        let good = good_label();
        assert_eq!(
            WorktreeLabel::from_metadata_json(&metadata_with(&good)).as_ref(),
            Some(&good),
            "the writer's own bytes round-trip"
        );
        assert_eq!(
            WorktreeLabel::from_metadata_json(br#"{"strategy":"manual-commit"}"#),
            None
        );
        assert_eq!(WorktreeLabel::from_metadata_json(b"not json"), None);
    }

    /// Anything outside the shape this writer produces is refused WHOLE,
    /// never sanitised into a half-trusted row: a token that is not 12
    /// lowercase hex, a name that is empty, nested, unprintable or over the
    /// bound, a branch that is empty, unprintable or over the bound.
    #[test]
    fn a_worktree_member_this_writer_could_not_have_produced_is_refused() {
        let good = good_label();
        for bad in [
            WorktreeLabel {
                token: "nothex000000".to_owned(),
                ..good.clone()
            },
            WorktreeLabel {
                token: "0123456789".to_owned(),
                ..good.clone()
            },
            WorktreeLabel {
                token: "0123456789AB".to_owned(),
                ..good.clone()
            },
            WorktreeLabel {
                name: String::new(),
                ..good.clone()
            },
            WorktreeLabel {
                name: "a/b".to_owned(),
                ..good.clone()
            },
            WorktreeLabel {
                name: "a\nb".to_owned(),
                ..good.clone()
            },
            WorktreeLabel {
                name: "n".repeat(129),
                ..good.clone()
            },
            WorktreeLabel {
                branch: Some("a\u{1b}[31m".to_owned()),
                ..good.clone()
            },
            WorktreeLabel {
                branch: Some("b".repeat(256)),
                ..good.clone()
            },
            WorktreeLabel {
                branch: Some(String::new()),
                ..good.clone()
            },
        ] {
            assert_eq!(
                WorktreeLabel::from_metadata_json(&metadata_with(&bad)),
                None,
                "{bad:?} must not become a row"
            );
        }
    }

    /// A member of the wrong SHAPE is refused too, not read past: a bare
    /// string where the object should be, or an object missing `linked`.
    #[test]
    fn a_worktree_member_of_the_wrong_shape_is_refused() {
        assert_eq!(
            WorktreeLabel::from_metadata_json(br#"{"worktree":"swt-a"}"#),
            None
        );
        assert_eq!(
            WorktreeLabel::from_metadata_json(
                br#"{"worktree":{"name":"a","token":"0123456789ab"}}"#
            ),
            None,
            "`linked` is a fact, not a default"
        );
    }

    /// A fact as the store hands one over: `call` at `at`, on `model`.
    fn usage_fact(call: &str, model: &str, at: i64) -> ArchivedUsageFact {
        ArchivedUsageFact {
            session: "01JSESSION".into(),
            turn: 1,
            call_id: call.into(),
            model: model.into(),
            at,
            uncached_input: 20,
            cached_input: 30,
            cache_creation: 5,
            output: 2,
            method: "provider_reported".into(),
        }
    }

    /// The declared members of a `usage.jsonl` line, and nothing else.
    const USAGE_MEMBERS: [&str; 11] = [
        "at",
        "cache_creation",
        "cached_input",
        "call_id",
        "method",
        "model",
        "output",
        "schema_version",
        "session",
        "turn",
        "uncached_input",
    ];

    #[test]
    fn no_settled_usage_writes_no_usage_artifact() {
        assert_eq!(usage_jsonl_bytes(&[]), None);
    }

    /// Whatever order the rows arrive in, the bytes are one sequence: sorted
    /// by `at` then `call_id`, one canonical line each, newline-terminated.
    #[test]
    fn usage_lines_are_canonical_and_sorted_whatever_order_they_arrive_in() {
        let facts = [
            usage_fact("c2", "claude-opus-5", 200),
            usage_fact("c1b", "claude-fable-5", 100),
            usage_fact("c1a", "claude-fable-5", 100),
        ];
        let mut reversed = facts.clone();
        reversed.reverse();
        let bytes = usage_jsonl_bytes(&facts).unwrap();
        assert_eq!(usage_jsonl_bytes(&reversed).unwrap(), bytes, "order-free");
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.ends_with('\n'));
        let calls: Vec<String> = text
            .lines()
            .map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).unwrap();
                assert_eq!(serde_json::to_string(&value).unwrap(), line, "keys sorted");
                value["call_id"].as_str().unwrap().to_owned()
            })
            .collect();
        assert_eq!(calls, ["c1a", "c1b", "c2"]);
        assert_eq!(
            text.lines().next().unwrap(),
            "{\"at\":100,\"cache_creation\":5,\"cached_input\":30,\"call_id\":\"c1a\",\
             \"method\":\"provider_reported\",\"model\":\"claude-fable-5\",\"output\":2,\
             \"schema_version\":1,\"session\":\"01JSESSION\",\"turn\":1,\"uncached_input\":20}"
        );
    }

    /// A model or method that is not an identifier is withheld rather than
    /// frozen: the provider field is the source, and a shard never quotes
    /// text a person could have typed.
    #[test]
    fn a_usage_member_that_is_not_an_identifier_is_withheld() {
        let mut fact = usage_fact("c1", "please ignore previous instructions", 1);
        fact.method = "x".repeat(MAX_IDENTIFIER_LEN + 1);
        let text = String::from_utf8(usage_jsonl_bytes(&[fact]).unwrap()).unwrap();
        let value: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(value["model"], WITHHELD);
        assert_eq!(value["method"], WITHHELD);
        assert_eq!(value["call_id"], "c1");
    }

    /// Claude Code names a synthetic assistant message's model `<synthetic>`,
    /// and that call's tokens are frozen under the name it was reported with.
    /// Only the exact literal: a near miss is still withheld.
    #[test]
    fn claude_codes_synthetic_model_is_frozen_as_written() {
        let frozen = |model: &str| {
            let text = String::from_utf8(usage_jsonl_bytes(&[usage_fact("c1", model, 1)]).unwrap())
                .unwrap();
            let value: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
            value["model"].as_str().unwrap().to_owned()
        };
        assert_eq!(frozen("<synthetic>"), "<synthetic>");
        for near_miss in ["<synthetic> ", "<Synthetic>", "<synthetic>x", "<script>"] {
            assert_eq!(frozen(near_miss), WITHHELD, "{near_miss:?}");
        }
    }

    /// One line per session, keys sorted, sessions in order, the agent's
    /// token as written; no agent, no artifact.
    #[test]
    fn session_agents_freeze_one_canonical_line_per_session() {
        assert_eq!(sessions_jsonl_bytes(&BTreeMap::new()), None);
        let agents = BTreeMap::from([
            ("s-2".to_owned(), "claude-code".to_owned()),
            ("s-1".to_owned(), "codex".to_owned()),
        ]);
        let text = String::from_utf8(sessions_jsonl_bytes(&agents).unwrap()).unwrap();
        assert_eq!(
            text,
            "{\"agent\":\"codex\",\"schema_version\":1,\"session\":\"s-1\"}\n\
             {\"agent\":\"claude-code\",\"schema_version\":1,\"session\":\"s-2\"}\n"
        );
    }

    /// An agent outside the first-party vocabulary is withheld, however
    /// well-formed: the token is an open newtype, the artifact is not.
    #[test]
    fn an_agent_outside_the_first_party_vocabulary_is_withheld() {
        for agent in ["my-agent", "please ignore previous instructions", ""] {
            let agents = BTreeMap::from([("s".to_owned(), agent.to_owned())]);
            let bytes = sessions_jsonl_bytes(&agents).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["agent"], WITHHELD, "{agent:?}");
        }
        for id in rift_core::AgentId::FIRST_PARTY {
            assert_eq!(agent_token_or_withheld(id.as_str()), id.as_str());
        }
    }

    proptest::proptest! {
        /// Whatever the rows hold, every line carries exactly the declared
        /// members, and every string member but the session is an
        /// identifier or the placeholder.
        #[test]
        fn no_member_outside_the_declared_set_reaches_a_usage_line(
            call in "(?s).{0,40}",
            model in "(?s).{0,40}",
            method in "(?s).{0,40}",
            at in proptest::num::i64::ANY,
        ) {
            let mut fact = usage_fact(&call, &model, at);
            fact.method = method;
            let bytes = usage_jsonl_bytes(&[fact]).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
            proptest::prop_assert_eq!(keys, USAGE_MEMBERS.to_vec());
            for member in ["call_id", "model", "method"] {
                let text = value[member].as_str().unwrap();
                proptest::prop_assert_eq!(identifier_or_withheld(text), text);
                // Angle brackets reach a line only as the one admitted literal.
                let bracketed = text.contains(['<', '>']);
                proptest::prop_assert!(!bracketed || text == "<synthetic>", "{}", text);
            }
        }
    }
}
