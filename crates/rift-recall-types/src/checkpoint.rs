//! Typed views of the on-disk checkpoint artifacts.
//!
//! These are the *lenient*, version-tolerant deserialization targets the
//! substrate parsers (`rift-recall-substrate::parse`) turn blob bytes into.
//! They are deliberately permissive on purpose: optional fields
//! default, unknown fields are ignored (NO `deny_unknown_fields` — forward-compat
//! across pre-Rift and Rift capture versions), and legacy-backend-vs-Rift field-name spellings
//! are accepted via `#[serde(alias = …)]`. Parsing never panics; the parser
//! reports problems out of band as `ParseIssue`s.
//!
//! Redacted input: these artifacts live on the durable, **redacted** checkpoint
//! branch — the types never reconstruct a redacted value.

use serde::Deserialize;

/// `metadata.json` — per-checkpoint header.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct CheckpointMetadata {
    /// Checkpoint strategy, e.g. `"manual-commit"`.
    #[serde(default)]
    pub strategy: Option<String>,
    /// Creation time (RFC3339; validated downstream, not here).
    #[serde(default)]
    pub created_at: Option<String>,
    /// Merkle root (hex) — cross-checked against the chain when a shard is verified.
    #[serde(default)]
    pub merkle_root: Option<String>,
    /// Prior checkpoint hash (hex), `None` at genesis.
    #[serde(default)]
    pub prev_hash: Option<String>,
    /// Session ids this checkpoint covers (the legacy backend spells it `session_ids`).
    #[serde(default, alias = "session_ids")]
    pub sessions: Vec<String>,
    /// The working-tree commit oid (hex) this checkpoint captured, when the
    /// capture side records it. Used by the outcome-label deriver to map
    /// a checkpoint to a real commit for reachability; absent ⇒ no outcome label.
    #[serde(default, alias = "commit_oid", alias = "head")]
    pub commit: Option<String>,
    /// The redaction/canonicalization epoch.
    /// A native checkpoint carries `1`; a pre-rewrite (legacy backend) checkpoint omits
    /// this field and is read as epoch `0`, so `verify` selects the matching leaf
    /// canonicalization and the two epochs coexist.
    #[serde(default)]
    pub canon_version: Option<u32>,
    /// The **hashed** developer id this checkpoint's sessions belong to, when
    /// the capture side records it (never a raw name/email — the identity plane
    /// persists salted hashes only). Populates the developer scope axis at
    /// ingest; absent ⇒ the ingest caller's session-binding join (or
    /// null) stands in.
    #[serde(default, alias = "developer_id", alias = "developer_hash")]
    pub developer: Option<String>,
    /// Name of the self-contained redacted compact transcript artifact, when
    /// this checkpoint carries one. Current native writers use
    /// `transcript.compact.jsonl`; readers keep the recorded name so future
    /// spellings are reported rather than silently rewritten.
    #[serde(default)]
    pub compact_transcript: Option<String>,
    /// Zero-based compact-line boundary where this checkpoint's new slice
    /// begins. `None` means the boundary was not recorded and MUST NOT be
    /// guessed from another checkpoint.
    #[serde(default)]
    pub compact_transcript_start: Option<usize>,
    /// Tool-use ids naming `tasks/<tool-use-id>/` subtrees, in the writer's
    /// deterministic order.
    #[serde(default)]
    pub tasks: Vec<String>,
    /// Optional parent-side rollup of archived subagent token facts.
    #[serde(default)]
    pub task_tokens: Option<TaskTokenRollup>,
    /// The worktree this shard was **archived in** (the shard's `worktree`
    /// member), screened. `None` when the member is absent — which is what
    /// every shard predating worktree provenance looks like — or when it carried
    /// something this side will not believe.
    ///
    /// Parsed leniently even by this file's standards: a `worktree` member of
    /// the wrong *shape* (a bare string, say) must not cost the reader the rest
    /// of the header, so the field degrades to `None` instead of failing the
    /// document (`lenient_worktree`, below).
    #[serde(default, deserialize_with = "lenient_worktree")]
    pub worktree: Option<CheckpointWorktree>,
}

