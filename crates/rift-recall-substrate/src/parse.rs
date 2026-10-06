//! Lenient, version-tolerant parsers for checkpoint artifacts.
//! Blob bytes in, typed [`rift_recall_types::checkpoint`] values out.
//!
//! **Skip-and-report**: a corrupt field or JSONL line is reported as a
//! [`ParseIssue`] and skipped, never panicked — one bad byte must not sink a
//! whole checkpoint. Untrusted bytes are size-capped **before**
//! allocation; [`redact_err`] surfaces only position/category, never raw payload
//! — a parse error never leaks the bytes that caused it.

mod window_journal;

use rift_recall_types::checkpoint::{
    AttributionFacts, CheckpointMetadata, CompactTranscriptRecord, CostEstimate, FullEventRecord,
    RedactionReportFacts, TaskMetadata, TaskRecord, TestResultsFacts, TokenEstimate,
    ToolCallRecord,
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

pub use window_journal::{parse_window_journal, WindowJournalRow};

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
    /// A read fact carried no token estimate and its read-time bytes could not
    /// be resolved — the read counts 0 tokens rather than a guess.
    TokenUnresolved,
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

/// One serve envelope a checkpoint froze into `serve_audit.jsonl`, validated
/// and ready for the recall appender, which re-derives the serve's rows from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServeAuditEnvelope {
    /// The key the envelope's rows are addressed by, read off the line and
    /// never recomputed: the frozen payload may carry `withheld` where the
    /// live one carried typed text, and the row must land on the live key.
    pub idempotency_key: String,
    /// The instant the live row was written at: a schema-1 line's own
    /// `recorded_at`, a schema-4 line's `serve_call.recorded_at`. The
    /// appender's clock for this envelope, so a rebuilt row never takes the
    /// rebuild's.
    pub recorded_at: i64,
    /// The envelope as the appender takes it: the line less its attribution
    /// and, on schema 1, less its `recorded_at`, which travels as the clock
    /// instead of as a member no live envelope carries.
    pub envelope: Vec<u8>,
    /// The session the serve was made in, when one did: the line's own
    /// `session_id`, which the rebuild writes back as the serve's attribution
    /// row. `None` is a serve no session made.
    pub session_id: Option<String>,
    /// The context window inside that session, when the writer knew it: the
    /// window journal's one-way key, never the ids it was hashed from.
    pub window_key: Option<String>,
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
///
/// **The recall ingest's one `/`-spelling chokepoint.** Every path the recall
/// projection keys a row by — file revisions, file reads, the changed hunks
/// between two revisions, the content→blob-oid map — is a `path` on one of
/// these records, and each one leaves here
/// [`slash_spelled`](crate::worktree::slash_spelled). It is done at the parse
/// rather than at each writer because a shard is *read* in exactly two places
/// and *written into rows* in several: normalising where the bytes become
/// records is the point past which no `\` exists to reach a row.
///
/// Shards themselves keep whatever they froze — they are canonical and
/// append-only, and a Windows capture from before the `/` rule wrote `\` into
/// `tool_calls.jsonl` — so this is also what makes a rebuild from that
/// history produce the spelling every lookup uses.
#[must_use]
pub fn parse_tool_calls(bytes: &[u8], checkpoint_id: &str) -> Outcome<Vec<ToolCallRecord>> {
    let mut out: Outcome<Vec<ToolCallRecord>> =
        parse_jsonl(bytes, "tool_calls.jsonl", checkpoint_id);
    for record in &mut out.value {
        if let Some(path) = record.path.as_mut() {
            if let std::borrow::Cow::Owned(respelled) = crate::worktree::slash_spelled(path) {
                *path = respelled;
            }
        }
    }
    out
}

/// Parse `token_estimates` — JSONL in some builds, a single object/array in
/// others. Tries JSONL first; that handles both a one-object file and a stream.
#[must_use]
pub fn parse_token_estimates(bytes: &[u8], checkpoint_id: &str) -> Outcome<Vec<TokenEstimate>> {
    parse_jsonl(bytes, "token_estimates.json", checkpoint_id)
}

/// Parse `test_results.json` — the captured test runs (the `TestsPassed`
/// outcome-label signal).
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

/// Parse a checkpoint's redacted `transcript.compact.jsonl`.
///
/// Unknown records and fields remain readable through the lenient typed view;
/// malformed and oversized lines are skipped and reported independently.
#[must_use]
pub fn parse_compact_transcript(
    bytes: &[u8],
    checkpoint_id: &str,
) -> Outcome<Vec<CompactTranscriptRecord>> {
    parse_compact_jsonl(bytes, "transcript.compact.jsonl", checkpoint_id)
}

/// Parse a nested task's redacted compact transcript. The artifact label is
/// intentionally structural rather than embedding an untrusted task id.
#[must_use]
pub fn parse_task_compact_transcript(
    bytes: &[u8],
    checkpoint_id: &str,
) -> Outcome<Vec<CompactTranscriptRecord>> {
    parse_compact_jsonl(bytes, "tasks/*/transcript.compact.jsonl", checkpoint_id)
}

fn parse_compact_jsonl(
    bytes: &[u8],
    artifact: &'static str,
    checkpoint_id: &str,
) -> Outcome<Vec<CompactTranscriptRecord>> {
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
    for (line_ordinal, line) in bytes.split(|&byte| byte == b'\n').enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_JSONL_LINE_BYTES {
            issues.push(oversize(
                checkpoint_id,
                artifact,
                Locator::Line(line_ordinal + 1),
                line.len(),
            ));
            continue;
        }
        match serde_json::from_slice::<CompactTranscriptRecord>(line) {
            Ok(mut record) => {
                record.artifact_line = line_ordinal;
                value.push(record);
            }
            Err(error) => issues.push(ParseIssue {
                checkpoint_id: checkpoint_id.to_owned(),
                artifact,
                locator: Locator::Line(line_ordinal + 1),
                kind: IssueKind::Corrupt,
                detail: redact_err(&error),
            }),
        }
    }
    Outcome { value, issues }
}

