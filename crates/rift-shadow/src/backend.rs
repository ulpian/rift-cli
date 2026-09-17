//! The write-side seam the checkpoint archive is built on.
//!
//! All of `rift-shadow`'s write-side capability is expressed as one trait,
//! [`ShadowBackend`], so callers (condense/executor) name a capability rather
//! than an implementation. [`NativeBackend`] is the implementation: it writes a
//! checkpoint shard onto the orphan `rift/checkpoints/v1` branch through
//! screened git plumbing, and it is the only one there is.
//!
//! **The strangler seam is gone because the strangler finished.** This module
//! used to carry a second backend that shelled out to a pinned foreign binary,
//! a per-capability `RouteTable` read from `.rift/rewrite.toml`, and a
//! kill-switch that forced every capability back to it. The native archive was
//! promoted to the default on 2026-08-10 and soaked clean; the
//! wrapper, its routes and its rollback dial were deleted on 2026-08-11. What
//! survives is what the routing was *for*: one seam, one implementation behind
//! it, and callers that never named either.
//!
//! Hashing, attribution, cost and redaction were always native — they never had
//! a foreign equivalent — so the git write was the last capability that had a
//! second answer, and now it does not.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rift_core::{RedactedBytes, Result, Sha256Id};
use rift_git::{
    GitFileMode, GitIdent, MemoryGitWrite, NativeGitWrite, OrphanCommit, TreeWriteEntry,
};

use crate::sha256_hex;

/// The fully-qualified orphan checkpoint branch the native archive writes
/// (`Dialect::Rift.checkpoint_ref()` on the read side — the LAYOUT contract).
pub const NATIVE_CHECKPOINT_REF: &str = "refs/heads/rift/checkpoints/v1";

/// The compact-transcript artifact's shard name — also the value
/// `metadata.json` records under `compact_transcript` (the "compact path
/// recorded in metadata" rule). MUST sort bytewise after `tool_calls.jsonl` and
/// before [`TRANSCRIPT_FULL_NAME`]: new artifacts are optional trailing
/// Merkle leaves, and a name sorting earlier would rewrite every existing
/// shard's leaf sequence (see [`shard_merkle_leaves`]).
pub const TRANSCRIPT_COMPACT_NAME: &str = "transcript.compact.jsonl";

/// The full-transcript artifact's shard name (see [`TRANSCRIPT_COMPACT_NAME`]
/// for the ordering constraint).
pub const TRANSCRIPT_FULL_NAME: &str = "transcript.jsonl";

/// The redaction/canonicalization version the native archive stamps into every
/// `metadata.json` (`canon_version`). This is the redaction→leaf migration
/// decision: rather than reproduce the legacy backend's exact redacted
/// bytes for pre-rewrite checkpoints, the canonicalization is **versioned**.
/// `rift verify` selects the leaf canonicalization by the checkpoint's version,
/// so promoting a derivation never breaks `verify` on old checkpoints — the
/// epochs coexist instead of one rewriting the other.
///
/// The epochs, in order:
///
/// * **0** — a pre-rewrite (legacy backend) checkpoint: no `canon_version` field at all.
/// * **1** — Rift-native redaction, and a `merkle_root` that was
///   [`rift_hashchain::empty_root`] on **every** shard. Nothing threaded an
///   event stream into [`CheckpointArchive::events`], so the root was taken over
///   the empty leaf set and 100+ archived shards published the identical
///   constant. It committed to nothing; that is the vacuous era.
/// * **2** — the root is DERIVED from the shard's own leaves
///   ([`shard_merkle_root`]) and cannot be supplied by a caller, so the value in
///   `metadata.json` is a claim about these bytes that `rift verify` re-derives.
///   The checkpoint id still bound only `content.redacted` and
///   `tool_calls.jsonl`, so any other artifact — `full.jsonl` then, a transcript
///   next — sat outside the shard's path.
/// * **3** — the checkpoint id is derived from that root
///   ([`rift_shadow::condense::derive_checkpoint_id`](crate::condense::derive_checkpoint_id)),
///   so the shard TREE is a function of the shard PATH and every content
///   artifact is bound by both. See [`ROOT_BOUND_ID_MIN_CANON_VERSION`].
///
/// The era is read off the shard itself, never off a date: see
/// [`STRICT_MERKLE_MIN_CANON_VERSION`].
pub const NATIVE_CANON_VERSION: u32 = 3;

/// The first `canon_version` whose recorded `merkle_root` is a verifiable claim,
/// and therefore the first that `rift verify` checks strictly.
///
/// Below it lies the **vacuous era** (epochs 0 and 1): those shards recorded
/// [`rift_hashchain::empty_root`] regardless of content, so re-deriving a root
/// for them and demanding a match would redden every repository's whole history
/// for a defect the history could not have avoided. They are accepted and
/// counted instead — reported, not hidden.
///
/// This gates only the derivation added with epoch 2. Every check that ran
/// before it (the `redaction_report.json` pass, the `merkle_leaf.txt` recompute
/// of the legacy dialect) still runs on every shard of every era: an era split
/// that silenced an existing check would be a hole wearing a migration's name.
pub const STRICT_MERKLE_MIN_CANON_VERSION: u32 = 2;

/// The first `canon_version` whose **checkpoint id** is derived from the shard's
/// Merkle root, and therefore the first for which the shard's path binds every
/// content artifact rather than two of them.
///
/// It is a separate constant from [`STRICT_MERKLE_MIN_CANON_VERSION`] because the
/// two are separate claims and the era split has to be checkable one claim at a
/// time: epoch 2 derives its root but not its id, so it is strict about the root
/// and permissive about the id. `rift verify` gates the accepted id set on this,
/// and the gate is asymmetric on purpose — see
/// [`checkpoint_ids_for_shard`](crate::condense::checkpoint_ids_for_shard).
///
/// **The leaf spec did not change with this epoch, and that is the point.** A
/// shard carrying no artifact beyond epoch 2's derives the same root at either
/// epoch, because new artifacts are optional trailing leaves. Only the ID rule
/// moved, so a reader that already re-derives roots needs one new rule, not a new
/// leaf order.
pub const ROOT_BOUND_ID_MIN_CANON_VERSION: u32 = 3;

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
/// [`NativeGitWrite::write_durable_blob`]), and the structural artifacts are
/// Rift-authored from counts/ids/content-addresses that never touched a secret.
///
/// No `Debug` derive: `content` is [`RedactedBytes`], which withholds `Debug` on
/// purpose so redacted bytes never reach a log line.
#[derive(Clone)]
pub struct CheckpointArchive {
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
            obj.insert("compact_transcript".into(), TRANSCRIPT_COMPACT_NAME.into());
            obj.insert("compact_transcript_start".into(), t.compact_start.into());
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
}

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
        if let (Some(tokens), Some(obj)) = (f.input_tokens, line.as_object_mut()) {
            obj.insert("input_tokens".to_owned(), serde_json::json!(tokens));
        }
        out.extend_from_slice(&canonical_json(&line));
    }
    out.push(b'\n');
    Some(out)
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

/// The shard bytes a checkpoint's Merkle leaves are derived from.
///
/// **The one input type both sides build.** The writer fills it with the bytes
/// it is about to commit; `rift verify` fills it with the bytes it reads back
/// out of the shard. The fields are named rather than positional precisely so
/// the two sides cannot quietly feed the same function a different set in a
/// different order — the failure this program has now paid for three times is
/// two writers of one fact, not a wrong hash function.
#[derive(Debug, Clone, Copy, Default)]
pub struct ShardLeafBytes<'a> {
    /// `content.redacted` — the redacted capture. Always present; may be empty
    /// (an empty capture still contributes a leaf, and `leaf_hash(&[])` is a
    /// real digest, not the empty-tree root).
    pub content_redacted: &'a [u8],
    /// `full.jsonl` — the archived event stream, or `None` when the shard is
    /// header-only.
    pub full_jsonl: Option<&'a [u8]>,
    /// `redaction_report.json` — the counts-only redaction facts. Always present.
    pub redaction_report: &'a [u8],
    /// `tool_calls.jsonl` — the archived read facts, or `None` when the session
    /// captured none.
    pub tool_calls: Option<&'a [u8]>,
    /// `transcript.compact.jsonl` — the redacted deterministic compaction, or
    /// `None` when no transcript reached the archive.
    pub transcript_compact: Option<&'a [u8]>,
    /// `transcript.jsonl` — the redacted full transcript, or `None` alongside
    /// [`Self::transcript_compact`]: the two travel together.
    pub transcript_full: Option<&'a [u8]>,
    /// The `tasks/<tool-use-id>/` subtrees' artifact bytes, or empty when the
    /// checkpoint carries no subagent task. Order here is
    /// irrelevant: [`shard_merkle_leaves`] orders by tool-use id itself, so
    /// the writer and the verifier cannot feed one function two orders.
    pub tasks: &'a [TaskLeafBytes<'a>],
    /// The `sessions/<index>/metadata.json` bytes of a **multi-session**
    /// checkpoint, or empty for the single-session shape —
    /// a single-session shard writes NO session dirs, so every existing
    /// shard's leaf sequence is untouched. Order here is irrelevant:
    /// [`shard_merkle_leaves`] orders by the decimal index name itself.
    pub session_metas: &'a [SessionMetaLeaf<'a>],
    /// The condense-time digest artifacts, each `None`
    /// when the shard does not carry it — absence is the compatibility
    /// contract, as everywhere in this struct.
    pub digests: DigestLeafBytes<'a>,
}

/// One per-session metadata document of a multi-session checkpoint — the
/// `sessions/<index>/metadata.json` slice of [`ShardLeafBytes`].
#[derive(Debug, Clone, Copy)]
pub struct SessionMetaLeaf<'a> {
    /// The decimal index-dir name (`"0"`, `"1"`, …) — the leaf *order* key
    /// (bytewise, matching git's tree order: `"10"` sorts before `"2"`).
    pub index_name: &'a str,
    /// `sessions/<index>/metadata.json` ([`session_meta_json`]).
    pub metadata: &'a [u8],
}

/// The four digest artifacts' bytes — a sub-struct so a
/// pre-digest call site names one field, not four. Leaf order: the `digest.*`
/// names sort between `content_hash.txt` (not a leaf) and `full.jsonl`, so
/// these are the leaves immediately after `content.redacted`, in the field
/// order below (`failures` < `intent` < `log` < `shape`, their bytewise
/// name order).
#[derive(Debug, Clone, Copy, Default)]
pub struct DigestLeafBytes<'a> {
    /// `digest.failures.json` — mechanical failure signatures.
    pub failures: Option<&'a [u8]>,
    /// `digest.intent.json` — the model-free intent/outcome slot.
    pub intent: Option<&'a [u8]>,
    /// `digest.log.json` — the log-shape digest.
    pub log: Option<&'a [u8]>,
    /// `digest.shape.json` — the file-shape digest.
    pub shape: Option<&'a [u8]>,
}

/// One task subtree's leaf bytes — the `tasks/<tool-use-id>/` slice of
/// [`ShardLeafBytes`], built by the writer from what it is about to commit and
/// by `rift verify` from what it read back (task ids come from the parent
/// `metadata.json`'s `tasks` list; a listed-but-missing artifact reads as
/// empty bytes, which cannot reproduce the writer's leaf, so the root — and
/// therefore the id — refuses for free).
#[derive(Debug, Clone, Copy)]
pub struct TaskLeafBytes<'a> {
    /// The subtree name — the leaf *order* key, never leaf content.
    pub tool_use_id: &'a str,
    /// `tasks/<id>/metadata.json` — always present ([`ArchivedTask::metadata_json`]).
    pub metadata: &'a [u8],
    /// `tasks/<id>/transcript.compact.jsonl`, when the task carries its pair.
    pub transcript_compact: Option<&'a [u8]>,
    /// `tasks/<id>/transcript.jsonl`, alongside the compact half.
    pub transcript_full: Option<&'a [u8]>,
}

