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
//! freezes from the recall rows (keys sorted, typed text already withheld,
//! the session and window that made them named), sorted by idempotency key.
//! So are the settled usage facts: two calls of the first session on two
//! models, in two turns, which `usage.jsonl` freezes one line each. And so are
//! the sessions' agents, two different ones, which `sessions.jsonl` freezes
//! one line each. And so are the window-journal rows: the first session's own
//! window reads, is declined, edits and is emptied, and one subagent window
//! reads, which `window_journal.jsonl` freezes one line each, revisions and
//! window keys computed rather than spelled. And so are the worktree
//! observations: the first session seen in the main checkout and then, by a
//! subagent's shell `cd`, in a linked member, and the second detached in the
//! main checkout, which `session_worktrees.jsonl` freezes one line each.

use std::path::PathBuf;

use rift_core::{Result, SecretBytes};

use crate::backend::{
    ArchivedEvent, ArchivedReadFact, ArchivedTask, ArchivedToolResult, ArchivedUsageFact,
    ArchivedWindowFact, ArchivedWindowRow, ArchivedWorktreeObservation, CheckpointArchive,
    RedactionReportView, WindowSliceKind,
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

/// One spill of the fixture: sealed through the gate when it has bytes,
/// recorded `spill_missing` when it has none.
fn fixture_spill(
    id: &str,
    raw: Option<&SecretBytes>,
    packs: &rift_shadow_redact::RulePackSet,
) -> Result<crate::backend::ArchivedSpill> {
    use crate::backend::{ArchivedSpill, SpillState};
    let body = raw
        .map(|r| crate::transcript::sealed_spill(r, packs))
        .transpose()?;
    Ok(ArchivedSpill {
        tool_use_id: id.to_owned(),
        state: body
            .as_ref()
            .map_or(SpillState::Missing, |_| SpillState::Sealed),
        body,
        source_bytes: None,
    })
}

/// One original of the fixture: the spill text's record through the gate,
/// in `state`, folded over its first two lines.
fn fixture_original(
    id: &str,
    state: crate::backend::OriginalState,
    stdout: &str,
    packs: &rift_shadow_redact::RulePackSet,
) -> Result<crate::backend::ArchivedOriginal> {
    use crate::backend::rewrite::{
        original_record_json, LineRange, RewriteForm, Rewritten, Stream,
    };
    use crate::backend::{ArchivedOriginal, OriginalState};
    let record = SecretBytes::new(original_record_json(stdout, "", false));
    let present = state == OriginalState::Sealed;
    Ok(ArchivedOriginal {
        tool_use_id: id.to_owned(),
        agent_id: None,
        form: Some(RewriteForm::FoldedOutput),
        replaced: vec![LineRange {
            stream: Stream::Stdout,
            start: 0,
            end: 2,
        }],
        line: present.then_some(4),
        rewritten: present.then_some(Rewritten::Delivered(RewriteForm::FoldedOutput)),
        state,
        body: Some(crate::transcript::sealed_original(&record, packs)?),
    })
}

/// The session transcript pair and the subagent's, through the real
/// compactor and the same gate, each carrying the spills and originals sealed
/// beside it.
fn fixture_transcripts(
    packs: &rift_shadow_redact::RulePackSet,
) -> Result<(
    crate::backend::ArchivedTranscript,
    crate::backend::ArchivedTranscript,
)> {
    let raw = SecretBytes::new(input("raw.jsonl")?);
    let (mut transcript, _raw_lines) = crate::transcript::archived_transcript(&raw, 0, packs)?;
    // A spilled tool output beside each transcript, through the same gate,
    // and one the session named but whose file was gone: every row of the
    // `spills` manifest's vocabulary a reader must parse, sealed or lost.
    let spill = SecretBytes::new(input("spill.txt")?);
    transcript.spills = vec![
        fixture_spill("toolu_01SPILLFIXTURE", Some(&spill), packs)?,
        fixture_spill("toolu_01SPILLGONE", None, packs)?,
    ];

    // The subagent's own pair, archived into its subtree.
    let task_raw = SecretBytes::new(input("task_raw.jsonl")?);
    let (mut task_transcript, _) = crate::transcript::archived_transcript(&task_raw, 0, packs)?;
    task_transcript.spills = vec![fixture_spill("toolu_01TASKSPILL", Some(&spill), packs)?];
    // The original of a rewritten result beside each transcript, and one
    // orphan the session spooled whose line no transcript holds: every row of
    // the `originals` manifest a reader must parse, matched or orphaned.
    let original_text = String::from_utf8_lossy(spill.as_slice()).into_owned();
    let (sealed, orphan) = (
        crate::backend::OriginalState::Sealed,
        crate::backend::OriginalState::LineAbsent,
    );
    transcript.originals = vec![
        fixture_original("toolu_01BASHA", sealed, &original_text, packs)?,
        fixture_original("toolu_01ORPHAN", orphan, &original_text, packs)?,
    ];
    task_transcript.originals = vec![fixture_original(
        "toolu_01TASKORIGINAL",
        sealed,
        &original_text,
        packs,
    )?];

    Ok((transcript, task_transcript))
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

    // The session's pair and its subagent's, each with what it seals beside it.
    let (transcript, task_transcript) = fixture_transcripts(&packs)?;

    let mut archive = CheckpointArchive {
        boundary_gaps: None,
        repo: PathBuf::from("/shard-goldens"),
        commit: "9".repeat(40),
        checkpoint_id: String::new(),
        sessions: FIXTURE_SESSIONS.iter().map(|s| (*s).to_owned()).collect(),
        strategy: "manual-commit".to_owned(),
        prev_hash: None,
        events: fixture_events(),
        read_facts: fixture_read_facts(),
        serve_audit: Some(fixture_serve_audit()),
        usage: fixture_usage(),
        session_agents: fixture_session_agents(),
        window_journal: fixture_window_journal(),
        worktree_observations: fixture_worktree_observations(),
        tool_results: fixture_tool_results(),
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
            provenance: None,
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
/// they are the shape digest's whole input, as `digest.shape.json` too. The
/// first carries every member a rebuild restores the savings denominator
/// from; the second carries none of them, the shape every line written
/// before those members existed has.
fn fixture_read_facts() -> Vec<ArchivedReadFact> {
    vec![
        ArchivedReadFact {
            tool_use_id: "toolu_01READA".to_owned(),
            tool_name: "Read".to_owned(),
            path: "crates/rift-shadow/src/backend.rs".to_owned(),
            content_sha256: "a".repeat(64),
            input_tokens: Some(1_200),
            at: Some(FIXTURE_CREATED_AT_MS - 8_000),
            turn: Some(1),
            blob_oid: Some("e".repeat(40)),
            tokenizer: Some("cl100k_base".to_owned()),
        },
        ArchivedReadFact {
            tool_use_id: "toolu_01GREPA".to_owned(),
            tool_name: "Grep".to_owned(),
            path: "crates/rift-shadow/src/lib.rs".to_owned(),
            content_sha256: "b".repeat(64),
            input_tokens: None,
            at: None,
            turn: None,
            blob_oid: None,
            tokenizer: None,
        },
    ]
}

/// The frozen serve envelopes the fixture archives as `serve_audit.jsonl`: a
/// read decline booked in both relations, its schema-4 call line first because
/// its key sorts first. A call row holds no handle and no reason, so its line's
/// payload names only its kind.
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
    let window = "e".repeat(64);
    let call = serde_json::json!({
        "idempotency_key": "01JSERVEFIXTURE",
        "payload": { "kind": "read_intercepted" },
        "schema": 4,
        "serve_call": {
            "call_id": "01JSERVEFIXTURE",
            "channel": "hook_inject",
            "recorded_at": FIXTURE_CREATED_AT_MS - 7_000,
            "tokens_avoided": 1_200,
            "tokens_returned": 40,
            "tool_name": "claude-code:read-gate",
        },
        "session_id": FIXTURE_SESSIONS[0],
        "verb": "memd.read_intercepted",
        "window_key": window,
    });
    let audit = serde_json::json!({
        "idempotency_key": "d".repeat(64),
        "payload": payload,
        "recorded_at": FIXTURE_CREATED_AT_MS - 7_000,
        "schema": 1,
        "session_id": FIXTURE_SESSIONS[0],
        "verb": "memd.read_intercepted",
        "window_key": window,
    });
    let mut out = Vec::new();
    for line in [call, audit] {
        out.extend_from_slice(&serde_json::to_vec(&line).unwrap_or_default());
        out.push(b'\n');
    }
    out
}

/// The settled usage the fixture archives as `usage.jsonl`: one call per
/// turn of the first session, on two different models, so the lines differ
/// in every member a rebuild keys on.
fn fixture_usage() -> Vec<ArchivedUsageFact> {
    let call = |turn: i64, model: &str, at: i64, call_id: char| ArchivedUsageFact {
        session: FIXTURE_SESSIONS[0].to_owned(),
        turn,
        call_id: call_id.to_string().repeat(64),
        model: model.to_owned(),
        at,
        uncached_input: 1_200 * turn,
        cached_input: 30_000,
        cache_creation: 512,
        output: 90 * turn,
        method: "provider_reported".to_owned(),
    };
    vec![
        call(1, "claude-sonnet-4-5", FIXTURE_CREATED_AT_MS - 7_500, '5'),
        call(2, "claude-opus-4-1", FIXTURE_CREATED_AT_MS - 1_500, '6'),
    ]
}

/// The agent each fixture session was captured from: two different agents,
/// so the lines differ in the member a rebuild keys on.
fn fixture_session_agents() -> std::collections::BTreeMap<String, String> {
    std::collections::BTreeMap::from([
        (FIXTURE_SESSIONS[0].to_owned(), "claude-code".to_owned()),
        (FIXTURE_SESSIONS[1].to_owned(), "codex".to_owned()),
    ])
}

/// The window-journal rows the fixture archives as `window_journal.jsonl`:
/// the first session's own window reads a file, is declined on the re-read,
/// edits it and is emptied by a compaction; a subagent window under the same
/// session reads one file. Every kind with members of its own is present.
fn fixture_window_journal() -> Vec<ArchivedWindowFact> {
    let session = FIXTURE_SESSIONS[0];
    let window =
        |agent: &str| rift_hashchain::leaf_hash(format!("{session}\0{agent}").as_bytes()).to_hex();
    let revision = |n: u8| crate::sha256_hex(&[b"src/lib.rs@", &[n]]);
    let at = |back: u64| FIXTURE_CREATED_AT_MS.unsigned_abs() - back;
    let slice = |kind, range, total_lines| ArchivedWindowRow::Slice {
        kind,
        path: "src/lib.rs".to_owned(),
        hash: revision(1),
        range,
        total_lines,
    };
    let edit = ArchivedWindowRow::Edit {
        path: "src/lib.rs".to_owned(),
        hash_old: Some(revision(1)),
        hash_new: revision(2),
        sites: vec![(10, 12)],
        shift: 2,
        full: true,
    };
    let evict = ArchivedWindowRow::Evict {
        reason: "pre_compact".to_owned(),
    };
    let row = |subagent: Option<&str>, turn, back, row| ArchivedWindowFact {
        window: window(subagent.unwrap_or_default()),
        subagent: subagent.map(str::to_owned),
        turn,
        at: at(back),
        row,
    };
    vec![
        row(
            None,
            1,
            8_000,
            slice(WindowSliceKind::Read, (1, 120), Some(120)),
        ),
        row(None, 2, 6_500, slice(WindowSliceKind::Deny, (1, 120), None)),
        row(None, 2, 6_000, edit),
        row(None, 3, 3_000, evict),
        row(
            Some("explore-1"),
            2,
            5_000,
            slice(WindowSliceKind::Read, (1, 40), Some(120)),
        ),
    ]
}

/// The worktree observations the fixture archives as
/// `session_worktrees.jsonl`: the first session in the main checkout on
/// `main`, then a subagent's shell `cd` into the linked member `lane-a`; the
/// second session detached in the main checkout. Every optional member is
/// present on one line and absent on another.
fn fixture_worktree_observations() -> Vec<ArchivedWorktreeObservation> {
    let main = |session: &str, branch: Option<&str>, back: i64| ArchivedWorktreeObservation {
        session: session.to_owned(),
        token: "0123456789ab".to_owned(),
        name: "main".to_owned(),
        linked: false,
        branch: branch.map(str::to_owned),
        subagent: None,
        basis: "payload_cwd".to_owned(),
        at: FIXTURE_CREATED_AT_MS - back,
    };
    let lane = ArchivedWorktreeObservation {
        token: "abcdef012345".to_owned(),
        name: "lane-a".to_owned(),
        linked: true,
        branch: Some("wt/lane-a".to_owned()),
        subagent: Some("explore-1".to_owned()),
        basis: "shell_cd".to_owned(),
        ..main(FIXTURE_SESSIONS[0], None, 4_000)
    };
    vec![
        main(FIXTURE_SESSIONS[0], Some("main"), 9_000),
        lane,
        main(FIXTURE_SESSIONS[1], None, 7_000),
    ]
}

/// The measured tool results the fixture archives as `tool_results.jsonl`:
/// the first session's glob, measured in process, and a piped shell search
/// nobody could measure, counted with no tokens; the second session's
/// re-executed search. Every channel a search arrives on and every basis.
fn fixture_tool_results() -> Vec<ArchivedToolResult> {
    let result = |session: &str, n: char, tool: &str, basis: &str, tokens: u32, back: i64| {
        ArchivedToolResult {
            id: n.to_string().repeat(64),
            session: session.to_owned(),
            tool: tool.to_owned(),
            channel: "search".to_owned(),
            output_tokens: tokens,
            tokenizer: "cl100k_base".to_owned(),
            basis: basis.to_owned(),
            recorded_at: FIXTURE_CREATED_AT_MS - back,
        }
    };
    vec![
        result(FIXTURE_SESSIONS[0], '7', "Glob", "in_process", 64, 7_800),
        result(FIXTURE_SESSIONS[0], '8', "Bash", "unmeasured", 0, 7_200),
        result(
            FIXTURE_SESSIONS[1],
            '9',
            "exec_command",
            "reexecuted",
            310,
            6_800,
        ),
    ]
}
