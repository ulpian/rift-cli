//! The `rift hooks <agent> <verb>` **wire contract**: a total, bounded
//! parser for the untrusted stdin JSON envelope, `--schema` introspection, and
//! the [`build_raw_hook`] entrypoint that the fuzz targets drive.
//!
//! The hook process parses **untrusted JSON from the agent** — a primary fuzz
//! target. [`parse_bounded`] caps size and is total (no panic), and
//! [`build_raw_hook`] turns the wire inputs into a [`RawHook`] without ever
//! panicking, so an oversized/malformed payload is a clean reject, never a crash
//! and never a partial write.

use rift_core::{AgentId, EventKind, Result, RiftError, Trust};

use crate::RawHook;

/// Max accepted stdin payload (1 MiB) — a hostile/oversized payload is rejected,
/// not buffered unboundedly.
pub const MAX_STDIN_BYTES: usize = 1 << 20;

/// Max accepted JSON nesting depth — defeats stack exhaustion via deeply nested
/// input. (`serde_json`'s own recursion limit rejects most pathological depth
/// before we even measure it; this is a tighter, explicit ceiling.)
pub const MAX_DEPTH: u32 = 64;

/// The field names agents use for their own session id, in the order
/// [`build_raw_hook`]'s native fallback tries them.
///
/// Every entry was **verified against that agent's real hook payload schema in
/// the reference implementation Rift was checked against** — with exactly one
/// carve-out, stated rather than
/// glossed: `conversationId` is Antigravity's, and that reference supported no
/// Antigravity at all (its agent registry was claude-code, codex, copilot-cli,
/// cursor, factoryai-droid, gemini, opencode, pi), so that spelling rests on
/// Antigravity's own documentation and Rift's own tests, with no upstream
/// cross-check behind it. A spelling nobody has been observed to send is
/// deliberately absent, because a guessed key either binds nothing or binds the
/// wrong string, and both are worse than the honest empty that leaves a
/// `deferred-no-session` receipt.
///
/// Paths below use the token-free `upstream: …` namespace rooted at that
/// reference source tree. The shorthand `agent/…` does not resolve there, which is how a false
/// "unverifiable" claim about Copilot survived a whole cycle unchecked.
///
/// | spelling | agents | evidence |
/// |---|---|---|
/// | `session_id` | Claude Code, Codex, Gemini CLI, OpenCode, Factory Droid, Copilot CLI (VS Code host) | Claude Code's own hook stdin; the `session_id` json tag on every hook-payload struct in `upstream: {codex,geminicli,opencode,factoryaidroid}/types.go`; Copilot's alternate at `upstream: copilotcli/compat.go:74` |
/// | `sessionId` | Copilot CLI | `upstream: copilotcli/compat.go:74` reads `firstString(raw, "sessionId", "session_id")`, and that package's `AGENT.md` lists `sessionId` on all five captured hook payloads (`userPromptSubmitted`, `sessionStart`, `agentStop`, `sessionEnd`, `subagentStop`) |
/// | `conversation_id` | Cursor | `upstream: cursor/types.go`: `conversation_id` on all seven of Cursor's hook payload structs |
/// | `conversationId` | Antigravity | Antigravity's documented camelCase hook input — the carve-out above; no upstream reference exists |
///
/// The order is fixed and documented so a payload carrying two spellings
/// resolves the same way every time; a hook that redelivered under a different
/// session id would defeat the whole idempotency index. Copilot's own reader
/// tries `sessionId` before `session_id` and Rift tries them the other way
/// round, which cannot change an outcome: in that reader the two are alternates
/// for one field, and no documented payload carries both.
pub const SESSION_ID_FIELDS: &[&str] = &[
    "session_id",
    "sessionId",
    "conversation_id",
    "conversationId",
];

