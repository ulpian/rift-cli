//! The **deterministic compact transcript**.
//!
//! A checkpoint should carry the conversation that produced it without carrying
//! every byte the agent read. The legacy backend's answer (consulted read-only at the pinned
//! tag, `upstream: transcript/compact/compact.go`) is a mechanical
//! reduction: tool calls keep their identifiers, tool results keep their shape
//! and lose their payload, and the whole thing stays JSONL so a reader can
//! replay it line by line. Rift takes that behaviour and nothing else — **no
//! model runs here**, so the same bytes always compact to the same
//! bytes and a checkpoint's transcript is reproducible rather than merely
//! plausible.
//!
//! Three properties this module is built around, each with the test that would
//! redden if it stopped being true:
//!
//! * **It is a pure function.** Same input, same output, byte for byte —
//!   asserted against bytes an EARLIER build froze into the tree
//!   (`compaction_reproduces_the_frozen_golden_bytes` over the maximal
//!   shard fixture), never against a second call in the same
//!   process, which is what the sentence used to be backed by and which no
//!   implementation can fail. Keys are emitted through `serde_json`'s `Map` (a
//!   `BTreeMap`), so ordering is the type's, not the writer's.
//! * **It never crosses the redaction gate.** Input is [`SecretBytes`] and so is
//!   output: compaction happens on the *raw* side, and the compact bytes reach
//!   durable storage only by being handed to `rift-shadow-redact::redact` like
//!   any other capture (`the_compact_output_is_still_secret_bytes`). This is
//!   the deliberate answer to a real constraint — a compaction performed
//!   *after* redaction would need a second `RedactedBytes` constructor, which
//!   the one-`seal`-site rule forbids and the owner has already ruled on.
//! * **What it drops, it says.** Records whose shape this module does not
//!   recognise are passed through and COUNTED
//!   (`CompactTranscript::unrecognised`), never silently reshaped. The
//!   elision guarantees below are scoped to recognised records, and the counter
//!   is how a caller can tell which it got.
//!
//! # The boundary, and why it overlaps
//!
//! A checkpoint records `compact_transcript_start`: the 0-based index into the
//! compact lines at which *this* checkpoint's slice begins. The shard carries
//! the whole compact transcript, so each checkpoint is self-contained; the index
//! says which part of it is new.
//!
//! It is derived, not counted:
//! `full_lines - delta_lines`, where `delta` is the compaction of the raw lines
//! from `start_line` on. That subtraction is what makes the boundary **round
//! toward inclusion**. Compaction merges streamed assistant fragments that share
//! a message id into one line, so a group straddling `start_line` becomes one
//! line in `full` and one line in `delta` — the counts differ by one less than a
//! naive split, the index lands one line earlier, and that line appears in BOTH
//! checkpoints' slices.
//!
//! **The rule is one line per straddling merge-group, not one line full stop**,
//! and the difference is a measurement rather than a nicety. This header used to
//! say "at most one line"; two streamed messages whose fragments interleave
//! across `start_line` are two straddling groups and overlap by two, which
//! `tests::overlap_is_one_line_per_straddling_merge_group` measures at 0, 1 and
//! 2 groups so the bound cannot silently shrink back. Overlap is bounded by the
//! number of merge-groups alive at the boundary, and nothing in the format bounds
//! that at one. The rounding direction is what matters and it is unchanged: a
//! duplicated line, never a lost half-message.

use rift_core::{Result, SecretBytes};
use serde_json::{Map, Value};

use crate::backend::ArchivedTranscript;

/// The compact-line schema version, carried on every emitted line as `v`.
///
/// A `/2` is required if an existing field's meaning changes; new optional
/// fields do not need one.
pub const COMPACT_SCHEMA_VERSION: u64 = 1;

