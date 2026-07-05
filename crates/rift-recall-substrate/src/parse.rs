//! Lenient, version-tolerant parsers for checkpoint artifacts (Stage 3 Phase 1
//! Step 2). Blob bytes in, typed [`rift_recall_types::checkpoint`] values out.
//!
//! **Skip-and-report**: a corrupt field or JSONL line is reported as a
//! [`ParseIssue`] and skipped, never panicked — one bad byte must not sink a
//! whole checkpoint (`06 §6.4`). Untrusted bytes are size-capped **before**
//! allocation; [`redact_err`] surfaces only position/category, never raw payload
//! (`05-security` §5.7).

use rift_recall_types::checkpoint::{
    AttributionFacts, CheckpointMetadata, CostEstimate, FullEventRecord, RedactionReportFacts,
    TaskRecord, TestResultsFacts, TokenEstimate, ToolCallRecord,
};
use serde::de::DeserializeOwned;

/// Hard cap on a single artifact blob, applied before any parse allocates.
pub const MAX_ARTIFACT_BYTES: usize = 32 * 1024 * 1024;
/// Hard cap on a single JSONL line.
pub const MAX_JSONL_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Where in an artifact a problem occurred.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Locator {
    /// The whole artifact (e.g. an object that failed to parse at all).
    WholeFile,
    /// A specific 1-based line in a JSONL artifact.
    Line(usize),
}

/// The class of a parse problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueKind {
    /// Bytes did not parse as the expected shape.
    Corrupt,
    /// An artifact (or line) exceeded its size cap and was skipped unparsed.
    OversizeSkipped,
}

/// A single problem worth reporting to `doctor`. Carries **no** raw payload bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseIssue {
    /// The checkpoint the problem was found in.
    pub checkpoint_id: String,
    /// The artifact filename, e.g. `"tool_calls.jsonl"`.
    pub artifact: &'static str,
    /// Where in the artifact.
    pub locator: Locator,
    /// What kind of problem.
    pub kind: IssueKind,
    /// Human message — position/category only, never payload bytes.
    pub detail: String,
}

/// Outcome of parsing one artifact: the good value plus every skip.
#[derive(Clone, Debug)]
pub struct Outcome<T> {
    /// The parsed value (default/empty if the whole artifact failed).
    pub value: T,
    /// Every problem encountered (empty on a clean parse).
    pub issues: Vec<ParseIssue>,
}

impl<T> Outcome<T> {
    /// Whether the parse was completely clean.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Position/category of a serde_json error — **never** the offending bytes.
#[must_use]
pub fn redact_err(e: &serde_json::Error) -> String {
    let category = match e.classify() {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
    };
    format!(
        "{category} error at line {} column {}",
        e.line(),
        e.column()
    )
}

fn oversize(
    checkpoint_id: &str,
    artifact: &'static str,
    locator: Locator,
    len: usize,
) -> ParseIssue {
    ParseIssue {
        checkpoint_id: checkpoint_id.to_string(),
        artifact,
        locator,
        kind: IssueKind::OversizeSkipped,
        detail: format!("{len} bytes over cap"),
    }
}

/// Parse a single JSON object artifact (`metadata.json`, …). On failure the
/// value defaults and one whole-file `ParseIssue` is reported.
fn parse_object<T: DeserializeOwned + Default>(
    bytes: &[u8],
    artifact: &'static str,
    checkpoint_id: &str,
) -> Outcome<T> {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Outcome {
            value: T::default(),
            issues: vec![oversize(
                checkpoint_id,
                artifact,
                Locator::WholeFile,
                bytes.len(),
            )],
        };
    }
    match serde_json::from_slice::<T>(bytes) {
        Ok(value) => Outcome {
            value,
            issues: vec![],
        },
        Err(e) => Outcome {
            value: T::default(),
            issues: vec![ParseIssue {
                checkpoint_id: checkpoint_id.to_string(),
                artifact,
                locator: Locator::WholeFile,
                kind: IssueKind::Corrupt,
                detail: redact_err(&e),
            }],
        },
    }
}