/// The prefix that marks an id as a **tool-use id, and therefore not a session
/// id at all**.
///
/// Copilot fires its *session* lifecycle hooks for subagent turns too, reusing
/// the subagent's tool-use id as the `sessionId` (`toolu_bdrk_01K…` on
/// Bedrock-backed models). Binding one opens a session that never receives a
/// matching end: an ownership marker and a session-cache file per subagent turn
/// today, and — once Copilot's verb surface maps (see the sweep's
/// `copilot-cli.verb_surface`) — a phantom *active* session per subagent that
/// pins its shadow branch open after the developer commits. Real session ids
/// are UUIDs, so the prefix is unambiguous.
///
/// Refused for **every** agent, not only Copilot: a tool-use id names no
/// session for anyone, so an agent that starts reusing one inherits the guard
/// rather than the bug.
///
/// Verified at the pinned tag:
/// `upstream: copilotcli/lifecycle.go:17`
/// (`subagentSessionIDPrefix = "toolu_"`) and the guard at :92, whose comment
/// describes exactly this failure in the retired reference implementation.
pub const TOOL_USE_ID_PREFIX: &str = "toolu_";

/// Whether `candidate` is a tool-use id ([`TOOL_USE_ID_PREFIX`]) and so names
/// no session.
#[must_use]
pub fn is_tool_use_id(candidate: &str) -> bool {
    candidate.starts_with(TOOL_USE_ID_PREFIX)
}

/// Parse untrusted hook stdin into a JSON value, bounded in size and nesting
/// depth. **Total**: returns `Err` (never panics) on oversized, malformed, or
/// too-deep input.
///
/// # Errors
/// [`RiftError::Validity`] if the input exceeds [`MAX_STDIN_BYTES`], is not valid
/// JSON, or nests deeper than [`MAX_DEPTH`].
pub fn parse_bounded(bytes: &[u8]) -> Result<serde_json::Value> {
    if bytes.len() > MAX_STDIN_BYTES {
        return Err(RiftError::validity("hook stdin: payload too large"));
    }
    // serde_json enforces its own recursion limit (~128), so this parse cannot
    // stack-overflow; the depth check below is a tighter, explicit policy bound.
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| RiftError::validity(format!("hook stdin: {e}")))?;
    if json_depth(&value) > MAX_DEPTH {
        return Err(RiftError::validity("hook stdin: nesting too deep"));
    }
    Ok(value)
}