/// The longest string leaf a compact record keeps inline. Anything longer is
/// replaced by an elision handle (`{"elided_chars":N,"elided_lines":M}`).
///
/// This is the whole size story: one rule, applied to tool-call inputs and tool
/// results alike, rather than a per-tool table that would need a new entry every
/// time an agent grows a tool. Assistant *text* is never elided — the
/// conversation is what a transcript is for.
pub const MAX_INLINE_CHARS: usize = 512;

/// A session's compact transcript plus the boundary a checkpoint records.
///
/// No `Debug`: [`Self::compact`] is [`SecretBytes`], which withholds it so raw
/// capture never reaches a log line.
pub struct CompactTranscript {
    /// The whole session, compacted, as JSONL — **self-contained**, so a reader
    /// with one shard needs no other shard.
    compact: SecretBytes,
    /// 0-based index into [`Self::lines`] at which this checkpoint's slice
    /// begins. Rounds toward inclusion (module header), so consecutive
    /// checkpoints share one line per merge-group that straddles the boundary —
    /// usually zero, one when a streamed message is cut in half, and more when
    /// several are.
    pub compact_transcript_start: usize,
    /// How many compact lines the whole session produced.
    pub lines: usize,
    /// How many **non-empty raw lines** the input held — which is exactly the
    /// value to pass as the next checkpoint's `start_line`, because it is the
    /// count taken by the same iterator that `start_line` skips.
    ///
    /// It exists so a caller never has to know that blank segments are not
    /// records. Counting raw lines with `split('\n').count()` and passing that
    /// would skip too far by one per blank line (and by one for a trailing
    /// newline), which is an off-by-N no test would have caught while the
    /// compactor had no caller.
    pub raw_lines: usize,
    /// Raw lines that were not JSON objects at all, dropped and counted. The
    /// input is an agent-written file this module does not own; a parse failure
    /// is a fact to report, never a panic and never a silent skip.
    pub unparsed: usize,
    /// Recognised-as-JSON records whose `content` this module could not reduce,
    /// passed through unchanged. The elision guarantees do not cover these, and
    /// this counter is how a caller knows.
    pub unrecognised: usize,
}

impl CompactTranscript {
    /// The compact JSONL bytes — still raw, still local-only, still owed to the
    /// redaction gate before anything durable may hold them.
    #[must_use]
    pub fn compact(&self) -> &SecretBytes {
        &self.compact
    }
}

