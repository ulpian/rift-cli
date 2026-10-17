//! Causal proofs over the real ingest: what the loader reads and what the
//! classifier refuses, never a mirror of its own rules.
use std::os::unix::fs::PermissionsExt as _;

use rift_shadow::hooks::run_state::{RunRoot, MOVED_MARKER};
use rift_shadow::hooks::{AgentId, RawHook};
use rift_shadow::ids;
use rift_shadow::Boundary;

use super::fixtures::{payload, payload_sha, Declaring, Rig, SeededEvent};
use super::{Member, Reason, Verdict, MAX_CACHE_KEYS, MAX_CURRENT_CANDIDATES};
use crate::agent_queue::BoundaryEdge::{End, Start};
use crate::agent_queue::{BoundaryEdge, BoundarySnapshot};
use crate::capture_recovery::codec::MAX_BYTES;
use crate::capture_recovery::test_support::Fixture;

const CLAUDE: &str = "claude-code";
const CODEX: &str = "codex";
const ANTIGRAVITY: &str = "antigravity";
const PROMPT: &str = "UserPromptSubmit";
const STOP: &str = "Stop";
const SHA: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn job(agent: &str, native: &str, ordinal: u32, edge: BoundaryEdge) -> BoundarySnapshot {
    Rig::snapshot(agent, native, ordinal, edge)
}

fn eligible(verdict: Verdict) -> super::Eligible {
    match verdict {
        Verdict::Eligible(facts) => facts,
        other => panic!("expected eligible, got {other:?}"),
    }
}

/// A healthy Claude session, derived ordinals, with the ingest clock, the
/// hook's instant and the queued job's instant all different.
#[test]
fn a_healthy_claude_prompt_and_stop_classify_eligible_at_the_current_edge_only() {
    let rig = Rig::new();
    let hold = rig.admit();
    let native = "claude-native-1";
    let body = payload("session_id", native);
    rig.ingest(CLAUDE, "SessionStart", &body, 10);
    let out = rig.ingest(CLAUDE, PROMPT, &body, 20);
    assert_eq!(out.boundaries, vec![(1, Boundary::Start)]);

    let start = eligible(rig.verdict(&hold, &job(CLAUDE, native, 1, Start)));
    let session_row = ids::session_row_id(CLAUDE, native);
    assert_eq!(start.verb, PROMPT);
    assert_eq!(start.session_row, session_row);
    assert_eq!(start.turn_id, ids::turn_row_id(&session_row, 1));
    assert_eq!(
        start.snapshot_id,
        ids::turn_boundary_snapshot_id(&session_row, 1, "start")
    );
    assert_eq!(start.payload_sha256, payload_sha(&body));
    assert_eq!(
        start.event_key,
        rift_agent::idempotency_key(
            &AgentId::CLAUDE_CODE,
            native,
            PROMPT,
            1,
            0,
            &rift_core::Sha256Id::parse_hex(&payload_sha(&body)).unwrap()
        )
        .to_hex()
    );
    assert_eq!(
        start.receipt_id,
        rift_shadow::bridge::hook_receipt_id(CLAUDE, PROMPT, native, "1", &payload_sha(&body))
    );
    // The turn is open: its end is not a current boundary yet.
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 1, End)),
        Verdict::Unsupported(Reason::CacheEdge)
    );

    rig.ingest(CLAUDE, STOP, &body, 30);
    let end = eligible(rig.verdict(&hold, &job(CLAUDE, native, 1, End)));
    assert_eq!(end.verb, STOP);
    assert_eq!(
        end.snapshot_id,
        ids::turn_boundary_snapshot_id(&session_row, 1, "end")
    );
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 1, Start)),
        Verdict::Unsupported(Reason::CacheEdge)
    );
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 2, End)),
        Verdict::Unsupported(Reason::CacheNotCurrent)
    );
}

/// Codex with explicit envelope turns: the stamp is the envelope's ordinal and
/// the receipt and event agree on it.
#[test]
fn a_healthy_codex_session_with_explicit_turns_classifies_at_the_stamped_ordinal() {
    let rig = Rig::new();
    let hold = rig.admit();
    let native = "codex-native-7";
    let body = payload("session_id", native);
    let raw = |verb: &str, turn: u32, at: i64| RawHook {
        agent: AgentId::CODEX,
        verb: verb.into(),
        session: native.into(),
        turn: Some(turn),
        occurred_at_ms: at,
        payload: body.clone(),
    };
    rig.ingest_raw(&raw("SessionStart", 0, 100), CODEX, 150);
    rig.ingest_raw(&raw(PROMPT, 3, 200), CODEX, 260);
    let start = eligible(rig.verdict(&hold, &job(CODEX, native, 3, Start)));
    assert_eq!(start.verb, PROMPT);
    assert_eq!(
        start.receipt_id,
        rift_shadow::bridge::hook_receipt_id(CODEX, PROMPT, native, "3", &payload_sha(&body))
    );
    assert_eq!(
        rig.verdict(&hold, &job(CODEX, native, 1, Start)),
        Verdict::Unsupported(Reason::CacheNotCurrent)
    );
    rig.ingest_raw(&raw(STOP, 3, 300), CODEX, 370);
    let end = eligible(rig.verdict(&hold, &job(CODEX, native, 3, End)));
    assert_eq!(end.verb, STOP);
    assert_eq!(end.payload_sha256, payload_sha(&body));
}

