//! The canonical form of a serve envelope — the only form a line may take in
//! a shard's `serve_audit.jsonl` — and the lines a session's serve rows are
//! frozen as.
//!
//! A shard artifact carries identifiers, hashes, paths, counts, timestamps
//! and Rift's closed vocabularies, and never a byte a person or a model
//! typed. A serve envelope is mostly that already; this module is where
//! "mostly" becomes "only", member by member.
//!
//! The canonical line keeps the declared members and nothing else, replaces
//! each member that can hold typed text with [`WITHHELD`], and is
//! serialised with sorted keys, so its bytes are a pure function of the
//! envelope and `git show` prints one object per line. A line with a member
//! the table does not declare is refused whole: an unknown member is one
//! nobody has audited.
//!
//! The idempotency key survives untouched. The appender reads it off the
//! envelope (`idempotency_key`) and never recomputes it from the payload, so
//! a frozen line appended again lands on the key of the row it was frozen
//! from, placeholder or not.
//!
//! # The audit
//!
//! Top level, schema 1: `schema`, `verb`, `idempotency_key`, `payload`, and
//! the integer `recorded_at` its row was written at, which a rebuild books
//! the row at. Schema 4 has no top-level instant and adds `serve_call`:
//! `call_id` (must equal the key), `channel` (the closed pair), `tool_name`
//! (Rift's `<family>:<step>` or the skill verb; kept when it is in that
//! grammar, [`WITHHELD`] otherwise), `recorded_at`, `tokens_returned`,
//! `tokens_avoided`. Schemas 2 and 3 are refused: nothing produces them any
//! more.
//!
//! Either schema may carry its attribution: `session_id`, the session that
//! made the serve, and beside it `window_key`, the window journal's one-way
//! key of the context window. Both are identifiers, never text: a session id
//! that is not safe to name a file with, or a window key that is not 64
//! lowercase hex, refuses the line rather than being withheld, and a window
//! without a session is refused too.
//!
//! | Payload member | Kinds | Frozen as |
//! |---|---|---|
//! | `kind` | every kind | kept; must name the verb's own kind |
//! | `estimated_tokens_*`, `sections`, `total_tokens` | every kind that has them | kept (counts) |
//! | `handle` | `read_*`, `doc_*` | kept when `file:<path>@sha256:<hash>[:L<lo>-<hi>]`, else withheld |
//! | `handle` | `summary_served`, `invalidated` | kept when `summary:<path>`, else withheld |
//! | `handle` | `exact_retrieved` | kept when a file, summary, `log:` or citation handle, else withheld |
//! | `handle` | `grep_capped` | kept when `grep:matches=<n>`, else withheld |
//! | `target` | `refused` | kept only when a citation; otherwise **withheld** |
//! | `reason` | `exact_retrieved` | **withheld** (the caller's own words), unless it is the grant tag |
//! | `reason` | every other kind that has one | kept when a closed tag (`[a-z0-9_-]`, at most 64 bytes), else withheld |
//! | `reason` | a kind that declares none | kept only when it is the grant tag |
//!
//! `ExactRetrieved.reason` is the reason the caller supplied, typed and
//! mandatory: a row rebuilt from a frozen line carries the placeholder,
//! and the caller's words stay where they were only ever meant to be. A
//! refusal's `target` is the string the caller asked for — a refusal is
//! exactly the case where nothing proved it names a real file, and a
//! fabricated target is text — so only a citation, a grammar the pack
//! compiler composes, is kept. Every other `handle` is composed by Rift
//! from a repository path and a content hash it computed.

use serde_json::{Map, Value};

/// What a frozen line carries where the envelope held typed text.
pub(crate) const WITHHELD: &str = "withheld";

/// How one payload member is frozen.
#[derive(Clone, Copy)]
enum Member {
    /// A token or section count.
    Count,
    /// A neutral tag from a closed vocabulary.
    Tag,
    /// Text the caller typed.
    Typed,
    /// `file:<path>@sha256:<hash>[:L<lo>-<hi>]`.
    FileHandle,
    /// `summary:<path>`.
    SummaryHandle,
    /// Any handle an exact retrieval can have served.
    ExactHandle,
    /// `grep:matches=<n>`.
    GrepHandle,
    /// What a refusal refused.
    RefusedTarget,
    /// A `reason` on a kind that declares none: only the grant tag.
    GrantTag,
}

