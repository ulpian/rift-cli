//! **Deterministic digests at condense** — the deterministic extractors'
//! condense-time half. No model runs here: every document below is a pure
//! function of bytes the archive
//! already holds, so the same shard always digests to the same bytes and the
//! digests are reproducible rather than merely plausible.
//!
//! Four families ship as shard artifacts, all **anchor-free** (derived from
//! capture content, never from the anchoring commit or the clock — a digest
//! leaf that drifted with the anchor would mint a new shard per re-archive
//! and break the re-condense dedup no-op):
//!
//! * `digest.failures.json` — mechanical failure signatures: every transcript
//!   record carrying `"is_error": true` anywhere in its tree, as
//!   `{line, sha256}` pairs. Written only when at least one exists.
//! * `digest.intent.json` — the **intent/outcome slot**, filled with no
//!   model: intent is a LOCATOR of the redacted prompt (first `"type":"user"`
//!   line of the redacted transcript — index, content address, byte length;
//!   the prompt bytes themselves already live in the shard's own transcript,
//!   so a reader resolves the reference without leaving the shard), outcome
//!   is mechanical facts (failure count, read count, transcript size).
//! * `digest.log.json` — the session log's shape: capture bytes, transcript
//!   lines, compact lines, the compact boundary.
//! * `digest.shape.json` — the file-shape digest: the distinct files the
//!   session read, path + content address, plus an extension histogram.
//!
//! **The fifth family the design names — the diff digest — deliberately lives in
//! the store, not the shard.** A diff derives from the anchoring commit, so
//! as a Merkle leaf it would move the checkpoint id on every re-archive of
//! unchanged capture at a later commit (destroying idempotence and the
//! trailer prediction, which runs before the commit exists), and as a
//! non-leaf it would either grow `ANCHOR_DEPENDENT_SHARD_ENTRIES` or
//! hard-fail the second archive of a live session. The store already holds
//! exactly this digest, durably and anchor-keyed: the `file_revision` +
//! `changed_hunk` fact rows `persist_commit_capture` writes beside every
//! archive.
//!
//! **Containment**: no digest document may quote transcript or capture
//! payload bytes — counts, indexes, classification tokens and SHA-256
//! addresses only — which is what lets them take the structural write path.
//! Pinned by `a_digest_never_quotes_the_prompt_it_locates`.

use serde_json::Value;

use crate::sha256_hex;

/// `digest.failures.json` — present only when a failure signature exists.
pub const DIGEST_FAILURES_NAME: &str = "digest.failures.json";
/// `digest.intent.json` — present whenever the shard carries a transcript.
pub const DIGEST_INTENT_NAME: &str = "digest.intent.json";
/// `digest.log.json` — present whenever the shard carries a transcript.
pub const DIGEST_LOG_NAME: &str = "digest.log.json";
/// `digest.shape.json` — present whenever the session captured read facts.
pub const DIGEST_SHAPE_NAME: &str = "digest.shape.json";

/// The non-empty lines of a JSONL byte stream — the same iteration rule the
/// leaf spec uses for `full.jsonl`, so "line N" means one thing everywhere.
fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty())
}

/// Whether a JSON value carries `"is_error": true` anywhere in its tree —
/// the one mechanical failure signal every captured agent dialect shares
/// (tool results mark failure with it; the compactor preserves the key).
fn carries_error(v: &Value) -> bool {
    match v {
        Value::Object(map) => map
            .iter()
            .any(|(k, val)| (k == "is_error" && val == &Value::Bool(true)) || carries_error(val)),
        Value::Array(items) => items.iter().any(carries_error),
        _ => false,
    }
}

/// One failure signature: the 0-based non-empty-line index into the redacted
/// full transcript, and the content address of that line's bytes.
struct FailureSignature {
    line: usize,
    sha256: String,
}

/// Scan the redacted full transcript for mechanical failure signatures.
fn failure_signatures(transcript_full: &[u8]) -> Vec<FailureSignature> {
    lines(transcript_full)
        .enumerate()
        .filter_map(|(i, line)| {
            let v = serde_json::from_slice::<Value>(line).ok()?;
            carries_error(&v).then(|| FailureSignature {
                line: i,
                sha256: sha256_hex(&[line]),
            })
        })
        .collect()
}