/// Antigravity has no Start: its first hook synthesizes a durable session
/// start (kind `session_start`, ordinal `u32::MAX`) and its Stop still ends
/// the turn with a plain event-ordinal-zero `turn_end`.
#[test]
fn an_antigravity_stop_is_eligible_and_its_synthesized_start_never_is() {
    let rig = Rig::new();
    let hold = rig.admit();
    let native = "conversation-9";
    let body = serde_json::json!({
        "conversationId": native,
        "toolCall": {"name": "view_file", "args": {"AbsolutePath": "/repo/a.rs"}},
    });
    rig.ingest(ANTIGRAVITY, "PreToolUse", &body, 10);
    let stop = payload("conversationId", native);
    let out = rig.ingest(ANTIGRAVITY, STOP, &stop, 20);
    assert_eq!(out.boundaries, vec![(1, Boundary::End)]);
    let rows = rig
        .rt
        .block_on(rift_store::CanonicalStore::query(
            &rig.store,
            "SELECT kind, event_ordinal FROM captured_event WHERE session_id = ?1 ORDER BY kind",
            &[rift_store::Value::text(native)],
        ))
        .unwrap();
    assert!(rows.iter().any(|row| {
        row.get_text(0).unwrap() == "session_start"
            && row.get_i64(1).unwrap() == i64::from(u32::MAX)
    }));

    let end = eligible(rig.verdict(&hold, &job(ANTIGRAVITY, native, 1, End)));
    assert_eq!(end.verb, STOP);
    assert_eq!(end.payload_sha256, payload_sha(&stop));
    rig.store.reset();
    assert_eq!(
        rig.verdict(&hold, &job(ANTIGRAVITY, native, 1, Start)),
        Verdict::Unsupported(Reason::ContractUnsupported)
    );
    assert_eq!(rig.store.calls(), 0, "no contract, no query");
}

/// Cursor-style forwarding: the owner's receipt exists for `Stop`, but the sole
/// native `turn_end` is keyed to the agent that fired it, so the owner's
/// boundary cannot be certified — and an unknown competing mapping changes
/// nothing, because the classifier never enumerates agents.
#[test]
fn a_sole_native_event_keyed_to_another_agent_refuses_even_with_the_owners_receipt() {
    let rig = Rig::new();
    let hold = rig.admit();
    let native = "shared-native";
    let body = payload("session_id", native);
    rig.ingest(CLAUDE, "SessionStart", &body, 10);
    rig.ingest(CLAUDE, PROMPT, &body, 20);
    let forwarded = rig.ingest(CODEX, STOP, &body, 30);
    assert_eq!(
        forwarded.owner_agent, CLAUDE,
        "first writer owns the session"
    );
    let receipts = rig
        .rt
        .block_on(rift_store::CanonicalStore::query(
            &rig.store,
            "SELECT agent FROM agent_hook_event WHERE verb = 'Stop' AND session_id = ?1",
            &[rift_store::Value::text(native)],
        ))
        .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        receipts[0].get_text(0).unwrap(),
        CLAUDE,
        "the receipt is the owner's"
    );

    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 1, End)),
        Verdict::Unsupported(Reason::EventKeyMismatch)
    );
    rig.rt
        .block_on(rift_store::record_capture_session_identity(
            &rig.store,
            &rift_store::CaptureSessionIdentityRow {
                session_id: ids::session_row_id("weird-agent", native),
                agent: "weird-agent".into(),
                native_session_id: native.into(),
                schema_version: 1,
            },
        ))
        .unwrap();
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 1, End)),
        Verdict::Unsupported(Reason::EventKeyMismatch)
    );
    // The firing agent never owned the session: the cache names the owner.
    assert_eq!(
        rig.verdict(&hold, &job(CODEX, native, 1, End)),
        Verdict::Unsupported(Reason::CacheAgent)
    );
}

/// A seeded End candidate: identity/session/turn rows and a closed active cache
/// at ordinal 1, with the event and receipt left to the case.
fn seeded_end(rig: &Rig, native: &str) {
    rig.seed_turn(CLAUDE, native, 1, None);
    rig.write_cache(&Rig::active_cache(CLAUDE, native, 1, false));
}

fn end_event<'a>(
    native: &'a str,
    verb: &'a str,
    kind: &'a str,
    trust: &'a str,
    event_ordinal: u32,
) -> SeededEvent<'a> {
    SeededEvent {
        firing: CLAUDE,
        native,
        verb,
        ordinal: 1,
        event_ordinal,
        kind,
        trust,
        sha: SHA,
    }
}

const TURN_END: &str = "turn_end";
const GIT_FACT: &str = "git_fact";

#[test]
fn a_missing_or_unrecorded_receipt_and_a_foreign_owner_stay_owed() {
    let rig = Rig::new();
    let hold = rig.admit();
    for (native, outcome, owner, expected) in [
        (
            "n-no-receipt",
            None,
            CLAUDE,
            Verdict::Unavailable(Reason::ReceiptMissing),
        ),
        (
            "n-skipped",
            Some("skipped-unknown-verb"),
            CLAUDE,
            Verdict::Unsupported(Reason::ReceiptOutcome),
        ),
        (
            "n-no-event",
            Some("recorded-no-event"),
            CLAUDE,
            Verdict::Unsupported(Reason::ReceiptOutcome),
        ),
        (
            "n-foreign-owner",
            Some("recorded"),
            CODEX,
            Verdict::Unavailable(Reason::ReceiptMissing),
        ),
        (
            "n-recorded",
            Some("recorded"),
            CLAUDE,
            Verdict::Eligible(super::Eligible {
                session_row: ids::session_row_id(CLAUDE, "n-recorded"),
                turn_id: ids::turn_row_id(&ids::session_row_id(CLAUDE, "n-recorded"), 1),
                snapshot_id: ids::turn_boundary_snapshot_id(
                    &ids::session_row_id(CLAUDE, "n-recorded"),
                    1,
                    "end",
                ),
                event_key: rift_agent::idempotency_key(
                    &AgentId::CLAUDE_CODE,
                    "n-recorded",
                    STOP,
                    1,
                    0,
                    &rift_core::Sha256Id::parse_hex(SHA).unwrap(),
                )
                .to_hex(),
                payload_sha256: SHA.into(),
                receipt_id: rift_shadow::bridge::hook_receipt_id(
                    CLAUDE,
                    STOP,
                    "n-recorded",
                    "1",
                    SHA,
                ),
                verb: STOP,
            }),
        ),
    ] {
        seeded_end(&rig, native);
        rig.seed_event(&end_event(native, STOP, TURN_END, GIT_FACT, 0));
        if let Some(outcome) = outcome {
            rig.seed_receipt(owner, STOP, native, 1, SHA, outcome);
        }
        assert_eq!(
            rig.verdict(&hold, &job(CLAUDE, native, 1, End)),
            expected,
            "{native}"
        );
    }
}

