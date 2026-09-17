//! JSON-aware walking — redaction that keeps a transcript **parsable**.
//!
//! Agent transcripts are JSONL. Redacting them as flat text can put a
//! placeholder across a quote, a colon or an escape and hand the store a line
//! that no longer parses, which costs replay the whole record rather than the
//! secret inside it. So the content is parsed, string leaves and object keys are
//! redacted individually, and the results are substituted back into the *raw*
//! bytes — formatting, key order and whitespace survive untouched, because only
//! the secret substrings move.
//!
//! **This walker does not reduce scope, and that is the point.** Three verifier
//! rejects in a row each found a new way to *aim* a narrowing that earlier
//! versions granted to signature/identifier/payload leaves, and the owner
//! ruled on 2026-08-08 that the machinery goes rather than be patched a fourth
//! time. There is no `Scope`, no skip-field list, no
//! payload-type test, and [`crate::redact_fragment`] has no parameter that could
//! ask for fewer layers. A key name is content an attacker writes, so nothing a
//! key name says can change what runs. The cost is real and is not hidden: an
//! extended-thinking `signature` is high-entropy base64 and **is now masked**,
//! so a session's thinking blocks no longer replay. What still survives is what
//! survives on its own merits — a transcript UUID sits below the entropy floor
//! ([`transcript_uuids_survive_on_the_entropy_floor`]), and so do ordinary
//! paths.
//!
//! Two rules keep the structural path from ever being a *wider* door than the
//! flat one.
//!
//! * **Substitution is verified.** A leaf's replacement is looked for in the raw
//!   text as JSON-encoded bytes. Valid JSON can encode the same string in more
//!   than one way (`A` is `A`, `\/` is `/`), so an attacker-shaped line can
//!   make a naive substitution silently no-op and leave the secret in place.
//!   When any replacement is not found, the whole line falls back to flat-text
//!   redaction — over-redacting the line rather than trusting a miss.
//! * **The structural output must be closed under the flat pipeline.** Judging a
//!   leaf on its own takes its context away, and some detectors *are* context: a
//!   labelled taxpayer id needs the `ssn` key and the nine digits in one text,
//!   and the entropy layer's token boundaries move when a `\n` escape stops
//!   being a letter. Both make a per-leaf walk seal less than a flat scan of the
//!   same bytes. So [`try_structured`] re-runs the flat pipeline over the
//!   assembled output and takes it only if nothing further would be masked
//!   ([`the_structural_output_is_a_fixpoint_of_the_flat_pipeline`]); otherwise
//!   the line goes flat. This is a checkable proxy for "never seals less", not a
//!   proof of it — the proof obligation lives with the generated differential
//!   guard, [`walk_seals_at_least_as_much_as_flat_on_generated_json`].
//!
//! Everything that is not parsable JSON falls back to flat-text redaction, so
//! malformed JSONL is a *narrower* path, never a bypass.
//!
//! Both rules rest on one premise, which [`parse_faithfully`] turns from an
//! assumption into a checked precondition: **every byte the raw line carries is
//! reachable in the parsed model, spelled the way the model spells it.** Where
//! that does not hold the structural path is not narrower, it is *blind* — and a
//! blind pass that then finds nothing to substitute would return the raw line
//! untouched.
//!
//! ## What was measured, and what the measurement does *not* say
//!
//! Four runs of the same 1 500-document corpus, one variable changed at a time.
//! The first two are why the closure check exists; the last two are the honest
//! part.
//!
//! | # | walker | closure check | scope-reduction mock | result |
//! |---|---|---|---|---|
//! | 1 | yes | **off** | no | 5 950 needles checked, **142 leaked** — 74 escape-adjacent, 68 labelled |
//! | 2 | yes | on | no | 0 leaked, 23/23 green |
//! | 3 | yes | on | **yes** | **0 leaked, 23/23 green** |
//! | 4 | yes | off | yes | 4 tests red, first naming `{"signature":"Zx9…"}` |
//!
//! Row 1 is the measurement that decided the design: both classes are real and
//! neither was hypothetical — though the escape-adjacent one produced **zero**
//! leaks until [`SHORT_NEEDLE`] was planted, having been reasoned about and not
//! reached. Rows 3 and 4 are the ones a reader should not skip. **The
//! differential guard does not detect scope reduction while the closure check is
//! present** — it cannot, because a reduced leaf produces a line the flat
//! pipeline would still mask, and such a line is refused and redacted flat. The
//! reduction is *neutralised*, not *observed*.
//!
//! So the guard proves "nothing seals less than flat", which is the property the
//! ruling needs, and it proves nothing about whether the machinery is gone. That
//! second thing is a **compile-time** property instead:
//! [`crate::redact_fragment`] takes no scope, so there is no parameter for a key
//! name to steer and no narrowing for a test to catch. Anyone re-adding one is
//! adding an argument to a function signature, not slipping past a property
//! test.

use std::collections::BTreeSet;
use std::fmt;

