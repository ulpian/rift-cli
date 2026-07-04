//! Typed views of the on-disk checkpoint artifacts.
//!
//! These are the *lenient*, version-tolerant deserialization targets the
//! substrate parsers ([`rift-recall-substrate::parse`]) turn blob bytes into.
//! They are deliberately permissive (Stage 3 Phase 1 Step 2): optional fields
//! default, unknown fields are ignored (NO `deny_unknown_fields` — forward-compat
//! across Entire/Rift capture versions), and Entire-vs-Rift field-name spellings
//! are accepted via `#[serde(alias = …)]`. Parsing never panics; the parser
//! reports problems out of band as `ParseIssue`s.
//!
//! Redacted input: these artifacts live on the durable, **redacted** checkpoint
//! branch (`05-security` §5.1) — the types never reconstruct a redacted value.

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
    /// Merkle root (hex) — cross-checked against the chain in Stage 1 P3 S4.
    #[serde(default)]
    pub merkle_root: Option<String>,
    /// Prior checkpoint hash (hex), `None` at genesis.
    #[serde(default)]
    pub prev_hash: Option<String>,
    /// Session ids this checkpoint covers (Entire spells it `session_ids`).
    #[serde(default, alias = "session_ids")]
    pub sessions: Vec<String>,
    /// The working-tree commit oid (hex) this checkpoint captured, when the
    /// capture side records it. Used by the outcome-label deriver (P4 S2) to map
    /// a checkpoint to a real commit for reachability; absent ⇒ no outcome label.
    #[serde(default, alias = "commit_oid", alias = "head")]
    pub commit: Option<String>,
    /// The redaction/canonicalization epoch (P7 migration decision, ADR-0018).
    /// A native checkpoint carries `1`; a pre-rewrite (Entire) checkpoint omits
    /// this field and is read as epoch `0`, so `verify` selects the matching leaf
    /// canonicalization and the two epochs coexist.
    #[serde(default)]
    pub canon_version: Option<u32>,
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
    /// The tool-use id naming this captured call (Entire spells it `id`).
    #[serde(default, alias = "id")]
    pub tool_use_id: String,
    /// The tool name, e.g. `"Read"` (Entire spells it `name` or `tool`).
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
    /// Whether this call is a file *read* (the only shape the reread ledger counts).
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
    /// The tool-use id these token counts belong to (Entire spells it `id`).
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
/// shard (ADR-0002; parsed leniently, judged strictly by the bridge).
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
    /// bridge refuses shards whose report fails this (ADR-0002 strict layer);
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
    /// The canonical event kind token (`session.start`, `tool.call`, …).
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
    fn metadata_ignores_unknown_and_accepts_entire_alias() {
        let json = r#"{"strategy":"manual-commit","session_ids":["s1"],"future_field":42}"#;
        let m: CheckpointMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(m.strategy.as_deref(), Some("manual-commit"));
        assert_eq!(m.sessions, vec!["s1".to_string()]);
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
}