/// One seeded End witness beside a recorded receipt: the verb its key was
/// derived from, its trust and event ordinal, the payload it hashes, and the
/// receipt's verb and payload.
struct WrongKind {
    native: &'static str,
    key_verb: &'static str,
    trust: &'static str,
    event_ordinal: u32,
    sha: &'static str,
    receipt_verb: &'static str,
    receipt_sha: &'static str,
    expected: Verdict,
}

const OTHER_SHA: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn wrong_kind(native: &'static str, key_verb: &'static str, expected: Verdict) -> WrongKind {
    WrongKind {
        native,
        key_verb,
        trust: GIT_FACT,
        event_ordinal: 0,
        sha: SHA,
        receipt_verb: key_verb,
        receipt_sha: SHA,
        expected,
    }
}

/// The idempotency key omits kind, trust and time, so a row whose stored kind
/// disagrees with the verb its key was derived from, or whose trust or event
/// ordinal breaks the closed contract, must refuse even beside a recorded
/// receipt — and a relabelled `SessionEnd`/`SubagentStop`/`PreToolUse` never
/// certifies an End, because only the one kind-specific verb is probed.
#[test]
fn wrong_kind_trust_ordinal_or_verb_rows_refuse_beside_recorded_receipts() {
    let rig = Rig::new();
    let hold = rig.admit();
    let owed = Verdict::Unavailable(Reason::ReceiptMissing);
    let cases = [
        WrongKind {
            receipt_verb: STOP,
            ..wrong_kind(
                "n-prompt-key",
                PROMPT,
                Verdict::Unsupported(Reason::EventKeyMismatch),
            )
        },
        WrongKind {
            trust: "transcript_untrusted",
            ..wrong_kind("n-trust", STOP, Verdict::Unsupported(Reason::EventContract))
        },
        WrongKind {
            event_ordinal: 1,
            ..wrong_kind(
                "n-ordinal",
                STOP,
                Verdict::Unsupported(Reason::EventContract),
            )
        },
        wrong_kind("n-session-end", "SessionEnd", owed.clone()),
        wrong_kind("n-subagent", "SubagentStop", owed.clone()),
        wrong_kind("n-pretool", "PreToolUse", owed.clone()),
        WrongKind {
            sha: OTHER_SHA,
            ..wrong_kind("n-other-sha", STOP, owed)
        },
    ];
    for case in cases {
        seeded_end(&rig, case.native);
        let mut event = end_event(
            case.native,
            case.key_verb,
            TURN_END,
            case.trust,
            case.event_ordinal,
        );
        event.sha = case.sha;
        rig.seed_event(&event);
        rig.seed_receipt(
            CLAUDE,
            case.receipt_verb,
            case.native,
            1,
            case.receipt_sha,
            "recorded",
        );
        assert_eq!(
            rig.verdict(&hold, &job(CLAUDE, case.native, 1, End)),
            case.expected,
            "{}",
            case.native
        );
    }
}

/// A Stop-keyed row relabelled `prompt_submit` beside its recorded Stop
/// receipt: neither edge can use it.
#[test]
fn a_relabelled_stop_row_certifies_neither_edge() {
    let rig = Rig::new();
    let hold = rig.admit();
    let native = "n-relabelled";
    rig.seed_turn(CLAUDE, native, 1, None);
    rig.seed_event(&end_event(
        native,
        STOP,
        "prompt_submit",
        "transcript_untrusted",
        0,
    ));
    rig.seed_receipt(CLAUDE, STOP, native, 1, SHA, "recorded");
    rig.write_cache(&Rig::active_cache(CLAUDE, native, 1, true));
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 1, Start)),
        Verdict::Unavailable(Reason::ReceiptMissing)
    );
    rig.write_cache(&Rig::active_cache(CLAUDE, native, 1, false));
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 1, End)),
        Verdict::Unavailable(Reason::EventMissing)
    );
}