const RETURNED: (&str, Member) = ("estimated_tokens_returned", Member::Count);
const AVOIDED: (&str, Member) = ("estimated_tokens_avoided", Member::Count);
const REASON: (&str, Member) = ("reason", Member::Tag);
const FILE: (&str, Member) = ("handle", Member::FileHandle);
const SUMMARY: (&str, Member) = ("handle", Member::SummaryHandle);

/// The canonical line of one envelope, or `None` when it is not one this
/// table declares.
pub(crate) fn canonical_serve_line(envelope: &[u8]) -> Option<Vec<u8>> {
    let Ok(Value::Object(env)) = serde_json::from_slice::<Value>(envelope) else {
        return None;
    };
    let schema = env.get("schema")?.as_u64()?;
    let members: &[&str] = match schema {
        1 => &[
            "idempotency_key",
            "payload",
            "recorded_at",
            "schema",
            "session_id",
            "verb",
            "window_key",
        ],
        4 => &[
            "idempotency_key",
            "payload",
            "schema",
            "serve_call",
            "session_id",
            "verb",
            "window_key",
        ],
        _ => return None,
    };
    if env.keys().any(|k| !members.contains(&k.as_str())) {
        return None;
    }
    let key = env.get("idempotency_key")?.as_str()?;
    let verb = env.get("verb")?.as_str()?;
    if !is_key(key) {
        return None;
    }
    let mut out = canonical_attribution(&env)?;
    out.insert("idempotency_key".into(), key.into());
    out.insert(
        "payload".into(),
        canonical_payload(verb, env.get("payload")?)?,
    );
    out.insert("schema".into(), schema.into());
    out.insert("verb".into(), verb.into());
    if schema == 4 {
        out.insert(
            "serve_call".into(),
            canonical_call(key, env.get("serve_call")?)?,
        );
    } else {
        out.insert(
            "recorded_at".into(),
            env.get("recorded_at")?.as_i64()?.into(),
        );
    }
    serde_json::to_vec(&Value::Object(out)).ok()
}

/// The frozen line of every serve `session_id` made, each with its key, in
/// the order of the rows; `None` in place of a row whose line the table
/// refuses.
///
/// A line is rebuilt from its row's columns and nothing else, then passed
/// through [`canonical_serve_line`] like any line, so the one table above
/// still decides every member. A decision's payload carries the members its
/// kind declares that a row holds — the handle (a refusal's target), the
/// reason, the two estimates; a pack's `sections` and `total_tokens` are in
/// no row, and the appender reads neither, so a rebuilt row is the row. A
/// call's payload carries only its kind: a call row holds no handle and no
/// reason, and its figures are the call record's own.
pub(crate) fn session_serve_lines(
    serves: &rift_recall_ledger::attribution::SessionServes,
    session_id: &str,
) -> Vec<Option<(String, Vec<u8>)>> {
    let decisions = serves.decisions.iter().map(|row| {
        let envelope = decision_envelope(row, session_id)?;
        keyed_line(&row.audit_id, &envelope)
    });
    let calls = serves.calls.iter().map(|row| {
        let envelope = call_envelope(row, session_id)?;
        keyed_line(&row.call_id, &envelope)
    });
    decisions.chain(calls).collect()
}

/// A rebuilt envelope's canonical line, under the key it was rebuilt with.
fn keyed_line(key: &str, envelope: &Value) -> Option<(String, Vec<u8>)> {
    let line = canonical_serve_line(&serde_json::to_vec(envelope).ok()?)?;
    Some((key.to_owned(), line))
}

/// The schema-1 envelope one decision row was booked from, as far as the row
/// holds it.
fn decision_envelope(
    row: &rift_recall_ledger::attribution::AttributedDecision,
    session_id: &str,
) -> Option<Value> {
    let kind = row.verb.strip_prefix("memd.")?;
    let members = declared(kind)?;
    let mut payload = Map::new();
    payload.insert("kind".into(), kind.into());
    for (name, _) in members {
        let value: Value = match *name {
            "handle" | "target" => row.handle.as_deref().map_or(Value::Null, Value::from),
            "reason" => row.reason.as_deref().map_or(Value::Null, Value::from),
            "estimated_tokens_returned" => row.tokens_returned.into(),
            "estimated_tokens_avoided" => row.tokens_avoided.into(),
            // `sections` and `total_tokens`: no row holds them.
            _ => Value::Null,
        };
        if !value.is_null() {
            payload.insert((*name).into(), value);
        }
    }
    // A reason on a kind that declares none is the grant tag, and the row
    // holds it like any other.
    if let Some(reason) = row.reason.as_deref() {
        payload
            .entry("reason")
            .or_insert_with(|| Value::from(reason));
    }
    let mut envelope = attribution(session_id, row.window_key.as_deref());
    envelope.insert("idempotency_key".into(), row.audit_id.as_str().into());
    envelope.insert("payload".into(), Value::Object(payload));
    envelope.insert("recorded_at".into(), row.recorded_at.into());
    envelope.insert("schema".into(), 1.into());
    envelope.insert("verb".into(), row.verb.as_str().into());
    Some(Value::Object(envelope))
}