/// The `worktree` member of `metadata.json` — the member a checkpoint shard was
/// archived in, as the **recall** side reads it.
///
/// A deliberate second reader of one shard member. The capture side has its own
/// (`rift_shadow`'s `WorktreeLabel`, which also *writes* it), and the recall
/// plane does not depend on the capture plane — the dependency direction is
/// one-way and this crate keeps it. What the two share is the screen below, and
/// the screen is the part that matters: the bytes arrive on an untrusted
/// artifact (a shard can come in a bundle from another machine) and land in an
/// append-only table nothing can correct.
///
/// **No path and no username.** A checkout's path is a local address, not
/// content; it lives in the projection-only relations, never in a shard.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CheckpointWorktree {
    /// The linked member's administrative name, or `main`.
    pub name: String,
    /// `RepoPaths::worktree_token()` — 12 lowercase hex.
    pub token: String,
    /// Whether the archiving checkout is a linked worktree. Required: an absent
    /// `linked` is a member this reader will not guess at.
    pub linked: bool,
    /// The branch it was on; absent when the archive ran detached.
    #[serde(default)]
    pub branch: Option<String>,
}

impl CheckpointWorktree {
    /// The label, or `None` if any field is outside what a Rift writer can
    /// produce: the token is exactly 12 lowercase hex, the name is one non-empty
    /// path component, and neither the name nor the branch carries a control
    /// character or runs past a bound a reader would have to truncate.
    ///
    /// Refusing the whole member rather than sanitising a field: a half-trusted
    /// label is a label a reader cannot reason about, and "unknown" is already a
    /// value this design carries everywhere.
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

/// Deserialize the `worktree` member without ever failing the document.
///
/// The member is optional *and* untrusted, so the two failure modes it can
/// present — absent, and present but wrong — must reach the reader the same
/// way: as `None`. Screening happens here so an unscreened
/// [`CheckpointWorktree`] cannot be produced by parsing at all.
///
/// # Errors
/// Never for the member itself; only if the surrounding document is not JSON,
/// which is the caller's error already.
fn lenient_worktree<'de, D>(de: D) -> Result<Option<CheckpointWorktree>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(de)?;
    Ok(raw
        .and_then(|v| serde_json::from_value::<CheckpointWorktree>(v).ok())
        .and_then(CheckpointWorktree::screened))
}

/// Token facts rolled up from the task subtrees named by a checkpoint.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct TaskTokenRollup {
    /// Agent-reported task input tokens.
    #[serde(default)]
    pub input_tokens: u64,
    /// Agent-reported task output tokens.
    #[serde(default)]
    pub output_tokens: u64,
}

/// One per-file authorship fact from `attribution.json`.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct FileAttribution {
    /// Repo-relative path.
    #[serde(default, alias = "file", alias = "file_path")]
    pub path: String,
    /// Lines attributed to the agent.
    #[serde(default)]
    pub agent_lines: u32,
    /// Lines attributed to a human.
    #[serde(default)]
    pub human_lines: u32,
}

/// `attribution.json` — per-file agent/human line attribution.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct AttributionFacts {
    /// Per-file attribution rows.
    #[serde(default, alias = "files")]
    pub attributions: Vec<FileAttribution>,
}

/// One captured test run from `test_results.json` (the CI/test signal the
/// `TestsPassed` outcome label derives from). Lenient like every other
/// artifact view: a run with neither `passed` nor `exit_code` is treated as
/// unknown (never mistaken for a pass).
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct TestRunRecord {
    /// The command that ran, when recorded (e.g. `"cargo test"`).
    #[serde(default, alias = "cmd")]
    pub command: Option<String>,
    /// The test framework, when detected (`"cargo"`, `"pytest"`, `"jest"`, `"go"`).
    #[serde(default)]
    pub framework: Option<String>,
    /// The process exit code, when captured.
    #[serde(default, alias = "exit", alias = "status")]
    pub exit_code: Option<i32>,
    /// An explicit pass/fail verdict, when the capture side records one.
    #[serde(default)]
    pub passed: Option<bool>,
}