#[test]
fn a_start_witness_is_exactly_one_prompt_submit_of_transcript_trust() {
    let rig = Rig::new();
    let hold = rig.admit();
    let prompt = |native: &'static str, kind: &'static str, trust: &'static str| SeededEvent {
        firing: CLAUDE,
        native,
        verb: PROMPT,
        ordinal: 1,
        event_ordinal: 0,
        kind,
        trust,
        sha: SHA,
    };
    for (native, events, expected) in [
        (
            "s-turn-start",
            vec![prompt("s-turn-start", "turn_start", "git_fact")],
            Verdict::Unsupported(Reason::EventContract),
        ),
        (
            "s-git-trust",
            vec![prompt("s-git-trust", "prompt_submit", "git_fact")],
            Verdict::Unsupported(Reason::EventContract),
        ),
        (
            "s-both",
            vec![
                prompt("s-both", "prompt_submit", "transcript_untrusted"),
                prompt("s-both", "turn_start", "git_fact"),
            ],
            Verdict::Unsupported(Reason::EventAmbiguous),
        ),
        ("s-none", vec![], Verdict::Unavailable(Reason::EventMissing)),
    ] {
        rig.seed_turn(CLAUDE, native, 1, None);
        rig.write_cache(&Rig::active_cache(CLAUDE, native, 1, true));
        for mut event in events {
            // Two rows at one slot need two keys: the second is keyed as a later event.
            if event.kind == "turn_start" && native == "s-both" {
                event.event_ordinal = 1;
            }
            rig.seed_event(&event);
        }
        rig.seed_receipt(CLAUDE, PROMPT, native, 1, SHA, "recorded");
        assert_eq!(
            rig.verdict(&hold, &job(CLAUDE, native, 1, Start)),
            expected,
            "{native}"
        );
    }
    let native = "s-healthy";
    rig.seed_turn(CLAUDE, native, 1, None);
    rig.write_cache(&Rig::active_cache(CLAUDE, native, 1, true));
    rig.seed_event(&prompt(native, "prompt_submit", "transcript_untrusted"));
    rig.seed_receipt(CLAUDE, PROMPT, native, 1, SHA, "recorded");
    assert_eq!(
        eligible(rig.verdict(&hold, &job(CLAUDE, native, 1, Start))).verb,
        PROMPT
    );
    // Two prompt_submit rows: ambiguity even though one of them matches.
    let three = "3".repeat(64);
    let mut second = prompt(native, "prompt_submit", "transcript_untrusted");
    second.sha = &three;
    rig.seed_event(&second);
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 1, Start)),
        Verdict::Unsupported(Reason::EventAmbiguous)
    );
}

/// The three primary-key cross-checks and the one bounded-cell rule: a cell a
/// guard refuses is an invalid row the classifier reports, never a row that
/// vanished.
#[test]
fn repo_session_turn_and_identity_disagreements_refuse_and_oversized_cells_are_bounded() {
    let rig = Rig::new();
    let hold = rig.admit();
    let end = |native: &str| job(CLAUDE, native, 1, End);
    let seed_event_and_receipt = |native: &str| {
        rig.seed_event(&end_event(native, STOP, TURN_END, GIT_FACT, 0));
        rig.seed_receipt(CLAUDE, STOP, native, 1, SHA, "recorded");
    };

    let native = "r-other-repo";
    rig.seed_turn(CLAUDE, native, 1, Some("other-repo"));
    rig.write_cache(&Rig::active_cache(CLAUDE, native, 1, false));
    seed_event_and_receipt(native);
    assert_eq!(
        rig.verdict(&hold, &end(native)),
        Verdict::Unsupported(Reason::SessionMismatch)
    );

    let native = "r-missing";
    rig.write_cache(&Rig::active_cache(CLAUDE, native, 1, false));
    seed_event_and_receipt(native);
    assert_eq!(
        rig.verdict(&hold, &end(native)),
        Verdict::Unavailable(Reason::IdentityMissing)
    );

    let native = "r-wrong-turn";
    rig.seed_turn(CLAUDE, native, 2, None);
    rig.write_cache(&Rig::active_cache(CLAUDE, native, 1, false));
    seed_event_and_receipt(native);
    assert_eq!(
        rig.verdict(&hold, &end(native)),
        Verdict::Unavailable(Reason::TurnMissing)
    );
}

/// A turn row at the derived id that disagrees about its ordinal, and an
/// identity row whose agent cell is past every bound: a bounded finding each,
/// never a hidden row.
#[test]
fn a_disagreeing_turn_row_and_an_oversized_identity_cell_are_bounded_findings() {
    let rig = Rig::new();
    let hold = rig.admit();
    let end = |native: &str| job(CLAUDE, native, 1, End);
    let seed_event_and_receipt = |native: &str| {
        rig.seed_event(&end_event(native, STOP, TURN_END, GIT_FACT, 0));
        rig.seed_receipt(CLAUDE, STOP, native, 1, SHA, "recorded");
    };

    let native = "r-turn-disagrees";
    rig.seed_turn(CLAUDE, native, 2, None);
    let session_row = ids::session_row_id(CLAUDE, native);
    rig.rt
        .block_on(rift_store::record_turn(
            &rig.store,
            &rift_store::TurnRow {
                turn_id: ids::turn_row_id(&session_row, 1),
                session_id: session_row,
                // A third ordinal: the turn relation is UNIQUE(session, ordinal).
                ordinal: 3,
                started_at: 1,
                schema_version: 1,
            },
        ))
        .unwrap();
    rig.write_cache(&Rig::active_cache(CLAUDE, native, 1, false));
    seed_event_and_receipt(native);
    assert_eq!(
        rig.verdict(&hold, &end(native)),
        Verdict::Unsupported(Reason::TurnMismatch)
    );

    let native = "r-oversized";
    let session_row = ids::session_row_id(CLAUDE, native);
    rig.rt
        .block_on(rift_store::record_capture_session_identity(
            &rig.store,
            &rift_store::CaptureSessionIdentityRow {
                session_id: session_row,
                agent: "x".repeat(100_000),
                native_session_id: native.into(),
                schema_version: 1,
            },
        ))
        .unwrap();
    rig.write_cache(&Rig::active_cache(CLAUDE, native, 1, false));
    assert_eq!(
        rig.verdict(&hold, &end(native)),
        Verdict::Unavailable(Reason::CellInvalid)
    );
    let pass = rig.pass(&hold, &[end(native)]).unwrap();
    assert!(
        pass.cells < 1024,
        "the guard bounded the cell: {}",
        pass.cells
    );
}