use rift_core::Result;
use serde::de::{Deserializer, Error as _, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use crate::{redact_fragment, RedactionReport, RulePackSet};

/// One (key, original → redacted) substitution collected from the walk.
struct Replacement {
    /// The object key the leaf sat under, or empty for array elements and
    /// top-level values. Substitution is anchored to this key so a redacted
    /// value cannot be written over some other field that happens to hold the
    /// same text.
    key: String,
    original: String,
    redacted: String,
}

/// A redacted object **key**. Applied in key position only, and only after every
/// value substitution, so a value is still located under its key's original
/// name.
struct KeyReplacement {
    original: String,
    redacted: String,
}

/// Everything one walk collected: value substitutions, key substitutions, and
/// the dedup sets that keep a repeated string from being substituted twice.
///
/// **The dedup key is a tuple, never a joined string.** Its previous form was
/// `format!("{anchor}\u{0}{leaf}")`, which is ambiguous — an anchor ending in a
/// NUL and a leaf beginning with one produce the same key as their neighbours,
/// and the collision *drops a replacement*, shipping that leaf's secret in an
/// otherwise-successful line. A verifier named that as a blocker; a tuple has no
/// encoding to be ambiguous about, so the class cannot be re-opened by choosing
/// a different separator ([`a_nul_bearing_key_and_value_do_not_collide`]).
#[derive(Default)]
struct Collected {
    values: Vec<Replacement>,
    keys: Vec<KeyReplacement>,
    seen_values: BTreeSet<(String, String)>,
    seen_keys: BTreeSet<String>,
}

/// Redact `text`, using its JSON structure when it has one.
pub(crate) fn redact_content(
    text: &str,
    packs: &RulePackSet,
    report: &mut RedactionReport,
) -> Result<String> {
    // A single JSON value covering the whole content (a pretty-printed export)
    // must be walked as one document — splitting it on newlines first would put
    // the flat-text path back in charge of every leaf.
    if let Some(out) = try_structured(text, text, packs, report)? {
        return Ok(out);
    }

    // Lines that go structural are handled one at a time; everything else is
    // gathered into maximal contiguous runs and redacted as a block. The run
    // matters: detectors that span lines (a PEM private-key block is the one
    // that bites) only see their subject if the lines stay together, so naive
    // per-line redaction would quietly stop sealing private keys.
    let lines: Vec<&str> = text.split('\n').collect();
    let mut segments: Vec<String> = Vec::new();
    let mut i = 0usize;
    while let Some(line) = lines.get(i) {
        if let Some(redacted) = try_structured(line, line.trim(), packs, report)? {
            segments.push(redacted);
            i += 1;
            continue;
        }
        let start = i;
        // Always consume the line that failed to go structural, so a parsable
        // line whose substitution could not be verified cannot stall the walk.
        i += 1;
        while lines.get(i).is_some_and(|l| !goes_structural(l, packs)) {
            i += 1;
        }
        let run = lines.get(start..i).unwrap_or_default().join("\n");
        segments.push(redact_fragment(&run, packs, report)?);
    }
    Ok(segments.join("\n"))
}

/// Whether a line will actually be walked — the test that decides where a
/// flat-text run ends.
///
/// It has to agree with [`try_structured`] or a line that ends a run and is then
/// refused would be redacted twice (once inside the run, once on its own). A
/// plain parse is not enough for that agreement, so this asks the same two
/// questions [`try_structured`] asks: does the parse model the raw bytes, and
/// does the assembled output stand up. Only the report is thrown away.
fn goes_structural(line: &str, packs: &RulePackSet) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let mut scratch = blank_report();
    try_structured(line, trimmed, packs, &mut scratch)
        .ok()
        .flatten()
        .is_some()
}

/// A throwaway report for a probe whose counts must never reach the real one.
fn blank_report() -> RedactionReport {
    RedactionReport {
        total_spans: 0,
        by_detector: std::collections::BTreeMap::new(),
        by_class: std::collections::BTreeMap::new(),
        bytes_masked: 0,
        allowlisted: 0,
        rule_pack_versions: Vec::new(),
        input_sha256: crate::sha256(&[]),
    }
}

/// Parse `parse_from` as one JSON value and, if that works, redact `raw`
/// structurally. Returns `None` when the content is not a single JSON value,
/// when substitution could not be verified, or when the assembled output is not
/// a fixpoint of the flat pipeline — all three mean "let the flat-text path
/// handle it".
///
/// `serde_json` rejects trailing non-whitespace, so a multi-line JSONL body
/// fails here and falls through to the per-line loop.
fn try_structured(
    raw: &str,
    parse_from: &str,
    packs: &RulePackSet,
    report: &mut RedactionReport,
) -> Result<Option<String>> {
    let Some(value) = parse_faithfully(parse_from) else {
        return Ok(None);
    };
    // **A scalar is not a structure, and treating it as one costs context.**
    // The walker exists to keep keys, ordering and leaf boundaries intact; a
    // line that is a bare number, string, bool or null has none of those, so
    // there is nothing here for it to preserve — and going structural has a
    // price the fixpoint check below cannot see, because that check reads one
    // line and the context that decides may be on the previous one.
    //
    // Measured, not reasoned to: `ssn=\n123456789` is a labelled identifier
    // whose value is alone on its line, `123456789` is a valid JSON document,
    // so the run of flat lines ENDED at the label and the nine digits were
    // judged with nothing beside them. They shipped. Sending scalars to the
    // flat path is strictly in the sealing direction — the flat path is what
    // the walk is checked against — and a real JSONL transcript is unaffected,
    // because every record in one is an object.
    if !matches!(value, Value::Object(_) | Value::Array(_)) {
        return Ok(None);
    }
    // Counts from an abandoned attempt must not survive into the report, so the
    // walk runs against a snapshot that is only kept once every check passes.
    let mut attempt = report.clone();
    let collected = collect(&value, packs, &mut attempt)?;
    let Some(out) = apply(raw, &collected) else {
        return Ok(None);
    };
    // The closure check. Every leaf was judged alone, so any detector that reads
    // context across a leaf boundary — a labelled id whose label is the key, an
    // entropy token whose length depends on an escape being a letter in the raw
    // bytes — could have been robbed of its subject. Re-running the flat
    // pipeline over the assembled line answers that in the direction that
    // matters: if it would still mask something, the structural line is not
    // safe to keep and the flat path takes over.
    let mut probe = blank_report();
    if redact_fragment(&out, packs, &mut probe)? != out {
        return Ok(None);
    }
    *report = attempt;
    Ok(Some(out))
}

/// Parse `text` as one JSON value **only when the model covers the raw bytes**.
///
/// The walk substitutes a redacted leaf back into the raw line by re-encoding it
/// and searching for it, which is only a total search if every string the raw
/// bytes carry reaches a detector through the model, spelled the way the model
/// spells it. Two legal JSON spellings break that, and both are
/// attacker-writable, because tool output is JSON someone else wrote:
///
/// * **A repeated object key.** `serde_json::Map` keeps the last occurrence, so
///   `{"text":"<secret>","text":"clean"}` drops the secret before any detector
///   runs; nothing is collected, and a walk that collects nothing returns the
///   raw line verbatim. That is not "narrower", it is a bypass.
/// * **A non-canonical string escape.** `A` is `A`, `\/` is `/`. The leaf
///   the walk judges is the decoded one, so a token the raw spells long enough
///   to trip the entropy floor can decode to one that is not
///   (`"ABCDEFGHIJKLMNOPQRS"` — 23 raw token characters, 19 decoded), and
///   again nothing is collected and the raw line ships.
///
/// Neither is worth a repair inside the walk: the flat path already reads the
/// raw bytes exactly as written, which is precisely what these inputs demand. So
/// they take it. The cost is that a producer which escapes ordinary characters
/// (Python's `ensure_ascii`, Go's `<`/`>`/`&`) loses structural handling on
/// those lines — over-redaction, the only direction this gate is allowed to err
/// in. `` and friends stay on the structural path, because that is how
/// `serde_json` spells a control character too, and terminal output is full of
/// them.
///
/// Recursion is bounded by `serde_json`'s own nesting limit, so a document deep
/// enough to overflow [`walk`]'s stack is refused here before the walk sees it
/// ([`a_document_too_deep_to_walk_is_refused_by_the_parse`]).
fn parse_faithfully(text: &str) -> Option<Value> {
    if !escapes_are_canonical(text) {
        return None;
    }
    serde_json::from_str::<UniqueKeys>(text).ok().map(|v| v.0)
}