/// Parse `tasks/<tool-use-id>/metadata.json`.
#[must_use]
pub fn parse_task_metadata(bytes: &[u8], checkpoint_id: &str) -> Outcome<TaskMetadata> {
    parse_object(bytes, "tasks/*/metadata.json", checkpoint_id)
}

/// Remove terminal-control and bidi-control characters from text selected for
/// display. Newline and tab are retained; carriage return is normalised to a
/// newline so it cannot return the cursor and overwrite already-rendered text.
///
/// The replacement marker makes the omission visible instead of silently
/// joining attacker-controlled text around a removed control sequence.
#[must_use]
pub fn sanitize_terminal_text(input: &str) -> String {
    input
        .chars()
        .map(|ch| match ch {
            '\r' => '\n',
            '\u{0000}'..='\u{0008}'
            | '\u{000b}'..='\u{001f}'
            | '\u{007f}'..='\u{009f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => '\u{fffd}',
            _ => ch,
        })
        .collect()
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

/// Parse `serve_audit.jsonl`: one frozen serve envelope per line.
///
/// A line is accepted only in the shape the canonical writer freezes — a
/// JSON object of schema 1 or 4 with a `memd.` verb, a string key and a
/// payload object; schema 1 also its integer `recorded_at`, schema 4 a
/// `serve_call` whose `call_id` is the key; either may carry the session
/// that made it and, beside that, the window inside it — and with no other
/// top-level member. Anything else is one issue for that line, and the walk goes on:
/// the appender would decline most of these anyway, but a schema-1 line
/// without its instant it would book at the rebuild's clock, which is why the
/// shape is checked here rather than left to it.
#[must_use]
pub fn parse_serve_audit(bytes: &[u8], checkpoint_id: &str) -> Outcome<Vec<ServeAuditEnvelope>> {
    const ARTIFACT: &str = "serve_audit.jsonl";
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Outcome {
            value: Vec::new(),
            issues: vec![oversize(
                checkpoint_id,
                ARTIFACT,
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
                ARTIFACT,
                Locator::Line(n + 1),
                line.len(),
            ));
            continue;
        }
        match serve_audit_envelope(line) {
            Ok(envelope) => value.push(envelope),
            Err(detail) => issues.push(ParseIssue {
                checkpoint_id: checkpoint_id.to_owned(),
                artifact: ARTIFACT,
                locator: Locator::Line(n + 1),
                kind: IssueKind::Corrupt,
                detail,
            }),
        }
    }
    Outcome { value, issues }
}