#[test]
fn snapshot_history_blocks_fresh_admission_but_an_earlier_start_does_not_block_an_end() {
    let rig = Rig::new();
    let hold = rig.admit();
    let native = "h-native";
    let body = payload("session_id", native);
    rig.ingest(CLAUDE, "SessionStart", &body, 10);
    rig.ingest(CLAUDE, PROMPT, &body, 20);
    rig.ingest(CLAUDE, STOP, &body, 30);
    let session_row = ids::session_row_id(CLAUDE, native);
    let snapshot =
        |ordinal: u32, boundary: &str, id: Option<String>| rift_store::TurnBoundarySnapshotRow {
            snapshot_id: id
                .unwrap_or_else(|| ids::turn_boundary_snapshot_id(&session_row, ordinal, boundary)),
            session_id: session_row.clone(),
            turn_ordinal: i64::from(ordinal),
            boundary: boundary.into(),
            actor: "agent".into(),
            worktree_hash: "11".repeat(32),
            captured_at: 1000,
            schema_version: 1,
        };
    let record = |row: &rift_store::TurnBoundarySnapshotRow| {
        assert!(rig
            .rt
            .block_on(rift_store::record_turn_boundary_snapshot(&rig.store, row))
            .unwrap());
    };
    eligible(rig.verdict(&hold, &job(CLAUDE, native, 1, End)));
    record(&snapshot(1, "start", None));
    eligible(rig.verdict(&hold, &job(CLAUDE, native, 1, End)));
    // A same-ordinal End blocks a Start; the Start's own presence blocks it too.
    rig.write_cache(&Rig::active_cache(CLAUDE, native, 1, true));
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 1, Start)),
        Verdict::Unsupported(Reason::TargetPresent)
    );
    rig.write_cache(&Rig::active_cache(CLAUDE, native, 1, false));
    record(&snapshot(2, "start", None));
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 1, End)),
        Verdict::Unsupported(Reason::LaterBoundary)
    );
    record(&snapshot(1, "end", None));
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 1, End)),
        Verdict::Unsupported(Reason::TargetPresent)
    );
}

#[test]
fn a_same_ordinal_end_malformed_edge_or_orphan_footprint_blocks_a_start() {
    let rig = Rig::new();
    let hold = rig.admit();
    let session_row = |native: &str| ids::session_row_id(CLAUDE, native);
    let open_start = |native: &str| open_start(&rig, native);
    let snapshot = |native: &str, boundary: &str, id: String| rift_store::TurnBoundarySnapshotRow {
        snapshot_id: id,
        session_id: session_row(native),
        turn_ordinal: 1,
        boundary: boundary.into(),
        actor: "agent".into(),
        worktree_hash: "11".repeat(32),
        captured_at: 1000,
        schema_version: 1,
    };
    let native = "m-end-blocks";
    open_start(native);
    eligible(rig.verdict(&hold, &job(CLAUDE, native, 1, Start)));
    let end_id = ids::turn_boundary_snapshot_id(&session_row(native), 1, "end");
    rig.rt
        .block_on(rift_store::record_turn_boundary_snapshot(
            &rig.store,
            &snapshot(native, "end", end_id),
        ))
        .unwrap();
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 1, Start)),
        Verdict::Unsupported(Reason::EndBlocksStart)
    );

    let native = "m-bogus-edge";
    open_start(native);
    rig.rt
        .block_on(rift_store::record_turn_boundary_snapshot(
            &rig.store,
            &snapshot(native, "bogus", "ab".repeat(32)),
        ))
        .unwrap();
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 1, Start)),
        Verdict::Unsupported(Reason::SnapshotMalformed)
    );

    let native = "m-foreign-id";
    open_start(native);
    rig.rt
        .block_on(rift_store::record_turn_boundary_snapshot(
            &rig.store,
            &snapshot(native, "end", "cd".repeat(32)),
        ))
        .unwrap();
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, native, 1, Start)),
        Verdict::Unsupported(Reason::SnapshotMalformed)
    );
}

/// A footprint row at the target id with no snapshot behind it is history
/// nothing here repairs: it blocks.
#[test]
fn an_orphan_footprint_at_the_target_blocks_a_start() {
    let rig = Rig::new();
    let hold = rig.admit();
    let session_row = |native: &str| ids::session_row_id(CLAUDE, native);
    for (native, table) in [
        ("m-orphan-observation", "turn_boundary_observation"),
        ("m-orphan-file", "turn_boundary_file"),
        ("m-orphan-kind", "turn_boundary_file_kind"),
    ] {
        open_start(&rig, native);
        let id = ids::turn_boundary_snapshot_id(&session_row(native), 1, "start");
        orphan_footprint(&rig, table, &id);
        assert_eq!(
            rig.verdict(&hold, &job(CLAUDE, native, 1, Start)),
            Verdict::Unsupported(Reason::OrphanFootprint),
            "{table}"
        );
    }
}

/// A seeded open Start at ordinal 1: turn rows, an open active cache, the
/// prompt witness and its recorded receipt.
fn open_start(rig: &Rig, native: &str) {
    rig.seed_turn(CLAUDE, native, 1, None);
    rig.write_cache(&Rig::active_cache(CLAUDE, native, 1, true));
    rig.seed_event(&SeededEvent {
        firing: CLAUDE,
        native,
        verb: PROMPT,
        ordinal: 1,
        event_ordinal: 0,
        kind: "prompt_submit",
        trust: "transcript_untrusted",
        sha: SHA,
    });
    rig.seed_receipt(CLAUDE, PROMPT, native, 1, SHA, "recorded");
}