/// Whether every escape in `text` is spelled the way `serde_json` would spell
/// it, which is the property that makes "re-encode the decoded leaf and find it
/// in the raw" a total search rather than a hopeful one.
///
/// A backslash can only appear inside a string literal in valid JSON, so the
/// scan needs no parser state — it only has to consume `\\` as a pair so the
/// second backslash is not read as opening an escape of its own.
fn escapes_are_canonical(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        if b != b'\\' {
            i += 1;
            continue;
        }
        match bytes.get(i + 1) {
            Some(b'"' | b'\\' | b'b' | b'f' | b'n' | b'r' | b't') => i += 2,
            Some(b'u') => {
                // serde_json writes `\u00xx`, lower-case, only for the control
                // characters that have no short form. Every other code point it
                // writes literally, so any other `\u` escape means the raw text
                // says something the model does not.
                let Some(hex) = text.get(i + 2..i + 6) else {
                    return false;
                };
                let Ok(code) = u32::from_str_radix(hex, 16) else {
                    return false;
                };
                if hex != hex.to_ascii_lowercase()
                    || code >= 0x20
                    || matches!(code, 0x08 | 0x09 | 0x0A | 0x0C | 0x0D)
                {
                    return false;
                }
                i += 6;
            }
            // `\/` is legal JSON and not how serde_json writes `/`; anything
            // else is not legal at all and the parse would reject it anyway.
            _ => return false,
        }
    }
    true
}

/// A [`Value`] that refuses to parse when an object repeats a key, at any depth.
///
/// This is the only way to see a repeat: by the time `serde_json` has built a
/// `Map`, the shadowed value is gone. Rejecting is a deliberate choice over
/// keeping both values — the walk would then have to substitute into raw bytes
/// that hold two values under one key, and the flat path already redacts those
/// bytes correctly.
struct UniqueKeys(Value);

impl<'de> serde::Deserialize<'de> for UniqueKeys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueKeysVisitor).map(Self)
    }
}

struct UniqueKeysVisitor;

impl<'de> Visitor<'de> for UniqueKeysVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value whose objects never repeat a key")
    }

    fn visit_bool<E>(self, v: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> std::result::Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> std::result::Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_f64<E>(self, v: f64) -> std::result::Result<Value, E> {
        Ok(serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number))
    }

    fn visit_str<E>(self, v: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(v.to_string()))
    }

    fn visit_unit<E>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }

    // No `visit_none` / `visit_some`: JSON has no `Option`, and this visitor is
    // only ever reached through `deserialize_any`, so those arms would be
    // unreachable code in the one place the crate cannot afford any.

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(UniqueKeys(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut out = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let UniqueKeys(value) = map.next_value()?;
            if out.insert(key, value).is_some() {
                return Err(A::Error::custom("object repeats a key"));
            }
        }
        Ok(Value::Object(out))
    }
}

/// Walk the parsed value, redacting every string leaf and every object key.
fn collect(value: &Value, packs: &RulePackSet, report: &mut RedactionReport) -> Result<Collected> {
    let mut acc = Collected::default();
    walk(value, "", packs, report, &mut acc)?;
    Ok(acc)
}