/// Compact a JSONL agent transcript, recording where `start_line`'s slice begins.
///
/// `start_line` is a 0-based index into the **non-empty** raw lines — the offset
/// the previous checkpoint stopped at, which is what that checkpoint's
/// [`CompactTranscript::raw_lines`] reported. Blank segments are not records and
/// are not counted, so the two sides agree by sharing one iterator rather than by
/// convention. `0` means "the whole session is this checkpoint's slice"; a value
/// past the end means nothing here is new. Both yield a boundary that is safe:
/// 0 and `lines` respectively.
///
/// # Panics
/// Never. Every failure mode of the input — invalid UTF-8, a truncated final
/// line, a record that is not an object — is counted, not raised: this reads a
/// file an agent is still writing.
#[must_use]
pub fn compact_jsonl(raw: &SecretBytes, start_line: usize) -> CompactTranscript {
    let full = compact_lines(raw.as_slice(), 0);
    let delta = compact_lines(raw.as_slice(), start_line);
    // The subtraction IS the rounding rule — see the module header.
    let boundary = full.lines.len().saturating_sub(delta.lines.len());

    let mut out = Vec::new();
    for line in &full.lines {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    CompactTranscript {
        compact: SecretBytes::new(out),
        compact_transcript_start: boundary,
        lines: full.lines.len(),
        raw_lines: full.raw_lines,
        unparsed: full.unparsed,
        unrecognised: full.unrecognised,
    }
}

/// Compact and seal one session transcript for archiving — **the one writer of
/// the raw→artifact flow**: the compactor runs on the
/// RAW side, then each of the two artifacts passes the redaction gate once.
/// Returns the artifact pair plus [`CompactTranscript::raw_lines`], which the
/// caller records as the NEXT checkpoint's `start_line` — that count must come
/// from the compactor's own iterator (its doc explains the off-by-N a second
/// counter reintroduces), which is why this function hands it out rather than
/// letting the caller re-count.
///
/// # Errors
/// [`rift_core::RiftError`] if redaction fails; the compactor itself cannot
/// fail (every malformed input is counted, not raised).
pub fn archived_transcript(
    raw: &SecretBytes,
    start_line: usize,
    packs: &rift_shadow_redact::RulePackSet,
) -> Result<(ArchivedTranscript, usize)> {
    let compacted = compact_jsonl(raw, start_line);
    let full = rift_shadow_redact::redact_with(raw, packs)?;
    let compact = rift_shadow_redact::redact_with(compacted.compact(), packs)?;
    Ok((
        ArchivedTranscript {
            full: full.redacted,
            compact: compact.redacted,
            compact_start: compacted.compact_transcript_start,
            spills: Vec::new(),
            originals: Vec::new(),
        },
        compacted.raw_lines,
    ))
}

/// Seal one spilled tool output — the file a transcript line names as holding
/// the result it only previews — through the same gate, with the same packs,
/// as the transcript that names it. Not compacted: a spill is the result's
/// own bytes, and the compact transcript already elides results.
///
/// # Errors
/// [`rift_core::RiftError`] if redaction fails.
pub fn sealed_spill(
    raw: &SecretBytes,
    packs: &rift_shadow_redact::RulePackSet,
) -> Result<rift_core::RedactedBytes> {
    Ok(rift_shadow_redact::redact_with(raw, packs)?.redacted)
}

/// Seal one spooled original — the record of a tool result a hook rewrote,
/// read back from the spool as [`SecretBytes`] — through the same gate, with
/// the same packs, as the transcript it is sealed beside. The spool already
/// holds redacted bytes, so this is a second pass: idempotent on every sample
/// measured, and the stricter of the two when the repository's packs changed
/// in between. No sealed blob of an earlier shard is reused; every archive
/// passes the gate again, through its one constructor.
///
/// # Errors
/// [`rift_core::RiftError`] if redaction fails.
pub fn sealed_original(
    raw: &SecretBytes,
    packs: &rift_shadow_redact::RulePackSet,
) -> Result<rift_core::RedactedBytes> {
    Ok(rift_shadow_redact::redact_with(raw, packs)?.redacted)
}

/// The compaction of one slice: its lines and what it could not read.
struct Compacted {
    lines: Vec<Vec<u8>>,
    /// Non-empty raw lines consumed — counted by the loop that skips
    /// `start_line` of them, so the resume offset and the skip cannot disagree.
    raw_lines: usize,
    unparsed: usize,
    unrecognised: usize,
}

/// Compact the raw lines from `start_line` on, merging streamed assistant
/// fragments that share a message id.
fn compact_lines(raw: &[u8], start_line: usize) -> Compacted {
    let mut lines: Vec<Vec<u8>> = Vec::new();
    // Message id → index in `lines`, so a later fragment of the same assistant
    // message extends the line it already produced instead of adding one.
    let mut by_id: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut unparsed = 0usize;
    let mut unrecognised = 0usize;
    let mut raw_lines = 0usize;

    for raw_line in raw
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .skip(start_line)
    {
        raw_lines += 1;
        let Ok(value) = serde_json::from_slice::<Value>(raw_line) else {
            unparsed += 1;
            continue;
        };
        let Some(record) = value.as_object() else {
            unparsed += 1;
            continue;
        };
        let (mut obj, recognised) = compact_record(record);
        if !recognised {
            unrecognised += 1;
        }
        let id = obj.get("id").and_then(Value::as_str).map(str::to_owned);
        if let Some(at) = id.as_ref().and_then(|i| by_id.get(i).copied()) {
            // A streamed continuation of a message already emitted: merge its
            // content into that line. This is the only reason the boundary can
            // overlap, and dropping it would not make the boundary exact — it
            // would split one message across two checkpoints with nothing
            // saying so.
            let merged = lines.get(at).and_then(|line| merge_content(line, &obj));
            if let (Some(merged), Some(slot)) = (merged, lines.get_mut(at)) {
                *slot = merged;
            }
        } else {
            if let Some(i) = id {
                by_id.insert(i, lines.len());
            }
            obj.insert("v".to_owned(), COMPACT_SCHEMA_VERSION.into());
            lines.push(canonical(&Value::Object(obj)));
        }
    }
    Compacted {
        lines,
        raw_lines,
        unparsed,
        unrecognised,
    }
}

/// Append `next`'s content blocks to the already-emitted `line`'s.
fn merge_content(line: &[u8], next: &Map<String, Value>) -> Option<Vec<u8>> {
    let mut existing: Map<String, Value> = serde_json::from_slice(line).ok()?;
    let mut blocks = match existing.get("content") {
        Some(Value::Array(a)) => a.clone(),
        _ => return None,
    };
    match next.get("content") {
        Some(Value::Array(a)) => blocks.extend(a.iter().cloned()),
        Some(other) => blocks.push(other.clone()),
        None => return None,
    }
    existing.insert("content".to_owned(), Value::Array(blocks));
    Some(canonical(&Value::Object(existing)))
}

/// Reduce one transcript record. Returns the compact object and whether its
/// content shape was recognised.
fn compact_record(record: &Map<String, Value>) -> (Map<String, Value>, bool) {
    let mut out = Map::new();
    if let Some(t) = record.get("type").and_then(Value::as_str) {
        out.insert("type".to_owned(), t.into());
    }
    for key in ["timestamp", "ts"] {
        if let Some(ts) = record.get(key) {
            out.insert("ts".to_owned(), ts.clone());
            break;
        }
    }
    // The agent's message envelope, when there is one; otherwise the record
    // itself carries the fields (the flatter dialects).
    let message = record
        .get("message")
        .and_then(Value::as_object)
        .unwrap_or(record);
    if let Some(id) = message.get("id").and_then(Value::as_str) {
        out.insert("id".to_owned(), id.into());
    }
    if let Some(usage) = message.get("usage").and_then(Value::as_object) {
        for key in ["input_tokens", "output_tokens"] {
            if let Some(n) = usage.get(key) {
                out.insert(key.to_owned(), n.clone());
            }
        }
    }
    match message.get("content") {
        Some(Value::Array(blocks)) => {
            let reduced: Vec<Value> = blocks.iter().filter_map(compact_block).collect();
            out.insert("content".to_owned(), Value::Array(reduced));
            (out, true)
        }
        Some(Value::String(text)) => {
            out.insert("content".to_owned(), text.as_str().into());
            (out, true)
        }
        Some(other) => {
            // Structurally unknown: passed through, and the caller is told.
            out.insert("content".to_owned(), other.clone());
            (out, false)
        }
        None => (out, false),
    }
}

/// Reduce one content block. `None` drops it.
fn compact_block(block: &Value) -> Option<Value> {
    let obj = block.as_object()?;
    let mut out = Map::new();
    match obj.get("type").and_then(Value::as_str) {
        // The conversation itself — kept verbatim, never elided.
        Some("text") => {
            out.insert("type".to_owned(), "text".into());
            out.insert("text".to_owned(), obj.get("text")?.clone());
        }
        // Extended thinking does not replay: the JSONL walker masks a
        // `signature` as high-entropy base64 (the walker says so in its own
        // header), so keeping the block would keep bytes
        // that cannot be verified and cost the space anyway.
        Some("thinking" | "redacted_thinking") => return None,
        // A tool call reduced to bare identifiers plus its arguments, with any
        // bulky argument elided to a handle.
        Some("tool_use") => {
            out.insert("type".to_owned(), "tool_use".into());
            for key in ["id", "name"] {
                if let Some(v) = obj.get(key) {
                    out.insert(key.to_owned(), v.clone());
                }
            }
            if let Some(input) = obj.get("input") {
                out.insert("input".to_owned(), elide(input));
            }
        }
        // A tool result keeps its shape and loses its payload: this is the
        // file-read elision, and it is the reason a compact transcript is small.
        Some("tool_result") => {
            out.insert("type".to_owned(), "tool_result".into());
            for key in ["tool_use_id", "is_error"] {
                if let Some(v) = obj.get(key) {
                    out.insert(key.to_owned(), v.clone());
                }
            }
            if let Some(content) = obj.get("content") {
                out.insert("result".to_owned(), elide(content));
            }
        }
        _ => return Some(block.clone()),
    }
    Some(Value::Object(out))
}

/// Replace every string leaf longer than [`MAX_INLINE_CHARS`] with an elision
/// handle, recursively. Counts are in **chars and lines**, never a digest: the
/// compact bytes go through the entropy redactor on their way to storage, which
/// would mask a hex digest and leave a handle that resolves to nothing.
fn elide(value: &Value) -> Value {
    match value {
        Value::String(s) if s.chars().count() > MAX_INLINE_CHARS => {
            let mut handle = Map::new();
            handle.insert("elided_chars".to_owned(), s.chars().count().into());
            handle.insert("elided_lines".to_owned(), s.split('\n').count().into());
            Value::Object(handle)
        }
        Value::Array(a) => Value::Array(a.iter().map(elide).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), elide(v))).collect()),
        other => other.clone(),
    }
}