/// A checkpoint's ordered Merkle leaves: **the shard's content artifacts in
/// canonical (git-tree) name order, with `full.jsonl` expanded to one leaf per
/// archived event line.**
///
/// So the sequence is `content.redacted`, then each `full.jsonl` line, then
/// `redaction_report.json`, then each `tasks/<tool-use-id>/` subtree's
/// artifacts (`metadata.json`, then the transcript pair when present — the
/// task dir's own bytewise name order — tasks ordered by tool-use id), then
/// `tool_calls.jsonl`, then `transcript.compact.jsonl`, then
/// `transcript.jsonl` — which is the artifacts' bytewise **full-path** order
/// (`tasks/…` sits between `redaction_report.json` and `tool_calls.jsonl`
/// because `'t','a' < 't','o'`), the same order
/// `NativeBackend::write_shard` sorts the tree into. One rule, and it makes
/// "the checkpoint's event leaves" literally true the moment an event stream is
/// threaded through condense: today a shard carries no events and the leaf set
/// is its other artifacts; tomorrow the events are simply more leaves in the
/// same sequence.
///
/// **New artifacts are optional leaves, and ABSENCE is the compatibility
/// contract** — a shard that does not carry the artifact derives, leaf for
/// leaf, the sequence it derived before the artifact existed, so its root and
/// its root-bound id never move. That is why neither the transcript pair (a
/// trailing pair) nor the task subtrees (a MIDDLE insertion — the honest
/// statement, since `tasks/` does not sort last) needed an era bump or a new
/// id rule: the sequence only ever changes for shards that actually carry the
/// new artifact, and those are new shards by construction. What a future
/// artifact's name must still respect is the one rule above — leaves sit in
/// bytewise full-path order, so pick the name knowing where it lands.
///
/// **Two artifacts are deliberately excluded.** `metadata.json` carries the root
/// itself, so a root over it is not computable; `content_hash.txt` is a digest
/// over a SEQUENCE that includes `metadata.json` — see
/// [`CheckpointArchive::content_hash_txt`], which digests
/// `metadata.json ‖ redaction_report.json ‖ full.jsonl` (the last only when the
/// shard carries one) — and inherits that circularity. The single-artifact
/// reading of that sentence was false, and was inherited by a published format
/// registry before anyone measured it: `sha256(metadata.json)` alone matches
/// **zero** live shards. `content_hash_covers_metadata_report_and_full_in_that_order`
/// pins the sequence by arithmetic so the two descriptions cannot drift again.
///
/// Blank segments are dropped, so a `full.jsonl` written with a trailing newline
/// and one written without produce the same leaves.
///
/// Order is part of the commitment (see [`rift_hashchain::merkle_root`]), so two
/// artifacts swapping bytes changes the root even though the leaf multiset does
/// not. That is an argument about this construction, not a guarantee about an
/// adversary: what is *tested* is that changing any single artifact's bytes
/// changes the root.
#[must_use]
pub fn shard_merkle_leaves(bytes: &ShardLeafBytes<'_>) -> Vec<Sha256Id> {
    let mut leaves = vec![rift_hashchain::leaf_hash(bytes.content_redacted)];
    // The digest artifacts: `digest.*` sorts after
    // `content_hash.txt` (excluded) and before `full.jsonl`, so their leaves
    // come here, in bytewise name order — failures, intent, log, shape.
    for digest in [
        bytes.digests.failures,
        bytes.digests.intent,
        bytes.digests.log,
        bytes.digests.shape,
    ]
    .into_iter()
    .flatten()
    {
        leaves.push(rift_hashchain::leaf_hash(digest));
    }
    if let Some(full) = bytes.full_jsonl {
        leaves.extend(
            full.split(|b| *b == b'\n')
                .filter(|line| !line.is_empty())
                .map(rift_hashchain::leaf_hash),
        );
    }
    leaves.push(rift_hashchain::leaf_hash(bytes.redaction_report));
    // The per-session metadata of a multi-session checkpoint, in bytewise
    // index-name order (`sessions/` sorts between `redaction_report.json`
    // and `tasks/`; within it git orders decimal names bytewise — "10"
    // before "2" — and the leaf order matches the tree, not the index).
    let mut session_metas: Vec<&SessionMetaLeaf<'_>> = bytes.session_metas.iter().collect();
    session_metas.sort_by(|a, b| a.index_name.as_bytes().cmp(b.index_name.as_bytes()));
    for m in session_metas {
        leaves.push(rift_hashchain::leaf_hash(m.metadata));
    }
    // The task subtrees, in bytewise tool-use-id order — ordered HERE, so the
    // writer and the verifier cannot hand one function two orders. The order
    // key is not itself a leaf, but it is not unbound either: every task's
    // metadata.json (a leaf) carries its own `tool_use_id`, so a renamed
    // subtree is a name/bytes disagreement `rift verify` flags rather than a
    // byte change the root would miss.
    let mut tasks: Vec<&TaskLeafBytes<'_>> = bytes.tasks.iter().collect();
    tasks.sort_by(|a, b| a.tool_use_id.as_bytes().cmp(b.tool_use_id.as_bytes()));
    for t in tasks {
        leaves.push(rift_hashchain::leaf_hash(t.metadata));
        if let Some(compact) = t.transcript_compact {
            leaves.push(rift_hashchain::leaf_hash(compact));
        }
        if let Some(full_transcript) = t.transcript_full {
            leaves.push(rift_hashchain::leaf_hash(full_transcript));
        }
    }
    if let Some(tool_calls) = bytes.tool_calls {
        leaves.push(rift_hashchain::leaf_hash(tool_calls));
    }
    if let Some(compact) = bytes.transcript_compact {
        leaves.push(rift_hashchain::leaf_hash(compact));
    }
    if let Some(full_transcript) = bytes.transcript_full {
        leaves.push(rift_hashchain::leaf_hash(full_transcript));
    }
    leaves
}

/// The Merkle root over [`shard_merkle_leaves`] — the value a
/// `canon_version >= `[`STRICT_MERKLE_MIN_CANON_VERSION`] shard records in
/// `metadata.json` and `rift verify` re-derives.
#[must_use]
pub fn shard_merkle_root(bytes: &ShardLeafBytes<'_>) -> Sha256Id {
    rift_hashchain::merkle_root(&shard_merkle_leaves(bytes))
}

impl CheckpointArchive {
    /// This shard's Merkle root — **derived from the bytes, never supplied.**
    ///
    /// It used to be a field, and every production caller filled it with
    /// `checkpoint_merkle_root(&[])` because no event stream was threaded: one
    /// constant across every archived shard, so the recorded root committed to
    /// nothing and `rift verify` had nothing to catch. A method that reads the
    /// archive's own artifact bytes is why no caller can record a root that
    /// disagrees with what it wrote — the disagreement is not reachable rather
    /// than merely tested for.
    #[must_use]
    pub fn merkle_root(&self) -> Sha256Id {
        let full = self.full_jsonl();
        let report = self.redaction_report_json();
        let tool_calls = self.tool_calls_jsonl();
        let digest_failures = self.digest_failures_json();
        let digest_intent = self.digest_intent_json();
        let digest_log = self.digest_log_json();
        let digest_shape = self.digest_shape_json();
        // Session/task metadata are rendered by the same functions
        // `write_shard` commits, so the root's leaves and the tree's bytes
        // cannot drift apart.
        let session_meta_docs = self.session_meta_docs();
        let session_meta_leaves: Vec<SessionMetaLeaf<'_>> = session_meta_docs
            .iter()
            .map(|(name, bytes)| SessionMetaLeaf {
                index_name: name,
                metadata: bytes,
            })
            .collect();
        let task_metas: Vec<Vec<u8>> = self.tasks.iter().map(ArchivedTask::metadata_json).collect();
        let task_leaves: Vec<TaskLeafBytes<'_>> = self
            .tasks
            .iter()
            .zip(&task_metas)
            .map(|(t, meta)| TaskLeafBytes {
                tool_use_id: &t.tool_use_id,
                metadata: meta,
                transcript_compact: t.transcript.as_ref().map(|tr| tr.compact.as_slice()),
                transcript_full: t.transcript.as_ref().map(|tr| tr.full.as_slice()),
            })
            .collect();
        shard_merkle_root(&ShardLeafBytes {
            content_redacted: self.content.as_slice(),
            full_jsonl: full.as_deref(),
            redaction_report: &report,
            tool_calls: tool_calls.as_deref(),
            transcript_compact: self.transcript.as_ref().map(|t| t.compact.as_slice()),
            transcript_full: self.transcript.as_ref().map(|t| t.full.as_slice()),
            tasks: &task_leaves,
            session_metas: &session_meta_leaves,
            digests: DigestLeafBytes {
                failures: digest_failures.as_deref(),
                intent: digest_intent.as_deref(),
                log: digest_log.as_deref(),
                shape: digest_shape.as_deref(),
            },
        })
    }

    /// The `(index_name, bytes)` pairs of this checkpoint's
    /// `sessions/<index>/metadata.json` documents — **empty below two
    /// sessions**, which is the compatibility contract: a single-session
    /// shard writes no session dirs and its tree, leaves, root and id are
    /// exactly what they always were.
    #[must_use]
    pub fn session_meta_docs(&self) -> Vec<(String, Vec<u8>)> {
        if self.sessions.len() < 2 {
            return Vec::new();
        }
        self.sessions
            .iter()
            .enumerate()
            .map(|(i, sid)| (i.to_string(), session_meta_json(i, sid)))
            .collect()
    }

    /// **The checkpoint id these artifacts address** — the single writer of a new
    /// id, derived from this archive's own Merkle root.
    ///
    /// It is a method on the archive rather than a function beside it so the id
    /// and the bytes cannot be assembled from two different field sets: whatever
    /// [`Self::merkle_root`] commits to is exactly what the id commits to,
    /// because it is the same call.
    ///
    /// `None` for a checkpoint whose session set the rule refuses: an empty
    /// list, or a session id carrying NUL (the frame byte). N ≥ 2 is DEFINED
    /// — [`rift_core::checkpoint_id::from_root_sessions`],
    /// whose N = 1 arm is byte-identical to the shipped epoch-3 rule, which is
    /// why defining it needed no era bump.
    #[must_use]
    pub fn derived_checkpoint_id(&self) -> Option<String> {
        let sessions: Vec<&str> = self.sessions.iter().map(String::as_str).collect();
        rift_core::checkpoint_id::from_root_sessions(&sessions, self.merkle_root())
    }

    /// **The task-token rollup**: the sum of every archived
    /// task's agent-reported token counts, or `None` when no task carries one —
    /// so a token-less task set adds no metadata field and a taskless shard's
    /// metadata is untouched.
    ///
    /// One writer: `metadata_json` records exactly this value, and `rift
    /// verify` re-sums the task subtrees' own metadata (Merkle leaves) against
    /// the recorded field — so a rollup that lies about the leaves it sums is
    /// tamper-evident even though the parent metadata itself is not a leaf.
    ///
    /// Deliberately NOT gated on when a task started: a subagent that began
    /// before this checkpoint's boundary still rolls up to the checkpoint that
    /// saw it finish. The pin for that clause lives in `condense::tests`.
    #[must_use]
    pub fn task_token_rollup(&self) -> Option<(u64, u64)> {
        if self
            .tasks
            .iter()
            .all(|t| t.input_tokens.is_none() && t.output_tokens.is_none())
        {
            return None;
        }
        Some(self.tasks.iter().fold((0u64, 0u64), |(i, o), t| {
            (
                i.saturating_add(t.input_tokens.unwrap_or(0)),
                o.saturating_add(t.output_tokens.unwrap_or(0)),
            )
        }))
    }