impl TestRunRecord {
    /// Whether this run is a **proven pass**: an explicit `passed: true`, or a
    /// captured exit code of 0. Absence of evidence is never a pass (fail-safe,
    /// the same posture as outcome-label reachability).
    #[must_use]
    pub fn is_pass(&self) -> bool {
        self.passed.unwrap_or(false) || self.exit_code == Some(0)
    }
}

/// `test_results.json` — the captured test runs for a checkpoint.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct TestResultsFacts {
    /// The captured runs.
    #[serde(default, alias = "tests")]
    pub runs: Vec<TestRunRecord>,
}

/// `cost_estimate.json` — four-class token cost for the checkpoint.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct CostEstimate {
    /// Uncached input tokens.
    #[serde(default, alias = "input_tokens")]
    pub uncached_input_tokens: u64,
    /// Cached input tokens (≈10× cheaper).
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// Cache-write tokens (a premium).
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// Output tokens.
    #[serde(default, alias = "completion_tokens")]
    pub output_tokens: u64,
}

/// One line of `tool_calls.jsonl` — the mechanical record the ledger needs.
///
/// Crucially this carries the **file-read facts** (`path` + `content_sha256`
/// captured at read time) that the delta ledger turns into `FileRead`s. Token
/// counts may be inline here or joined from `token_estimates` by `tool_use_id`.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct ToolCallRecord {
    /// The tool-use id naming this captured call (the legacy backend spells it `id`).
    #[serde(default, alias = "id")]
    pub tool_use_id: String,
    /// The tool name, e.g. `"Read"` (the legacy backend spells it `name` or `tool`).
    #[serde(default, alias = "name", alias = "tool")]
    pub tool_name: Option<String>,
    /// Repo-relative path read (for read-shaped tools).
    #[serde(default, alias = "file_path", alias = "file")]
    pub path: Option<String>,
    /// SHA-256 (hex) of the content at read time.
    #[serde(default, alias = "sha256", alias = "content_hash")]
    pub content_sha256: Option<String>,
    /// Input tokens this call contributed, when captured inline.
    #[serde(default, alias = "input")]
    pub input_tokens: Option<u32>,
}

impl ToolCallRecord {
    /// Whether this call is a file *read* (the only shape the window journal
    /// counts).
    #[must_use]
    pub fn is_read(&self) -> bool {
        self.tool_name.as_deref().is_some_and(|n| {
            n.eq_ignore_ascii_case("read") || n.to_ascii_lowercase().contains("read")
        })
    }
}

/// One line of `token_estimates.json`/`.jsonl` — tokens keyed by `tool_use_id`.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct TokenEstimate {
    /// The tool-use id these token counts belong to (the legacy backend spells it `id`).
    #[serde(default, alias = "id")]
    pub tool_use_id: String,
    /// Input tokens for this tool use.
    #[serde(default, alias = "input", alias = "tokens_in")]
    pub input_tokens: u32,
    /// Output tokens for this tool use.
    #[serde(default, alias = "output", alias = "tokens_out")]
    pub output_tokens: u32,
}

/// `redaction_report.json` — the counts-only proof that redaction ran over a
/// shard (parsed leniently, judged strictly by the bridge).
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct RedactionReportFacts {
    /// Total spans masked.
    #[serde(default)]
    pub total_spans: u32,
    /// Count per detector id.
    #[serde(default)]
    pub by_detector: std::collections::BTreeMap<String, u32>,
    /// Count per detector class (class token as a string — lenient).
    #[serde(default)]
    pub by_class: std::collections::BTreeMap<String, u32>,
    /// Aggregate masked byte length.
    #[serde(default)]
    pub bytes_masked: u64,
    /// Spans deliberately kept via the allowlist.
    #[serde(default)]
    pub allowlisted: u32,
    /// Exactly which rule packs/versions ran.
    #[serde(default)]
    pub rule_pack_versions: Vec<String>,
    /// SHA-256 (hex) of the raw input — the recompute-verify anchor.
    #[serde(default)]
    pub input_sha256: Option<String>,
}