/// `digest.failures.json` bytes, or `None` when the transcript holds no
/// failure signature (absence is the "nothing failed" claim — a zero-count
/// document on every shard would add a leaf that says nothing).
#[must_use]
pub fn failures_json(transcript_full: &[u8]) -> Option<Vec<u8>> {
    let sigs = failure_signatures(transcript_full);
    if sigs.is_empty() {
        return None;
    }
    let doc = serde_json::json!({
        "count": sigs.len(),
        "schema_version": 1u32,
        "signatures": sigs
            .iter()
            .map(|s| serde_json::json!({
                "kind": "is_error",
                "line": s.line,
                "sha256": s.sha256,
            }))
            .collect::<Vec<_>>(),
    });
    Some(serde_json::to_vec(&doc).unwrap_or_default())
}

/// `digest.intent.json` bytes: the intent/outcome slot, model-free.
///
/// `intent` locates the redacted prompt — the first `"type":"user"` record of
/// the redacted full transcript — by line index, content address and byte
/// length; `null` when the transcript holds none. `outcome` is mechanical:
/// the failure count, the read-fact count, and the transcript's line count.
#[must_use]
pub fn intent_json(transcript_full: &[u8], read_fact_count: usize) -> Vec<u8> {
    let prompt = lines(transcript_full).enumerate().find_map(|(i, line)| {
        let v = serde_json::from_slice::<Value>(line).ok()?;
        (v.get("type").and_then(Value::as_str) == Some("user")).then(|| {
            serde_json::json!({
                "prompt_bytes": line.len(),
                "prompt_line": i,
                "prompt_sha256": sha256_hex(&[line]),
            })
        })
    });
    let doc = serde_json::json!({
        "intent": prompt,
        "outcome": {
            "failures": failure_signatures(transcript_full).len(),
            "reads": read_fact_count,
            "transcript_lines": lines(transcript_full).count(),
        },
        "schema_version": 1u32,
    });
    serde_json::to_vec(&doc).unwrap_or_default()
}

/// `digest.log.json` bytes: the log-shape digest.
#[must_use]
pub fn log_json(
    capture_bytes: usize,
    transcript_full: &[u8],
    transcript_compact: &[u8],
    compact_start: usize,
) -> Vec<u8> {
    let doc = serde_json::json!({
        "capture_bytes": capture_bytes,
        "compact_lines": lines(transcript_compact).count(),
        "compact_start": compact_start,
        "schema_version": 1u32,
        "transcript_lines": lines(transcript_full).count(),
    });
    serde_json::to_vec(&doc).unwrap_or_default()
}