/// One line of `serve_audit.jsonl` validated, or why not — in words of its
/// shape only, never a byte of the line.
fn serve_audit_envelope(line: &[u8]) -> Result<ServeAuditEnvelope, String> {
    let mut env = match serde_json::from_slice::<Map<String, Value>>(line) {
        Ok(env) => env,
        Err(err) => return Err(redact_err(&err)),
    };
    let schema = env.get("schema").and_then(Value::as_u64);
    let members: &[&str] = match schema {
        Some(1) => &[
            "idempotency_key",
            "payload",
            "recorded_at",
            "schema",
            "session_id",
            "verb",
            "window_key",
        ],
        Some(4) => &[
            "idempotency_key",
            "payload",
            "schema",
            "serve_call",
            "session_id",
            "verb",
            "window_key",
        ],
        _ => return Err("not a schema-1 or schema-4 serve envelope".to_owned()),
    };
    if env.keys().any(|member| !members.contains(&member.as_str())) {
        return Err("a top-level member the envelope does not declare".to_owned());
    }
    let (session_id, window_key) = take_attribution(&mut env)?;
    let Some(key) = env.get("idempotency_key").and_then(Value::as_str) else {
        return Err("no string idempotency key".to_owned());
    };
    let key = key.to_owned();
    let is_memd_verb = env
        .get("verb")
        .and_then(Value::as_str)
        .is_some_and(|verb| verb.starts_with("memd."));
    if !is_memd_verb || !env.get("payload").is_some_and(Value::is_object) {
        return Err("no serve verb or no payload object".to_owned());
    }
    let recorded_at = if schema == Some(4) {
        serve_call_instant(&env, &key)?
    } else {
        match env.remove("recorded_at").as_ref().and_then(Value::as_i64) {
            Some(recorded_at) => recorded_at,
            None => return Err("a schema-1 line without an integer recorded_at".to_owned()),
        }
    };
    let envelope = serde_json::to_vec(&env).map_err(|err| redact_err(&err))?;
    Ok(ServeAuditEnvelope {
        idempotency_key: key,
        recorded_at,
        envelope,
        session_id,
        window_key,
    })
}

/// Take a line's attribution out of it: the session that made the serve and
/// the window inside that session, each checked against its own grammar.
///
/// A window without a session is refused rather than read as unattributed,
/// because no writer produces one. The grammars are the ones the writer
/// freezes under — a session id that is safe to name a file with, and a
/// window key of 64 lowercase hex — restated here because this crate reads
/// shards and depends on no capture crate.
fn take_attribution(
    env: &mut Map<String, Value>,
) -> Result<(Option<String>, Option<String>), String> {
    let session_id = match env.remove("session_id") {
        None => None,
        Some(Value::String(id)) if is_session_id(&id) => Some(id),
        Some(_) => return Err("a session_id that is not a session id".to_owned()),
    };
    let window_key = match env.remove("window_key") {
        None => None,
        Some(Value::String(key)) if is_window_key(&key) => Some(key),
        Some(_) => return Err("a window_key that is not a window key".to_owned()),
    };
    if session_id.is_none() && window_key.is_some() {
        return Err("a window_key without a session_id".to_owned());
    }
    Ok((session_id, window_key))
}