/// Parse a line-delimited (JSONL) artifact: one bad line is one `ParseIssue`, the
/// rest still parse.
fn parse_jsonl<T: DeserializeOwned>(
    bytes: &[u8],
    artifact: &'static str,
    checkpoint_id: &str,
) -> Outcome<Vec<T>> {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Outcome {
            value: Vec::new(),
            issues: vec![oversize(
                checkpoint_id,
                artifact,
                Locator::WholeFile,
                bytes.len(),
            )],
        };
    }
    let mut value = Vec::new();
    let mut issues = Vec::new();
    for (n, line) in bytes.split(|&b| b == b'\n').enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_JSONL_LINE_BYTES {
            issues.push(oversize(
                checkpoint_id,
                artifact,
                Locator::Line(n + 1),
                line.len(),
            ));
            continue;
        }
        match serde_json::from_slice::<T>(line) {
            Ok(rec) => value.push(rec),
            Err(e) => issues.push(ParseIssue {
                checkpoint_id: checkpoint_id.to_string(),
                artifact,
                locator: Locator::Line(n + 1),
                kind: IssueKind::Corrupt,
                detail: redact_err(&e),
            }),
        }
    }
    Outcome { value, issues }
}

/// Parse `metadata.json`.
#[must_use]
pub fn parse_metadata(bytes: &[u8], checkpoint_id: &str) -> Outcome<CheckpointMetadata> {
    parse_object(bytes, "metadata.json", checkpoint_id)
}

/// Parse `attribution.json`.
#[must_use]
pub fn parse_attribution(bytes: &[u8], checkpoint_id: &str) -> Outcome<AttributionFacts> {
    parse_object(bytes, "attribution.json", checkpoint_id)
}

/// Parse `cost_estimate.json`.
#[must_use]
pub fn parse_cost_estimate(bytes: &[u8], checkpoint_id: &str) -> Outcome<CostEstimate> {
    parse_object(bytes, "cost_estimate.json", checkpoint_id)
}

/// Parse `tool_calls.jsonl` (line-delimited).
#[must_use]
pub fn parse_tool_calls(bytes: &[u8], checkpoint_id: &str) -> Outcome<Vec<ToolCallRecord>> {
    parse_jsonl(bytes, "tool_calls.jsonl", checkpoint_id)
}

/// Parse `token_estimates` — JSONL in some builds, a single object/array in
/// others. Tries JSONL first; that handles both a one-object file and a stream.
#[must_use]
pub fn parse_token_estimates(bytes: &[u8], checkpoint_id: &str) -> Outcome<Vec<TokenEstimate>> {
    parse_jsonl(bytes, "token_estimates.json", checkpoint_id)
}

/// Parse `test_results.json` — the captured test runs (the `TestsPassed`
/// outcome-label signal, P4 S2).
#[must_use]
pub fn parse_test_results(bytes: &[u8], checkpoint_id: &str) -> Outcome<TestResultsFacts> {
    parse_object(bytes, "test_results.json", checkpoint_id)
}

/// Parse `redaction_report.json` — the counts-only proof redaction ran. The
/// **lenient** view: the bridge layers strict semantics
/// ([`RedactionReportFacts::is_valid`]) on top and refuses invalid shards.
#[must_use]
pub fn parse_redaction_report(bytes: &[u8], checkpoint_id: &str) -> Outcome<RedactionReportFacts> {
    parse_object(bytes, "redaction_report.json", checkpoint_id)
}

/// Parse `full.jsonl` — the archived canonical-event stream (line-delimited).
#[must_use]
pub fn parse_full_events(bytes: &[u8], checkpoint_id: &str) -> Outcome<Vec<FullEventRecord>> {
    parse_jsonl(bytes, "full.jsonl", checkpoint_id)
}

/// Parse one `tasks/` artifact (JSONL; a single-object file parses as one line).
#[must_use]
pub fn parse_tasks(bytes: &[u8], checkpoint_id: &str) -> Outcome<Vec<TaskRecord>> {
    parse_jsonl(bytes, "tasks", checkpoint_id)
}

