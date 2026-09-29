//! **The maximal shard fixture** — one deterministic checkpoint that carries
//! every artifact the pipeline can write, built from committed raw inputs
//! through the real compactor and the real redaction gate.
//!
//! It exists because the two byte-stability claims this crate made were
//! vacuous: `compaction_is_byte_stable` compared `compact_jsonl(x)` with
//! `compact_jsonl(x)` in one process, `digests_are_byte_stable` compared
//! `failures_json(x)` with `failures_json(x)`, and **no fixture in the tree
//! carried a transcript at all**. `f(x) == f(x)` cannot fail, so neither test
//! could: every compactor and every digest writer, however rewritten, satisfies
//! it. What a byte-stability claim needs is bytes frozen in the tree from an
//! earlier build — which is what `shard-goldens/` holds and what this fixture
//! produces the live side of.
//!
//! **Determinism, stated rather than assumed.** Every input is a committed file
//! or a literal here; the clock is [`FIXTURE_CREATED_AT_MS`], the ids are
//! literals, and the checkpoint id is DERIVED from the archive's own root by
//! the writer's own function — so the fixture cannot carry an id its bytes do
//! not address. Nothing reads the wall clock, the environment or the network.
//!
//! The inputs, and what each is there to exercise:
//!
//! * `raw.jsonl` — a session transcript with a user prompt (the intent digest's
//!   locator), a streamed assistant message in two fragments sharing one id
//!   (the merge-group the compact boundary rounds toward), a thinking block
//!   (dropped, so a signature never replays), a tool result over
//!   [`crate::transcript::MAX_INLINE_CHARS`] (elided to a handle), a failing
//!   tool result (the failures digest), a `Task` dispatch (the subagent case), a
//!   line that is not JSON (the `unparsed` counter) and a record shape the
//!   compactor does not model (the `unrecognised` counter).
//! * `task_raw.jsonl` — the dispatched subagent's own transcript, archived into
//!   its `tasks/<tool-use-id>/` subtree.
//! * `capture.txt` — the session capture document that becomes
//!   `content.redacted`.
//!
//! The frozen serve envelopes are literals rather than an input file: one
//! schema-4 line and one schema-1 line, in the canonical shape the one writer
//! freezes (keys sorted, typed text already withheld), sorted by idempotency
//! key.

use std::path::PathBuf;

use rift_core::{Result, SecretBytes};

use crate::backend::{
    ArchivedEvent, ArchivedReadFact, ArchivedTask, CheckpointArchive, RedactionReportView,
};

/// The fixture's capture time — a literal, so no artifact moves with the clock.
pub const FIXTURE_CREATED_AT_MS: i64 = 1_756_000_000_000;

/// The fixture's two session ids (N ≥ 2, so the shard writes `sessions/`).
pub const FIXTURE_SESSIONS: [&str; 2] = ["01JSHARDGOLDENA", "01JSHARDGOLDENB"];

/// The dispatched subagent's tool-use id — the `tasks/<id>/` subtree name.
pub const FIXTURE_TASK_ID: &str = "toolu_01TASKFIXTURE";

/// The committed golden root (`<crate>/shard-goldens`).
#[must_use]
pub fn goldens_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shard-goldens")
}

/// The fixture's directory under [`goldens_root`].
#[must_use]
pub fn fixture_dir() -> PathBuf {
    goldens_root().join("every_artifact")
}

/// Read one committed input file. A missing input is an ERROR, never empty
/// bytes: a fixture that silently reads nothing is a golden that proves
/// nothing, and this module is compiled into the library, so it may not panic.
///
/// # Errors
/// [`rift_core::RiftError::Io`] if the committed input is absent or unreadable.
pub fn input(name: &str) -> Result<Vec<u8>> {
    let path = fixture_dir().join(name);
    std::fs::read(&path).map_err(|e| {
        rift_core::RiftError::validity(format!(
            "committed fixture input {} unreadable: {e}",
            path.display()
        ))
    })
}