impl RedactionReportFacts {
    /// Whether the report is **structurally valid** for projection: it names at
    /// least one rule pack that ran and carries the input content address. The
    /// bridge refuses shards whose report fails this (the strict layer);
    /// the lenient parser merely enumerates.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.rule_pack_versions.is_empty()
            && self
                .input_sha256
                .as_deref()
                .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
    }
}

/// One `full.jsonl` line — a canonical event as archived (the authoritative
/// event stream the bridge re-derives rows from).
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct FullEventRecord {
    /// The agent's native session id.
    #[serde(default, alias = "session_id")]
    pub session: String,
    /// Turn ordinal within the session.
    #[serde(default, alias = "turn")]
    pub turn_ordinal: u32,
    /// Event ordinal within the turn.
    #[serde(default, alias = "event")]
    pub event_ordinal: u32,
    /// The event kind token **as the shard archived it**, not normalised here.
    ///
    /// Two spellings exist in the wild and this field carries whichever it was
    /// handed: Rift writes `rift_core::EventKind`'s snake_case (`session_start`,
    /// `tool_call`), and the archived legacy dialect uses dots
    /// (`session.start`) — the committed read-compat fixtures under
    /// `crates/rift-shadow/legacy-fixtures/` are the evidence. Normalising is
    /// the ingesting side's job (`EventKind::from_wire_token`); a parser that
    /// rewrote the token here would destroy the only record of what the shard
    /// actually said. This doc claimed `session.start`/`tool.call` were "the
    /// canonical" tokens until 2026-08-12; the second was never a spelling
    /// anything wrote.
    #[serde(default)]
    pub kind: String,
    /// The native hook verb that produced the event (idempotency-key input).
    #[serde(default)]
    pub verb: String,
    /// The agent token (`claude-code` | …), when recorded per event.
    #[serde(default)]
    pub agent: Option<String>,
    /// Transaction time (epoch ms).
    #[serde(default, alias = "occurred_at")]
    pub occurred_at_ms: i64,
    /// Content address of the (redacted) payload, as archived.
    #[serde(default)]
    pub payload_sha256: Option<String>,
    /// The redacted payload (identity fields only are read back).
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// One `tasks/` entry — a subagent task record.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct TaskRecord {
    /// The task id.
    #[serde(default, alias = "id")]
    pub task_id: Option<String>,
    /// The parent session's native id.
    #[serde(default, alias = "session_id")]
    pub parent_session: Option<String>,
    /// The parent turn ordinal.
    #[serde(default, alias = "turn")]
    pub parent_turn_ordinal: u32,
    /// The child (subagent) session's native id.
    #[serde(default, alias = "subagent_session_id", alias = "child_session_id")]
    pub child_session: Option<String>,
    /// The task kind, when recorded.
    #[serde(default)]
    pub kind: Option<String>,
}

/// `tasks/<tool-use-id>/metadata.json` — the native nested-task header.
///
/// This is separate from [`TaskRecord`], the older flat task-event shape. Both
/// are accepted because durable repositories may contain either dialect.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct TaskMetadata {
    /// Schema version of this task metadata object.
    #[serde(default)]
    pub schema_version: Option<u32>,
    /// Parent transcript tool-use id and subtree name.
    #[serde(default, alias = "task_id", alias = "id")]
    pub tool_use_id: String,
    /// Captured subagent identifier/type.
    #[serde(default)]
    pub agent_id: String,
    /// Capture-time start in epoch milliseconds.
    #[serde(default)]
    pub started_at_ms: Option<i64>,
    /// Capture-time end in epoch milliseconds.
    #[serde(default)]
    pub ended_at_ms: Option<i64>,
    /// Agent-reported input tokens, when captured.
    #[serde(default)]
    pub input_tokens: Option<u64>,
    /// Agent-reported output tokens, when captured.
    #[serde(default)]
    pub output_tokens: Option<u64>,
    /// Name of the task's redacted compact transcript artifact.
    #[serde(default)]
    pub compact_transcript: Option<String>,
    /// Zero-based task compact-transcript boundary.
    #[serde(default)]
    pub compact_transcript_start: Option<usize>,
}