/// Parse `merkle_leaf.txt` — one lowercase-hex SHA-256 leaf per line.
/// Skip-and-report: malformed lines are issues, valid leaves still parse.
#[must_use]
pub fn parse_merkle_leaves(bytes: &[u8], checkpoint_id: &str) -> Outcome<Vec<String>> {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Outcome {
            value: Vec::new(),
            issues: vec![oversize(
                checkpoint_id,
                "merkle_leaf.txt",
                Locator::WholeFile,
                bytes.len(),
            )],
        };
    }
    let text = String::from_utf8_lossy(bytes);
    let mut value = Vec::new();
    let mut issues = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
            value.push(t.to_ascii_lowercase());
        } else {
            issues.push(ParseIssue {
                checkpoint_id: checkpoint_id.to_owned(),
                artifact: "merkle_leaf.txt",
                locator: Locator::Line(i + 1),
                kind: IssueKind::Corrupt,
                detail: "not a 64-hex leaf".to_owned(),
            });
        }
    }
    Outcome { value, issues }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_metadata() {
        let out = parse_metadata(br#"{"strategy":"manual-commit"}"#, "cp");
        assert!(out.is_clean());
        assert_eq!(out.value.strategy.as_deref(), Some("manual-commit"));
    }

    #[test]
    fn ignores_unknown_object_fields() {
        let out = parse_metadata(br#"{"future_field":1,"strategy":"x"}"#, "cp");
        assert!(
            out.is_clean(),
            "unknown fields are forward-compat, not fatal"
        );
    }

    #[test]
    fn jsonl_skips_single_bad_line() {
        let blob = b"{\"id\":\"a\"}\nnot json\n{\"id\":\"b\"}\n";
        let out = parse_tool_calls(blob, "cp");
        assert_eq!(out.value.len(), 2);
        assert_eq!(out.issues.len(), 1);
        assert_eq!(out.issues[0].locator, Locator::Line(2));
        assert_eq!(out.issues[0].kind, IssueKind::Corrupt);
    }

    #[test]
    fn oversize_blob_is_skipped_not_parsed() {
        // A vec just over the cap; we must not attempt to parse it.
        let big = vec![b'x'; MAX_ARTIFACT_BYTES + 1];
        let out = parse_metadata(&big, "cp");
        assert_eq!(out.issues.len(), 1);
        assert_eq!(out.issues[0].kind, IssueKind::OversizeSkipped);
    }

    #[test]
    fn parse_error_detail_contains_no_raw_payload() {
        // Payload contains a sentinel that must NOT appear in the issue detail.
        let blob = b"{\"id\":\"a\"}\nSUPERSECRETTOKEN_zzz\n";
        let out = parse_tool_calls(blob, "cp");
        assert_eq!(out.issues.len(), 1);
        assert!(
            !out.issues[0].detail.contains("SUPERSECRET"),
            "redact_err must not echo payload bytes: {}",
            out.issues[0].detail
        );
    }

    #[test]
    fn redaction_report_parses_and_judges_validity() {
        let good = format!(
            r#"{{"total_spans":3,"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "b".repeat(64)
        );
        let out = parse_redaction_report(good.as_bytes(), "cp");
        assert!(out.is_clean());
        assert!(out.value.is_valid());
        // an empty/absent report parses (lenient) but is INVALID (strict layer)
        let empty = parse_redaction_report(b"{}", "cp");
        assert!(empty.is_clean());
        assert!(!empty.value.is_valid());
    }

    #[test]
    fn full_events_parse_line_delimited() {
        let blob = b"{\"session_id\":\"s1\",\"turn\":0,\"event\":0,\"kind\":\"session.start\",\"verb\":\"SessionStart\"}\n\
                     {\"session_id\":\"s1\",\"turn\":1,\"event\":0,\"kind\":\"tool.call\",\"verb\":\"PreToolUse\"}\n";
        let out = parse_full_events(blob, "cp");
        assert!(out.is_clean());
        assert_eq!(out.value.len(), 2);
        assert_eq!(out.value[1].turn_ordinal, 1);
    }

    #[test]
    fn tasks_parse_single_object_or_stream() {
        let out = parse_tasks(br#"{"id":"t1","subagent_session_id":"c1"}"#, "cp");
        assert!(out.is_clean());
        assert_eq!(out.value.len(), 1);
    }

    #[test]
    fn merkle_leaves_skip_and_report_bad_lines() {
        let good = "a".repeat(64);
        let blob = format!("{good}\nnot-a-leaf\n\n{good}\n");
        let out = parse_merkle_leaves(blob.as_bytes(), "cp");
        assert_eq!(out.value.len(), 2);
        assert_eq!(out.issues.len(), 1);
        assert_eq!(out.issues[0].locator, Locator::Line(2));
    }

    proptest::proptest! {
        /// Lenient parse never panics on arbitrary bytes (skip-and-report totality).
        #[test]
        fn lenient_parse_never_panics(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..512)) {
            let _ = parse_metadata(&bytes, "cp");
            let _ = parse_tool_calls(&bytes, "cp");
            let _ = parse_token_estimates(&bytes, "cp");
            let _ = parse_attribution(&bytes, "cp");
            let _ = parse_cost_estimate(&bytes, "cp");
            let _ = parse_redaction_report(&bytes, "cp");
            let _ = parse_full_events(&bytes, "cp");
            let _ = parse_tasks(&bytes, "cp");
            let _ = parse_merkle_leaves(&bytes, "cp");
        }
    }
}