/// One footprint row at `snapshot_id` with no snapshot row behind it.
fn orphan_footprint(rig: &Rig, table: &str, snapshot_id: &str) {
    use rift_store::Value::{Int, Text};
    let (columns, values): (&[&str], Vec<rift_store::Value>) = match table {
        "turn_boundary_observation" => (
            &[
                "snapshot_id",
                "worktree_hash",
                "collection_started_at",
                "collection_finished_at",
                "schema_version",
            ],
            vec![
                Text(snapshot_id.into()),
                Text("11".repeat(32)),
                Int(1),
                Int(2),
                Int(1),
            ],
        ),
        "turn_boundary_file" => (
            &[
                "snapshot_id",
                "path",
                "blob_oid",
                "content_sha256",
                "schema_version",
            ],
            vec![
                Text(snapshot_id.into()),
                Text("a.rs".into()),
                Text("ab".repeat(20)),
                Text("cd".repeat(32)),
                Int(1),
            ],
        ),
        _ => (
            &["snapshot_id", "path", "kind", "schema_version"],
            vec![
                Text(snapshot_id.into()),
                Text("a.rs".into()),
                Text("regular".into()),
                Int(1),
            ],
        ),
    };
    rig.rt
        .block_on(rift_store::CanonicalStore::insert(
            &rig.store, table, columns, &values,
        ))
        .unwrap();
}

type Mutation = Box<dyn Fn(&mut rift_shadow::hooks::session_cache::SessionCache)>;

/// Every disagreement a closed-at-ordinal-one End candidate's cache can hold,
/// one mutation each, and the verdict it earns.
fn cache_cases(other_tree: std::path::PathBuf) -> Vec<(&'static str, Mutation, Verdict)> {
    vec![
        (
            "c-ending",
            Box::new(|c| c.state = "ending".into()),
            Verdict::Unsupported(Reason::CachePhase),
        ),
        (
            "c-compacting",
            Box::new(|c| c.state = "compacting".into()),
            Verdict::Unsupported(Reason::CachePhase),
        ),
        (
            "c-agent",
            Box::new(|c| c.agent = CODEX.into()),
            Verdict::Unsupported(Reason::CacheAgent),
        ),
        (
            "c-cursor",
            Box::new(|c| c.agent = "cursor".into()),
            Verdict::Unsupported(Reason::CacheAgent),
        ),
        (
            "c-no-tree",
            Box::new(|c| c.work_tree = Some(String::new())),
            Verdict::Unsupported(Reason::CacheWorkTree),
        ),
        (
            "c-other-tree",
            Box::new(move |c| c.work_tree = Some(other_tree.to_string_lossy().into_owned())),
            Verdict::Unsupported(Reason::CacheWorkTree),
        ),
        (
            "c-schema",
            Box::new(|c| c.schema = 2),
            Verdict::Unavailable(Reason::CacheSchema),
        ),
        (
            "c-session",
            Box::new(|c| c.session_id = "someone-else".into()),
            Verdict::Unsupported(Reason::CacheSession),
        ),
        (
            "c-zero",
            Box::new(|c| c.current_turn_ordinal = 0),
            Verdict::Unsupported(Reason::CacheNotCurrent),
        ),
        (
            "c-ahead",
            Box::new(|c| c.current_turn_ordinal = 2),
            Verdict::Unsupported(Reason::CacheNotCurrent),
        ),
        (
            "c-open",
            Box::new(|c| c.turn_open = true),
            Verdict::Unsupported(Reason::CacheEdge),
        ),
    ]
}

#[test]
fn cache_semantics_refuse_every_disagreement_and_absence_is_unknown() {
    let rig = Rig::new();
    let hold = rig.admit();
    let cases = cache_cases(rig.fixture.work_tree.join("elsewhere"));
    for (native, mutate, expected) in cases {
        let mut cache = Rig::active_cache(CLAUDE, native, 1, false);
        mutate(&mut cache);
        let path = rig.sessions_dir.join(format!("{native}.json"));
        if cache.session_id == native {
            rig.write_cache(&cache);
        } else {
            // `write_cache` names the file after the cache's own id; the job
            // names the file by the native id it carries.
            std::fs::create_dir_all(&rig.sessions_dir).unwrap();
            std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
        }
        let job = job(CLAUDE, native, u32::from(native != "c-zero"), End);
        assert_eq!(rig.verdict(&hold, &job), expected, "{native}");
    }
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, "never-seen", 1, End)),
        Verdict::Unavailable(Reason::CacheAbsent)
    );
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, "../escape", 1, End)),
        Verdict::Unavailable(Reason::NativeUnsafe)
    );
    assert_eq!(
        rig.verdict(&hold, &job("cursor", "c-cursor", 1, End)),
        Verdict::Unsupported(Reason::ContractUnsupported)
    );
}