    /// The `<id[:2]>/<id[2:]>` shard path this checkpoint lands at (LAYOUT).
    #[must_use]
    pub fn shard_path(&self) -> String {
        let id = &self.checkpoint_id;
        match (id.get(..2), id.get(2..)) {
            (Some(hi), Some(rest)) => format!("{hi}/{rest}"),
            _ => id.clone(),
        }
    }

    /// The canonical `metadata.json` bytes (deterministic key order). Carries the
    /// `canon_version` epoch ([`NATIVE_CANON_VERSION`]) so the redaction→leaf
    /// migration is explicit: old (legacy backend) checkpoints lack it and read as epoch 0,
    /// and the vacuous-Merkle era is epoch 1.
    #[must_use]
    pub fn metadata_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert("canon_version".into(), NATIVE_CANON_VERSION.into());
        obj.insert("strategy".into(), self.strategy.clone().into());
        obj.insert("merkle_root".into(), self.merkle_root().to_hex().into());
        // The capture event's own recorded time. Present only when the
        // archive carries one, so a time-less archive's metadata — including
        // every committed anchor and golden — is byte-identical to what it
        // always was (the same absent-when-empty rule as every field below).
        // Written in exactly the shape `created_at_for`'s lenient parse reads
        // back, so the projection's `checkpoint.created_at` stops being the
        // sentinel on native shards and the squash tie-break
        // ranks by real creation time instead of degrading to trailer order.
        // Not a Merkle leaf (`metadata.json` never is — it RECORDS the root),
        // but inside `content_hash.txt`'s preimage like the rest of this
        // document. No `canon_version` bump: the field changes no id
        // derivation and no verification rule, and the reader has parsed it
        // leniently since the bridge landed.
        if let Some(ms) = self.created_at_ms.filter(|ms| *ms > 0) {
            obj.insert("created_at".into(), rfc3339_utc_ms(ms).into());
        }
        if let Some(prev) = &self.prev_hash {
            obj.insert("prev_hash".into(), prev.clone().into());
        }
        // The compact path + boundary metadata is required to carry. Present
        // only when the shard carries the artifacts, so a transcript-less
        // shard's metadata is byte-identical to what it was before the pair
        // existed (the anchors' compatibility claim).
        if let Some(t) = &self.transcript {
            obj.insert("compact_transcript".into(), TRANSCRIPT_COMPACT_NAME.into());
            obj.insert("compact_transcript_start".into(), t.compact_start.into());
        }
        // The task list — the reader's ENUMERATION of `tasks/<id>/` subtrees
        // (`SubstrateReader` has no tree listing, and does not need one: hiding
        // a listed task changes the leaves `rift verify` re-derives, so the
        // root and the id both refuse). Bytewise id order = leaf order = tree
        // order — one order, three places. Absent when no task, so a taskless
        // shard's metadata is byte-identical to what it always was.
        if !self.tasks.is_empty() {
            let mut ids: Vec<&str> = self.tasks.iter().map(|t| t.tool_use_id.as_str()).collect();
            ids.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            obj.insert("tasks".into(), serde_json::Value::from(ids));
        }
        // The rollup — the parent-side sum of the tasks' own (leaf-bound)
        // token counts, present only when at least one task reported any.
        if let Some((input, output)) = self.task_token_rollup() {
            obj.insert(
                "task_tokens".into(),
                serde_json::json!({"input_tokens": input, "output_tokens": output}),
            );
        }
        obj.insert(
            "sessions".into(),
            serde_json::Value::from(self.sessions.clone()),
        );
        obj.insert("commit".into(), self.commit.clone().into());
        // WHICH WORKTREE ARCHIVED THIS SHARD. Name, token,
        // linked-ness and branch — no absolute path and no username, because
        // this document crosses machines inside a transfer bundle. Present
        // only when the archiving worktree is known, so a label-less archive's
        // metadata — every committed anchor and golden among them — is
        // byte-identical to what it always was.
        //
        // Not a Merkle leaf (`metadata.json` never is — it RECORDS the root),
        // but inside `content_hash.txt`'s preimage like the rest of this
        // document. **No `canon_version` bump**: the member changes no id
        // derivation and no verification rule — the same reasoning, in the
        // same words, that `created_at` above was added under.
        if let Some(label) = &self.worktree {
            obj.insert("worktree".into(), label.to_json());
        }
        if !self.session_worktrees.is_empty() {
            obj.insert(
                "session_worktrees".into(),
                serde_json::Value::Object(
                    self.session_worktrees
                        .iter()
                        .map(|(session, label)| (session.clone(), label.to_json()))
                        .collect(),
                ),
            );
        }
        canonical_json(&serde_json::Value::Object(obj))
    }

    /// The canonical `redaction_report.json` bytes (valid for the strict bridge gate).
    #[must_use]
    pub fn redaction_report_json(&self) -> Vec<u8> {
        let r = &self.report;
        let obj = serde_json::json!({
            "total_spans": r.total_spans,
            "bytes_masked": r.bytes_masked,
            "allowlisted": r.allowlisted,
            "rule_pack_versions": r.rule_pack_versions,
            "input_sha256": r.input_sha256,
            "by_detector": r.by_detector,
            "by_class": r.by_class,
        });
        canonical_json(&obj)
    }

    /// Render `tool_calls.jsonl` — one line per captured read fact, in the
    /// field spelling the recall parser's `ToolCallRecord` aliases accept.
    /// `None` when the session produced no read facts (the artifact is omitted,
    /// never written empty).
    #[must_use]
    pub fn tool_calls_jsonl(&self) -> Option<Vec<u8>> {
        tool_calls_jsonl_bytes(&self.read_facts)
    }

    /// The `full.jsonl` bytes (one canonical event per line), or `None` if empty.
    #[must_use]
    pub fn full_jsonl(&self) -> Option<Vec<u8>> {
        if self.events.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        for (i, e) in self.events.iter().enumerate() {
            if i > 0 {
                out.push(b'\n');
            }
            let line = serde_json::json!({
                "session": e.session,
                "turn": e.turn_ordinal,
                "event": e.event_ordinal,
                "kind": e.kind,
                "verb": e.verb,
                "agent": e.agent,
                "occurred_at_ms": e.occurred_at_ms,
                "payload_sha256": e.payload_sha256,
                "payload": e.payload,
            });
            out.extend_from_slice(&canonical_json(&line));
        }
        Some(out)
    }

    /// `digest.failures.json`, or `None` without a
    /// transcript or without a failure — one derivation shared by condense,
    /// the trailer prediction and the shard writer, because it is a method of
    /// the same archive all three hold.
    #[must_use]
    pub fn digest_failures_json(&self) -> Option<Vec<u8>> {
        self.transcript
            .as_ref()
            .and_then(|t| crate::digest::failures_json(t.full.as_slice()))
    }

    /// `digest.intent.json` — present whenever the shard carries a
    /// transcript: the intent/outcome slot, filled by the redacted prompt's
    /// LOCATOR plus mechanical outcome facts, no model.
    #[must_use]
    pub fn digest_intent_json(&self) -> Option<Vec<u8>> {
        self.transcript
            .as_ref()
            .map(|t| crate::digest::intent_json(t.full.as_slice(), self.read_facts.len()))
    }

    /// `digest.log.json` — present whenever the shard carries a transcript.
    #[must_use]
    pub fn digest_log_json(&self) -> Option<Vec<u8>> {
        self.transcript.as_ref().map(|t| {
            crate::digest::log_json(
                self.content.as_slice().len(),
                t.full.as_slice(),
                t.compact.as_slice(),
                t.compact_start,
            )
        })
    }

    /// `digest.shape.json` — present whenever the session captured read facts.
    #[must_use]
    pub fn digest_shape_json(&self) -> Option<Vec<u8>> {
        crate::digest::shape_json(&self.read_facts)
    }

    /// **Every artifact this checkpoint writes, keyed by its shard-relative
    /// path** — the same set, the same bytes and the same paths
    /// `NativeBackend::write_shard` commits to the tree.
    ///
    /// It exists because the artifact NAME SET was unenumerated. The parity
    /// anchors compare four names, so `content.redacted`, the transcript pair,
    /// the four `digest.*` documents, the `tasks/<id>/` subtrees and the
    /// `sessions/<index>/` metadata could be added, changed or **planted**
    /// without moving a compared byte. [`crate::artifact_registry`] declares
    /// which names may exist; this says which ones a given shard carries; and
    /// `the_committed_shard_tree_is_exactly_the_declared_artifact_set`
    /// (`tests/native_archive_roundtrip.rs`) binds both to what git actually
    /// holds, so this cannot drift from `write_shard` unnoticed.
    ///
    /// The durable artifacts' bytes are COPIED out of their [`RedactedBytes`].
    /// That is sound because this is a READ surface — goldens, oracles,
    /// assertions — and never a write path: `write_shard` still hands the typed
    /// value to `write_durable_blob`, which is the only door those bytes fit
    /// through, and nothing here can mint a [`RedactedBytes`].
    #[must_use]
    pub fn artifact_map(&self) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        out.insert(
            "content.redacted".to_owned(),
            self.content.as_slice().to_vec(),
        );
        out.insert("content_hash.txt".to_owned(), self.content_hash_txt());
        out.insert("metadata.json".to_owned(), self.metadata_json());
        out.insert(
            "redaction_report.json".to_owned(),
            self.redaction_report_json(),
        );
        if let Some(full) = self.full_jsonl() {
            out.insert("full.jsonl".to_owned(), full);
        }
        if let Some(tool_calls) = self.tool_calls_jsonl() {
            out.insert("tool_calls.jsonl".to_owned(), tool_calls);
        }
        if let Some(t) = &self.transcript {
            out.insert(
                TRANSCRIPT_COMPACT_NAME.to_owned(),
                t.compact.as_slice().to_vec(),
            );
            out.insert(TRANSCRIPT_FULL_NAME.to_owned(), t.full.as_slice().to_vec());
        }
        for (name, bytes) in [
            (
                crate::digest::DIGEST_FAILURES_NAME,
                self.digest_failures_json(),
            ),
            (crate::digest::DIGEST_INTENT_NAME, self.digest_intent_json()),
            (crate::digest::DIGEST_LOG_NAME, self.digest_log_json()),
            (crate::digest::DIGEST_SHAPE_NAME, self.digest_shape_json()),
        ] {
            if let Some(bytes) = bytes {
                out.insert(name.to_owned(), bytes);
            }
        }
        for (index_name, bytes) in self.session_meta_docs() {
            out.insert(format!("sessions/{index_name}/metadata.json"), bytes);
        }
        for task in &self.tasks {
            let id = &task.tool_use_id;
            out.insert(format!("tasks/{id}/metadata.json"), task.metadata_json());
            if let Some(t) = &task.transcript {
                out.insert(
                    format!("tasks/{id}/{TRANSCRIPT_COMPACT_NAME}"),
                    t.compact.as_slice().to_vec(),
                );
                out.insert(
                    format!("tasks/{id}/{TRANSCRIPT_FULL_NAME}"),
                    t.full.as_slice().to_vec(),
                );
            }
        }
        out
    }

    /// The `content_hash.txt` bytes: the shard's content address over its ordered
    /// artifacts (metadata ‖ report ‖ full), lowercase hex + trailing newline.
    #[must_use]
    pub fn content_hash_txt(&self) -> Vec<u8> {
        let mut parts: Vec<Vec<u8>> = vec![self.metadata_json(), self.redaction_report_json()];
        if let Some(full) = self.full_jsonl() {
            parts.push(full);
        }
        let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
        let mut hex = sha256_hex(&refs);
        hex.push('\n');
        hex.into_bytes()
    }
}