/// Serialize to canonical bytes: `serde_json`'s `Map` is a `BTreeMap`, so key
/// order is the type's and the output is byte-stable.
fn canonical(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One streamed assistant message, in `n` fragments sharing a message id.
    fn fragments(id: &str, texts: &[&str]) -> Vec<String> {
        texts
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "assistant",
                    "timestamp": "2026-08-12T00:00:00Z",
                    "message": {"id": id, "content": [{"type": "text", "text": t}]}
                })
                .to_string()
            })
            .collect()
    }

    fn jsonl(lines: &[String]) -> SecretBytes {
        SecretBytes::new(lines.join("\n").into_bytes())
    }

    fn text_of(compact: &CompactTranscript) -> String {
        String::from_utf8(compact.compact().as_slice().to_vec()).unwrap()
    }

    /// A tool call keeps its identifiers and loses its payload; a tool result
    /// keeps its shape and loses the file. That is the whole reduction, and it
    /// is what makes a compact transcript worth storing beside the full one.
    #[test]
    fn tool_calls_reduce_to_identifiers_and_reads_elide_to_handles() {
        let big = "x".repeat(MAX_INLINE_CHARS + 1);
        let raw = jsonl(&[
            serde_json::json!({
                "type": "assistant",
                "message": {"id": "m1", "content": [
                    {"type": "text", "text": "reading"},
                    {"type": "thinking", "thinking": "…", "signature": "abcd"},
                    {"type": "tool_use", "id": "t1", "name": "Write",
                     "input": {"file_path": "a.rs", "content": big}}
                ]}
            })
            .to_string(),
            serde_json::json!({
                "type": "user",
                "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": big}
                ]}
            })
            .to_string(),
        ]);
        let out = compact_jsonl(&raw, 0);
        let text = text_of(&out);

        assert!(!text.contains(&big), "no elided payload may survive");
        assert!(
            text.contains("\"name\":\"Write\""),
            "the tool is named: {text}"
        );
        assert!(
            text.contains("\"id\":\"t1\""),
            "the call id survives: {text}"
        );
        assert!(
            text.contains("\"file_path\":\"a.rs\""),
            "a short argument stays inline: {text}"
        );
        assert!(
            text.contains(&format!("\"elided_chars\":{}", MAX_INLINE_CHARS + 1)),
            "a long one becomes a handle: {text}"
        );
        assert!(
            text.contains("\"tool_use_id\":\"t1\""),
            "the result links back"
        );
        assert!(!text.contains("signature"), "thinking blocks do not replay");
        assert!(
            text.contains("\"text\":\"reading\""),
            "the conversation stays"
        );
        assert_eq!(out.unparsed, 0);
        assert_eq!(out.unrecognised, 0);
    }

    /// **Byte stability against bytes an EARLIER build froze into the tree.**
    ///
    /// This replaces `compaction_is_byte_stable`, which was
    /// `assert_eq!(compact_jsonl(&raw, 0).compact(), compact_jsonl(&raw, 0).compact())`
    /// — `f(x) == f(x)` inside one process. **It could not fail.** Every
    /// possible compactor satisfies it, including one rewritten overnight to
    /// emit different bytes, different key order or a different schema version;
    /// the module header cited it for a claim it never held. And the tree had
    /// no way to notice, because **no fixture anywhere carried a transcript**,
    /// so nothing compared real compaction output with anything at all.
    ///
    /// The oracle is the maximal shard fixture: a committed raw
    /// transcript in, the committed compact and full bytes out, compared byte
    /// for byte. A changed merge rule, elision handle, key order, schema
    /// version or redaction outcome reds here — and each of those is exercised,
    /// because the fixture was chosen to contain them (see
    /// [`crate::shard_fixture`]).
    #[test]
    fn compaction_reproduces_the_frozen_golden_bytes() {
        let raw = SecretBytes::new(
            crate::shard_fixture::input("raw.jsonl").expect("committed fixture input"),
        );
        let packs = rift_shadow_redact::RulePackSet::builtin();
        let (pair, _) = archived_transcript(&raw, 0, &packs).expect("archiving the fixture");
        for (name, live) in [
            (
                crate::backend::TRANSCRIPT_COMPACT_NAME,
                pair.compact.as_slice(),
            ),
            (crate::backend::TRANSCRIPT_FULL_NAME, pair.full.as_slice()),
        ] {
            let frozen = std::fs::read(
                crate::shard_fixture::fixture_dir()
                    .join("artifacts")
                    .join(name),
            )
            .unwrap_or_else(|_| {
                panic!("{name}: no frozen golden — see crates/rift-shadow/tests/shard_goldens.rs")
            });
            assert_eq!(
                live,
                frozen.as_slice(),
                "{name} drifted from the bytes frozen in shard-goldens/"
            );
        }
    }

    /// …and the fixture's compaction is not trivially the input: the four
    /// reductions the frozen bytes are supposed to demonstrate are asserted on
    /// the live output, so a golden that stopped demonstrating them would be
    /// caught even while matching itself.
    #[test]
    fn the_frozen_fixture_actually_exercises_every_reduction() {
        let raw = SecretBytes::new(
            crate::shard_fixture::input("raw.jsonl").expect("committed fixture input"),
        );
        let out = compact_jsonl(&raw, 0);
        let text = String::from_utf8(out.compact().as_slice().to_vec()).expect("utf-8");
        assert_eq!(out.raw_lines, 10, "the fixture's non-empty raw lines");
        assert_eq!(out.lines, 8, "10 raw − 1 merged fragment − 1 unparsed");
        assert_eq!(out.unparsed, 1, "the deliberately non-JSON line");
        assert_eq!(
            out.unrecognised, 1,
            "the record shape this module does not model"
        );
        assert!(
            !text.contains("signature"),
            "a thinking signature never replays"
        );
        assert!(
            text.contains("\"elided_chars\""),
            "the long tool result elides to a handle"
        );
        assert!(
            text.contains("\"is_error\":true"),
            "the failure signal survives compaction"
        );
        assert!(
            text.matches("msg_01FIXTUREA").count() == 1,
            "the two streamed fragments merged into ONE line"
        );
    }

    /// **The boundary rounds toward inclusion, and this is the case that makes
    /// it do so.** Three fragments of one message; the previous checkpoint
    /// stopped after the first. The fragments merge into ONE compact line, so
    /// the delta is one line, the full is one line, and the boundary is 0 — this
    /// checkpoint's slice re-includes material the previous one already carried.
    /// One straddling group, one line of overlap, and never a lost half-message —
    /// the general bound is measured in
    /// [`overlap_is_one_line_per_straddling_merge_group`].
    #[test]
    fn a_straddling_message_is_included_rather_than_split() {
        let raw = jsonl(&fragments("m1", &["a", "b", "c"]));
        let out = compact_jsonl(&raw, 1);
        assert_eq!(out.lines, 1, "one message → one compact line");
        assert_eq!(
            out.compact_transcript_start, 0,
            "the straddling line is included, not skipped"
        );
        // …and the whole message is in the shard, so a reader of THIS checkpoint
        // alone sees all three fragments.
        let text = text_of(&out);
        for fragment in ["\"a\"", "\"b\"", "\"c\""] {
            assert!(text.contains(fragment), "{fragment} missing from {text}");
        }
    }

    /// With no straddle the boundary is exact: distinct messages, distinct
    /// lines, and the index is the count of what came before.
    #[test]
    fn distinct_messages_give_an_exact_boundary() {
        let raw = jsonl(&[
            fragments("m1", &["a"]).remove(0),
            fragments("m2", &["b"]).remove(0),
            fragments("m3", &["c"]).remove(0),
        ]);
        assert_eq!(compact_jsonl(&raw, 0).compact_transcript_start, 0);
        assert_eq!(compact_jsonl(&raw, 2).compact_transcript_start, 2);
        // Past the end is not an error: nothing is new, everything is prior.
        assert_eq!(compact_jsonl(&raw, 99).compact_transcript_start, 3);
    }

    /// The input is a file an agent is still writing, so every way it can be
    /// wrong is counted rather than raised — and a record this module cannot
    /// reduce is passed through and COUNTED, so a caller is never told the
    /// elision guarantees held when they did not apply.
    #[test]
    fn unreadable_and_unrecognised_records_are_counted_not_dropped_silently() {
        let raw = SecretBytes::new(
            [
                "{not json",
                "[1,2,3]",
                r#"{"type":"weird","message":{"content":{"shape":"unknown"}}}"#,
                r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"ok"}]}}"#,
            ]
            .join("\n")
            .into_bytes(),
        );
        let out = compact_jsonl(&raw, 0);
        assert_eq!(out.unparsed, 2, "a broken line and a non-object");
        assert_eq!(out.unrecognised, 1, "the unknown content shape");
        assert_eq!(out.lines, 2, "both readable records still produced a line");
        assert_eq!(
            out.raw_lines, 4,
            "the resume offset counts what was consumed, including what failed to parse"
        );
        assert!(
            text_of(&out).contains("\"shape\":\"unknown\""),
            "passed through"
        );
    }

    /// The gate does not move. Compaction happens on the RAW side: what comes
    /// out is [`SecretBytes`], so the only way these bytes reach durable storage
    /// is through `rift-shadow-redact::redact`, exactly like any other capture.
    #[test]
    fn the_compact_output_is_still_secret_bytes() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let raw = jsonl(&fragments("m1", &[&format!("key {secret}")]));
        let compact = compact_jsonl(&raw, 0);
        // It is raw, and it says so by type — the compactor is not a redactor
        // and must never be mistaken for one.
        let out = rift_shadow_redact::redact(compact.compact()).unwrap();
        let redacted = String::from_utf8_lossy(out.redacted.as_slice()).into_owned();
        assert!(
            !redacted.contains(secret),
            "the gate is what removes it, and it did: {redacted}"
        );
    }

    /// **The overlap bound, measured rather than asserted.**
    ///
    /// The module header claimed "at most one line" of overlap between
    /// consecutive checkpoints. That is false: the boundary is
    /// `full − delta`, so every merge-group alive at `start_line` contributes one
    /// line to both slices, and nothing bounds the number of live groups at one.
    /// Interleaved streamed messages are exactly that shape and they happen.
    ///
    /// Overlap is stated operationally, in the terms two adjacent checkpoints
    /// actually see: the previous checkpoint stored `prev` compact lines, this one
    /// says its new material starts at `compact_transcript_start`, so
    /// `prev − start` lines are carried by both. Three shapes, three answers, and
    /// the answer is always the number of straddling groups.
    #[test]
    fn overlap_is_one_line_per_straddling_merge_group() {
        /// What two adjacent checkpoints would duplicate: the previous slice's
        /// line count minus where this one says its own slice begins.
        fn overlap(raw_lines: &[String], start_line: usize) -> usize {
            let prev = compact_jsonl(&jsonl(&raw_lines[..start_line]), 0).lines;
            let here = compact_jsonl(&jsonl(raw_lines), start_line);
            prev - here.compact_transcript_start
        }

        // 0 straddling groups: three whole messages, cut between them.
        let none: Vec<String> = ["m1", "m2", "m3"]
            .iter()
            .map(|id| fragments(id, &["x"]).remove(0))
            .collect();
        assert_eq!(
            overlap(&none, 2),
            0,
            "no group straddles, nothing is shared"
        );

        // 1 straddling group: one message in three fragments, cut after the first.
        let one = fragments("m1", &["a", "b", "c"]);
        assert_eq!(overlap(&one, 1), 1, "the straddling group is shared once");

        // 2 straddling groups: two streamed messages whose fragments INTERLEAVE
        // across the cut. This is the case the old sentence excluded.
        let two = vec![
            fragments("m1", &["a1"]).remove(0),
            fragments("m2", &["b1"]).remove(0),
            fragments("m1", &["a2"]).remove(0),
            fragments("m2", &["b2"]).remove(0),
        ];
        assert_eq!(
            overlap(&two, 2),
            2,
            "two straddling groups overlap by two — the bound is per group"
        );
    }

    /// **`start_line` counts what the compactor counts.** The parameter's doc
    /// once said "an index into the RAW lines" while the code filters blank
    /// segments before skipping, so a caller that counted raw lines would skip
    /// too far by one per blank line. [`CompactTranscript::raw_lines`] is the
    /// resume offset, reported by the same iterator that consumes it, so the two
    /// cannot drift.
    #[test]
    fn the_resume_offset_counts_non_empty_lines_and_round_trips() {
        let msgs: Vec<String> = ["m1", "m2", "m3"]
            .iter()
            .map(|id| fragments(id, &["x"]).remove(0))
            .collect();
        // Blank segments between records, and a trailing newline — both are what
        // a file an agent is still writing looks like.
        let raw =
            SecretBytes::new(format!("{}\n\n{}\n{}\n\n", msgs[0], msgs[1], msgs[2]).into_bytes());
        let out = compact_jsonl(&raw, 0);
        assert_eq!(out.lines, 3);
        assert_eq!(
            out.raw_lines, 3,
            "three records — the blanks and the trailing newline are not lines"
        );
        // Resuming at the reported offset leaves nothing new, which is the
        // property a caller depends on: everything is prior, nothing is lost.
        let resumed = compact_jsonl(&raw, out.raw_lines);
        assert_eq!(resumed.compact_transcript_start, resumed.lines);
        // …and one record back leaves exactly one new line. Had `start_line`
        // indexed raw segments, this offset would have landed past the third
        // record and reported nothing new.
        let one_back = compact_jsonl(&raw, out.raw_lines - 1);
        assert_eq!(one_back.compact_transcript_start, 2);
    }

    /// An empty transcript compacts to nothing without a boundary of its own —
    /// the empty-transcript bug class (legacy backend 0x0011) starts here.
    #[test]
    fn an_empty_transcript_is_empty_and_not_an_error() {
        let out = compact_jsonl(&SecretBytes::new(Vec::new()), 0);
        assert_eq!(out.lines, 0);
        assert_eq!(out.raw_lines, 0);
        assert_eq!(out.compact_transcript_start, 0);
        assert!(out.compact().is_empty());
    }
}