/// The schema-4 envelope one call row was booked from.
fn call_envelope(
    row: &rift_recall_ledger::attribution::AttributedCall,
    session_id: &str,
) -> Option<Value> {
    let kind = row.terminal_verb.strip_prefix("memd.")?;
    let call = serde_json::json!({
        "call_id": row.call_id,
        "channel": row.channel,
        "recorded_at": row.recorded_at,
        "tokens_avoided": row.tokens_avoided,
        "tokens_returned": row.tokens_returned,
        "tool_name": row.tool_name,
    });
    let mut envelope = attribution(session_id, row.window_key.as_deref());
    envelope.insert("idempotency_key".into(), row.call_id.as_str().into());
    envelope.insert("payload".into(), serde_json::json!({ "kind": kind }));
    envelope.insert("schema".into(), 4.into());
    envelope.insert("serve_call".into(), call);
    envelope.insert("verb".into(), row.terminal_verb.as_str().into());
    Some(Value::Object(envelope))
}

/// The two attribution members, the window only where the row names one.
fn attribution(session_id: &str, window_key: Option<&str>) -> Map<String, Value> {
    let mut members = Map::new();
    members.insert("session_id".into(), session_id.into());
    if let Some(window_key) = window_key {
        members.insert("window_key".into(), window_key.into());
    }
    members
}

/// A line's attribution members, checked, as the start of its canonical
/// form; `None` refuses the line.
fn canonical_attribution(env: &Map<String, Value>) -> Option<Map<String, Value>> {
    let mut out = Map::new();
    let session = match env.get("session_id") {
        None => None,
        Some(id) => Some(id.as_str().filter(|id| is_session_id(id))?),
    };
    let window = match env.get("window_key") {
        None => None,
        Some(key) => Some(key.as_str().filter(|key| is_window_key(key))?),
    };
    match (session, window) {
        (None, Some(_)) => return None,
        (None, None) => (),
        (Some(session), window) => {
            out.insert("session_id".into(), session.into());
            if let Some(window) = window {
                out.insert("window_key".into(), window.into());
            }
        }
    }
    Some(out)
}

/// The declared members of one event kind's payload, as the audit table
/// in the module doc states them.
fn declared(kind: &str) -> Option<&'static [(&'static str, Member)]> {
    use Member::{ExactHandle, GrepHandle, RefusedTarget, Typed};
    Some(match kind {
        "manifest_served" | "search_served" => &[RETURNED, AVOIDED],
        "manifest_injected" | "recovery_injected" => &[RETURNED],
        "summary_served" => &[SUMMARY, RETURNED, AVOIDED],
        "exact_retrieved" => &[("handle", ExactHandle), ("reason", Typed), RETURNED],
        "pack_built" => &[
            ("sections", Member::Count),
            ("total_tokens", Member::Count),
            RETURNED,
            AVOIDED,
        ],
        "invalidated" => &[SUMMARY, RETURNED],
        "refused" => &[("target", RefusedTarget), REASON, RETURNED],
        "read_intercepted" | "doc_rewritten" | "doc_served" => &[FILE, REASON, RETURNED, AVOIDED],
        "read_repeat_observed" => &[FILE, REASON],
        "read_prevented" => &[FILE, REASON, AVOIDED],
        "search_answered" => &[REASON, RETURNED, AVOIDED],
        "grep_capped" => &[("handle", GrepHandle)],
        "doc_deferred" => &[FILE, REASON, RETURNED],
        _ => return None,
    })
}

/// The payload with every member frozen by its rule, or `None` when the
/// kind is unknown, disagrees with the verb, or carries an undeclared
/// member.
fn canonical_payload(verb: &str, payload: &Value) -> Option<Value> {
    let kind = verb.strip_prefix("memd.")?;
    let (Some(members), Value::Object(fields)) = (declared(kind), payload) else {
        return None;
    };
    if fields.get("kind")?.as_str()? != kind {
        return None;
    }
    let mut out = Map::new();
    for (name, value) in fields {
        let rule = match members.iter().find(|(declared, _)| declared == name) {
            Some((_, rule)) => *rule,
            None if name == "kind" => {
                out.insert(name.clone(), kind.into());
                continue;
            }
            None if name == "reason" => Member::GrantTag,
            None => return None,
        };
        out.insert(name.clone(), frozen_member(rule, value)?);
    }
    Some(Value::Object(out))
}