/// Serialize a JSON value to canonical bytes: `serde_json`'s default `Map` is a
/// `BTreeMap` (keys sorted), so this is deterministic byte-for-byte.
fn canonical_json(v: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

/// Render a positive epoch-ms instant as `YYYY-MM-DDTHH:MM:SS.mmmZ` — the
/// exact subset [`crate::artifact_bridge`]'s lenient view parses back, so
/// `parse(format(ms)) == ms` for every value this writer can emit (the
/// round-trip is pinned in the bridge's tests). Civil-from-days is Howard
/// Hinnant's algorithm over the proleptic Gregorian calendar — the inverse of
/// the parser's days-from-civil, and the same one-field-no-date-dependency
/// call.
pub(crate) fn rfc3339_utc_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// The canonical `sessions/<index>/metadata.json` bytes of one session of a
/// multi-session checkpoint: the session's **stable 0-based
/// index** (its position in the parent metadata's `sessions` array — assigned
/// once, never re-ordered by a later session joining) and its id. A new
/// serialized artifact, so it carries `schema_version`.
#[must_use]
pub fn session_meta_json(index: usize, session_id: &str) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    obj.insert("schema_version".into(), 1u32.into());
    obj.insert("index".into(), index.into());
    obj.insert("session_id".into(), session_id.to_owned().into());
    canonical_json(&serde_json::Value::Object(obj))
}

/// Refuse a task set the tree cannot faithfully hold: a tool-use id that is
/// not a safe single tree component (it names the `tasks/<id>/` subtree), or
/// two tasks sharing one id (one name cannot carry two subtrees, and silently
/// keeping either would drop capture). `rift-git`'s `mktree` screens names
/// again downstream; this refusal is the typed one that fires before any
/// object write, so a bad dispatch costs nothing durable.
fn validate_task_set(tasks: &[ArchivedTask]) -> Result<()> {
    let mut ids: Vec<&str> = Vec::with_capacity(tasks.len());
    for t in tasks {
        let id = t.tool_use_id.as_str();
        if id.is_empty() || id == "." || id == ".." || id.contains('/') || id.contains('\0') {
            return Err(rift_core::RiftError::validity(
                "a task tool-use id must be a safe git tree name",
            ));
        }
        ids.push(id);
    }
    ids.sort_unstable();
    let distinct = ids.len();
    ids.dedup();
    if ids.len() != distinct {
        return Err(rift_core::RiftError::validity(
            "two archived tasks share one tool-use id",
        ));
    }
    Ok(())
}

/// The seam the write side is named through.
///
/// One method and one implementation ([`NativeBackend`]). It stays a trait
/// because the callers — `condense`, `executor`, `commit_capture` — are written
/// against the capability rather than the writer, which is what lets a test
/// substitute a backend that refuses (the policy-denied audit path) or fails
/// (the no-false-`archived=1` property) without a git repository.
pub trait ShadowBackend {
    /// Archive a checkpoint shard: write the full shard from `archive`.
    ///
    /// # Errors
    /// [`rift_core::RiftError`] on an archive failure.
    fn archive_checkpoint(&self, archive: &CheckpointArchive) -> Result<()>;
}

/// The native backend: writes a byte/layout-compatible checkpoint shard
/// onto the orphan `rift/checkpoints/v1` branch through screened git plumbing
/// ([`NativeGitWrite`]). Generic over the writer so unit tests use
/// [`MemoryGitWrite`] and production uses `PlumbingGitWrite`. Hashing/attribution/
/// cost/redaction are already native (the default trait methods); this closes the
/// last wrapped capability.
pub struct NativeBackend<W: NativeGitWrite = MemoryGitWrite> {
    writer: W,
    ident: GitIdent,
}

impl NativeBackend<MemoryGitWrite> {
    /// A native backend over the in-memory writer double (tests / dry-run).
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            writer: MemoryGitWrite::default(),
            ident: GitIdent::default(),
        }
    }

    /// The in-memory writer, for post-archive assertions.
    #[must_use]
    pub fn writer(&self) -> &MemoryGitWrite {
        &self.writer
    }
}

impl<W: NativeGitWrite> NativeBackend<W> {
    /// A native backend over an explicit writer (production: `PlumbingGitWrite`).
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            ident: GitIdent::default(),
        }
    }

    /// Write the checkpoint shard: structural artifacts + the durable content blob,
    /// assembled into a leaf tree grafted onto the orphan branch. Idempotent (the
    /// underlying `commit_on_orphan` no-ops an already-present shard).
    fn write_shard(&self, archive: &CheckpointArchive) -> Result<()> {
        // Refuse malformed task sets BEFORE any object write: a tool-use id is
        // a git tree name, and a duplicate would be two entries with one name.
        validate_task_set(&archive.tasks)?;
        // The one content path through the redaction wall: the redacted capture.
        let content_oid = self.writer.write_durable_blob(&archive.content)?;

        // Structural artifacts (Rift-authored; secret-free by construction).
        let mut entries = vec![
            TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "content.redacted".into(),
                oid: content_oid,
            },
            TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "content_hash.txt".into(),
                oid: self
                    .writer
                    .write_structural_blob(&archive.content_hash_txt())?,
            },
            TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "metadata.json".into(),
                oid: self
                    .writer
                    .write_structural_blob(&archive.metadata_json())?,
            },
            TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "redaction_report.json".into(),
                oid: self
                    .writer
                    .write_structural_blob(&archive.redaction_report_json())?,
            },
        ];
        if let Some(full) = archive.full_jsonl() {
            entries.push(TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "full.jsonl".into(),
                oid: self.writer.write_structural_blob(&full)?,
            });
        }
        // The read facts the recall plane ingests: ids/paths/hashes
        // only — structural by construction, like metadata.json.
        if let Some(tool_calls) = archive.tool_calls_jsonl() {
            entries.push(TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "tool_calls.jsonl".into(),
                oid: self.writer.write_structural_blob(&tool_calls)?,
            });
        }
        // The transcript pair. CAPTURE, not structure: both
        // are `RedactedBytes` and take the durable path like `content.redacted`
        // — the type is the proof they passed the gate, and `write_durable_blob`
        // is the only door bytes of that type fit through.
        if let Some(t) = &archive.transcript {
            entries.push(TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: TRANSCRIPT_COMPACT_NAME.into(),
                oid: self.writer.write_durable_blob(&t.compact)?,
            });
            entries.push(TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: TRANSCRIPT_FULL_NAME.into(),
                oid: self.writer.write_durable_blob(&t.full)?,
            });
        }
        self.push_digest_entries(archive, &mut entries)?;
        // The per-session dirs of a multi-session checkpoint:
        // `sessions/<index>/metadata.json`, structural. Absent below
        // two sessions, so a single-session shard's tree never moved.
        let session_docs = archive.session_meta_docs();
        if !session_docs.is_empty() {
            let mut dir_entries = Vec::with_capacity(session_docs.len());
            for (index_name, bytes) in &session_docs {
                let inner = vec![TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: "metadata.json".into(),
                    oid: self.writer.write_structural_blob(bytes)?,
                }];
                dir_entries.push(TreeWriteEntry {
                    mode: GitFileMode::Tree,
                    name: index_name.clone(),
                    oid: self.writer.write_tree(&inner)?,
                });
            }
            dir_entries.sort_by(|a, b| a.name.cmp(&b.name));
            entries.push(TreeWriteEntry {
                mode: GitFileMode::Tree,
                name: "sessions".into(),
                oid: self.writer.write_tree(&dir_entries)?,
            });
        }
        // The subagent subtrees.
        if let Some(tasks_entry) = self.write_task_trees(&archive.tasks)? {
            entries.push(tasks_entry);
        }
        // Canonical git tree order (bytewise by name) so the leaf tree is stable.
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let leaf = self.writer.write_tree(&entries)?;

        let shard_path = archive.shard_path();
        // Epoch seconds for the commit date (derived from content, never wall
        // clock); a time-less archive dates its commit at epoch, exactly as
        // every pre-`created_at` shard did.
        let date_epoch = archive.created_at_ms.unwrap_or(0).div_euclid(1000);
        self.writer.commit_on_orphan(&OrphanCommit {
            branch: NATIVE_CHECKPOINT_REF,
            shard_path: &shard_path,
            tree_oid: &leaf,
            message: "rift: checkpoint",
            ident: &self.ident,
            date_epoch,
        })?;
        Ok(())
    }
}

impl<W: NativeGitWrite> NativeBackend<W> {
    /// Write the present `digest.*` artifacts: counts,
    /// indexes and content addresses only — structural by the containment pin
    /// (`a_digest_never_quotes_the_prompt_it_locates`).
    fn push_digest_entries(
        &self,
        archive: &CheckpointArchive,
        entries: &mut Vec<TreeWriteEntry>,
    ) -> Result<()> {
        for (name, bytes) in [
            (
                crate::digest::DIGEST_FAILURES_NAME,
                archive.digest_failures_json(),
            ),
            (
                crate::digest::DIGEST_INTENT_NAME,
                archive.digest_intent_json(),
            ),
            (crate::digest::DIGEST_LOG_NAME, archive.digest_log_json()),
            (
                crate::digest::DIGEST_SHAPE_NAME,
                archive.digest_shape_json(),
            ),
        ] {
            if let Some(bytes) = bytes {
                entries.push(TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: name.into(),
                    oid: self.writer.write_structural_blob(&bytes)?,
                });
            }
        }
        Ok(())
    }

    /// Write the `tasks/<tool-use-id>/` subtrees and return
    /// the top-level `tasks` tree entry, or `None` when the checkpoint carries
    /// no task — so a taskless shard's tree is exactly what it always was.
    ///
    /// Each subtree holds the task's own metadata (structural — identifiers,
    /// capture-time stamps, agent-reported counts) and, when present, its
    /// transcript pair (capture — the durable path, same wall as the parent's).
    fn write_task_trees(&self, tasks: &[ArchivedTask]) -> Result<Option<TreeWriteEntry>> {
        if tasks.is_empty() {
            return Ok(None);
        }
        let mut sorted: Vec<&ArchivedTask> = tasks.iter().collect();
        sorted.sort_by(|a, b| a.tool_use_id.as_bytes().cmp(b.tool_use_id.as_bytes()));
        let mut task_entries = Vec::with_capacity(sorted.len());
        for task in sorted {
            let mut inner = vec![TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "metadata.json".into(),
                oid: self.writer.write_structural_blob(&task.metadata_json())?,
            }];
            if let Some(t) = &task.transcript {
                inner.push(TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: TRANSCRIPT_COMPACT_NAME.into(),
                    oid: self.writer.write_durable_blob(&t.compact)?,
                });
                inner.push(TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: TRANSCRIPT_FULL_NAME.into(),
                    oid: self.writer.write_durable_blob(&t.full)?,
                });
            }
            inner.sort_by(|a, b| a.name.cmp(&b.name));
            task_entries.push(TreeWriteEntry {
                mode: GitFileMode::Tree,
                name: task.tool_use_id.clone(),
                oid: self.writer.write_tree(&inner)?,
            });
        }
        Ok(Some(TreeWriteEntry {
            mode: GitFileMode::Tree,
            name: "tasks".into(),
            oid: self.writer.write_tree(&task_entries)?,
        }))
    }
}