/// A cache of exactly the cap reads; one byte more refuses. Nothing that is not
/// a plain regular current file is read, and a valid cache in the old tree
/// never stands in for a missing or malformed current one.
#[test]
fn cache_reads_obey_the_cap_plus_one_and_never_fall_back_to_the_legacy_tree() {
    let rig = Rig::new();
    let hold = rig.admit();
    std::fs::create_dir_all(&rig.sessions_dir).unwrap();
    let padded = |native: &str, len: usize| {
        let mut cache = Rig::active_cache(CLAUDE, native, 1, false);
        cache.work_tree = Some(rig.fixture.work_tree.to_string_lossy().into_owned());
        cache.transcript_path = Some(String::new());
        let base = serde_json::to_vec(&cache).unwrap().len();
        cache.transcript_path = Some("t".repeat(len - base));
        let bytes = serde_json::to_vec(&cache).unwrap();
        assert_eq!(bytes.len(), len);
        bytes
    };
    let path = |native: &str| rig.sessions_dir.join(format!("{native}.json"));
    std::fs::write(path("k-exact"), padded("k-exact", MAX_BYTES)).unwrap();
    std::fs::write(path("k-over"), padded("k-over", MAX_BYTES + 1)).unwrap();
    std::fs::write(path("k-torn"), &padded("k-torn", 4096)[..2000]).unwrap();
    std::fs::write(path("k-dup"), br#"{"schema":1,"schema":1}"#).unwrap();
    std::fs::create_dir(path("k-dir")).unwrap();
    std::os::unix::fs::symlink(path("k-exact"), path("k-link")).unwrap();
    let legacy = RunRoot::Sessions.legacy_under(rig.fixture.owner.git_dir());
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("k-legacy.json"), padded("k-legacy", 4096)).unwrap();
    std::fs::write(legacy.join("k-torn.json"), padded("k-torn", 4096)).unwrap();

    // The exact-cap cache is read whole: the classifier gets past it to the rows.
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, "k-exact", 1, End)),
        Verdict::Unavailable(Reason::IdentityMissing)
    );
    for (native, expected) in [
        ("k-over", Reason::CacheOversize),
        ("k-torn", Reason::CacheMalformed),
        ("k-dup", Reason::CacheMalformed),
        ("k-dir", Reason::CacheNotRegular),
        ("k-link", Reason::CacheNotRegular),
        ("k-legacy", Reason::CacheAbsent),
    ] {
        assert_eq!(
            rig.verdict(&hold, &job(CLAUDE, native, 1, End)),
            Verdict::Unavailable(expected),
            "{native}"
        );
    }
    let pass = rig
        .pass(
            &hold,
            &[
                job(CLAUDE, "k-exact", 1, End),
                job(CLAUDE, "k-over", 1, End),
                job(CLAUDE, "k-torn", 1, End),
            ],
        )
        .unwrap();
    assert_eq!(pass.cache_bytes, (MAX_BYTES + MAX_BYTES + 1 + 2000) as u64);
}

#[test]
fn too_many_cache_keys_or_current_candidates_fail_before_any_sql() {
    let rig = Rig::new();
    let hold = rig.admit();
    rig.write_cache(&Rig::active_cache(CLAUDE, "warm", 1, false));
    let jobs: Vec<_> = (0..=MAX_CACHE_KEYS)
        .map(|n| job(CLAUDE, &format!("k-{n}"), 1, End))
        .collect();
    assert_eq!(
        rig.pass(&hold, &jobs).unwrap_err(),
        Reason::CacheKeysOverflow
    );
    assert_eq!(rig.store.calls(), 0);
    // One native, many ordinals: 257 candidates but one cache key is fine.
    let jobs: Vec<_> = (1..=(MAX_CACHE_KEYS as u32 + 1))
        .map(|n| job(CLAUDE, "one-native", n, End))
        .collect();
    assert!(rig.pass(&hold, &jobs).is_ok());

    for n in 0..=MAX_CURRENT_CANDIDATES {
        rig.write_cache(&Rig::active_cache(CLAUDE, &format!("cur-{n}"), 1, false));
    }
    let jobs: Vec<_> = (0..=MAX_CURRENT_CANDIDATES)
        .map(|n| job(CLAUDE, &format!("cur-{n}"), 1, End))
        .collect();
    rig.store.reset();
    assert_eq!(
        rig.pass(&hold, &jobs).unwrap_err(),
        Reason::CandidatesOverflow
    );
    assert_eq!(rig.store.calls(), 0);
    let pass = rig.pass(&hold, &jobs[..MAX_CURRENT_CANDIDATES]).unwrap();
    assert_eq!(pass.classified.len(), MAX_CURRENT_CANDIDATES);
    assert_eq!(pass.classified[0].key.native, "cur-0");
    assert_eq!(pass.calls, rig.store.calls());
    // Repeated inputs of one key: one classification each, one set of reads.
    rig.store.reset();
    let pass = rig
        .pass(
            &hold,
            &[job(CLAUDE, "cur-0", 1, End), job(CLAUDE, "cur-0", 1, End)],
        )
        .unwrap();
    assert_eq!(pass.classified.len(), 2);
    assert_eq!(pass.classified[0].context_id, pass.classified[1].context_id);
    assert_eq!(
        rig.store.calls(),
        10,
        "one key, one set of reads, no receipt probe"
    );
}

#[test]
fn declared_verb_lists_are_preflighted_before_any_receipt_query() {
    let rig = Rig::new();
    let hold = rig.admit();
    let native = "v-native";
    let body = payload("session_id", native);
    rig.ingest(CLAUDE, "SessionStart", &body, 10);
    rig.ingest(CLAUDE, PROMPT, &body, 20);
    rig.ingest(CLAUDE, STOP, &body, 30);
    let verbs = |count: usize| -> Vec<String> {
        let mut verbs: Vec<String> = (1..count).map(|n| format!("Verb{n}")).collect();
        verbs.push(STOP.into());
        verbs
    };
    let receipt_queries = || rig.store.queries_starting_with(super::rows::RECEIPT_BY_PK);
    for (label, list) in [
        ("sixty-five", verbs(65)),
        ("long", vec![STOP.into(), "v".repeat(129)]),
        ("duplicate", vec![STOP.into(), STOP.into()]),
        ("undeclared", vec!["PreToolUse".into()]),
    ] {
        rig.store.reset();
        let registry = Declaring::registry(CLAUDE, list);
        let pass = rig
            .pass_with(&hold, &registry, &[job(CLAUDE, native, 1, End)])
            .unwrap();
        assert_eq!(
            pass.classified[0].verdict,
            Verdict::Unavailable(Reason::AdapterVerbs),
            "{label}"
        );
        assert_eq!(receipt_queries(), 0, "{label}");
    }
    rig.store.reset();
    let registry = Declaring::registry(CODEX, verbs(64));
    let pass = rig
        .pass_with(&hold, &registry, &[job(CLAUDE, native, 1, End)])
        .unwrap();
    assert_eq!(
        pass.classified[0].verdict,
        Verdict::Unavailable(Reason::AdapterVerbs),
        "no adapter"
    );
    assert_eq!(receipt_queries(), 0);

    rig.store.reset();
    let registry = Declaring::registry(CLAUDE, verbs(64));
    let pass = rig
        .pass_with(&hold, &registry, &[job(CLAUDE, native, 1, End)])
        .unwrap();
    assert!(matches!(pass.classified[0].verdict, Verdict::Eligible(_)));
    assert_eq!(
        receipt_queries(),
        1,
        "one kind-specific probe, not sixty-four"
    );
    assert_eq!(rig.store.calls(), 11);
}