/// The call record of a schema-4 line, or `None` when it is not the one
/// the appender would book.
fn canonical_call(key: &str, call: &Value) -> Option<Value> {
    const MEMBERS: [&str; 6] = [
        "call_id",
        "channel",
        "recorded_at",
        "tokens_avoided",
        "tokens_returned",
        "tool_name",
    ];
    let Value::Object(fields) = call else {
        return None;
    };
    if fields.keys().any(|k| !MEMBERS.contains(&k.as_str())) {
        return None;
    }
    let channel = fields.get("channel")?.as_str()?;
    let tool_name = fields.get("tool_name")?.as_str()?;
    if fields.get("call_id")?.as_str()? != key
        || rift_recall_serve::ServeChannel::parse(channel).is_none()
    {
        return None;
    }
    let mut out = Map::new();
    out.insert("call_id".into(), key.into());
    out.insert("channel".into(), channel.into());
    out.insert(
        "recorded_at".into(),
        fields.get("recorded_at")?.as_i64()?.into(),
    );
    for count in ["tokens_avoided", "tokens_returned"] {
        out.insert(count.into(), fields.get(count)?.as_u64()?.into());
    }
    let tool_name = if is_tool_name(tool_name) {
        tool_name
    } else {
        WITHHELD
    };
    out.insert("tool_name".into(), tool_name.into());
    Some(Value::Object(out))
}

/// One member frozen by its rule; `None` when its JSON type is not the
/// one the rule reads, which refuses the whole line.
fn frozen_member(rule: Member, value: &Value) -> Option<Value> {
    if let Member::Count = rule {
        return Some(value.as_u64()?.into());
    }
    let text = value.as_str()?;
    Some(if is_kept(rule, text) { text } else { WITHHELD }.into())
}

/// Whether a text member's value is one its rule lets through.
fn is_kept(rule: Member, text: &str) -> bool {
    let grant = text == rift_recall_serve::CROSS_BRANCH_GRANT_TAG;
    match rule {
        // A count that arrives as text is not a count.
        Member::Count => false,
        Member::Tag => is_tag(text),
        Member::Typed | Member::GrantTag => grant,
        Member::FileHandle => text.is_empty() || is_file_handle(text),
        Member::SummaryHandle => text.strip_prefix("summary:").is_some_and(is_path),
        Member::ExactHandle => is_exact_handle(text),
        Member::GrepHandle => text.strip_prefix("grep:matches=").is_some_and(is_digits),
        Member::RefusedTarget => is_citation(text),
    }
}

/// A handle an exact retrieval can have served: a file, a summary, a log
/// entry or a citation.
fn is_exact_handle(text: &str) -> bool {
    is_file_handle(text)
        || text.strip_prefix("summary:").is_some_and(is_path)
        || text.strip_prefix("log:").is_some_and(is_key)
        || is_citation(text)
}

/// `pack:<key>/evidence:<n>[@sha256:<hash>]`, optionally followed by
/// `=file:…` — the address the pack compiler composed and, after the `=`,
/// the record it resolved to.
fn is_citation(text: &str) -> bool {
    let (head, resolved) = match text.split_once('=') {
        Some((head, tail)) => (head, Some(tail)),
        None => (text, None),
    };
    rift_recall_serve::pack::CitationTarget::parse(head).is_some()
        && resolved.is_none_or(is_file_handle)
}

/// `file:<path>@sha256:<64 hex>`, optionally `:L<lo>-<hi>`.
fn is_file_handle(text: &str) -> bool {
    let Some((path, tail)) = text
        .strip_prefix("file:")
        .and_then(|rest| rest.rsplit_once("@sha256:"))
    else {
        return false;
    };
    let (hash, lines) = match tail.split_once(':') {
        Some((hash, lines)) => (hash, Some(lines)),
        None => (tail, None),
    };
    let range_ok = lines.is_none_or(|lines| {
        lines
            .strip_prefix('L')
            .and_then(|range| range.split_once('-'))
            .is_some_and(|(lo, hi)| is_digits(lo) && is_digits(hi))
    });
    is_path(path) && is_hex64(hash) && range_ok
}

/// A repository path as a handle carries it: non-empty, bounded, and free
/// of control characters.
fn is_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= 4096 && !path.chars().any(char::is_control)
}