/// **The maximal archive**: every artifact the registry declares, produced by
/// the same functions the post-commit condense path calls.
///
/// # Errors
/// [`rift_core::RiftError`] if a committed input is missing, if redaction
/// fails, or if the session set mints no checkpoint id.
pub fn every_artifact_archive() -> Result<CheckpointArchive> {
    let packs = rift_shadow_redact::RulePackSet::builtin();

    // The capture document — through the one redaction gate, like every
    // captured byte.
    let capture = SecretBytes::new(input("capture.txt")?);
    let redacted = rift_shadow_redact::redact_with(&capture, &packs)?;

    // The session transcript pair, through the real compactor + the same gate.
    let raw = SecretBytes::new(input("raw.jsonl")?);
    let (transcript, _raw_lines) = crate::transcript::archived_transcript(&raw, 0, &packs)?;

    // The subagent's own pair, archived into its subtree.
    let task_raw = SecretBytes::new(input("task_raw.jsonl")?);
    let (task_transcript, _) = crate::transcript::archived_transcript(&task_raw, 0, &packs)?;

    let mut archive = CheckpointArchive {
        repo: PathBuf::from("/shard-goldens"),
        commit: "9".repeat(40),
        checkpoint_id: String::new(),
        sessions: FIXTURE_SESSIONS.iter().map(|s| (*s).to_owned()).collect(),
        strategy: "manual-commit".to_owned(),
        prev_hash: None,
        events: fixture_events(),
        read_facts: fixture_read_facts(),
        serve_audit: Some(fixture_serve_audit()),
        content: redacted.redacted,
        report: RedactionReportView::from_redact(&redacted.report),
        transcript: Some(transcript),
        tasks: vec![ArchivedTask {
            tool_use_id: FIXTURE_TASK_ID.to_owned(),
            agent_id: "general-purpose".to_owned(),
            started_at_ms: FIXTURE_CREATED_AT_MS - 6_000,
            ended_at_ms: FIXTURE_CREATED_AT_MS - 1_000,
            input_tokens: Some(800),
            output_tokens: Some(24),
            transcript: Some(task_transcript),
        }],
        // `None`, deliberately, though the fixture HAS a literal clock: a
        // recorded time would add `created_at` to `metadata.json` and move the
        // frozen `metadata.json` + `content_hash.txt` goldens, which this
        // fixture exists to hold still. The recorded-time arm of the format is
        // frozen byte-for-byte by the provenance verb goldens instead
        // (`bins/rift/tests/cli_provenance_goldens_e2e.rs`), over a shard that carries it.
        created_at_ms: None,
        // A hand-built archive is in no repository: the member is ABSENT, which
        // is what keeps every frozen anchor and golden byte-identical.
        worktree: None,
        session_worktrees: std::collections::BTreeMap::new(),
    };
    // The id is DERIVED from the archive's own root by the writer's own
    // function — never chosen — so the fixture addresses its own bytes exactly
    // as a real shard does. No artifact's bytes depend on it, so assigning it
    // last is not a cycle.
    archive.checkpoint_id = archive.derived_checkpoint_id().ok_or_else(|| {
        rift_core::RiftError::validity("the fixture's session set mints no checkpoint id")
    })?;
    Ok(archive)
}

/// The canonical event stream the fixture archives as `full.jsonl`.
fn fixture_events() -> Vec<ArchivedEvent> {
    let session = FIXTURE_SESSIONS[0].to_owned();
    vec![
        ArchivedEvent {
            session: session.clone(),
            turn_ordinal: 0,
            event_ordinal: 0,
            kind: "session_start".to_owned(),
            verb: "SessionStart".to_owned(),
            agent: "claude-code".to_owned(),
            occurred_at_ms: FIXTURE_CREATED_AT_MS - 9_000,
            payload_sha256: "1".repeat(64),
            payload: serde_json::json!({"model": "claude-sonnet-4-5"}),
        },
        ArchivedEvent {
            session: session.clone(),
            turn_ordinal: 1,
            event_ordinal: 0,
            kind: "tool_call".to_owned(),
            verb: "PreToolUse".to_owned(),
            agent: "claude-code".to_owned(),
            occurred_at_ms: FIXTURE_CREATED_AT_MS - 8_000,
            payload_sha256: "2".repeat(64),
            payload: serde_json::json!({"tool_name": "Read"}),
        },
        ArchivedEvent {
            session,
            turn_ordinal: 1,
            event_ordinal: 1,
            kind: "session_end".to_owned(),
            verb: "SessionEnd".to_owned(),
            agent: "claude-code".to_owned(),
            occurred_at_ms: FIXTURE_CREATED_AT_MS - 500,
            payload_sha256: "3".repeat(64),
            payload: serde_json::json!({}),
        },
    ]
}

/// The read facts the fixture archives as `tool_calls.jsonl` — and, because
/// they are the shape digest's whole input, as `digest.shape.json` too.
fn fixture_read_facts() -> Vec<ArchivedReadFact> {
    vec![
        ArchivedReadFact {
            tool_use_id: "toolu_01READA".to_owned(),
            tool_name: "Read".to_owned(),
            path: "crates/rift-shadow/src/backend.rs".to_owned(),
            content_sha256: "a".repeat(64),
            input_tokens: Some(1_200),
        },
        ArchivedReadFact {
            tool_use_id: "toolu_01GREPA".to_owned(),
            tool_name: "Grep".to_owned(),
            path: "crates/rift-shadow/src/lib.rs".to_owned(),
            content_sha256: "b".repeat(64),
            input_tokens: None,
        },
    ]
}

/// The frozen serve envelopes the fixture archives as `serve_audit.jsonl`: a
/// read decline booked in both relations, its schema-4 call line first because
/// its key sorts first.
fn fixture_serve_audit() -> Vec<u8> {
    let handle = format!(
        "file:crates/rift-shadow/src/backend.rs@sha256:{}:L1-40",
        "a".repeat(64)
    );
    let payload = serde_json::json!({
        "estimated_tokens_avoided": 1_200,
        "estimated_tokens_returned": 40,
        "handle": handle,
        "kind": "read_intercepted",
        "reason": "already_held",
    });
    let call = serde_json::json!({
        "idempotency_key": "01JSERVEFIXTURE",
        "payload": payload,
        "schema": 4,
        "serve_call": {
            "call_id": "01JSERVEFIXTURE",
            "channel": "hook_inject",
            "recorded_at": FIXTURE_CREATED_AT_MS - 7_000,
            "tokens_avoided": 1_200,
            "tokens_returned": 40,
            "tool_name": "claude-code:read-gate",
        },
        "verb": "memd.read_intercepted",
    });
    let audit = serde_json::json!({
        "idempotency_key": "d".repeat(64),
        "payload": payload,
        "schema": 1,
        "verb": "memd.read_intercepted",
    });
    let mut out = Vec::new();
    for line in [call, audit] {
        out.extend_from_slice(&serde_json::to_vec(&line).unwrap_or_default());
        out.push(b'\n');
    }
    out
}