/// One line of a redacted compact transcript.
///
/// Compact records deliberately remain lenient: the compactor preserves
/// unrecognised JSON objects, so a reader must retain their structural fields
/// without pretending to understand them. Unknown fields stay in `extra` and
/// are never rendered automatically.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct CompactTranscriptRecord {
    /// Zero-based physical line ordinal in the stored JSONL artifact. Filled by
    /// the substrate parser (never deserialized) so a malformed skipped line
    /// cannot shift `compact_transcript_start` onto the wrong record.
    #[serde(skip)]
    pub artifact_line: usize,
    /// Compact schema version (`v`), when the record was emitted by Rift's
    /// compactor rather than passed through from an older producer.
    #[serde(default, rename = "v")]
    pub schema_version: Option<u64>,
    /// Record/role token (`user`, `assistant`, `system`, ...).
    #[serde(default, rename = "type")]
    pub record_type: Option<String>,
    /// Stream/message identifier, when captured.
    #[serde(default)]
    pub id: Option<String>,
    /// Some transcript dialects carry a nested `message` object/string.
    #[serde(default)]
    pub message: serde_json::Value,
    /// Canonical compact content: string, array of blocks, or an elision
    /// object. Consumers must select only explicit text leaves for display.
    #[serde(default)]
    pub content: serde_json::Value,
    /// All unrecognised fields. Retained for version-tolerant inspection but
    /// intentionally not flattened into display text.
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_report_validity_requires_packs_and_hash() {
        let ok: RedactionReportFacts = serde_json::from_str(&format!(
            r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        ))
        .unwrap();
        assert!(ok.is_valid());
        let no_packs: RedactionReportFacts =
            serde_json::from_str(&format!(r#"{{"input_sha256":"{}"}}"#, "a".repeat(64))).unwrap();
        assert!(!no_packs.is_valid());
        let bad_hash: RedactionReportFacts =
            serde_json::from_str(r#"{"rule_pack_versions":["x"],"input_sha256":"zz"}"#).unwrap();
        assert!(!bad_hash.is_valid());
    }

    #[test]
    fn full_event_record_accepts_aliases_and_defaults() {
        let e: FullEventRecord = serde_json::from_str(
            r#"{"session_id":"s1","turn":2,"event":1,"kind":"tool.call","verb":"PreToolUse","future":true}"#,
        )
        .unwrap();
        assert_eq!(e.session, "s1");
        assert_eq!(e.turn_ordinal, 2);
        assert_eq!(e.event_ordinal, 1);
        assert_eq!(e.kind, "tool.call");
    }

    #[test]
    fn task_record_aliases() {
        let t: TaskRecord = serde_json::from_str(
            r#"{"id":"t1","session_id":"s1","turn":3,"subagent_session_id":"c1"}"#,
        )
        .unwrap();
        assert_eq!(t.task_id.as_deref(), Some("t1"));
        assert_eq!(t.child_session.as_deref(), Some("c1"));
        assert_eq!(t.parent_turn_ordinal, 3);
    }

    #[test]
    fn metadata_ignores_unknown_and_accepts_legacy_alias() {
        let json = r#"{"strategy":"manual-commit","session_ids":["s1"],"future_field":42}"#;
        let m: CheckpointMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(m.strategy.as_deref(), Some("manual-commit"));
        assert_eq!(m.sessions, vec!["s1".to_string()]);
    }

    #[test]
    fn metadata_reads_compact_boundary_and_task_rollup() {
        let m: CheckpointMetadata = serde_json::from_str(
            r#"{"compact_transcript":"transcript.compact.jsonl","compact_transcript_start":3,"tasks":["toolu_1"],"task_tokens":{"input_tokens":7,"output_tokens":2}}"#,
        )
        .unwrap();
        assert_eq!(m.compact_transcript_start, Some(3));
        assert_eq!(m.tasks, vec!["toolu_1"]);
        assert_eq!(m.task_tokens.unwrap().input_tokens, 7);
    }

    #[test]
    fn compact_record_retains_unknown_structure_without_promoting_it_to_text() {
        let r: CompactTranscriptRecord = serde_json::from_str(
            r#"{"v":1,"type":"assistant","id":"m1","content":"done","future_payload":"opaque"}"#,
        )
        .unwrap();
        assert_eq!(r.schema_version, Some(1));
        assert_eq!(r.record_type.as_deref(), Some("assistant"));
        assert_eq!(r.content, serde_json::json!("done"));
        assert_eq!(
            r.extra.get("future_payload"),
            Some(&serde_json::json!("opaque"))
        );
    }

    #[test]
    fn tool_call_read_detection_and_aliases() {
        let json = r#"{"id":"toolu_1","name":"Read","file_path":"src/a.rs","sha256":"abc"}"#;
        let t: ToolCallRecord = serde_json::from_str(json).unwrap();
        assert_eq!(t.tool_use_id, "toolu_1");
        assert_eq!(t.path.as_deref(), Some("src/a.rs"));
        assert_eq!(t.content_sha256.as_deref(), Some("abc"));
        assert!(t.is_read());
    }

    #[test]
    fn token_estimate_aliases() {
        let t: TokenEstimate = serde_json::from_str(r#"{"id":"toolu_1","input":120}"#).unwrap();
        assert_eq!(t.tool_use_id, "toolu_1");
        assert_eq!(t.input_tokens, 120);
    }

    /// A well-formed `worktree` member parses and screens through.
    #[test]
    fn a_worktree_member_parses_and_screens() {
        let m: CheckpointMetadata = serde_json::from_str(
            r#"{"strategy":"manual-commit","worktree":{"name":"swt-c","token":"0123456789ab","linked":true,"branch":"swt/c-recall"}}"#,
        )
        .unwrap();
        let w = m.worktree.expect("the member parses");
        assert_eq!(w.name, "swt-c");
        assert_eq!(w.token, "0123456789ab");
        assert!(w.linked);
        assert_eq!(w.branch.as_deref(), Some("swt/c-recall"));
    }

    /// The header survives a `worktree` member this side will not believe.
    ///
    /// Four ways to disbelieve one, and each must cost the member and nothing
    /// else: the whole rest of the header is the reader's, and a bundle from
    /// another machine must not be able to take `commit` away by malforming a
    /// field beside it.
    #[test]
    fn an_unbelievable_worktree_member_costs_the_member_and_not_the_header() {
        for member in [
            r#""swt-c""#,                                                // wrong shape
            r#"{"name":"swt-c","token":"0123456789ab"}"#,                // no `linked`
            r#"{"name":"swt-c","token":"nothex","linked":true}"#,        // token not hex
            r#"{"name":"a/b","token":"0123456789ab","linked":true}"#,    // name is a path
            r#"{"name":"swt-c","token":"0123456789ab0","linked":true}"#, // token too long
            r#"{"name":"","token":"0123456789ab","linked":false}"#,      // empty name
            r#"{"name":"swt-c","token":"0123456789ab","linked":true,"branch":"a\u0007b"}"#,
        ] {
            let doc = format!(r#"{{"commit":"deadbeef","worktree":{member}}}"#);
            let m: CheckpointMetadata =
                serde_json::from_str(&doc).unwrap_or_else(|e| panic!("{member} -> {e}"));
            assert!(m.worktree.is_none(), "{member} must not be believed");
            assert_eq!(
                m.commit.as_deref(),
                Some("deadbeef"),
                "{member} must not cost the header its anchor commit"
            );
        }
    }

    /// A shard with no member at all predates worktree provenance and reads as
    /// unknown — never as `main`.
    #[test]
    fn a_shard_without_the_member_reads_as_unknown_not_main() {
        let m: CheckpointMetadata =
            serde_json::from_str(r#"{"strategy":"manual-commit"}"#).unwrap();
        assert!(m.worktree.is_none());
    }
}