/// `digest.shape.json` bytes, or `None` when no read facts were captured:
/// the distinct files the session read (path + content address, bytewise
/// path order, first-seen address kept per path) and an extension histogram.
#[must_use]
pub fn shape_json(read_facts: &[crate::backend::ArchivedReadFact]) -> Option<Vec<u8>> {
    if read_facts.is_empty() {
        return None;
    }
    let mut files: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    for f in read_facts {
        files.entry(&f.path).or_insert(&f.content_sha256);
    }
    let mut by_ext: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for path in files.keys() {
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        *by_ext.entry(ext.to_owned()).or_insert(0) += 1;
    }
    let doc = serde_json::json!({
        "by_ext": by_ext,
        "files": files
            .iter()
            .map(|(path, sha)| serde_json::json!({"path": path, "sha256": sha}))
            .collect::<Vec<_>>(),
        "schema_version": 1u32,
    });
    Some(serde_json::to_vec(&doc).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRANSCRIPT: &[u8] = br#"{"type":"user","message":"please fix the build"}
{"type":"assistant","message":[{"type":"text","text":"looking"}]}
{"type":"user","message":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"boom"}]}
"#;

    /// **Byte stability against bytes an EARLIER build froze into the tree**,
    /// asserted on all four families at once.
    ///
    /// This replaces `digests_are_byte_stable`, which was four lines of
    /// `assert_eq!(failures_json(T), failures_json(T))` and the same for the
    /// other three — `f(x) == f(x)` inside one process. **It could not fail.**
    /// A rewritten digest writer emitting a different key order, a different
    /// field set or a different `schema_version` satisfies it exactly as well as
    /// this one does, which is why `schema_version` could go from 1 to 7 with
    /// the whole suite green. It was also the only place item 6's byte-stability
    /// claim was defended.
    ///
    /// The oracle is the maximal fixture's frozen artifacts: the same four
    /// documents the archive writes into a real shard, compared byte for byte
    /// against the maximal fixture's committed artifacts. Going through
    /// `artifact_map` rather than calling the four functions directly is
    /// deliberate — it is the WRITER's own composition (which transcript, which
    /// capture length, which read facts, which boundary), so a digest that
    /// stays stable as a function while being fed different inputs by the
    /// archive still reds.
    #[test]
    fn digests_reproduce_the_frozen_golden_bytes() {
        let archive =
            crate::shard_fixture::every_artifact_archive().expect("the maximal fixture builds");
        let map = archive.artifact_map();
        for name in [
            DIGEST_FAILURES_NAME,
            DIGEST_INTENT_NAME,
            DIGEST_LOG_NAME,
            DIGEST_SHAPE_NAME,
        ] {
            let live = map
                .get(name)
                .unwrap_or_else(|| panic!("{name} is not emitted by the maximal fixture"));
            let frozen = std::fs::read(
                crate::shard_fixture::fixture_dir()
                    .join("artifacts")
                    .join(name),
            )
            .unwrap_or_else(|_| {
                panic!("{name}: no frozen golden — see crates/rift-shadow/tests/shard_goldens.rs")
            });
            assert_eq!(
                live, &frozen,
                "{name} drifted from the bytes frozen in shard-goldens/"
            );
        }
    }

    /// …and the frozen four are not four empty documents: each carries the
    /// content it exists to carry, asserted on the live side so a golden that
    /// quietly stopped saying anything would still be caught while matching
    /// itself.
    #[test]
    fn the_frozen_digests_actually_carry_their_findings() {
        let archive =
            crate::shard_fixture::every_artifact_archive().expect("the maximal fixture builds");
        let map = archive.artifact_map();
        let doc = |name: &str| -> Value {
            serde_json::from_slice(map.get(name).expect("emitted")).expect("json")
        };
        assert_eq!(
            doc(DIGEST_FAILURES_NAME)
                .get("count")
                .and_then(Value::as_u64),
            Some(1),
            "the fixture's one failing tool result is found"
        );
        assert!(
            doc(DIGEST_INTENT_NAME)
                .pointer("/intent/prompt_sha256")
                .is_some(),
            "the intent slot locates the prompt"
        );
        assert_eq!(
            doc(DIGEST_LOG_NAME)
                .get("compact_lines")
                .and_then(Value::as_u64),
            Some(8),
            "the log digest counts the compaction the compactor produced"
        );
        assert_eq!(
            doc(DIGEST_SHAPE_NAME)
                .pointer("/by_ext/rs")
                .and_then(Value::as_u64),
            Some(2),
            "the shape digest histograms the files the session read"
        );
    }

    /// The failure scan finds nested `is_error: true` and names the line; a
    /// clean transcript digests to ABSENCE, not to a zero-count document.
    #[test]
    fn failures_are_found_nested_and_absent_when_clean() {
        let doc = failures_json(TRANSCRIPT).expect("one failure");
        let v: serde_json::Value = serde_json::from_slice(&doc).unwrap();
        assert_eq!(v.get("count").and_then(Value::as_u64), Some(1));
        assert_eq!(
            v.pointer("/signatures/0/line").and_then(Value::as_u64),
            Some(2),
            "the third non-empty line carries the failure"
        );
        assert_eq!(
            failures_json(br#"{"type":"user","message":"all good"}"#),
            None,
            "a clean transcript writes no failure artifact"
        );
    }

    /// **Containment (the redaction-gate argument for the structural path):**
    /// the intent digest LOCATES the prompt — index, address, length — and
    /// never quotes it; the prompt bytes live in the shard's own transcript.
    #[test]
    fn a_digest_never_quotes_the_prompt_it_locates() {
        let intent = intent_json(TRANSCRIPT, 0);
        let text = String::from_utf8(intent.clone()).unwrap();
        assert!(
            !text.contains("please fix the build"),
            "the prompt must not ride the digest: {text}"
        );
        let v: serde_json::Value = serde_json::from_slice(&intent).unwrap();
        assert_eq!(
            v.pointer("/intent/prompt_line").and_then(Value::as_u64),
            Some(0)
        );
        let addr = v
            .pointer("/intent/prompt_sha256")
            .and_then(Value::as_str)
            .unwrap();
        // The address resolves against the shard's own transcript line.
        let first_line = TRANSCRIPT.split(|b| *b == b'\n').next().unwrap();
        assert_eq!(addr, sha256_hex(&[first_line]));
        assert_eq!(
            v.pointer("/outcome/failures").and_then(Value::as_u64),
            Some(1),
            "outcome facts are mechanical"
        );
    }

    /// **Cross-family containment.** A secret-looking string
    /// planted in FAILURE output and in the prompt rides NONE of the four
    /// digest families. The intent pin above covers one family against the
    /// prompt; this pin covers every family against the payload byte the
    /// verifier showed was unpinned — the failing line's own content, which
    /// `failures_json` reads and must only ever ADDRESS (line index + sha256).
    /// Weakening any family to quote a raw transcript line reds this test.
    #[test]
    fn a_planted_secret_in_failure_output_rides_no_digest_family() {
        const SECRET: &str = "AKIA_PLANTED_FAILURE_SECRET_7Q";
        let transcript = format!(
            "{{\"type\":\"user\",\"message\":\"deploy with {SECRET} please\"}}\n\
             {{\"type\":\"user\",\"message\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"is_error\":true,\"content\":\"fatal: {SECRET} rejected\"}}]}}\n"
        );
        let t = transcript.as_bytes();
        let facts = vec![crate::backend::ArchivedReadFact {
            tool_use_id: "t1".into(),
            tool_name: "Read".into(),
            path: "src/a.rs".into(),
            content_sha256: "aa".into(),
            input_tokens: None,
        }];
        let families: [(&str, Option<Vec<u8>>); 4] = [
            ("failures", failures_json(t)),
            ("intent", Some(intent_json(t, facts.len()))),
            ("log", Some(log_json(t.len(), t, t, 0))),
            ("shape", shape_json(&facts)),
        ];
        for (name, doc) in &families {
            let doc = doc
                .as_deref()
                .unwrap_or_else(|| panic!("family {name} must be present for the plant to bite"));
            let text = core::str::from_utf8(doc).unwrap();
            assert!(
                !text.contains(SECRET),
                "digest family {name} quoted planted payload bytes: {text}"
            );
        }
        // The plant bit: the failure scan really saw the planted line (index 1)
        // — containment over a signature that missed it would prove nothing.
        let failures = families[0].1.as_deref().unwrap();
        let v: serde_json::Value = serde_json::from_slice(failures).unwrap();
        assert_eq!(
            v.pointer("/signatures/0/line").and_then(Value::as_u64),
            Some(1)
        );
        let planted_line = transcript.lines().nth(1).unwrap().as_bytes();
        assert_eq!(
            v.pointer("/signatures/0/sha256").and_then(Value::as_str),
            Some(sha256_hex(&[planted_line]).as_str()),
            "the signature ADDRESSES the planted line rather than quoting it"
        );
    }

    /// The shape digest dedups paths, keeps the first-seen address, and
    /// histograms extensions — and a fact-less session digests to absence.
    #[test]
    fn shape_dedups_paths_and_counts_extensions() {
        let fact = |path: &str, sha: &str| crate::backend::ArchivedReadFact {
            tool_use_id: "t".into(),
            tool_name: "Read".into(),
            path: path.into(),
            content_sha256: sha.into(),
            input_tokens: None,
        };
        let doc = shape_json(&[
            fact("src/a.rs", "a1"),
            fact("src/a.rs", "a2"),
            fact("doc/b.md", "b1"),
        ])
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&doc).unwrap();
        assert_eq!(
            v.pointer("/files").and_then(Value::as_array).map(Vec::len),
            Some(2),
            "one entry per distinct path"
        );
        assert_eq!(
            v.pointer("/files/1/sha256").and_then(Value::as_str),
            Some("a1"),
            "first-seen address kept (files are path-sorted: doc/… then src/…)"
        );
        assert_eq!(v.pointer("/by_ext/rs").and_then(Value::as_u64), Some(1));
        assert_eq!(v.pointer("/by_ext/md").and_then(Value::as_u64), Some(1));
        assert_eq!(shape_json(&[]), None);
    }
}