/// Structural nesting depth of a parsed value (scalars = 0). Safe to recurse: the
/// value already passed `serde_json`'s recursion limit, so depth is bounded.
fn json_depth(v: &serde_json::Value) -> u32 {
    match v {
        serde_json::Value::Array(a) => 1 + a.iter().map(json_depth).max().unwrap_or(0),
        serde_json::Value::Object(o) => 1 + o.values().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// Build a [`RawHook`] from the wire inputs, **totally and safely** (the fuzz
/// seam). Validates the agent-token shape, bounds the stdin, and extracts
/// the `session`/`turn`/`payload` envelope fields. Input that carries none of
/// the envelope keys but has a `session_id` string is accepted as an
/// **agent-native** payload (e.g. Claude Code hook stdin): the session comes
/// from the first present of [`SESSION_ID_FIELDS`] that names a session, the
/// whole object becomes the payload, and the turn is left `None` for the ingest
/// layer to derive. An id that is really a tool-use id
/// ([`TOOL_USE_ID_PREFIX`] — Copilot's subagent lifecycle hooks) binds nothing
/// on either shape, so a subagent turn leaves a countable `deferred-no-session`
/// receipt instead of opening a session that never ends. Never panics; an unparsable,
/// oversized, or malformed payload is a clean error the caller swallows as
/// "deferred" (the hook never fails the agent).
///
/// The session id is checked here, at the boundary, because everything
/// downstream treats it as a name: the transient cache file, its temp file, and
/// the claim markers beside them. An id that is not
/// [`is_safe_session_id`](crate::session_id::is_safe_session_id) — `../evil`
/// being the shape that matters — defers the hook before anything is written,
/// rather than letting a payload the agent authored choose a path. An *absent*
/// session id is a different case and keeps its existing meaning (empty, for
/// the ingest layer to reject or derive).
///
/// # Errors
/// [`RiftError::Validity`] for a malformed agent token, an unsafe session id,
/// or a payload that fails [`parse_bounded`].
pub fn build_raw_hook(
    agent: &str,
    verb: &str,
    stdin: &[u8],
    occurred_at_ms: i64,
) -> Result<RawHook> {
    let agent_id = AgentId::new(agent);
    if !agent_id.is_well_formed() {
        return Err(RiftError::validity("hook: malformed agent token"));
    }
    let json = parse_bounded(stdin)?;
    let is_envelope = json.get("session").is_some()
        || json.get("turn").is_some()
        || json.get("payload").is_some();
    let (session, turn, payload) = if is_envelope {
        (
            json.get("session")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            json.get("turn")
                .and_then(serde_json::Value::as_u64)
                .and_then(|t| u32::try_from(t).ok()),
            json.get("payload")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
    } else {
        let named = |only_bindable: bool| {
            SESSION_ID_FIELDS.iter().find_map(|field| {
                json.get(*field)
                    .and_then(serde_json::Value::as_str)
                    .filter(|candidate| !(only_bindable && is_tool_use_id(candidate)))
            })
        };
        match (named(true), named(false)) {
            // Native fallback: the agent's own hook shape, ingested whole. A
            // tool-use id in an earlier spelling is skipped rather than bound,
            // so a later spelling holding the real session still attributes it.
            (Some(sid), _) => (sid.to_owned(), None, json.clone()),
            // Every spelling this payload carries is a tool-use id: bind
            // nothing, but keep the payload, so each refused subagent turn
            // hashes to its own `deferred-no-session` receipt instead of every
            // refusal collapsing onto one shared null.
            (None, Some(_)) => (String::new(), None, json.clone()),
            (None, None) => (String::new(), None, serde_json::Value::Null),
        }
    };
    // A tool-use id names no session on either shape (the envelope states its
    // session outright, so the fallback's skip above cannot cover it). Empty is
    // the honest answer: the ingest layer leaves a `deferred-no-session`
    // receipt, so a refused subagent lifecycle hook is a countable row rather
    // than a phantom session — where the retired reference implementation dropped
    // it to a debug log.
    let session = if is_tool_use_id(&session) {
        String::new()
    } else {
        session
    };
    if !session.is_empty() && !crate::session_id::is_safe_session_id(&session) {
        return Err(RiftError::validity("hook: unsafe session id"));
    }
    Ok(RawHook {
        agent: agent_id,
        verb: verb.to_owned(),
        session,
        turn,
        occurred_at_ms,
        payload,
    })
}

/// The current canonical-event schema, for `rift hooks --schema` (SDK codegen /
/// adapter authors). Emits the schema version, the full `EventKind`/`Trust`
/// vocabularies, the canonical-event field order, and the wire limits.
#[must_use]
pub fn schema_json() -> serde_json::Value {
    let kinds: Vec<&str> = EventKind::ALL.iter().map(|k| k.as_str()).collect();
    let trust: Vec<&str> = Trust::ALL.iter().map(|t| t.as_str()).collect();
    serde_json::json!({
        "schema_version": rift_agent::SCHEMA_VERSION,
        "event_kinds": kinds,
        "trust_classes": trust,
        "envelope": {
            "invocation": "rift hooks <agent> <verb>  (JSON on stdin)",
            "stdin_fields": { "session": "string", "turn": "u32", "payload": "object" },
            "native_fallback": "an object with none of the envelope keys but one of session_id_fields is ingested whole as the payload; turn derived at ingest",
            "session_id_fields": SESSION_ID_FIELDS,
            "canonical_event_fields": [
                "schema_version", "session", "turn_ordinal", "event_ordinal",
                "kind", "trust", "occurred_at_ms", "payload_sha256"
            ],
        },
        "limits": { "max_stdin_bytes": MAX_STDIN_BYTES, "max_depth": MAX_DEPTH },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the session-key sweep: the agent token, the native verb,
    /// and the payload exactly as that agent sends it.
    type SpellingCase = (&'static str, &'static str, serde_json::Value);

    #[test]
    fn parse_bounded_rejects_oversize_and_garbage_without_panic() {
        assert!(parse_bounded(&vec![b'x'; MAX_STDIN_BYTES + 1]).is_err());
        assert!(parse_bounded(b"{not json").is_err());
        assert!(parse_bounded(b"{}").is_ok());
    }

    #[test]
    fn parse_bounded_rejects_too_deep() {
        // 70 levels of nested arrays (> MAX_DEPTH 64), still under serde's ~128.
        let deep = format!("{}{}", "[".repeat(70), "]".repeat(70));
        assert!(parse_bounded(deep.as_bytes()).is_err());
    }

    #[test]
    fn build_raw_hook_extracts_envelope_and_is_total() {
        let body = br#"{"session":"01J","turn":3,"payload":{"tool_name":"Bash"}}"#;
        let raw = build_raw_hook("claude-code", "PreToolUse", body, 42).unwrap();
        assert_eq!(raw.agent, AgentId::CLAUDE_CODE);
        assert_eq!(raw.session, "01J");
        assert_eq!(raw.turn, Some(3));
        assert_eq!(raw.occurred_at_ms, 42);
        // a malformed agent token is rejected, not panicked
        assert!(build_raw_hook("../evil", "v", b"{}", 0).is_err());
    }

    #[test]
    fn build_raw_hook_accepts_native_shape_without_a_shim() {
        // Claude Code's native hook stdin — no envelope keys, a
        // session_id string. The whole object is the payload; the turn is left
        // for the ingest layer to derive.
        let body =
            br#"{"session_id":"2227d6bc","hook_event_name":"PreToolUse","tool_name":"Bash"}"#;
        let raw = build_raw_hook("claude-code", "PreToolUse", body, 7).unwrap();
        assert_eq!(raw.session, "2227d6bc");
        assert_eq!(raw.turn, None);
        assert_eq!(raw.payload["tool_name"], "Bash");
        assert_eq!(raw.payload["session_id"], "2227d6bc");

        // Envelope keys win: a payload-bearing envelope never falls back.
        let env = br#"{"session":"S","payload":{"session_id":"native"}}"#;
        let raw = build_raw_hook("claude-code", "Stop", env, 0).unwrap();
        assert_eq!(raw.session, "S");
        // Neither shape matches → empty defaults, still total.
        let neither = build_raw_hook("claude-code", "Stop", b"{\"x\":1}", 0).unwrap();
        assert_eq!(neither.session, "");
        assert_eq!(neither.turn, None);
        assert!(neither.payload.is_null());

        let antigravity = build_raw_hook(
            "antigravity",
            "PreInvocation",
            br#"{"conversationId":"agy-1","invocationNum":0}"#,
            8,
        )
        .unwrap();
        assert_eq!(antigravity.session, "agy-1");
        assert_eq!(antigravity.payload["conversationId"], "agy-1");
    }

    /// The session id is agent-authored text that becomes a filename
    /// downstream; a traversing id would have written the session cache
    /// wherever it pointed, with the developer's own privileges, under a hook
    /// contract that swallows every error in silence.
    #[test]
    fn a_session_id_that_escapes_the_sessions_directory_defers_the_hook() {
        for hostile in [
            br#"{"session":"../evil","payload":{}}"#.to_vec(),
            br#"{"session_id":"../../../../etc/cron.d/evil"}"#.to_vec(),
            br#"{"session":"a/b","payload":{}}"#.to_vec(),
            br#"{"conversationId":".hidden"}"#.to_vec(),
            format!("{{\"session\":\"{}\"}}", "9".repeat(500)).into_bytes(),
        ] {
            let built = build_raw_hook("claude-code", "Stop", &hostile, 0);
            assert!(
                built.is_err(),
                "unsafe session id must defer: {}",
                String::from_utf8_lossy(&hostile)
            );
        }
        // The ids real agents send still pass, and an absent one keeps its
        // existing meaning.
        assert_eq!(
            build_raw_hook(
                "claude-code",
                "Stop",
                br#"{"session_id":"f5927747-7be6-4b5e-b379-579c7193520a"}"#,
                0,
            )
            .unwrap()
            .session,
            "f5927747-7be6-4b5e-b379-579c7193520a"
        );
        assert_eq!(
            build_raw_hook("claude-code", "Stop", b"{\"x\":1}", 0)
                .unwrap()
                .session,
            ""
        );
    }

    /// The session-key sweep. Each case is a real hook payload's own
    /// spelling, taken from that agent's documented schema, so a wire that
    /// stops recognizing one fails here instead of silently capturing nothing.
    ///
    /// The Cursor row is the one this test was written for: Cursor names its
    /// session `conversation_id`, the wire recognized only `session_id` and
    /// `conversationId`, and so **every Cursor hook landed session-less** —
    /// a whole agent's capture lost to one underscore, and lost without even
    /// a receipt until the deferred-no-session receipt landed.
    #[test]
    fn every_verified_agent_session_spelling_binds_its_own_id() {
        let snake_case = fixture_snake_case_session_spellings();
        let other = fixture_other_session_spellings();
        for (agent, verb, payload) in snake_case.iter().chain(&other) {
            let body = serde_json::to_vec(payload).unwrap();
            let raw = build_raw_hook(agent, verb, &body, 0)
                .unwrap_or_else(|e| panic!("{agent}/{verb} must build: {e}"));
            assert!(
                !raw.session.is_empty(),
                "{agent}/{verb}: the wire did not find this agent's session id — \
                 every hook it fires would be captured under no session at all"
            );
            assert!(
                crate::session_id::is_safe_session_id(&raw.session),
                "{agent}/{verb}: the bound id must be usable as a name"
            );
            // The whole native object is the payload, so the adapter still
            // sees every field it needs.
            assert_eq!(
                raw.payload, *payload,
                "{agent}/{verb}: payload ingested whole"
            );
        }
    }

    /// The agents that name their session `session_id`.
    fn fixture_snake_case_session_spellings() -> Vec<SpellingCase> {
        vec![
            (
                "claude-code",
                "Stop",
                serde_json::json!({
                    "session_id": "f5927747-7be6-4b5e-b379-579c7193520a",
                    "transcript_path": "/t.jsonl",
                    "hook_event_name": "Stop"
                }),
            ),
            (
                "codex",
                "UserPromptSubmit",
                // `session_id` + a `turn_id` Rift does not read yet.
                serde_json::json!({
                    "session_id": "0199a1b2-c3d4-7e8f-9012-3456789abcde",
                    "turn_id": "turn-7",
                    "cwd": "/repo",
                    "hook_event_name": "UserPromptSubmit",
                    "model": "gpt-5.4-codex"
                }),
            ),
            (
                "gemini",
                "BeforeModel",
                serde_json::json!({
                    "session_id": "9f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f",
                    "transcript_path": "/t.json",
                    "llm_request": {"model": "gemini-2.5-pro"}
                }),
            ),
            (
                "opencode",
                "session.start",
                serde_json::json!({"session_id": "ses_7Kq2"}),
            ),
            (
                // Copilot under the VS Code host, whose payloads use the
                // snake_case alternate compat.go:74 also accepts; the CLI's
                // own `sessionId` spelling is in the other table.
                "copilot-cli",
                "prompt",
                serde_json::json!({
                    "session_id": "b0ff98c0-8e01-4b73-bf92-9649b139931b",
                    "hookEventName": "userPromptSubmitted",
                    "transcript_path": "/t.jsonl"
                }),
            ),
        ]
    }

    /// The agents that spell their session key some other way.
    fn fixture_other_session_spellings() -> Vec<SpellingCase> {
        vec![
            (
                "cursor",
                "stop",
                serde_json::json!({
                    "conversation_id": "b1e2f3a4-5678-49ab-cdef-0123456789ab",
                    "generation_id": "gen-3",
                    "model": "claude-4.6-sonnet",
                    "hook_event_name": "stop",
                    "input_tokens": 1200,
                    "output_tokens": 340
                }),
            ),
            (
                "antigravity",
                "PreInvocation",
                serde_json::json!({"conversationId": "agy-1", "invocationNum": 0}),
            ),
            (
                // The tag's own captured `agentStop` payload, verbatim from
                // upstream: copilotcli/AGENT.md. The previous cycle
                // called this spelling unverifiable and shipped Copilot
                // session-less; compat.go:74 states it outright.
                "copilot-cli",
                "session_end",
                serde_json::json!({
                    "timestamp": 1_771_480_085_412_i64,
                    "cwd": "/path/to/repo",
                    "sessionId": "b0ff98c0-8e01-4b73-bf92-9649b139931b",
                    "transcriptPath": "/home/u/.copilot/session-state/b0ff98c0/events.jsonl",
                    "stopReason": "end_turn"
                }),
            ),
        ]
    }

    /// The other half of the Copilot pair. Copilot fires its *session*
    /// lifecycle hooks for subagent turns too, reusing the subagent's tool-use
    /// id as the `sessionId`. Bound, each one would open a session that never
    /// receives a matching end — so the moment the spelling above starts
    /// binding, this must already be refusing. The two are one commit for that
    /// reason.
    #[test]
    fn a_subagents_tool_use_id_binds_no_session_on_either_shape() {
        // Native: the payload Copilot really sends for a subagent turn.
        let native = build_raw_hook(
            "copilot-cli",
            "session_start",
            br#"{"sessionId":"toolu_bdrk_01KqZ8mR3fN2","cwd":"/repo","source":"new"}"#,
            0,
        )
        .unwrap();
        assert_eq!(
            native.session, "",
            "a tool-use id must not become a session id"
        );
        // The payload is still carried, so each refused subagent turn hashes to
        // its own receipt rather than every refusal collapsing onto one row.
        assert_eq!(native.payload["sessionId"], "toolu_bdrk_01KqZ8mR3fN2");
        let second = build_raw_hook(
            "copilot-cli",
            "session_start",
            br#"{"sessionId":"toolu_bdrk_99other","cwd":"/repo","source":"new"}"#,
            0,
        )
        .unwrap();
        assert_ne!(
            native.payload, second.payload,
            "two refused subagent turns must stay two countable receipts"
        );

        // Envelope: states its session outright, so the fallback's skip cannot
        // cover it.
        let envelope = build_raw_hook(
            "copilot-cli",
            "session_start",
            br#"{"session":"toolu_x","payload":{}}"#,
            0,
        )
        .unwrap();
        assert_eq!(envelope.session, "");

        // A real Copilot session id is a UUID and is unaffected.
        assert_eq!(
            build_raw_hook(
                "copilot-cli",
                "session_start",
                br#"{"sessionId":"b0ff98c0-8e01-4b73-bf92-9649b139931b"}"#,
                0,
            )
            .unwrap()
            .session,
            "b0ff98c0-8e01-4b73-bf92-9649b139931b"
        );

        // An earlier spelling holding a tool-use id does not veto a later one
        // that holds the real session.
        assert_eq!(
            build_raw_hook(
                "copilot-cli",
                "session_start",
                br#"{"session_id":"toolu_bdrk_01K","sessionId":"b0ff98c0-8e01-4b73-bf92-9649b139931b"}"#,
                0,
            )
            .unwrap()
            .session,
            "b0ff98c0-8e01-4b73-bf92-9649b139931b"
        );

        assert!(is_tool_use_id("toolu_bdrk_01K"));
        assert!(!is_tool_use_id("toolu"));
        assert!(!is_tool_use_id("b0ff98c0-8e01-4b73-bf92-9649b139931b"));
    }

    /// The spelling order is a contract, not an accident: a hook that bound a
    /// different session id on redelivery would defeat the idempotency index,
    /// so a payload carrying two spellings must always resolve the same way.
    #[test]
    fn two_spellings_in_one_payload_resolve_in_the_documented_order() {
        let both = br#"{"conversation_id":"second","session_id":"first"}"#;
        assert_eq!(
            build_raw_hook("cursor", "stop", both, 0).unwrap().session,
            "first"
        );
        // …and the same bytes with the keys written the other way round.
        let swapped = br#"{"session_id":"first","conversation_id":"second"}"#;
        assert_eq!(
            build_raw_hook("cursor", "stop", swapped, 0)
                .unwrap()
                .session,
            "first",
            "key order in the JSON text must not choose the session"
        );
        assert_eq!(SESSION_ID_FIELDS[0], "session_id");
    }

    #[test]
    fn schema_json_lists_all_kinds_and_trust() {
        let s = schema_json();
        assert_eq!(s["schema_version"], rift_agent::SCHEMA_VERSION);
        assert_eq!(
            s["event_kinds"].as_array().unwrap().len(),
            EventKind::ALL.len()
        );
        assert!(s["event_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "tool_call"));
    }

    proptest::proptest! {
        /// Parse totality: for arbitrary bytes, `parse_bounded` returns
        /// Ok/Err and never panics.
        #[test]
        fn parse_bounded_is_total(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..4096)) {
            let _ = parse_bounded(&bytes);
        }

        /// `build_raw_hook` is total over arbitrary agent/verb/stdin.
        #[test]
        fn build_raw_hook_is_total(
            agent in "[a-z0-9-]{0,12}",
            verb in "\\PC{0,16}",
            bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..2048),
        ) {
            let _ = build_raw_hook(&agent, &verb, &bytes, 0);
        }
    }
}