fn is_hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.len() <= 10 && text.bytes().all(|b| b.is_ascii_digit())
}

/// A closed-vocabulary tag; the empty tag is what a decision without one
/// carries.
fn is_tag(text: &str) -> bool {
    text.len() <= 64
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A session id the capture plane would name a file with.
fn is_session_id(text: &str) -> bool {
    rift_shadow::hooks::is_safe_session_id(text)
}

/// A window journal key: 64 lowercase hex.
pub(crate) fn is_window_key(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// An idempotency key or a log id: a hash, a ULID or a caller's call id.
fn is_key(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `<family>:<step>` or a skill verb.
fn is_tool_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.'))
}

/// What a serve envelope may carry into a shard.
#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rift_recall_serve::{MemdEvent, ServeChannel};
    use serde_json::Value;

    use super::*;

    /// Every event kind the type has, with `text` in every string member.
    fn every_event(text: &str) -> Vec<MemdEvent> {
        let mut events = counted_events();
        events.extend(text_events(text));
        events
    }

    /// The kinds that carry counts and nothing else.
    fn counted_events() -> Vec<MemdEvent> {
        vec![
            MemdEvent::ManifestServed {
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
                pull_claim: None,
            },
            MemdEvent::ManifestInjected {
                estimated_tokens_returned: 3,
            },
            MemdEvent::PackBuilt {
                sections: 2,
                total_tokens: 7,
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
                pull_claim: None,
            },
            MemdEvent::RecoveryInjected {
                estimated_tokens_returned: 3,
            },
            MemdEvent::SearchServed {
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
        ]
    }

    /// The kinds that carry a string member, each holding `text`.
    fn text_events(text: &str) -> Vec<MemdEvent> {
        let s = || text.to_owned();
        vec![
            MemdEvent::SummaryServed {
                handle: s(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
                pull_claim: None,
            },
            MemdEvent::ExactRetrieved {
                handle: s(),
                reason: s(),
                estimated_tokens_returned: 3,
            },
            MemdEvent::Invalidated {
                handle: s(),
                estimated_tokens_returned: 3,
            },
            MemdEvent::Refused {
                target: s(),
                reason: s(),
                estimated_tokens_returned: 3,
            },
            MemdEvent::ReadIntercepted {
                handle: s(),
                reason: s(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
            MemdEvent::ReadRepeatObserved {
                handle: s(),
                reason: s(),
            },
            MemdEvent::ReadPrevented {
                handle: s(),
                reason: s(),
                estimated_tokens_avoided: 9,
            },
            MemdEvent::SearchAnswered {
                reason: s(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
            MemdEvent::GrepCapped { handle: s() },
            MemdEvent::DocRewritten {
                handle: s(),
                reason: s(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
            MemdEvent::DocDeferred {
                handle: s(),
                reason: s(),
                estimated_tokens_returned: 3,
            },
            MemdEvent::DocServed {
                handle: s(),
                reason: s(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
        ]
    }

    /// Both envelopes one event becomes, schema 1 then schema 4.
    fn envelopes(event: &MemdEvent) -> [String; 2] {
        let at = rift_core::Timestamp::from_millis(1_700_000_000_000);
        [
            timed(&event.to_hook_json_keyed("call-1")),
            event.to_serve_call_json(
                ServeChannel::HookInject,
                "call-1",
                "claude-code:read-gate",
                at,
            ),
        ]
    }

    /// A live envelope with the instant its row was written at, which a
    /// schema-1 line carries and a live schema-1 envelope does not.
    fn timed(envelope: &str) -> String {
        let mut value: Value = serde_json::from_str(envelope).unwrap();
        if value["schema"] == 1 && value.get("recorded_at").is_none() {
            value["recorded_at"] = 1_700_000_000_000_i64.into();
        }
        value.to_string()
    }

    fn frozen(envelope: &str) -> Value {
        let line =
            canonical_serve_line(timed(envelope).as_bytes()).expect("a declared envelope freezes");
        serde_json::from_slice(&line).unwrap()
    }

    /// Every kind, both schemas: one line, keys sorted (re-serialising it is
    /// the identity), the key and the verb the envelope carried, and a line
    /// that is its own canonical form.
    #[test]
    fn every_event_kind_freezes_to_a_sorted_line_that_keeps_its_key() {
        let file = format!("file:src/a.rs@sha256:{}:L1-4", "a".repeat(64));
        for event in every_event(&file) {
            for envelope in envelopes(&event) {
                let line = canonical_serve_line(envelope.as_bytes()).expect("freezes");
                let value: Value = serde_json::from_slice(&line).unwrap();
                assert_eq!(serde_json::to_vec(&value).unwrap(), line, "keys sorted");
                let original: Value = serde_json::from_str(&envelope).unwrap();
                assert_eq!(value["idempotency_key"], original["idempotency_key"]);
                assert_eq!(value["verb"], original["verb"]);
                assert_eq!(
                    canonical_serve_line(&line),
                    Some(line.clone()),
                    "idempotent"
                );
                assert!(!line.contains(&b'\n'), "one line");
            }
        }
    }

    /// The caller's reason and a refusal's typed target are withheld; Rift's
    /// own handles, tags and the grant tag are kept.
    #[test]
    fn typed_text_is_withheld_and_rifts_own_vocabulary_is_kept() {
        let exact = MemdEvent::ExactRetrieved {
            handle: format!("file:src/a.rs@sha256:{}", "b".repeat(64)),
            reason: "because the customer password is hunter2".to_owned(),
            estimated_tokens_returned: 1,
        };
        let line = frozen(&exact.to_hook_json_keyed("k"));
        assert_eq!(line["payload"]["reason"], WITHHELD);
        assert_eq!(line["payload"]["handle"], exact_handle(&exact));
        let granted = rift_recall_serve::grant_tagged_envelope(&exact, "k");
        assert_eq!(
            frozen(&granted)["payload"]["reason"],
            rift_recall_serve::CROSS_BRANCH_GRANT_TAG
        );
        let served = MemdEvent::ManifestServed {
            estimated_tokens_returned: 1,
            estimated_tokens_avoided: 2,
            pull_claim: None,
        };
        let granted = rift_recall_serve::grant_tagged_envelope(&served, "k");
        assert_eq!(
            frozen(&granted)["payload"]["reason"],
            rift_recall_serve::CROSS_BRANCH_GRANT_TAG
        );

        let refused = |target: &str| MemdEvent::Refused {
            target: target.to_owned(),
            reason: "policy_denied_exact".to_owned(),
            estimated_tokens_returned: 1,
        };
        let typed = frozen(&refused("secrets/prod.env please").to_hook_json_keyed("k"));
        assert_eq!(typed["payload"]["target"], WITHHELD);
        assert_eq!(typed["payload"]["reason"], "policy_denied_exact");
        let citation = format!("pack:{}/evidence:3", "c".repeat(64));
        assert_eq!(
            frozen(&refused(&citation).to_hook_json_keyed("k"))["payload"]["target"],
            citation.as_str()
        );

        let odd_tag = MemdEvent::SearchAnswered {
            reason: "Not A Tag".to_owned(),
            estimated_tokens_returned: 1,
            estimated_tokens_avoided: 1,
        };
        assert_eq!(
            frozen(&odd_tag.to_hook_json_keyed("k"))["payload"]["reason"],
            WITHHELD
        );
    }

    fn exact_handle(event: &MemdEvent) -> String {
        match event {
            MemdEvent::ExactRetrieved { handle, .. } => handle.clone(),
            _ => String::new(),
        }
    }

    /// A member nobody audited refuses the whole line, at every level, and so
    /// do the retired schemas and a call record the appender would not book.
    #[test]
    fn a_line_with_a_member_nobody_audited_is_refused() {
        let event = MemdEvent::GrepCapped {
            handle: "grep:matches=4".to_owned(),
        };
        let [audited, called] = envelopes(&event);
        let plant = |envelope: &str, pointer: &str| {
            let mut value: Value = serde_json::from_str(envelope).unwrap();
            value
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("note".into(), "hi".into());
            value.to_string()
        };
        for planted in [
            plant(&audited, ""),
            plant(&audited, "/payload"),
            plant(&called, "/serve_call"),
        ] {
            assert_eq!(canonical_serve_line(planted.as_bytes()), None, "{planted}");
        }
        let edit = |envelope: &str, pointer: &str, to: Value| {
            let mut value: Value = serde_json::from_str(envelope).unwrap();
            *value.pointer_mut(pointer).unwrap() = to;
            value.to_string()
        };
        for refused in [
            edit(&audited, "/schema", 2.into()),
            edit(&audited, "/verb", "memd.refused".into()),
            edit(&called, "/serve_call/call_id", "another".into()),
            edit(&called, "/serve_call/channel", "mcp".into()),
            edit(&audited, "/idempotency_key", "a key with spaces".into()),
        ] {
            assert_eq!(canonical_serve_line(refused.as_bytes()), None, "{refused}");
        }
        assert_eq!(canonical_serve_line(b"not json"), None);
    }

    /// Every string value in a parsed line, at any depth.
    fn strings_in(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(text) => out.push(text.clone()),
            Value::Object(members) => members.values().for_each(|v| strings_in(v, out)),
            Value::Array(items) => items.iter().for_each(|v| strings_in(v, out)),
            _ => (),
        }
    }

    /// A schema-1 line without the instant its row was written at is refused:
    /// a rebuild would book it at its own clock.
    #[test]
    fn a_schema_one_line_without_its_instant_is_refused() {
        let event = MemdEvent::GrepCapped {
            handle: "grep:matches=4".to_owned(),
        };
        let live = event.to_hook_json_keyed("call-1");
        assert_eq!(canonical_serve_line(live.as_bytes()), None);
        assert!(canonical_serve_line(timed(&live).as_bytes()).is_some());
    }

    /// The attribution is an identifier or nothing: a session that could not
    /// name a file, a window that is not a window key, and a window without a
    /// session each refuse the line, and a well-formed pair is kept as is.
    #[test]
    fn an_attribution_that_is_not_an_identifier_refuses_the_line() {
        let event = MemdEvent::GrepCapped {
            handle: "grep:matches=4".to_owned(),
        };
        let window = "e".repeat(64);
        for envelope in envelopes(&event) {
            let with = |session: Option<&str>, window: Option<&str>| {
                let mut value: Value = serde_json::from_str(&envelope).unwrap();
                if let Some(session) = session {
                    value["session_id"] = session.into();
                }
                if let Some(window) = window {
                    value["window_key"] = window.into();
                }
                canonical_serve_line(value.to_string().as_bytes())
            };
            let kept: Value =
                serde_json::from_slice(&with(Some("s-1"), Some(&window)).unwrap()).unwrap();
            assert_eq!(kept["session_id"], "s-1");
            assert_eq!(kept["window_key"], window.as_str());
            assert!(with(Some("s-1"), None).is_some());
            assert_eq!(with(Some("../escape"), None), None);
            assert_eq!(with(Some("s-1"), Some("not-a-window")), None);
            assert_eq!(with(None, Some(&window)), None);
        }
    }

    proptest! {
        /// Whatever text reaches a member that can hold it, no run of it longer
        /// than a hash survives into the line: every string left is a declared
        /// member's own value — a key, a verb, a tag, a handle Rift composed —
        /// or the placeholder.
        #[test]
        fn no_typed_text_longer_than_a_hash_survives(
            words in "[ -~]{0,20}",
            run in "[!-~]{65,90}",
        ) {
            let text = format!("{words} {run}");
            for event in every_event(&text) {
                for envelope in envelopes(&event) {
                    let line = canonical_serve_line(envelope.as_bytes()).expect("freezes");
                    let mut strings = Vec::new();
                    strings_in(&serde_json::from_slice(&line).unwrap(), &mut strings);
                    prop_assert!(strings.iter().all(|s| !s.contains(&run)), "{strings:?}");
                }
            }
        }
    }

    /// Every event kind with every string member in its own kind's grammar,
    /// as a live serve carries it: a handle Rift composed, a closed tag, a
    /// citation target, and the grant tag where the caller's words would be.
    fn composed_events() -> Vec<MemdEvent> {
        let file = format!("file:src/a.rs@sha256:{}:L1-4", "a".repeat(64));
        let mut events = counted_events();
        events.extend([
            MemdEvent::SummaryServed {
                handle: "summary:src/a.rs".to_owned(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
                pull_claim: None,
            },
            MemdEvent::ExactRetrieved {
                handle: file.clone(),
                reason: rift_recall_serve::CROSS_BRANCH_GRANT_TAG.to_owned(),
                estimated_tokens_returned: 3,
            },
            MemdEvent::Invalidated {
                handle: "summary:src/a.rs".to_owned(),
                estimated_tokens_returned: 3,
            },
            MemdEvent::Refused {
                target: format!("pack:{}/evidence:3", "c".repeat(64)),
                reason: "policy_denied_exact".to_owned(),
                estimated_tokens_returned: 3,
            },
            MemdEvent::GrepCapped {
                handle: "grep:matches=4".to_owned(),
            },
        ]);
        events.extend(file_events(&file, "already_held"));
        events
    }

    /// The kinds that name a file and carry a tag, with `handle` and `reason`.
    fn file_events(handle: &str, reason: &str) -> Vec<MemdEvent> {
        let (h, r) = (|| handle.to_owned(), || reason.to_owned());
        vec![
            MemdEvent::ReadIntercepted {
                handle: h(),
                reason: r(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
            MemdEvent::ReadRepeatObserved {
                handle: h(),
                reason: r(),
            },
            MemdEvent::ReadPrevented {
                handle: h(),
                reason: r(),
                estimated_tokens_avoided: 9,
            },
            MemdEvent::SearchAnswered {
                reason: r(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
            MemdEvent::DocRewritten {
                handle: h(),
                reason: r(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
            MemdEvent::DocDeferred {
                handle: h(),
                reason: r(),
                estimated_tokens_returned: 3,
            },
            MemdEvent::DocServed {
                handle: h(),
                reason: r(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
        ]
    }

    /// Every row of every relation a round trip touches: the decision rows,
    /// the call rows and the attribution rows.
    fn every_row(store: &rift_store::SqliteStore) -> Vec<String> {
        use rift_store::CanonicalStore as _;
        let queries = [
            "SELECT audit_id || '|' || verb || '|' || COALESCE(handle, '-') || '|' || \
             COALESCE(reason, '-') || '|' || tokens_returned || '|' || tokens_avoided || '|' || \
             recorded_at FROM memd_audit_event",
            "SELECT call_id || '|' || channel || '|' || tool_name || '|' || terminal_verb || \
             '|' || tokens_returned || '|' || tokens_avoided || '|' || recorded_at \
             FROM serve_call_event",
            "SELECT serve_key || '|' || relation || '|' || session_id || '|' || \
             COALESCE(window_key, '-') FROM memd_audit_session",
        ];
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mut rows: Vec<String> = queries
            .iter()
            .flat_map(|sql| rt.block_on(store.query(sql, &[])).unwrap())
            .map(|row| row.get_text(0).unwrap())
            .collect();
        rows.sort();
        rows
    }

    /// ★ **Rows to lines to rows.** Every event kind a session serves, booked
    /// live with its attribution, frozen from its rows, read back through the
    /// shard reader and re-appended into an empty projection, leaves the rows
    /// it was frozen from — every column of every relation. Typed text is the
    /// one thing that does not come back, which the withholding tests above
    /// pin; these events carry none.
    #[test]
    fn a_sessions_rows_frozen_and_read_back_are_the_same_rows() {
        use rift_recall_ledger::attribution::{
            append_attributed_serve, session_serves, ServeAttribution,
        };
        let fresh = || {
            let store = rift_store::SqliteStore::open_in_memory().unwrap();
            rift_store::apply_migrations(&store, &rift_store::recall_migrations()).unwrap();
            store
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let session = ServeAttribution {
            session_id: "s-1".to_owned(),
            window_key: Some("e".repeat(64)),
        };
        let live = fresh();
        let events = composed_events();
        assert_eq!(events.len(), every_event("").len(), "every kind, once");
        for (n, event) in events.iter().enumerate() {
            let call = format!("call-{n}");
            let at = 1_700_000_000_000 + i64::try_from(n).unwrap();
            let called = event.to_serve_call_json(
                ServeChannel::SkillCli,
                &call,
                "known-state",
                rift_core::Timestamp::from_millis(at),
            );
            for envelope in [event.to_hook_json_keyed(&call), called] {
                rt.block_on(append_attributed_serve(
                    &live,
                    envelope.as_bytes(),
                    at,
                    Some(&session),
                ))
                .unwrap();
            }
        }
        let serves = rt.block_on(session_serves(&live, "s-1")).unwrap();
        let lines: Vec<Vec<u8>> = session_serve_lines(&serves, "s-1")
            .into_iter()
            .map(|line| line.expect("every row freezes").1)
            .collect();
        let artifact: Vec<u8> = lines.join(&b'\n');
        let parsed = rift_recall_substrate::parse::parse_serve_audit(&artifact, "cp");
        assert!(parsed.is_clean(), "{:?}", parsed.issues);

        let rebuilt = fresh();
        for frozen in &parsed.value {
            let attribution = frozen
                .session_id
                .clone()
                .map(|session_id| ServeAttribution {
                    session_id,
                    window_key: frozen.window_key.clone(),
                });
            rt.block_on(append_attributed_serve(
                &rebuilt,
                &frozen.envelope,
                frozen.recorded_at,
                attribution.as_ref(),
            ))
            .unwrap();
        }
        assert_eq!(every_row(&rebuilt), every_row(&live));
        assert_eq!(
            every_row(&live).len(),
            4 * events.len(),
            "a decision row, a call row and two attributions per serve"
        );
    }
}