/// `anchor` is the object key a substitution is anchored to (empty for array
/// elements, whose position cannot be addressed by key).
///
/// Every node is judged the same way, whatever it is called: [`redact_fragment`]
/// takes no scope and this function passes it none, so there is no state here
/// that a key name could steer. That absence is the ruling.
fn walk(
    value: &Value,
    anchor: &str,
    packs: &RulePackSet,
    report: &mut RedactionReport,
    acc: &mut Collected,
) -> Result<()> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                collect_key(key, packs, report, acc)?;
                walk(child, key, packs, report, acc)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                // An array element cannot be anchored by its parent key, so its
                // substitution must be exact and global.
                walk(item, "", packs, report, acc)?;
            }
        }
        Value::String(leaf) => {
            let redacted = redact_fragment(leaf, packs, report)?;
            if &redacted != leaf && acc.seen_values.insert((anchor.to_string(), leaf.clone())) {
                acc.values.push(Replacement {
                    key: anchor.to_string(),
                    original: leaf.clone(),
                    redacted,
                });
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    Ok(())
}

/// Redact an object key. A secret written *as* a key is exactly as durable as
/// one written as a value, and flat redaction sealed those bytes, so the
/// structural path has to as well — a key position is not a shelter. A redacted
/// key is re-encoded the same way a value is, so the line stays valid JSON.
fn collect_key(
    key: &str,
    packs: &RulePackSet,
    report: &mut RedactionReport,
    acc: &mut Collected,
) -> Result<()> {
    if key.is_empty() {
        return Ok(());
    }
    let redacted = redact_fragment(key, packs, report)?;
    if redacted != key && acc.seen_keys.insert(key.to_string()) {
        acc.keys.push(KeyReplacement {
            original: key.to_string(),
            redacted,
        });
    }
    Ok(())
}

/// Substitute every replacement into the raw text, or return `None` if any of
/// them could not be located — the caller then redacts the line flat rather
/// than shipping a silently-missed secret.
///
/// Values go first and keys second: a value is located by its key's *original*
/// name, so renaming a key before the values under it are placed would fail
/// every lookup there and drop an otherwise-fine line to the flat path.
fn apply(raw: &str, collected: &Collected) -> Option<String> {
    if collected.values.is_empty() && collected.keys.is_empty() {
        return Some(raw.to_string());
    }
    let mut text = raw.to_string();
    for r in &collected.values {
        let original = encode(&r.original)?;
        let redacted = encode(&r.redacted)?;
        let next = if r.key.is_empty() {
            if !text.contains(&original) {
                return None;
            }
            text.replace(&original, &redacted)
        } else {
            replace_after_key(&text, &encode(&r.key)?, &original, &redacted)?
        };
        text = next;
    }
    for k in &collected.keys {
        text = replace_in_key_position(&text, &encode(&k.original)?, &encode(&k.redacted)?)?;
    }
    Some(text)
}

/// JSON-encode a string exactly as `serde_json` would write it (no HTML
/// escaping), so the needle matches conventionally-encoded raw bytes.
fn encode(s: &str) -> Option<String> {
    serde_json::to_string(s).ok()
}

/// Replace `original` with `redacted` only where it stands in a value position
/// for `key` — i.e. directly after `key` + optional whitespace + `:` + optional
/// whitespace. Restricting to value positions stops a key's own redacted text
/// from being written over a different field that holds the same string.
///
/// Returns `None` when no occurrence was replaced, which the caller treats as an
/// unverified substitution.
fn replace_after_key(text: &str, key: &str, original: &str, redacted: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    let mut hits = 0usize;
    while let Some(rest) = text.get(cursor..) {
        let Some(found) = rest.find(key) else {
            out.push_str(rest);
            break;
        };
        let key_end = cursor + found + key.len();
        out.push_str(text.get(cursor..key_end)?);
        let mut probe = key_end;
        probe = skip_ws(text, probe);
        if text.as_bytes().get(probe) != Some(&b':') {
            cursor = key_end;
            continue;
        }
        probe = skip_ws(text, probe + 1);
        if text.get(probe..probe + original.len()) == Some(original) {
            out.push_str(text.get(key_end..probe)?);
            out.push_str(redacted);
            cursor = probe + original.len();
            hits += 1;
        } else {
            cursor = key_end;
        }
    }
    if hits == 0 {
        return None;
    }
    Some(out)
}

/// Replace an encoded key wherever it stands in a **key position** — i.e. is
/// followed by optional whitespace and `:`. The position test is what keeps a
/// redacted key from being written over a value that happens to hold the same
/// text.
///
/// Returns `None` when no occurrence was replaced, which the caller treats as an
/// unverified substitution and answers by redacting the whole line flat.
fn replace_in_key_position(text: &str, key: &str, redacted: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    let mut hits = 0usize;
    while let Some(rest) = text.get(cursor..) {
        let Some(found) = rest.find(key) else {
            out.push_str(rest);
            break;
        };
        let start = cursor + found;
        let end = start + key.len();
        out.push_str(text.get(cursor..start)?);
        if text.as_bytes().get(skip_ws(text, end)) == Some(&b':') {
            out.push_str(redacted);
            hits += 1;
        } else {
            out.push_str(text.get(start..end)?);
        }
        cursor = end;
    }
    if hits == 0 {
        return None;
    }
    Some(out)
}

fn skip_ws(text: &str, mut at: usize) -> usize {
    while matches!(text.as_bytes().get(at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        at += 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{redact, RedactionOutput};
    use rift_core::SecretBytes;

    fn out(s: &str) -> RedactionOutput {
        redact(&SecretBytes::from(s)).expect("redaction")
    }

    fn masked(s: &str) -> String {
        String::from_utf8(out(s).redacted.as_slice().to_vec()).expect("utf8")
    }

    /// The differential guard's reference leg: the **same bytes**, redacted as
    /// one flat fragment with every layer running — which is what this crate did
    /// before the walker existed. Comparing against this, rather than against
    /// the needle in some other carrier string, is what makes "structure never
    /// seals less" a statement about the input actually under test.
    fn flat(s: &str) -> String {
        let mut report = blank_report();
        redact_fragment(s, &RulePackSet::builtin(), &mut report).expect("flat")
    }

    #[test]
    fn jsonl_formatting_survives_redaction() {
        let line = r#"{ "role": "user",  "text": "key AKIAIOSFODNN7EXAMPLE here" }"#;
        let m = masked(line);
        assert!(!m.contains("AKIAIOSFODNN7EXAMPLE"));
        // Every byte outside the secret is untouched: spacing, key order, quoting.
        assert!(m.starts_with(r#"{ "role": "user",  "text": "key "#), "{m}");
        assert!(m.ends_with(r#" here" }"#), "{m}");
    }

    #[test]
    fn multi_line_pretty_printed_json_is_walked_as_one_document() {
        let doc = "{\n  \"note\": \"token AKIAIOSFODNN7EXAMPLE\",\n  \
                   \"keep\": \"ordinary prose\"\n}";
        let m = masked(doc);
        assert!(!m.contains("AKIAIOSFODNN7EXAMPLE"), "{m}");
        assert!(m.contains(r#""keep": "ordinary prose""#), "{m}");
        // One document, so the line breaks and the two-space indent survive.
        assert!(m.starts_with("{\n  \"note\""), "{m}");
    }

    /// The whole reason the walker exists: the output still parses. A flat scan
    /// can put a placeholder across the quote that ends a string.
    #[test]
    fn redacted_jsonl_line_stays_parsable() {
        let line = r#"{"text":"line1\nZx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2q"}"#;
        let m = masked(line);
        let parsed: Value = serde_json::from_str(&m).expect("redacted line must still be JSON");
        let text = parsed["text"].as_str().expect("text leaf");
        assert!(
            text.starts_with("line1\n"),
            "escape survived decoding: {text}"
        );
        assert!(!m.contains("Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2q"));
    }

    /// The ruling's cost, stated as a test rather than as a sentence: a key name
    /// buys nothing. Every one of these was a *protected* leaf before
    /// 2026-08-08, and each is now judged exactly as its neighbours are.
    #[test]
    fn no_key_name_buys_a_leaf_any_protection() {
        let secret = "Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2q";
        for key in [
            "signature",
            "thinkingSignature",
            "session_id",
            "tool_use_id",
            "traceID",
            "ids",
            "file_path",
            "cwd",
            "path",
            "data",
        ] {
            let m = masked(&format!(r#"{{"{key}":"{secret}"}}"#));
            assert!(!m.contains(secret), "key {key} sheltered a secret: {m}");
        }
        // …including the payload declaration that used to grant a `data` leaf a
        // reduced scope for the whole object it labelled.
        for declared in ["image", "base64"] {
            let m = masked(&format!(r#"{{"type":"{declared}","data":"{secret}"}}"#));
            assert!(!m.contains(secret), "type {declared} bought scope: {m}");
        }
    }

    /// What still survives, and why — not because a key protects it, but because
    /// a UUID's Shannon entropy sits below the 4.0-bit floor. Claude Code writes
    /// `uuid`/`parentUuid` on every transcript record, so this is the difference
    /// between a readable transcript and a wall of placeholders.
    #[test]
    fn transcript_uuids_survive_on_the_entropy_floor() {
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        let m = masked(&format!(r#"{{"uuid":"{uuid}","parentUuid":"{uuid}"}}"#));
        assert!(m.contains(uuid), "a transcript uuid was masked: {m}");
        // The flat path agrees, which is what makes this the floor's doing and
        // not the walker's.
        assert!(flat(&format!(r#"{{"uuid":"{uuid}"}}"#)).contains(uuid));
    }

    /// Substitution is anchored to the key, so a redacted value never overwrites
    /// an identical string sitting under a different key — and both copies are
    /// still sealed, because neither key means anything.
    #[test]
    fn key_scoped_substitution_rewrites_each_field_in_place() {
        let shared = "Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2q";
        let m = masked(&format!(r#"{{"signature":"{shared}","body":"{shared}"}}"#));
        assert_eq!(
            m, r#"{"signature":"[REDACTED:entropy]","body":"[REDACTED:entropy]"}"#,
            "{m}"
        );
    }

    /// Malformed JSONL is a narrower path, never a bypass: an unparsable line is
    /// redacted flat, with every layer running.
    #[test]
    fn malformed_line_falls_back_to_flat_redaction() {
        let content = format!(
            "{}\n{}",
            r#"{"ok":"clean line"}"#, r#"{"signature":"AKIAIOSFODNN7EXAMPLE","broken":"#
        );
        let m = masked(&content);
        assert!(
            !m.contains("AKIAIOSFODNN7EXAMPLE"),
            "a truncated line must not shelter a secret: {m}"
        );
    }

    /// Valid JSON can encode one string many ways: a leading `A` written as a
    /// six-character unicode escape decodes to the same secret, but the needle the
    /// walk re-encodes ("A...") is then nowhere in the raw line. A naive
    /// substitution no-ops and the secret ships. Fail closed to flat redaction.
    #[test]
    fn unverifiable_substitution_falls_back_instead_of_leaking() {
        // Built by concatenation so the escape reaches the JSON, not rustc.
        let line = ["{\"note\":\"", "\\", "u0041KIAIOSFODNN7EXAMPLE\"}"].concat();
        let m = masked(&line);
        assert!(
            !m.contains("KIAIOSFODNN7EXAMPLE"),
            "escaped-encoding secret survived: {m}"
        );
    }

    /// A multi-line detector still sees its subject: consecutive non-JSON lines
    /// are redacted as one block, not one at a time.
    #[test]
    fn non_json_runs_keep_multi_line_detectors_whole() {
        let content = "{\"ok\":1}\n-----BEGIN RSA PRIVATE KEY-----\nABCDEF0123\nGHIJKL4567\n-----END RSA PRIVATE KEY-----\n{\"ok\":2}";
        let m = masked(content);
        assert!(!m.contains("ABCDEF0123"), "{m}");
        assert!(m.contains("[REDACTED:key]"), "{m}");
        assert!(m.starts_with("{\"ok\":1}\n"), "{m}");
        assert!(m.ends_with("\n{\"ok\":2}"), "{m}");
    }

    /// A secret written *as a key* is sealed, and the line it produces is still
    /// JSON.
    #[test]
    fn object_keys_are_redacted_and_stay_valid_json() {
        let m = masked(r#"{"AKIAIOSFODNN7EXAMPLE":"value","note":"ok"}"#);
        assert!(!m.contains("AKIAIOSFODNN7EXAMPLE"), "{m}");
        let parsed: Value = serde_json::from_str(&m).expect("redacted line must still be JSON");
        assert_eq!(parsed["[REDACTED:vendor]"], "value");
        assert_eq!(parsed["note"], "ok", "untouched fields must survive: {m}");
    }

    /// An empty key is nothing to redact — but the leaf under it still is, and
    /// its substitution falls back to the exact-and-global form, because an
    /// empty anchor can address no key position.
    #[test]
    fn empty_object_key_still_redacts_its_leaf() {
        let m = masked(r#"{"":"AKIAIOSFODNN7EXAMPLE"}"#);
        assert_eq!(m, r#"{"":"[REDACTED:vendor]"}"#);
    }

    /// The verified-substitution rule covers keys as well: a key encoded another
    /// valid way re-encodes to a needle that is nowhere in the raw line, so the
    /// line falls back to flat redaction instead of shipping the secret.
    #[test]
    fn unverifiable_key_substitution_falls_back_instead_of_leaking() {
        // Built by concatenation so the escape reaches the JSON, not rustc.
        let line = ["{\"", "\\", "u0041KIAIOSFODNN7EXAMPLE\":\"v\"}"].concat();
        let m = masked(&line);
        assert!(
            !m.contains("KIAIOSFODNN7EXAMPLE"),
            "escaped-encoding key secret survived: {m}"
        );
    }

    #[test]
    fn blank_and_plain_lines_are_preserved() {
        let m = masked("hello\n\nworld");
        assert_eq!(m, "hello\n\nworld");
    }

    /// Only the escapes `serde_json` itself writes keep a line on the structural
    /// path. The control-character forms matter as much as the rejections do:
    /// terminal output is full of them, and rejecting those would push most real
    /// tool results onto the flat path.
    #[test]
    fn only_canonical_escapes_keep_a_line_structural() {
        let bs = '\\';
        for ok in [
            format!(r#"{{"a":"line{bs}nbreak"}}"#),
            format!(r#"{{"a":"tab{bs}there"}}"#),
            format!(r#"{{"a":"quote{bs}"inside"}}"#),
            format!(r#"{{"a":"back{bs}{bs}slash"}}"#),
            format!(r#"{{"a":"esc{bs}u001b[0m"}}"#),
            format!(r#"{{"a":"nul{bs}u0000here"}}"#),
        ] {
            assert!(
                escapes_are_canonical(&ok),
                "rejected a serde spelling: {ok}"
            );
        }
        for rejected in [
            // a printable character spelled long-hand
            format!(r#"{{"a":"{bs}u0041BCDEFGHIJKLMNOPQRS"}}"#),
            // a control character that has a short form
            format!(r#"{{"a":"{bs}u000aline"}}"#),
            // upper-case hex; serde writes lower-case
            format!(r#"{{"a":"{bs}u001B[0m"}}"#),
            // legal JSON, not how serde writes a slash
            format!(r#"{{"a":"http:{bs}/{bs}/host"}}"#),
            // truncated escape (the parse would reject it too)
            format!(r#"{{"a":"{bs}u00"}}"#),
            format!(r#"{{"a":"{bs}q"}}"#),
        ] {
            assert!(
                !escapes_are_canonical(&rejected),
                "accepted a spelling the model does not carry: {rejected}"
            );
        }
    }

    /// A repeated key is refused at every depth, and a document without one is
    /// still walked.
    #[test]
    fn a_repeated_key_refuses_the_parse_at_any_depth() {
        for repeated in [
            r#"{"k":"a","k":"b"}"#,
            r#"{"outer":{"k":"a","k":"b"}}"#,
            r#"[{"k":"a","k":"b"}]"#,
            r#"{"a":[1,{"deep":[{"k":"a","k":"b"}]}]}"#,
            r#"{"k":"a","k":"a"}"#,
        ] {
            assert!(
                parse_faithfully(repeated).is_none(),
                "walked a document with a repeated key: {repeated}"
            );
        }
        assert!(parse_faithfully(r#"{"a":{"k":1},"b":{"k":2}}"#).is_some());
        // Every scalar shape the visitor has to carry, including the negative
        // integer that is the only way to reach its signed arm.
        assert!(parse_faithfully(r#"[1,-1,2.5,true,false,null,"s"]"#).is_some());
    }

    /// [`walk`] recurses once per level, so a document deeper than the stack
    /// would abort the process rather than fail a test. It never gets the
    /// chance: `serde_json` refuses the parse first, and the refusal is what
    /// bounds the walk.
    #[test]
    fn a_document_too_deep_to_walk_is_refused_by_the_parse() {
        let deep = format!("{}1{}", "[".repeat(50_000), "]".repeat(50_000));
        assert!(parse_faithfully(&deep).is_none());
        // …and the content path answers with flat redaction rather than a walk.
        let mut report = blank_report();
        assert!(redact_content(&deep, &RulePackSet::builtin(), &mut report).is_ok());
    }

    /// The last line of defence, kept even though the faithful-parse guard now
    /// stops these inputs earlier: a replacement that cannot be located refuses
    /// the whole line rather than shipping a silent miss.
    #[test]
    fn an_unlocatable_replacement_refuses_the_whole_line() {
        let values = Collected {
            values: vec![Replacement {
                key: "k".to_string(),
                original: "nowhere".to_string(),
                redacted: "[REDACTED:vendor]".to_string(),
            }],
            ..Collected::default()
        };
        assert!(apply(r#"{"k":"here"}"#, &values).is_none());
        let keys = Collected {
            keys: vec![KeyReplacement {
                original: "nowhere".to_string(),
                redacted: "[REDACTED:vendor]".to_string(),
            }],
            ..Collected::default()
        };
        assert!(apply(r#"{"k":"here"}"#, &keys).is_none());
        // An array element has no key to anchor to, so its substitution is
        // exact and global — and refuses the line just the same when the text
        // it was built from is not there.
        let unanchored = Collected {
            values: vec![Replacement {
                key: String::new(),
                original: "nowhere".to_string(),
                redacted: "[REDACTED:vendor]".to_string(),
            }],
            ..Collected::default()
        };
        assert!(apply(r#"["here"]"#, &unanchored).is_none());
    }

    /// A key's text can also stand in a *value* position. Substitution is
    /// anchored to a key **position** — the text followed by a colon — so the
    /// element `"k"` in the array below is stepped over, and the real member is
    /// the one that gets rewritten.
    #[test]
    fn a_key_shaped_value_is_not_mistaken_for_a_key() {
        let out = replace_after_key(
            r#"["k",{"k":"S"}]"#,
            r#""k""#,
            r#""S""#,
            r#""[REDACTED:vendor]""#,
        );
        assert_eq!(
            out.as_deref(),
            Some(r#"["k",{"k":"[REDACTED:vendor]"}]"#),
            "the array element was treated as a key position"
        );
    }

    /// The dedup key is `(anchor, leaf)` as a **tuple**. Joined with a NUL it
    /// was ambiguous, and the ambiguity dropped a replacement — the secret then
    /// shipped inside an otherwise-successful line. Both documents here collide
    /// under `format!("{anchor}\0{leaf}")` and must not collide now.
    #[test]
    fn a_nul_bearing_key_and_value_do_not_collide() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let nul = '\u{0}';
        // ("a\0b", secret) and ("a", "b\0" + secret) share a NUL-joined key.
        let mut acc = Collected::default();
        assert!(acc
            .seen_values
            .insert((format!("a{nul}b"), secret.to_string())));
        assert!(acc
            .seen_values
            .insert(("a".to_string(), format!("b{nul}{secret}"))));
        assert_eq!(acc.seen_values.len(), 2, "the two keys collided");
        // End to end, through a real document carrying both.
        let line = format!("{{\"a\\u0000b\":\"{secret}\",\"a\":\"b\\u0000{secret}\"}}");
        let m = masked(&line);
        assert!(!m.contains(secret), "a NUL-joined key dropped a leaf: {m}");
    }

    /// A fixed-seed xorshift. The corpus below has to be the same on every
    /// machine and every run — a differential guard that only sometimes covers
    /// the bypass is not a guard — so this is a seeded generator, not an OS RNG,
    /// and a failure prints the exact document that produced it.
    struct Gen(u64);

    impl Gen {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, n: usize) -> usize {
            let n = u64::try_from(n).unwrap_or(1).max(1);
            usize::try_from(self.next() % n).unwrap_or(0)
        }

        fn chance(&mut self, percent: u64) -> bool {
            self.next() % 100 < percent
        }
    }

    /// Secrets every layer of the flat pipeline seals on its own.
    const NEEDLES: &[&str] = &[
        "AKIAIOSFODNN7EXAMPLE",
        "Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2q",
        "alice@example.com",
        "ghp_QWERTYUIOPASDFGHJKLZXCVBNM0123456789",
        "10.0.0.42",
    ];

    /// **No exclusions.** The old generator kept structural keys and `type`/
    /// `data` out of this list, because a reduced scope was the one place the
    /// walk was allowed to seal less — and a verifier's third reject named that
    /// exclusion as the hole: the only differential guard excluded exactly the
    /// machinery that could reduce scope. With reduction gone the guard is
    /// total, so every key a transcript can carry belongs here, including the
    /// ones that used to grant protection.
    const KEYS: &[&str] = &[
        "note",
        "text",
        "body",
        "content",
        "value",
        "signature",
        "thinkingSignature",
        "session_id",
        "tool_use_id",
        "traceID",
        "ids",
        "file_path",
        "cwd",
        "path",
        "type",
        "data",
        "uuid",
        "ssn",
        "password",
    ];

    /// **19 characters, and the number is the whole point.** The entropy floor
    /// is 20, so this token is invisible to the flat pipeline on its own — but
    /// pressed against a `\n` escape it is not on its own: the raw bytes spell
    /// the escape as a backslash and an `n`, the backslash is not a token
    /// character and the `n` is, so the flat scanner reads a 20-character token
    /// and masks it. The model spells the same leaf with a real newline, so the
    /// walk reads 19 and keeps it.
    ///
    /// This class was reasoned about first and produced **zero** leaks in the
    /// first measurement, because every other needle here is long enough to be
    /// sealed with or without the escape. It is planted rather than argued.
    const SHORT_NEEDLE: &str = "Zx9Qw3eR7tY2uI8oP1a";

    /// Values that make a *key* load-bearing: the detector needs the label and
    /// the digits in one text, so a per-leaf walk that judges the value alone
    /// cannot see them. Each is planted as a whole leaf under its own key.
    const LABELLED: &[(&str, &str)] = &[
        ("ssn", "123456789"),
        ("social_security", "123456789"),
        ("tax_id", "123456789"),
        ("password", "hunter2secretvalue"),
    ];

    const FILLER: &[&str] = &[
        "hello world",
        "the quick brown fox",
        r"line\none",
        r#"quote\"inside"#,
        "ok",
    ];

    /// One generated document, planting needles as it goes.
    fn generate(g: &mut Gen, depth: usize, planted: &mut Vec<String>) -> String {
        if depth >= 3 || g.chance(25) {
            return string_literal(g, planted);
        }
        if g.chance(55) {
            let members = 1 + g.below(3);
            let mut parts = Vec::new();
            for _ in 0..members {
                // a labelled pair, whose detector reads the key and the value
                // together and therefore cannot survive being split into leaves
                if g.chance(12) {
                    let (label, value) = LABELLED[g.below(LABELLED.len())];
                    planted.push((*value).to_string());
                    parts.push(format!("\"{label}\":\"{value}\""));
                    continue;
                }
                // a needle in key position: a key is not a shelter either
                let key = if g.chance(15) {
                    let needle = NEEDLES[g.below(NEEDLES.len())];
                    planted.push((*needle).to_string());
                    format!("\"{needle}\"")
                } else {
                    format!("\"{}\"", KEYS[g.below(KEYS.len())])
                };
                // …and sometimes the same key twice, the earlier value being the
                // one every last-wins parser throws away before any detector
                // could have seen it
                if g.chance(30) {
                    let shadowed = generate(g, depth + 1, planted);
                    parts.push(format!("{key}:{shadowed}"));
                }
                let value = generate(g, depth + 1, planted);
                parts.push(format!("{key}:{value}"));
            }
            return format!("{{{}}}", parts.join(","));
        }
        let items = 1 + g.below(3);
        let parts: Vec<String> = (0..items)
            .map(|_| generate(g, depth + 1, planted))
            .collect();
        format!("[{}]", parts.join(","))
    }

    /// One string literal: filler, or filler carrying a needle — sometimes with
    /// the needle's first character spelled as a six-character escape, so the
    /// raw bytes and the decoded leaf disagree about what is written there, and
    /// sometimes with the needle pressed directly against a `\n`, whose
    /// backslash is not a token character in the raw bytes while its `n` is.
    fn string_literal(g: &mut Gen, planted: &mut Vec<String>) -> String {
        let filler = FILLER[g.below(FILLER.len())];
        if !g.chance(60) {
            return format!("\"{filler}\"");
        }
        let needle = NEEDLES[g.below(NEEDLES.len())];
        // an occasional canonical control escape, which must *not* cost the line
        // its structural walk
        let lead = if g.chance(15) {
            format!("{}u001b[0m ", '\\')
        } else {
            String::new()
        };
        if g.chance(25) {
            let (head, tail) = needle.split_at(1);
            planted.push(tail.to_string());
            let code = u32::from(head.as_bytes()[0]);
            return format!("\"{lead}{}u{code:04x}{tail} {filler}\"", '\\');
        }
        // Pressed against a short-form escape: in the raw bytes the token starts
        // at the escape's letter and runs into the needle, so the flat scanner
        // and the walk are looking at different tokens. The short needle is the
        // one that makes the difference visible — the others clear the entropy
        // floor either way.
        if g.chance(30) {
            let short = g.chance(50);
            let n = if short { SHORT_NEEDLE } else { needle };
            planted.push(n.to_string());
            return format!("\"{lead}{filler}{}n{n}\"", '\\');
        }
        planted.push((*needle).to_string());
        format!("\"{lead}{filler} {needle}\"")
    }

    /// **The differential guard, and it is total.** Structure may never seal
    /// less than flat text — asserted over generated documents rather than a
    /// list of the bypasses somebody already thought of, and with **no key
    /// excluded**, which is the change the 2026-08-08 ruling requires: with no
    /// scope to reduce, there is no category of key the guard is allowed to look
    /// away from.
    ///
    /// The corpus mixes what a hostile tool result can legally write: repeated
    /// keys at any depth (whose earlier value no last-wins parser keeps),
    /// alternative escape spellings (whose decoded leaf is not what the raw
    /// bytes say), needles pressed against a short-form escape (whose token
    /// boundary moves between the raw bytes and the model), labelled values
    /// whose detector needs the key and the value in one text, and needles in
    /// leaves, in array elements and in key position. Each needle is a secret
    /// the flat pipeline seals on these exact bytes; the walk is then required
    /// to seal it too.
    #[test]
    fn walk_seals_at_least_as_much_as_flat_on_generated_json() {
        let mut g = Gen(0x5eed_1234_9876_abcd);
        let mut checked = 0usize;
        for case in 0..1500 {
            let mut planted = Vec::new();
            let input = generate(&mut g, 0, &mut planted);
            let reference = flat(&input);
            let walked = masked(&input);
            for needle in &planted {
                // flat text did not seal this one either — nothing to prove, and
                // pretending otherwise would only pin the heuristics' own gaps
                if reference.contains(needle.as_str()) {
                    continue;
                }
                checked += 1;
                assert!(
                    !walked.contains(needle.as_str()),
                    "case {case}: structure sealed less than flat text\n  input:  {input}\n  \
                     needle: {needle}\n  flat:   {reference}\n  walk:   {walked}"
                );
            }
        }
        // A guard that proved nothing would pass silently; this is the floor the
        // generator has to keep clearing.
        assert!(
            checked >= 1000,
            "only {checked} needles were actually sealed by flat text — the corpus stopped \
             exercising the guard"
        );
    }

    /// The generator reaches the three shapes it was extended for. Without this
    /// the guard above could go quietly vacuous on any of them — a corpus that
    /// stops producing labelled pairs still passes, and proves nothing about the
    /// class it was written for.
    #[test]
    fn the_generated_corpus_reaches_every_shape_it_claims_to() {
        let mut g = Gen(0x5eed_1234_9876_abcd);
        let (mut labelled, mut escape_adjacent, mut structural_key, mut key_needle) = (0, 0, 0, 0);
        for _ in 0..1500 {
            let mut planted = Vec::new();
            let input = generate(&mut g, 0, &mut planted);
            if LABELLED
                .iter()
                .any(|(l, v)| input.contains(&format!("\"{l}\":\"{v}\"")))
            {
                labelled += 1;
            }
            if input.contains(&format!("\\n{SHORT_NEEDLE}")) {
                escape_adjacent += 1;
            }
            if input.contains("\"signature\":") || input.contains("\"session_id\":") {
                structural_key += 1;
            }
            if NEEDLES.iter().any(|n| input.contains(&format!("\"{n}\":"))) {
                key_needle += 1;
            }
        }
        assert!(labelled > 0, "no labelled key/value pair was generated");
        assert!(
            escape_adjacent > 0,
            "no needle was pressed against a short-form escape"
        );
        assert!(
            structural_key > 0,
            "no formerly-protected key name was generated"
        );
        assert!(key_needle > 0, "no needle was generated in key position");
    }

    /// The mechanism the guard above rests on, asserted on its own so a
    /// regression names itself: a line the walk keeps is a line the flat
    /// pipeline has nothing further to say about. The two inputs are the two
    /// classes that made this necessary — a label the key carries, and a token
    /// boundary that moves with an escape — and both are shown to reach the flat
    /// path rather than a structural output that seals less.
    #[test]
    fn the_structural_output_is_a_fixpoint_of_the_flat_pipeline() {
        for line in [
            r#"{"ssn":"123456789"}"#,
            &["{\"note\":\"abc", "\\", "nZx9Qw3eR7tY2uI8oP1aS5dF6gH\"}"].concat(),
        ] {
            let m = masked(line);
            let mut report = blank_report();
            let again = redact_fragment(&m, &RulePackSet::builtin(), &mut report).expect("flat");
            assert_eq!(again, m, "the emitted line is not closed under redaction");
            // …and each is masked at all, so the fixpoint is not vacuous.
            assert_ne!(m, line, "{line} was emitted unchanged");
        }
    }

    /// The premise the whole structural path rests on, asserted directly instead
    /// of inferred from its consequences: when the parse is accepted, every
    /// string the model holds — leaf or key — is present in the raw bytes
    /// spelled exactly as the model spells it. That is what makes "re-encode and
    /// search" a total search, and it is the sentence that was false while a
    /// repeated key could drop a value out of the model.
    #[test]
    fn an_accepted_parse_spells_every_string_the_way_the_raw_bytes_do() {
        let mut g = Gen(0xfeed_face_dead_beef);
        let mut accepted = 0usize;
        // The generator writes strings, objects and arrays; this one document
        // carries the scalars too, so the walk over the model is exercised on
        // every node kind a transcript can hold.
        let scalars = r#"{"n":-1,"big":42,"f":2.5,"t":true,"f2":false,"nil":null,"s":"text"}"#;
        for case in 0..1500 {
            let mut planted = Vec::new();
            let input = if case == 0 {
                scalars.to_string()
            } else {
                generate(&mut g, 0, &mut planted)
            };
            let Some(value) = parse_faithfully(&input) else {
                continue;
            };
            accepted += 1;
            for text in strings_of(&value) {
                let encoded = encode(&text).expect("encodable");
                assert!(
                    input.contains(&encoded),
                    "the model holds a string the raw bytes do not: {encoded} not in {input}"
                );
            }
        }
        // Anti-vacuity, not a tuning knob: most generated documents are
        // deliberately unfaithful (a repeat or an odd escape) and take the flat
        // path, so this floor sits well under the number that are walked today.
        assert!(
            accepted >= 250,
            "only {accepted} documents were walked at all"
        );
    }

    /// Every string in a parsed document: object keys included, because they are
    /// scanned like any other text.
    fn strings_of(value: &Value) -> Vec<String> {
        fn walk_strings(value: &Value, out: &mut Vec<String>) {
            match value {
                Value::String(s) => out.push(s.clone()),
                Value::Array(items) => {
                    for item in items {
                        walk_strings(item, out);
                    }
                }
                Value::Object(map) => {
                    for (key, child) in map {
                        out.push(key.clone());
                        walk_strings(child, out);
                    }
                }
                Value::Null | Value::Bool(_) | Value::Number(_) => {}
            }
        }
        let mut out = Vec::new();
        walk_strings(value, &mut out);
        out
    }
}