#[test]
fn an_unmoved_member_gains_the_sessions_capability_only_through_shared_readiness() {
    let rig = Rig::over(Fixture::cold());
    let hold = rig.admit();
    assert!(hold.sessions_namespace().is_err(), "nothing moved yet");
    let legacy = RunRoot::Sessions.legacy_under(rig.fixture.owner.git_dir());
    std::fs::create_dir_all(&legacy).unwrap();
    let mut old = Rig::active_cache(CLAUDE, "old-native", 1, false);
    old.work_tree = Some(rig.fixture.work_tree.to_string_lossy().into_owned());
    std::fs::write(
        legacy.join("old-native.json"),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    assert!(
        hold.sessions_namespace().is_err(),
        "an unmoved legacy tree is not a capability"
    );

    crate::family::run_state::prepare_recovery(&hold).unwrap();
    assert!(rig.fixture.member.join(MOVED_MARKER).is_file());
    hold.check_current().unwrap();
    let ready = hold.sessions_namespace().unwrap();
    assert_eq!(ready.repo_id(), &rig.repo_id);
    assert_eq!(ready.work_tree(), rig.fixture.work_tree);
    // The migrated cache is read from the current tree now, as it stands.
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, "old-native", 1, End)),
        Verdict::Unavailable(Reason::IdentityMissing)
    );
    assert!(!legacy.join("old-native.json").exists());
    // Written after the move into the old tree: never read.
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(
        legacy.join("late-native.json"),
        serde_json::to_vec(&Rig::active_cache(CLAUDE, "late-native", 1, false)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        rig.verdict(&hold, &job(CLAUDE, "late-native", 1, End)),
        Verdict::Unavailable(Reason::CacheAbsent)
    );
}

#[test]
fn the_capability_needs_a_regular_marker_and_a_present_sessions_directory() {
    let rig = Rig::new();
    let hold = rig.admit();
    let marker = rig.fixture.member.join(MOVED_MARKER);
    assert!(marker.is_file());
    assert!(!rig.sessions_dir.exists());
    assert!(
        hold.sessions_namespace().is_err(),
        "no sessions directory yet"
    );
    rig.write_cache(&Rig::active_cache(CLAUDE, "any", 1, false));
    let ready = hold.sessions_namespace().unwrap();
    ready.recheck().unwrap();

    let saved = rig.fixture.member.join("marker-saved");
    std::fs::rename(&marker, &saved).unwrap();
    assert!(ready.recheck().is_err());
    assert!(hold.sessions_namespace().is_err(), "a missing marker");
    std::os::unix::fs::symlink(&saved, &marker).unwrap();
    assert!(hold.sessions_namespace().is_err(), "a symlinked marker");
    std::fs::remove_file(&marker).unwrap();
    std::fs::rename(&saved, &marker).unwrap();
    let ready = hold.sessions_namespace().unwrap();
    // The sessions directory replaced under the capability is noticed.
    let aside = rig.fixture.member.join("sessions-aside");
    std::fs::rename(&rig.sessions_dir, &aside).unwrap();
    std::fs::create_dir(&rig.sessions_dir).unwrap();
    std::fs::set_permissions(&rig.sessions_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(ready.recheck().is_err());
    assert!(aside.exists());
}

#[test]
fn a_held_members_namespace_refuses_another_members_input() {
    let rig_a = Rig::new();
    let rig_b = Rig::new();
    let hold = rig_a.admit();
    rig_a.write_cache(&Rig::active_cache(CLAUDE, "a-native", 1, false));
    assert!(rig_a
        .pass(&hold, &[job(CLAUDE, "a-native", 1, End)])
        .is_ok());
    rig_a.store.reset();
    assert_eq!(
        rig_a
            .pass_as(&hold, &rig_b.member(), &[job(CLAUDE, "a-native", 1, End)])
            .unwrap_err(),
        Reason::MemberMismatch
    );
    let same_repo_other_tree = Member::new(
        rig_a.repo_id.clone(),
        rig_a.fixture.owner.git_dir().to_str().unwrap(),
        rig_b.fixture.work_tree.to_str().unwrap(),
    )
    .unwrap();
    assert_eq!(
        rig_a
            .pass_as(
                &hold,
                &same_repo_other_tree,
                &[job(CLAUDE, "a-native", 1, End)]
            )
            .unwrap_err(),
        Reason::MemberMismatch
    );
    assert_eq!(rig_a.store.calls(), 0, "a refused member reads nothing");
    assert!(Member::new(rig_a.repo_id.clone(), "relative/git", "/tree").is_err());
}