/// A session id that may name a file: non-empty, at most 200 bytes of ASCII
/// alphanumerics, `-`, `_` and `.`, not starting with `.`.
fn is_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// A window journal key: 64 lowercase hex.
fn is_window_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A schema-4 line's own instant, once its call record is the one the
/// appender books: an object whose `call_id` is the envelope's key.
fn serve_call_instant(env: &Map<String, Value>, key: &str) -> Result<i64, String> {
    let Some(Value::Object(call)) = env.get("serve_call") else {
        return Err("no serve_call object".to_owned());
    };
    if call.get("call_id").and_then(Value::as_str) != Some(key) {
        return Err("a serve_call whose call_id is not the key".to_owned());
    }
    match call.get("recorded_at").and_then(Value::as_i64) {
        Some(recorded_at) => Ok(recorded_at),
        None => Err("a serve_call without an integer recorded_at".to_owned()),
    }
}

/// Split a JSONL artifact into lines and validate each with `validate`,
/// under the same size caps as every other artifact: a refused line is one
/// [`IssueKind::Corrupt`] issue carrying the validator's words, never its bytes.
fn parse_validated_lines<T>(
    bytes: &[u8],
    artifact: &'static str,
    checkpoint_id: &str,
    validate: fn(&[u8]) -> Result<T, String>,
) -> Outcome<Vec<T>> {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        let issue = oversize(checkpoint_id, artifact, Locator::WholeFile, bytes.len());
        return Outcome {
            value: Vec::new(),
            issues: vec![issue],
        };
    }
    let mut value = Vec::new();
    let mut issues = Vec::new();
    for (n, line) in bytes.split(|&b| b == b'\n').enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let locator = Locator::Line(n + 1);
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_JSONL_LINE_BYTES {
            issues.push(oversize(checkpoint_id, artifact, locator, line.len()));
            continue;
        }
        match validate(line) {
            Ok(parsed) => value.push(parsed),
            Err(detail) => issues.push(ParseIssue {
                checkpoint_id: checkpoint_id.to_owned(),
                artifact,
                locator,
                kind: IssueKind::Corrupt,
                detail,
            }),
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

    /// The chokepoint, on the shape that made it necessary: a shard frozen by
    /// a Windows capture before the `/` rule. The bytes on the branch keep
    /// their `\` — history is append-only — and every record that leaves this
    /// function is `/`-spelled, so a rebuild from that history keys its rows
    /// the way every lookup spells them.
    #[test]
    fn a_backslash_spelled_shard_parses_into_slash_spelled_records() {
        let blob = br#"{"id":"t1","tool":"Bash","path":"bins\\rift\\src\\hooks.rs","sha256":"aa"}
{"id":"t2","tool":"Read","path":"docs/guide/06-recall.md","sha256":"bb"}
"#;
        let out = parse_tool_calls(blob, "cp");
        assert!(out.is_clean(), "{:?}", out.issues);
        let paths: Vec<&str> = out.value.iter().filter_map(|r| r.path.as_deref()).collect();
        assert_eq!(
            paths,
            vec!["bins/rift/src/hooks.rs", "docs/guide/06-recall.md"]
        );
        assert!(
            !paths.iter().any(|p| p.contains('\\')),
            "no record leaves the parser carrying a separator no row may hold"
        );
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
    fn compact_transcript_is_lenient_and_skips_only_bad_lines() {
        let blob = br#"{"v":1,"type":"user","content":"hello","future":true}
not json
{"type":"assistant","message":{"content":"done"}}
"#;
        let out = parse_compact_transcript(blob, "cp");
        assert_eq!(out.value.len(), 2);
        assert_eq!(out.issues.len(), 1);
        assert_eq!(out.issues[0].locator, Locator::Line(2));
        assert_eq!(out.value[0].record_type.as_deref(), Some("user"));
        assert_eq!(out.value[0].artifact_line, 0);
        assert_eq!(out.value[1].artifact_line, 2);
        assert_eq!(
            out.value[0].extra.get("future"),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn native_nested_task_metadata_is_bounded_and_lenient() {
        let out = parse_task_metadata(
            br#"{"schema_version":1,"tool_use_id":"toolu_1","agent_id":"explore","started_at_ms":10,"ended_at_ms":20,"compact_transcript":"transcript.compact.jsonl","compact_transcript_start":2,"future":1}"#,
            "cp",
        );
        assert!(out.is_clean());
        assert_eq!(out.value.tool_use_id, "toolu_1");
        assert_eq!(out.value.compact_transcript_start, Some(2));
    }

    #[test]
    fn oversized_compact_transcript_is_reported_without_partial_records() {
        let big = vec![b'x'; MAX_ARTIFACT_BYTES + 1];
        let out = parse_compact_transcript(&big, "cp");
        assert!(out.value.is_empty());
        assert_eq!(out.issues.len(), 1);
        assert_eq!(out.issues[0].kind, IssueKind::OversizeSkipped);
    }

    #[test]
    fn terminal_controls_cannot_reposition_or_bidi_spoof() {
        let sanitized = sanitize_terminal_text("ok\u{1b}[2J\rnext\u{202e}txt\u{009b}");
        assert_eq!(sanitized, "ok�[2J\nnext�txt�");
        assert!(!sanitized.contains('\u{1b}'));
        assert!(!sanitized.contains('\u{202e}'));
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

    /// The maximal fixture's two lines: a schema-4 call and its schema-1
    /// audit twin, which carries the instant its live row was written at;
    /// both name the session and the window that made them.
    const FIXTURE_CALL: &str = r#"{"idempotency_key":"01JSERVEFIXTURE","payload":{"kind":"read_intercepted"},"schema":4,"serve_call":{"call_id":"01JSERVEFIXTURE","channel":"hook_inject","recorded_at":1755999993000,"tokens_avoided":1200,"tokens_returned":40,"tool_name":"claude-code:read-gate"},"session_id":"01JSHARDGOLDENA","verb":"memd.read_intercepted","window_key":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"}"#;
    const FIXTURE_AUDIT: &str = r#"{"idempotency_key":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","payload":{"estimated_tokens_avoided":1200,"estimated_tokens_returned":40,"handle":"file:crates/rift-shadow/src/backend.rs@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:L1-40","kind":"read_intercepted","reason":"already_held"},"recorded_at":1755999993000,"schema":1,"session_id":"01JSHARDGOLDENA","verb":"memd.read_intercepted","window_key":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"}"#;

    #[test]
    fn both_frozen_schemas_parse_into_the_envelope_the_appender_takes() {
        let blob = format!("{FIXTURE_CALL}\n{FIXTURE_AUDIT}\n");
        let out = parse_serve_audit(blob.as_bytes(), "cp");
        assert!(out.is_clean(), "{:?}", out.issues);
        let [call, audit] = out.value.as_slice() else {
            panic!("two envelopes: {:?}", out.value);
        };
        assert_eq!(call.idempotency_key, "01JSERVEFIXTURE");
        assert_eq!(call.recorded_at, 1_755_999_993_000);
        let window = Some("e".repeat(64));
        for parsed in [call, audit] {
            assert_eq!(parsed.session_id.as_deref(), Some("01JSHARDGOLDENA"));
            assert_eq!(parsed.window_key, window);
            let handed: serde_json::Value = serde_json::from_slice(&parsed.envelope).unwrap();
            for member in ["session_id", "window_key", "recorded_at"] {
                assert_eq!(handed.get(member), None, "{member} is not the appender's");
            }
        }
        assert_eq!(audit.idempotency_key, "d".repeat(64));
        assert_eq!(audit.recorded_at, 1_755_999_993_000);
        let handed: serde_json::Value = serde_json::from_slice(&audit.envelope).unwrap();
        assert_eq!(handed["idempotency_key"], "d".repeat(64));
        assert_eq!(handed["payload"]["reason"], "already_held");
        let handed: serde_json::Value = serde_json::from_slice(&call.envelope).unwrap();
        assert_eq!(handed["serve_call"]["call_id"], "01JSERVEFIXTURE");
    }

    /// A serve no session made is a line with neither member, and it parses
    /// with no attribution rather than with an invented one.
    #[test]
    fn a_line_without_a_session_parses_unattributed() {
        let mut bare: serde_json::Value = serde_json::from_str(FIXTURE_AUDIT).unwrap();
        let members = bare.as_object_mut().unwrap();
        members.remove("session_id");
        members.remove("window_key");
        let out = parse_serve_audit(bare.to_string().as_bytes(), "cp");
        assert!(out.is_clean(), "{:?}", out.issues);
        assert_eq!(out.value[0].session_id, None);
        assert_eq!(out.value[0].window_key, None);

        let mut sessioned: serde_json::Value = serde_json::from_str(FIXTURE_CALL).unwrap();
        sessioned.as_object_mut().unwrap().remove("window_key");
        let out = parse_serve_audit(sessioned.to_string().as_bytes(), "cp");
        assert!(out.is_clean(), "{:?}", out.issues);
        assert_eq!(out.value[0].session_id.as_deref(), Some("01JSHARDGOLDENA"));
        assert_eq!(
            out.value[0].window_key, None,
            "a window the writer did not know"
        );
    }

    /// Every refusal is one issue naming the checkpoint and the line, the
    /// walk goes on to the good line after it, and no issue repeats a byte of
    /// the line it refused.
    #[test]
    fn a_refused_serve_line_is_an_issue_that_never_stops_the_walk_or_echoes_it() {
        let edit = |line: &str, pointer: &str, to: serde_json::Value| {
            let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
            *value.pointer_mut(pointer).unwrap() = to;
            value.to_string()
        };
        let mut foreign: serde_json::Value = serde_json::from_str(FIXTURE_AUDIT).unwrap();
        foreign["secret_note_zq"] = "hunter2-zq".into();
        let mut clockless: serde_json::Value = serde_json::from_str(FIXTURE_AUDIT).unwrap();
        clockless.as_object_mut().unwrap().remove("recorded_at");
        let refused = [
            foreign.to_string(),
            edit(
                FIXTURE_CALL,
                "/serve_call/call_id",
                "another-call-zq".into(),
            ),
            edit(FIXTURE_AUDIT, "/schema", 2.into()),
            edit(FIXTURE_AUDIT, "/schema", 3.into()),
            clockless.to_string(),
            edit(FIXTURE_AUDIT, "/verb", "retrieve.zq".into()),
            edit(FIXTURE_AUDIT, "/session_id", "../zq".into()),
            edit(FIXTURE_CALL, "/session_id", 7.into()),
            edit(FIXTURE_CALL, "/window_key", "zq-not-hex".into()),
            windowless_session(),
            "not json at all zq".to_owned(),
        ];
        let mut blob = String::new();
        for line in &refused {
            blob.push_str(line);
            blob.push('\n');
        }
        blob.push_str(FIXTURE_CALL);
        let out = parse_serve_audit(blob.as_bytes(), "cp-serves");
        assert_eq!(out.value.len(), 1, "the good line after them still parses");
        assert_eq!(out.issues.len(), refused.len(), "{:?}", out.issues);
        for (n, issue) in out.issues.iter().enumerate() {
            assert_eq!(issue.checkpoint_id, "cp-serves");
            assert_eq!(issue.artifact, "serve_audit.jsonl");
            assert_eq!(issue.locator, Locator::Line(n + 1));
            assert_eq!(issue.kind, IssueKind::Corrupt);
            assert!(!issue.detail.contains("zq"), "{}", issue.detail);
            assert!(!issue.detail.contains("memd"), "{}", issue.detail);
        }
    }

    /// The fixture's call line with its window and no session: a shape no
    /// writer produces.
    fn windowless_session() -> String {
        let mut value: serde_json::Value = serde_json::from_str(FIXTURE_CALL).unwrap();
        value.as_object_mut().unwrap().remove("session_id");
        value.to_string()
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
            let _ = parse_compact_transcript(&bytes, "cp");
            let _ = parse_task_metadata(&bytes, "cp");
            let _ = parse_merkle_leaves(&bytes, "cp");
            let _ = parse_serve_audit(&bytes, "cp");
            let _ = window_journal::parse_window_journal(&bytes, "cp");
        }
    }
}