impl<W: NativeGitWrite> ShadowBackend for NativeBackend<W> {
    fn archive_checkpoint(&self, archive: &CheckpointArchive) -> Result<()> {
        self.write_shard(archive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_core::SecretBytes;

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

    /// A redacted (empty) capture minted through the sole sanctioned gate.
    fn redacted() -> RedactedBytes {
        rift_shadow_redact::redact(&SecretBytes::new(Vec::new()))
            .unwrap()
            .redacted
    }

    /// Redacted bytes over real content — same gate, non-empty.
    fn redacted_of(bytes: &[u8]) -> RedactedBytes {
        rift_shadow_redact::redact(&SecretBytes::new(bytes.to_vec()))
            .unwrap()
            .redacted
    }

    /// An [`ArchivedTranscript`] over the given raw transcript bytes, built the
    /// way condense builds one: compact on the RAW side, then the gate once per
    /// artifact.
    fn transcript_of(raw: &[u8]) -> ArchivedTranscript {
        let compacted = crate::transcript::compact_jsonl(&SecretBytes::new(raw.to_vec()), 0);
        ArchivedTranscript {
            full: redacted_of(raw),
            compact: redacted_of(compacted.compact().as_slice()),
            compact_start: compacted.compact_transcript_start,
        }
    }

    /// A minimal, valid checkpoint archive over a real 64-hex id.
    fn archive() -> CheckpointArchive {
        CheckpointArchive {
            repo: std::path::PathBuf::from("/tmp/r"),
            commit: "abc123".to_owned(),
            checkpoint_id: "ab".to_string() + &"cd".repeat(31),
            sessions: vec!["native-1".to_owned()],
            strategy: "manual-commit".to_owned(),
            prev_hash: None,
            events: Vec::new(),
            read_facts: Vec::new(),
            content: redacted(),
            report: RedactionReportView {
                total_spans: 0,
                bytes_masked: 0,
                allowlisted: 0,
                rule_pack_versions: vec!["builtin@2026.06.0".to_owned()],
                input_sha256: "c".repeat(64),
                by_detector: BTreeMap::new(),
                by_class: BTreeMap::new(),
            },
            transcript: None,
            tasks: Vec::new(),
            // `Some`, so every roundtrip/idempotence/root-invariance test in
            // this module runs over a metadata document that CARRIES the
            // recorded time — including the proof just below that the time is
            // not a Merkle leaf.
            created_at_ms: Some(1_700_000_000_000),
            // A hand-built archive is in no repository: the member is ABSENT, which
            // is what keeps every frozen anchor and golden byte-identical.
            worktree: None,
            session_worktrees: std::collections::BTreeMap::new(),
        }
    }

    /// **The sequence `content_hash.txt` digests, pinned by arithmetic rather
    /// than by prose.** Two descriptions of this one artifact lived a screen
    /// apart in this file and disagreed: the writer said
    /// `metadata ‖ report ‖ full`, the leaf-spec docstring said "a digest over
    /// `metadata.json`". The second was inherited by a published format
    /// registry, where a blind second implementation would have written a shard
    /// nothing could address. Measured on the reference branch, the
    /// single-artifact reading matches **0 of 104** shards.
    #[test]
    fn content_hash_covers_metadata_report_and_full_in_that_order() {
        let expect = |a: &CheckpointArchive| -> Vec<u8> {
            let mut parts: Vec<Vec<u8>> = vec![a.metadata_json(), a.redaction_report_json()];
            if let Some(full) = a.full_jsonl() {
                parts.push(full);
            }
            let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
            let mut hex = sha256_hex(&refs);
            hex.push('\n');
            hex.into_bytes()
        };

        // (a) WITHOUT `full.jsonl` — the shape 103 of the 104 live shards have,
        //     and the shape under which the false sentence is least visible.
        let bare = archive();
        assert!(bare.full_jsonl().is_none(), "the control carries no events");
        assert_eq!(bare.content_hash_txt(), expect(&bare));
        // The single-artifact reading is a DIFFERENT value, so the test would
        // pass under it only if the two readings coincided.
        let metadata_only = {
            let mut hex = sha256_hex(&[&bare.metadata_json()]);
            hex.push('\n');
            hex.into_bytes()
        };
        assert_ne!(
            bare.content_hash_txt(),
            metadata_only,
            "a digest over `metadata.json` alone is not this artifact"
        );

        // (b) WITH `full.jsonl` — the third member of the sequence, which is
        //     what makes it a sequence at all.
        let mut with_full = archive();
        with_full.events = vec![ArchivedEvent {
            session: "native-1".into(),
            turn_ordinal: 0,
            event_ordinal: 0,
            kind: "session_start".into(),
            verb: "SessionStart".into(),
            agent: "claude-code".into(),
            occurred_at_ms: 1,
            payload_sha256: "a".repeat(64),
            payload: serde_json::json!({}),
        }];
        assert!(with_full.full_jsonl().is_some());
        assert_eq!(with_full.content_hash_txt(), expect(&with_full));
        assert_ne!(
            with_full.content_hash_txt(),
            bare.content_hash_txt(),
            "the third artifact must be inside the digest"
        );

        // …and ORDER is part of it: report-then-metadata is a different value.
        let swapped = {
            let mut hex = sha256_hex(&[&bare.redaction_report_json(), &bare.metadata_json()]);
            hex.push('\n');
            hex.into_bytes()
        };
        assert_ne!(bare.content_hash_txt(), swapped);

        // The trailing newline is part of the artifact — a reader that trims it
        // computes a different file.
        assert_eq!(bare.content_hash_txt().last(), Some(&b'\n'));
        assert_eq!(bare.content_hash_txt().len(), 65);
    }

    /// **`metadata.json` records the capture event's own time,
    /// absent-when-empty**: `Some` emits `created_at` in exactly the
    /// shape the bridge's lenient view parses back into
    /// `checkpoint.created_at`; `None` — and the hook's 0 for an unreadable
    /// clock — omits the key, so a time-less archive's metadata is
    /// byte-identical to what every pre-`created_at` shard wrote. That
    /// absent-when-empty rule is what every optional member of this document
    /// follows, and it is the reason no committed anchor or golden moved when
    /// the field landed.
    #[test]
    fn metadata_records_the_capture_time_and_omits_it_when_unknown() {
        let dated = archive();
        let doc: serde_json::Value = serde_json::from_slice(&dated.metadata_json()).unwrap();
        assert_eq!(doc["created_at"], "2023-11-14T22:13:20.000Z");

        // Byte-level, not just key-level: the undated document is the dated
        // one minus exactly the `created_at` member.
        let mut undated = archive();
        undated.created_at_ms = None;
        let undoc: serde_json::Value = serde_json::from_slice(&undated.metadata_json()).unwrap();
        assert!(undoc.get("created_at").is_none(), "{undoc}");
        let dated_bytes = String::from_utf8(dated.metadata_json()).unwrap();
        let undated_bytes = String::from_utf8(undated.metadata_json()).unwrap();
        assert_eq!(
            dated_bytes.replace("\"created_at\":\"2023-11-14T22:13:20.000Z\",", ""),
            undated_bytes,
        );

        // An unreadable clock (`now_ms`'s 0) and a negative instant both read
        // as "no recorded time", never as an epoch date.
        for ms in [Some(0), Some(-5)] {
            let mut a = archive();
            a.created_at_ms = ms;
            let v: serde_json::Value = serde_json::from_slice(&a.metadata_json()).unwrap();
            assert!(v.get("created_at").is_none(), "{ms:?}");
        }

        // The time is NOT part of the identity — the anchoring-commit rule,
        // applied to the clock: same capture at a different instant keeps the
        // root and the id, while the document and `content_hash.txt` both
        // record that the instant differed.
        let mut later = archive();
        later.created_at_ms = Some(1_700_000_000_000 + 60_000);
        assert_eq!(later.merkle_root(), dated.merkle_root());
        assert_eq!(later.derived_checkpoint_id(), dated.derived_checkpoint_id());
        assert_ne!(later.metadata_json(), dated.metadata_json());
        assert_ne!(later.content_hash_txt(), dated.content_hash_txt());
    }

    /// A subagent task with metadata only — the minimal member of the class.
    fn task(id: &str) -> ArchivedTask {
        ArchivedTask {
            tool_use_id: id.to_owned(),
            agent_id: "explore".to_owned(),
            started_at_ms: 5,
            ended_at_ms: 9,
            input_tokens: None,
            output_tokens: None,
            transcript: None,
        }
    }

    /// **The task subtree's signal test.** A
    /// `tasks/<tool-use-id>/` subtree is part of the leaf spec: adding one
    /// MOVES the checkpoint id (so the shard path binds it), the second
    /// archive appends a new shard instead of colliding, and re-archiving the
    /// identical task-carrying checkpoint stays a no-op — the same three facts
    /// `an_id_bound_artifact_mints_a_new_shard_instead_of_refusing` pinned for
    /// `full.jsonl`, now for a nested artifact.
    #[test]
    fn a_task_subtree_joins_the_leaf_spec_and_moves_the_id() {
        let n = NativeBackend::in_memory();
        let mut taskless = archive();
        taskless.checkpoint_id = taskless.derived_checkpoint_id().unwrap();
        n.archive_checkpoint(&taskless).unwrap();

        let mut with_task = archive();
        with_task.tasks = vec![task("toolu_01")];
        with_task.checkpoint_id = with_task.derived_checkpoint_id().unwrap();
        assert_ne!(
            with_task.checkpoint_id, taskless.checkpoint_id,
            "the task subtree is bound, so the id — and the shard path — moved"
        );
        n.archive_checkpoint(&with_task)
            .expect("a bound task appends a new shard rather than colliding");
        assert_eq!(
            n.writer().shard_paths().len(),
            2,
            "both shards on the branch"
        );

        // The parent metadata enumerates the subtree for the reader.
        let meta = String::from_utf8(with_task.metadata_json()).unwrap();
        assert!(
            meta.contains(r#""tasks":["toolu_01"]"#),
            "metadata lists the task ids: {meta}"
        );

        // Idempotence survives the subtree: same bytes, same path, no-op.
        let tip = n.writer().branch_tip();
        n.archive_checkpoint(&with_task).unwrap();
        assert_eq!(n.writer().branch_tip(), tip, "re-archive is a no-op");
        assert_eq!(n.writer().shard_paths().len(), 2);
    }

    /// **The compat twin, stated the honest way.** `tasks/` sorts BETWEEN
    /// `redaction_report.json` and `tool_calls.jsonl` (`'t','a' < 't','o'`),
    /// so the compatibility argument is NOT "new leaves trail" — it is that a
    /// taskless shard's leaf sequence is UNCHANGED, leaf for leaf, and a
    /// task-carrying one inserts its leaves at exactly the position the name
    /// order dictates. Both halves asserted against hand-built sequences.
    #[test]
    fn a_taskless_shard_derives_the_same_root_it_did_before_tasks_existed() {
        use rift_hashchain::leaf_hash;

        let (content, report, tool_calls) = (b"c".as_slice(), b"r".as_slice(), b"t".as_slice());
        let taskless = ShardLeafBytes {
            content_redacted: content,
            redaction_report: report,
            tool_calls: Some(tool_calls),
            ..ShardLeafBytes::default()
        };
        // The pre-task sequence, built by hand: content, report, tool_calls.
        assert_eq!(
            shard_merkle_leaves(&taskless),
            vec![leaf_hash(content), leaf_hash(report), leaf_hash(tool_calls)],
            "a taskless shard's leaf sequence is unchanged"
        );

        // Two tasks, deliberately handed over UNSORTED: their leaves land
        // between the report and the read facts, in bytewise id order.
        let (meta_a, meta_b) = (b"task a meta".as_slice(), b"task b meta".as_slice());
        let with_tasks = ShardLeafBytes {
            tasks: &[
                TaskLeafBytes {
                    tool_use_id: "toolu_b",
                    metadata: meta_b,
                    transcript_compact: None,
                    transcript_full: Some(b"task b transcript"),
                },
                TaskLeafBytes {
                    tool_use_id: "toolu_a",
                    metadata: meta_a,
                    transcript_compact: None,
                    transcript_full: None,
                },
            ],
            ..taskless
        };
        assert_eq!(
            shard_merkle_leaves(&with_tasks),
            vec![
                leaf_hash(content),
                leaf_hash(report),
                leaf_hash(meta_a),
                leaf_hash(meta_b),
                leaf_hash(b"task b transcript"),
                leaf_hash(tool_calls),
            ],
            "task leaves insert between redaction_report.json and tool_calls.jsonl, id-sorted"
        );
    }

    /// A tool-use id names a git tree entry, so the writer refuses the ids a
    /// tree cannot faithfully hold — BEFORE any object write, so a bad
    /// dispatch costs nothing durable.
    #[test]
    fn an_unsafe_or_duplicate_task_id_is_refused_before_any_write() {
        for bad in ["", ".", "..", "a/b", "a\0b"] {
            let n = NativeBackend::in_memory();
            let mut a = archive();
            a.tasks = vec![task(bad)];
            let err = n.archive_checkpoint(&a).expect_err("unsafe id must refuse");
            assert!(err.to_string().contains("safe git tree name"), "{err}");
            assert_eq!(n.writer().durable_count(), 0, "refused before any write");
            assert_eq!(n.writer().structural_count(), 0);
        }
        let n = NativeBackend::in_memory();
        let mut dup = archive();
        dup.tasks = vec![task("toolu_01"), task("toolu_01")];
        let err = n
            .archive_checkpoint(&dup)
            .expect_err("duplicate ids refuse");
        assert!(err.to_string().contains("share one tool-use id"), "{err}");
        assert_eq!(n.writer().durable_count(), 0);
    }

    /// The rollup's arithmetic and its absence contract: sums treat a missing
    /// count as zero, and a task set with NO reported counts records no field
    /// at all — so token-less shards' metadata bytes never moved.
    #[test]
    fn the_task_token_rollup_sums_reported_counts_and_omits_when_none() {
        let mut a = archive();
        a.tasks = vec![task("toolu_a"), task("toolu_b")];
        assert_eq!(a.task_token_rollup(), None, "no task reported tokens");
        assert!(
            !String::from_utf8(a.metadata_json())
                .unwrap()
                .contains("task_tokens"),
            "no field when nothing was reported"
        );

        a.tasks[0].input_tokens = Some(100);
        a.tasks[0].output_tokens = Some(7);
        a.tasks[1].input_tokens = Some(20);
        // tasks[1].output_tokens stays None — sums as zero.
        assert_eq!(a.task_token_rollup(), Some((120, 7)));
        let meta = String::from_utf8(a.metadata_json()).unwrap();
        assert!(
            meta.contains(r#""task_tokens":{"input_tokens":120,"output_tokens":7}"#),
            "{meta}"
        );
    }

    /// **The multi-session layout + order rules, in one place.** A
    /// multi-session checkpoint writes `sessions/<index>/metadata.json` per
    /// session (stable 0-based indexes = positions in `sessions`), those
    /// documents are leaves between `redaction_report.json` and the task
    /// subtrees, their leaf order is git's BYTEWISE name order — `"10"` sorts
    /// before `"2"`, pinned so nobody "fixes" it to numeric — and a
    /// single-session shard writes none of it, so every existing shard's tree
    /// and leaf sequence are untouched.
    #[test]
    fn a_multi_session_shard_writes_indexed_session_dirs_and_orders_leaves_bytewise() {
        use rift_hashchain::leaf_hash;

        // Single-session: no dirs, no leaves — the compat half.
        let single = archive();
        assert!(single.session_meta_docs().is_empty());

        let mut multi = archive();
        multi.sessions = (0..11).map(|i| format!("01JSESSION{i:02}")).collect();
        let docs = multi.session_meta_docs();
        assert_eq!(docs.len(), 11);
        assert_eq!(docs[0].0, "0");
        assert_eq!(docs[10].0, "10");
        // The document binds index and id both.
        assert_eq!(
            docs[10].1,
            session_meta_json(10, "01JSESSION10"),
            "one renderer for the writer and the tests"
        );

        // The leaf order is BYTEWISE on the decimal name: 0,1,10,2,…
        let metas: Vec<SessionMetaLeaf<'_>> = docs
            .iter()
            .map(|(name, bytes)| SessionMetaLeaf {
                index_name: name,
                metadata: bytes,
            })
            .collect();
        let leaves = shard_merkle_leaves(&ShardLeafBytes {
            content_redacted: b"c",
            redaction_report: b"r",
            session_metas: &metas,
            ..ShardLeafBytes::default()
        });
        assert_eq!(leaves[0], leaf_hash(b"c"));
        assert_eq!(leaves[1], leaf_hash(b"r"));
        assert_eq!(leaves[2], leaf_hash(&docs[0].1), "index 0 first");
        assert_eq!(leaves[3], leaf_hash(&docs[1].1), "then 1");
        assert_eq!(leaves[4], leaf_hash(&docs[10].1), "then 10 — BYTEWISE");
        assert_eq!(leaves[5], leaf_hash(&docs[2].1), "then 2");

        // The writer writes the dirs (2 sessions ⇒ sessions/0 + sessions/1).
        let n = NativeBackend::in_memory();
        let mut a = archive();
        a.sessions = vec!["01JFIRST".to_owned(), "01JSECOND".to_owned()];
        a.checkpoint_id = a.derived_checkpoint_id().unwrap();
        n.archive_checkpoint(&a).unwrap();
        // content_hash + metadata + report + 2 session metas = 5 structural.
        assert_eq!(n.writer().structural_count(), 5);
        // leaf tree + sessions/ tree + 2 index trees = 4 trees.
        assert_eq!(n.writer().tree_count(), 4);
    }

    /// **The empty-transcript class — legacy backend defect 0x0011 — covered by
    /// name.** A session whose transcript file exists but is EMPTY archives
    /// cleanly, and the resulting shard is a DIFFERENT identity from the same
    /// capture with no transcript at all: empty is a value, absent is a
    /// shape, and collapsing them is the bug class. (leaf_hash(&[]) is a real
    /// digest, not the empty-tree root — the leaf spec's own doc contract.)
    #[test]
    fn an_empty_transcript_archives_distinct_from_no_transcript_the_0x0011_class() {
        let empty_pair = transcript_of(b"");
        assert_eq!(empty_pair.compact_start, 0);

        let mut without = archive();
        without.checkpoint_id = without.derived_checkpoint_id().unwrap();
        let mut with_empty = archive();
        with_empty.transcript = Some(empty_pair);
        with_empty.checkpoint_id = with_empty.derived_checkpoint_id().unwrap();
        assert_ne!(
            with_empty.checkpoint_id, without.checkpoint_id,
            "an empty transcript is capture content, not absence"
        );

        // Both archive cleanly — the class does not crash the writer — and
        // both shards coexist on the branch.
        let n = NativeBackend::in_memory();
        n.archive_checkpoint(&without).unwrap();
        n.archive_checkpoint(&with_empty).unwrap();
        assert_eq!(n.writer().shard_paths().len(), 2);
        // The empty pair still records its boundary in metadata.
        let meta = String::from_utf8(with_empty.metadata_json()).unwrap();
        assert!(meta.contains(r#""compact_transcript_start":0"#), "{meta}");
    }

    /// **The digest artifacts' binding signal test, the same shape as the task subtree's.**
    /// The digest artifacts are Merkle leaves: their bytes sit in the leaf
    /// sequence right after `content.redacted` (their bytewise name position),
    /// they move the checkpoint id through the root, and the writer commits
    /// them to the tree — so an edited stored digest stops the shard
    /// addressing its own path exactly like any other leaf.
    #[test]
    fn a_digest_artifact_joins_the_leaf_spec_and_moves_the_id() {
        use rift_hashchain::leaf_hash;

        let mut with_facts = archive();
        with_facts.read_facts = vec![ArchivedReadFact {
            tool_use_id: "t1".into(),
            tool_name: "Read".into(),
            path: "src/a.rs".into(),
            content_sha256: "aa".into(),
            input_tokens: None,
        }];
        let shape = with_facts
            .digest_shape_json()
            .expect("facts => shape digest");
        let tool_calls = with_facts.tool_calls_jsonl().unwrap();
        let report = with_facts.redaction_report_json();

        // The leaf sequence, hand-built: content, then the digest (its
        // bytewise position — after content_hash.txt, before full.jsonl),
        // then report, then tool_calls.
        let leaves = shard_merkle_leaves(&ShardLeafBytes {
            content_redacted: with_facts.content.as_slice(),
            redaction_report: &report,
            tool_calls: Some(&tool_calls),
            digests: DigestLeafBytes {
                shape: Some(&shape),
                ..DigestLeafBytes::default()
            },
            ..ShardLeafBytes::default()
        });
        assert_eq!(
            leaves,
            vec![
                leaf_hash(with_facts.content.as_slice()),
                leaf_hash(&shape),
                leaf_hash(&report),
                leaf_hash(&tool_calls),
            ],
            "digest leaves sit right after content.redacted"
        );
        // …and the archive derives its id THROUGH that sequence: the same
        // artifacts minus the digest leaf address a different path.
        let with_digest_root = with_facts.merkle_root();
        let without_digest_root = shard_merkle_root(&ShardLeafBytes {
            content_redacted: with_facts.content.as_slice(),
            redaction_report: &report,
            tool_calls: Some(&tool_calls),
            ..ShardLeafBytes::default()
        });
        assert_ne!(with_digest_root, without_digest_root, "the digest is bound");

        // The writer commits it: content_hash + metadata + report +
        // tool_calls + digest.shape = 5 structural blobs.
        let n = NativeBackend::in_memory();
        let mut a = with_facts.clone();
        a.checkpoint_id = a.derived_checkpoint_id().unwrap();
        n.archive_checkpoint(&a).unwrap();
        assert_eq!(n.writer().structural_count(), 5);
    }

    /// **The compat twin, stated the same honest way as item 4's:** a shard
    /// carrying no digest artifact derives, leaf for leaf, the sequence it
    /// derived before digests existed — and a digest-less archive is exactly
    /// one with neither transcript nor read facts, which is every
    /// pre-digest-era test fixture in this file (the anchors' claim).
    #[test]
    fn a_digestless_shard_derives_the_same_root_it_did_before_digests_existed() {
        use rift_hashchain::leaf_hash;

        let bare = archive();
        assert_eq!(bare.digest_failures_json(), None);
        assert_eq!(bare.digest_intent_json(), None);
        assert_eq!(bare.digest_log_json(), None);
        assert_eq!(bare.digest_shape_json(), None);
        let report = bare.redaction_report_json();
        assert_eq!(
            shard_merkle_leaves(&ShardLeafBytes {
                content_redacted: bare.content.as_slice(),
                redaction_report: &report,
                ..ShardLeafBytes::default()
            }),
            vec![leaf_hash(bare.content.as_slice()), leaf_hash(&report)],
            "no digest, no leaf — the pre-digest sequence verbatim"
        );
    }

    /// A transcript-carrying archive derives all three transcript-side
    /// digests, and they are functions of the archive's own fields — the
    /// drift-proofing that lets condense, the trailer prediction and the
    /// shard writer share one derivation.
    #[test]
    fn a_transcript_carrying_archive_derives_its_digest_documents() {
        let mut a = archive();
        a.transcript = Some(transcript_of(
            br#"{"type":"user","message":"do the thing"}
{"type":"user","message":[{"type":"tool_result","tool_use_id":"t","is_error":true}]}"#,
        ));
        let failures = a.digest_failures_json().expect("one failure");
        let intent = a.digest_intent_json().expect("transcript => intent");
        let log = a.digest_log_json().expect("transcript => log");
        let v: serde_json::Value = serde_json::from_slice(&failures).unwrap();
        assert_eq!(v.get("count").and_then(serde_json::Value::as_u64), Some(1));
        let v: serde_json::Value = serde_json::from_slice(&intent).unwrap();
        assert_eq!(
            v.pointer("/intent/prompt_line")
                .and_then(serde_json::Value::as_u64),
            Some(0)
        );
        let v: serde_json::Value = serde_json::from_slice(&log).unwrap();
        assert_eq!(
            v.get("transcript_lines")
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );
        // All durable + structural writes still land (wall intact): content +
        // 2 transcript blobs durable; content_hash/metadata/report + 3 digest
        // docs structural.
        let n = NativeBackend::in_memory();
        a.checkpoint_id = a.derived_checkpoint_id().unwrap();
        n.archive_checkpoint(&a).unwrap();
        assert_eq!(n.writer().durable_count(), 3);
        assert_eq!(n.writer().structural_count(), 6);
    }

    #[test]
    fn the_archive_writes_the_shard_the_wall_and_the_branch() {
        let n = NativeBackend::in_memory();
        n.archive_checkpoint(&archive()).unwrap();
        let w = n.writer();
        assert_eq!(w.durable_count(), 1, "the redacted content blob (the wall)");
        // content_hash.txt + metadata.json + redaction_report.json (no full.jsonl).
        assert_eq!(w.structural_count(), 3);
        assert_eq!(w.shard_paths().len(), 1, "one shard grafted");
        assert!(w.branch_tip().is_some(), "the branch advanced");
    }

    #[test]
    fn native_archive_is_idempotent() {
        let n = NativeBackend::in_memory();
        n.archive_checkpoint(&archive()).unwrap();
        let tip1 = n.writer().branch_tip();
        // Re-archiving the identical checkpoint is a no-op (same shard path).
        n.archive_checkpoint(&archive()).unwrap();
        assert_eq!(n.writer().branch_tip(), tip1, "idempotent re-archive");
        assert_eq!(n.writer().shard_paths().len(), 1);
    }

    #[test]
    fn native_archive_metadata_drift_is_a_noop_keeping_the_first_anchor() {
        // The checkpoint id excludes the anchoring commit (trailer prediction),
        // so a later commit in the same session re-archives the SAME shard path
        // with only metadata.json differing. That must be an idempotent no-op —
        // the regression this closed: the second commit of a session once failed
        // with 'shard path already holds different content'.
        let n = NativeBackend::in_memory();
        n.archive_checkpoint(&archive()).unwrap();
        let tip1 = n.writer().branch_tip();
        let mut later = archive();
        later.commit = "def456".to_owned();
        later.created_at_ms = later.created_at_ms.map(|ms| ms + 60_000);
        n.archive_checkpoint(&later).unwrap();
        assert_eq!(n.writer().branch_tip(), tip1, "first anchor kept");
        assert_eq!(n.writer().shard_paths().len(), 1);
    }

    #[test]
    fn native_archive_divergent_capture_content_still_refuses() {
        // Same shard path with DIFFERENT capture entries stays an integrity
        // refusal — only metadata.json may drift.
        let n = NativeBackend::in_memory();
        n.archive_checkpoint(&archive()).unwrap();
        let mut diverged = archive();
        diverged.content = rift_shadow_redact::redact(&SecretBytes::new(b"other".to_vec()))
            .unwrap()
            .redacted;
        let err = n
            .archive_checkpoint(&diverged)
            .expect_err("divergent capture content at the same shard path");
        assert!(
            err.to_string().contains("different content"),
            "refusal names the conflict: {err}"
        );
    }

    /// **The positive form of the id-unbound-artifact defect, which this replaced by closing it.**
    ///
    /// It was planted here as an `expect_err`: `full.jsonl` was written when
    /// `events` was non-empty and `events` was NOT in the checkpoint id, so the
    /// id — and the shard path — did not move while the tree did, and
    /// `rift_git::ANCHOR_DEPENDENT_SHARD_ENTRIES` (`["metadata.json",
    /// "content_hash.txt"]`) made the second archive of a live session take a
    /// hard `Integrity` refusal. The only reason it had never fired is that
    /// `events` is empty at every production call site — which is exactly
    /// what the archived transcript changes.
    ///
    /// The fix was NOT to add names to that list. That list means "ignore this
    /// entry when comparing", so a transcript on it would let the FIRST one win
    /// and drop every later one in silence — a class an earlier commit was
    /// spent removing. The fix is that the id is derived from the shard's
    /// Merkle root,
    /// which commits to every content artifact: a changed artifact mints a NEW
    /// checkpoint, cleanly appended, exactly as a grown read-fact set already did.
    ///
    /// So this asserts the three things that must now all hold at once: the id
    /// MOVES when an artifact appears, the second archive SUCCEEDS, and both
    /// shards survive — because appending is the whole point of not colliding.
    #[test]
    fn an_id_bound_artifact_mints_a_new_shard_instead_of_refusing() {
        let n = NativeBackend::in_memory();
        let mut first = archive();
        first.checkpoint_id = first.derived_checkpoint_id().unwrap();
        n.archive_checkpoint(&first).unwrap();

        // The same session archiving again after a turn produced events.
        let mut with_events = archive();
        with_events.events = vec![ArchivedEvent {
            session: "native-1".into(),
            turn_ordinal: 0,
            event_ordinal: 0,
            kind: "session_start".into(),
            verb: "SessionStart".into(),
            agent: "claude-code".into(),
            occurred_at_ms: 1,
            payload_sha256: "a".repeat(64),
            payload: serde_json::json!({}),
        }];
        with_events.checkpoint_id = with_events.derived_checkpoint_id().unwrap();
        assert_ne!(
            with_events.checkpoint_id, first.checkpoint_id,
            "the artifact is bound, so the shard path moved with it"
        );
        n.archive_checkpoint(&with_events)
            .expect("a bound artifact appends a new shard rather than colliding");
        assert_eq!(
            n.writer().shard_paths().len(),
            2,
            "both checkpoints are on the branch — appended, not overwritten"
        );

        // And the refusal is still there for what it is FOR: the same id
        // presented with different bytes. That is a forged path, not a new turn.
        let mut forged = with_events.clone();
        forged.checkpoint_id = first.checkpoint_id.clone();
        let err = n
            .archive_checkpoint(&forged)
            .expect_err("a shard path holding different content must still refuse");
        assert!(
            err.to_string().contains("different content"),
            "the refusal names the conflict: {err}"
        );
    }

    #[test]
    fn metadata_and_report_are_canonical_and_valid() {
        use rift_recall_types::checkpoint::{CheckpointMetadata, RedactionReportFacts};
        let a = archive();
        let meta: CheckpointMetadata = serde_json::from_slice(&a.metadata_json()).unwrap();
        assert_eq!(
            meta.merkle_root.as_deref(),
            Some(a.merkle_root().to_hex()).as_deref()
        );
        assert_eq!(meta.sessions, a.sessions);
        assert_eq!(meta.commit.as_deref(), Some(a.commit.as_str()));
        let report: RedactionReportFacts =
            serde_json::from_slice(&a.redaction_report_json()).unwrap();
        assert!(
            report.is_valid(),
            "the archived report passes the strict bridge gate"
        );
        // Canonical (byte-stable) serialization: identical archive → identical bytes.
        assert_eq!(archive().metadata_json(), a.metadata_json());
    }

    /// The defect this construction exists to kill: every native shard recorded
    /// `merkle_root(&[])`, so 100+ archived checkpoints published the identical
    /// constant and the Merkle half of tamper-evidence committed to nothing.
    ///
    /// The root must (a) not be the empty-tree root and (b) move when ANY of the
    /// leaf artifacts moves. Each arm below is the mutation that would have to
    /// go undetected for the defect to be back.
    #[test]
    fn the_shard_root_is_not_vacuous_and_moves_with_every_leaf_artifact() {
        let base = archive();
        let root = base.merkle_root();
        assert_ne!(
            root,
            rift_hashchain::empty_root(),
            "a shard root equal to the empty-tree root is the vacuous era returning"
        );

        // (1) content.redacted
        let mut other_content = archive();
        other_content.content = rift_shadow_redact::redact(&SecretBytes::new(b"other".to_vec()))
            .unwrap()
            .redacted;
        assert_ne!(other_content.merkle_root(), root, "content.redacted");

        // (2) redaction_report.json
        let mut other_report = archive();
        other_report.report.total_spans += 1;
        assert_ne!(other_report.merkle_root(), root, "redaction_report.json");

        // (3) full.jsonl (an event stream appears)
        let mut with_events = archive();
        with_events.events = vec![ArchivedEvent {
            session: "native-1".into(),
            turn_ordinal: 0,
            event_ordinal: 0,
            kind: "session_start".into(),
            verb: "SessionStart".into(),
            agent: "claude-code".into(),
            occurred_at_ms: 1,
            payload_sha256: "a".repeat(64),
            payload: serde_json::json!({}),
        }];
        assert_ne!(with_events.merkle_root(), root, "full.jsonl");

        // (4) tool_calls.jsonl
        let mut with_facts = archive();
        with_facts.read_facts = vec![ArchivedReadFact {
            tool_use_id: "t1".into(),
            tool_name: "Read".into(),
            path: "a.rs".into(),
            content_sha256: "aa".into(),
            input_tokens: None,
        }];
        assert_ne!(with_facts.merkle_root(), root, "tool_calls.jsonl");

        // …and the anchoring commit — which is NOT a leaf artifact — must not
        // move it, or a second commit in one session would mint a new root for
        // unchanged capture and the shard's idempotence would break.
        let mut later = archive();
        later.commit = "def456".to_owned();
        later.created_at_ms = later.created_at_ms.map(|ms| ms + 60_000);
        assert_eq!(
            later.merkle_root(),
            root,
            "anchor drift is not capture drift"
        );
    }

    /// One event line per leaf: two archived events produce two more leaves than
    /// none, in file order, and reordering them changes the root.
    #[test]
    fn full_jsonl_contributes_one_leaf_per_event_line() {
        let ev = |ordinal: u32, payload: u8| ArchivedEvent {
            session: "native-1".into(),
            turn_ordinal: 0,
            event_ordinal: ordinal,
            kind: "session_start".into(),
            verb: "SessionStart".into(),
            agent: "claude-code".into(),
            occurred_at_ms: 1,
            payload_sha256: format!("{payload:02x}").repeat(32),
            payload: serde_json::json!({}),
        };
        let mut a = archive();
        a.events = vec![ev(0, 0xaa), ev(1, 0xbb)];
        let full = a.full_jsonl().unwrap();
        let report = a.redaction_report_json();
        let leaves = shard_merkle_leaves(&ShardLeafBytes {
            content_redacted: a.content.as_slice(),
            full_jsonl: Some(&full),
            redaction_report: &report,
            tool_calls: None,
            transcript_compact: None,
            transcript_full: None,
            tasks: &[],
            session_metas: &[],
            digests: DigestLeafBytes::default(),
        });
        assert_eq!(
            leaves.len(),
            4,
            "content.redacted + two event lines + redaction_report.json"
        );

        let mut swapped = archive();
        swapped.events = vec![ev(1, 0xbb), ev(0, 0xaa)];
        assert_ne!(
            swapped.merkle_root(),
            a.merkle_root(),
            "order is part of the commitment"
        );

        // A trailing newline must not mint a phantom leaf: the same stream with
        // and without one derives the same root.
        let mut terminated = full.clone();
        terminated.push(b'\n');
        assert_eq!(
            shard_merkle_root(&ShardLeafBytes {
                content_redacted: a.content.as_slice(),
                full_jsonl: Some(&terminated),
                redaction_report: &report,
                tool_calls: None,
                transcript_compact: None,
                transcript_full: None,
                tasks: &[],
                session_metas: &[],
                digests: DigestLeafBytes::default(),
            }),
            a.merkle_root(),
            "trailing newline is not a record"
        );
    }

    /// **The transcript's signal test.** A transcript is a content artifact
    /// like any other: it moves the root, so it moves the id, so a session
    /// whose transcript changed mints a NEW shard that APPENDS — and none of
    /// that needed an era bump, which is precisely what binding the id to the
    /// root bought (the pre-root eras needed a new formula per artifact).
    #[test]
    fn a_transcript_artifact_is_bound_without_an_era_bump() {
        let n = NativeBackend::in_memory();
        let mut bare = archive();
        bare.checkpoint_id = bare.derived_checkpoint_id().unwrap();
        n.archive_checkpoint(&bare).unwrap();

        let mut with_transcript = archive();
        with_transcript.transcript = Some(transcript_of(
            br#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
        ));
        with_transcript.checkpoint_id = with_transcript.derived_checkpoint_id().unwrap();
        assert_ne!(
            with_transcript.checkpoint_id, bare.checkpoint_id,
            "the transcript must move the id, or it sits outside the binding"
        );
        n.archive_checkpoint(&with_transcript).unwrap();
        assert_eq!(
            n.writer().shard_paths().len(),
            2,
            "a changed capture APPENDS a new shard; nothing is overwritten or refused"
        );
        // Both transcript artifacts are CAPTURE and take the durable path:
        // content.redacted ×2 shards + compact + full = 4 durable writes.
        assert_eq!(
            n.writer().durable_count(),
            4,
            "transcripts go through write_durable_blob, like every redacted capture"
        );
        assert_eq!(
            NATIVE_CANON_VERSION, 3,
            "and the era did NOT move: a new artifact extends the leaf spec, never the id rule"
        );
    }

    /// **The transcript's signal test — the prefix claim.** A shard carrying no
    /// transcript derives the identical root it derived before the transcript
    /// pair existed, because the pair's names sort after every earlier
    /// artifact and absent artifacts contribute no leaf. The committed parity
    /// anchors assert the same fact against frozen bytes; this asserts the
    /// structural reason.
    #[test]
    fn a_shard_without_a_transcript_derives_the_same_root_it_did_before() {
        let report = br#"{"x":1}"#.as_slice();
        let without = ShardLeafBytes {
            content_redacted: b"capture",
            full_jsonl: Some(br#"{"e":0}"#),
            redaction_report: report,
            tool_calls: Some(br#"{"id":"t1"}"#),
            transcript_compact: None,
            transcript_full: None,
            tasks: &[],
            session_metas: &[],
            digests: DigestLeafBytes::default(),
        };
        let with = ShardLeafBytes {
            transcript_compact: Some(b"compact line"),
            transcript_full: Some(b"full line"),
            ..without
        };
        let base = shard_merkle_leaves(&without);
        let grown = shard_merkle_leaves(&with);
        assert_eq!(
            grown.get(..base.len()),
            Some(base.as_slice()),
            "a transcript-less leaf sequence must be a strict PREFIX of a transcript-carrying one"
        );
        assert_eq!(
            grown.len(),
            base.len() + 2,
            "one leaf per transcript artifact"
        );
        assert_ne!(
            shard_merkle_root(&without),
            shard_merkle_root(&with),
            "…and the added leaves genuinely move the root"
        );
    }

    /// The metadata half of the requirement: the compact path and its
    /// boundary are recorded when — and only when — the shard carries the
    /// artifacts, so a transcript-less shard's metadata is byte-identical to
    /// what it was before the pair existed.
    #[test]
    fn metadata_records_the_compact_name_and_start_only_when_present() {
        let bare = archive();
        let meta: serde_json::Value = serde_json::from_slice(&bare.metadata_json()).unwrap();
        assert!(meta.get("compact_transcript").is_none());
        assert!(meta.get("compact_transcript_start").is_none());

        let mut with = archive();
        let mut t = transcript_of(br#"{"type":"user","message":{"role":"user","content":"hi"}}"#);
        t.compact_start = 7;
        with.transcript = Some(t);
        let meta: serde_json::Value = serde_json::from_slice(&with.metadata_json()).unwrap();
        assert_eq!(
            meta.get("compact_transcript")
                .and_then(serde_json::Value::as_str),
            Some(TRANSCRIPT_COMPACT_NAME),
            "'compact path recorded in metadata' means the artifact NAME"
        );
        assert_eq!(
            meta.get("compact_transcript_start")
                .and_then(serde_json::Value::as_u64),
            Some(7),
        );
    }

    /// The era is stamped, and it is the field `rift verify` reads to decide
    /// which claims a shard is making about itself.
    ///
    /// Both thresholds are asserted against what a shard written NOW carries,
    /// because the two are separate claims: the recorded root must be strictly
    /// checkable (epoch ≥ 2) and the id must be root-bound (epoch ≥ 3). A stamp
    /// that satisfied one and not the other would put shards into an era whose
    /// rules no writer implements.
    #[test]
    fn the_archive_stamps_an_era_that_is_both_strict_and_root_bound() {
        let a = archive();
        let meta: serde_json::Value = serde_json::from_slice(&a.metadata_json()).unwrap();
        let stamped = meta
            .get("canon_version")
            .and_then(serde_json::Value::as_u64)
            .unwrap();
        assert_eq!(stamped, u64::from(NATIVE_CANON_VERSION));
        assert!(
            stamped >= u64::from(STRICT_MERKLE_MIN_CANON_VERSION),
            "a shard written now must be strictly verifiable"
        );
        assert!(
            stamped >= u64::from(ROOT_BOUND_ID_MIN_CANON_VERSION),
            "…and its id must address its own root"
        );
        // The stamp is a claim about this shard, so the shard must satisfy it:
        // the id the archive derives is the one that era's rule accepts.
        let ids = crate::condense::checkpoint_ids_for_shard(
            &a.sessions,
            NATIVE_CANON_VERSION,
            &ShardLeafBytes {
                content_redacted: a.content.as_slice(),
                full_jsonl: None,
                redaction_report: &a.redaction_report_json(),
                tool_calls: None,
                transcript_compact: None,
                transcript_full: None,
                tasks: &[],
                session_metas: &[],
                digests: DigestLeafBytes::default(),
            },
        );
        assert_eq!(
            ids.first().map(String::as_str),
            a.derived_checkpoint_id().as_deref(),
            "the writer's id must be what this era's checker accepts"
        );
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
    /// The central byte claim: the label rides in `metadata.json`,
    /// which is NOT a Merkle leaf — so an archive written from a linked
    /// worktree names it, and addresses **the same checkpoint id** as the
    /// byte-identical capture archived from nowhere in particular.
    #[test]
    fn an_archive_from_a_linked_worktree_names_it_and_keeps_the_checkpoint_id() {
        let unlabelled = archive();
        let mut labelled = archive();
        labelled.worktree = Some(WorktreeLabel {
            name: "swt-a".to_owned(),
            token: "0123456789ab".to_owned(),
            linked: true,
            branch: Some("swt/a-foundations".to_owned()),
        });

        assert_eq!(
            labelled.merkle_root(),
            unlabelled.merkle_root(),
            "metadata.json is not a leaf: the root cannot see the member"
        );
        assert_eq!(
            labelled.derived_checkpoint_id(),
            unlabelled.derived_checkpoint_id(),
            "the id is root-bound, so the label cannot move it"
        );

        let text = String::from_utf8(labelled.metadata_json()).unwrap();
        assert!(
            text.contains(
                r#""worktree":{"branch":"swt/a-foundations","linked":true,"name":"swt-a","token":"0123456789ab"}"#
            ),
            "{text}"
        );
        // No absolute path and no username: this document crosses machines in
        // a bundle, and a checkout's path is a local address.
        assert!(!text.contains('/') || !text.contains("Users"), "{text}");

        // …and the absence is exactly the bytes that were there before, which
        // is the whole compatibility claim for the committed anchors.
        let before = String::from_utf8(unlabelled.metadata_json()).unwrap();
        assert!(!before.contains("worktree"), "{before}");
        assert_eq!(
            before.len() + r#","worktree":{"branch":"swt/a-foundations","linked":true,"name":"swt-a","token":"0123456789ab"}"#.len(),
            text.len(),
            "the member is the ONLY difference"
        );

        // A detached archive omits `branch` rather than writing null: absent is
        // this document's spelling of unknown, everywhere.
        let mut detached = archive();
        detached.worktree = Some(WorktreeLabel {
            name: "main".to_owned(),
            token: "0123456789ab".to_owned(),
            linked: false,
            branch: None,
        });
        let detached_text = String::from_utf8(detached.metadata_json()).unwrap();
        assert!(
            detached_text.contains(r#""worktree":{"linked":false,"#),
            "{detached_text}"
        );
        assert!(!detached_text.contains("branch"), "{detached_text}");
    }

    /// `session_worktrees` is for the sessions of a MULTI-session
    /// shard whose last observation differs from the archiving worktree — and
    /// for nothing else. A single-session shard, and a multi-session shard
    /// whose sessions all sat where the archive ran, write no key at all, so
    /// their bytes are what they always were.
    #[test]
    fn a_multi_session_shard_records_session_worktrees_only_where_they_differ() {
        let here = WorktreeLabel {
            name: "main".to_owned(),
            token: "aaaaaaaaaaaa".to_owned(),
            linked: false,
            branch: Some("main".to_owned()),
        };
        let elsewhere = WorktreeLabel {
            name: "swt-d".to_owned(),
            token: "bbbbbbbbbbbb".to_owned(),
            linked: true,
            branch: Some("swt/d-surfaces".to_owned()),
        };

        // Every session sat where the archive ran → nothing to record.
        let mut agreed = archive();
        agreed.sessions = vec!["s-a".to_owned(), "s-b".to_owned()];
        agreed.worktree = Some(here.clone());
        let agreed_text = String::from_utf8(agreed.metadata_json()).unwrap();
        assert!(!agreed_text.contains("session_worktrees"), "{agreed_text}");

        // One session moved → exactly that session is named, and the archiving
        // label stays the shard's own.
        let mut moved = archive();
        moved.sessions = vec!["s-a".to_owned(), "s-b".to_owned()];
        moved.worktree = Some(here);
        moved
            .session_worktrees
            .insert("s-b".to_owned(), elsewhere.clone());
        let moved_text = String::from_utf8(moved.metadata_json()).unwrap();
        assert!(
            moved_text.contains(
                r#""session_worktrees":{"s-b":{"branch":"swt/d-surfaces","linked":true,"name":"swt-d","token":"bbbbbbbbbbbb"}}"#
            ),
            "{moved_text}"
        );
        assert!(
            !moved_text.contains(r#""s-a":{"#),
            "the agreeing session is not restated"
        );

        // Neither member touches the id or the root: both are metadata.
        assert_eq!(
            moved.derived_checkpoint_id(),
            agreed.derived_checkpoint_id()
        );
        assert_eq!(moved.merkle_root(), agreed.merkle_root());
        // …nor the per-session LEAF, which is a Merkle leaf and must not move.
        assert_eq!(moved.session_meta_docs(), agreed.session_meta_docs());
    }

    /// The member arrives on untrusted bytes — a shard can come from another
    /// machine in a bundle — and lands in an append-only table nothing can
    /// correct. Anything outside the shape this writer produces is refused
    /// WHOLE, never sanitised into a half-trusted row.
    #[test]
    fn a_worktree_member_this_writer_could_not_have_produced_is_refused() {
        let good = WorktreeLabel {
            name: "swt-a".to_owned(),
            token: "0123456789ab".to_owned(),
            linked: true,
            branch: Some("swt/a-foundations".to_owned()),
        };
        let doc = |label: &WorktreeLabel| {
            serde_json::to_vec(&serde_json::json!({ "worktree": label.to_json() })).unwrap()
        };
        assert_eq!(
            WorktreeLabel::from_metadata_json(&doc(&good)).as_ref(),
            Some(&good),
            "the writer's own bytes round-trip"
        );

        // A shard that names no worktree is UNKNOWN, not an error — every
        // shard written before worktree provenance existed looks exactly like this.
        assert_eq!(
            WorktreeLabel::from_metadata_json(br#"{"strategy":"manual-commit"}"#),
            None
        );
        assert_eq!(WorktreeLabel::from_metadata_json(b"not json"), None);

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
                WorktreeLabel::from_metadata_json(&doc(&bad)),
                None,
                "{bad:?} must not become a row"
            );
        }
        // A member of the wrong SHAPE is refused too, not read past.
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
}
