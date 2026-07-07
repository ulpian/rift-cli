//! ADR-0002's load-bearing invariant: **hook-rows ∪ bridge-rows is a set-union
//! no-op**, and ingest is idempotent under duplication and permutation.
//!
//! The live hook path and the artifact bridge derive identical content-addressed
//! ids (the shared id-derivation module + the shared projector), so feeding the
//! same session through both sources — in either order, any number of times —
//! must land exactly one row set.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
use rift_shadow::bridge::ingest_hook;
use rift_shadow::hooks::{dispatch, AdapterRegistry, AgentId, RawHook};
use rift_store::{CanonicalStore, SqliteStore};

fn store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    s.create_canonical_schema().unwrap();
    s
}

/// The scripted two-turn session with a human edit between turns (the M1 gate
/// shape): start → prompt → tool → turn end (usage) → prompt → turn end → end.
fn session_hooks() -> Vec<RawHook> {
    let h = |verb: &str, turn: u32, at: i64, payload: serde_json::Value| RawHook {
        agent: AgentId::CLAUDE_CODE,
        verb: verb.into(),
        session: "native-dual".into(),
        turn: Some(turn),
        occurred_at_ms: at,
        payload,
    };
    vec![
        h(
            "SessionStart",
            0,
            10,
            serde_json::json!({"model": "claude-sonnet-4-5"}),
        ),
        h("UserPromptSubmit", 1, 20, serde_json::json!({})),
        h(
            "PreToolUse",
            1,
            30,
            serde_json::json!({"tool_name": "Bash"}),
        ),
        h(
            "Stop",
            1,
            40,
            serde_json::json!({"model": "claude-sonnet-4-5",
                "usage": {"input_tokens": 100, "output_tokens": 50}}),
        ),
        h("UserPromptSubmit", 2, 50, serde_json::json!({})),
        h(
            "Stop",
            2,
            60,
            serde_json::json!({"model": "claude-sonnet-4-5",
                "usage": {"input_tokens": 40, "output_tokens": 20}}),
        ),
        h("SessionEnd", 3, 70, serde_json::json!({})),
    ]
}

/// Archive the hook sequence the way the capture side would: one `full.jsonl`
/// record per canonical event, carrying the SAME payload hash the hook path
/// stamped (artifacts record content addresses, they never re-derive them).
fn full_jsonl_for(hooks: &[RawHook]) -> String {
    let registry = AdapterRegistry::with_builtin();
    let mut lines = Vec::new();
    for raw in hooks {
        for ev in dispatch(raw, &registry).unwrap() {
            lines.push(
                serde_json::json!({
                    "session": ev.session,
                    "turn": ev.turn_ordinal,
                    "event": ev.event_ordinal,
                    "kind": ev.kind.as_str(),
                    "verb": raw.verb,
                    "agent": "claude-code",
                    "occurred_at_ms": ev.occurred_at_ms,
                    "payload_sha256": ev.payload_sha256.to_hex(),
                    "payload": raw.payload,
                })
                .to_string(),
            );
        }
    }
    lines.join("\n")
}

fn checkpoint_substrate(full: &str) -> MemorySubstrate {
    let id = "ab".repeat(32);
    let report = format!(
        r#"{{"total_spans":0,"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
        "c".repeat(64)
    );
    MemorySubstrate::local()
        .with_shard(CheckpointShard {
            checkpoint_id: id.clone(),
            dialect: Dialect::Rift,
            prev: None,
            ordinal: 0,
        })
        .with_artifact(&id, "redaction_report.json", report.as_bytes())
        .with_artifact(
            &id,
            "metadata.json",
            format!(
                r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["native-dual"]}}"#,
                "d".repeat(64)
            )
            .as_bytes(),
        )
        .with_artifact(&id, "full.jsonl", full.as_bytes())
}

/// Every table both sources write into (the union surface).
const UNION_TABLES: &[&str] = &[
    "captured_event",
    "session",
    "session_event",
    "turn",
    "model_call",
    "tool_call",
    "token_usage",
    "subagent_session",
];

async fn counts(s: &SqliteStore) -> Vec<(&'static str, i64)> {
    let mut out = Vec::new();
    for t in UNION_TABLES {
        out.push((*t, s.count(t).await.unwrap()));
    }
    out
}

async fn ingest_all(s: &SqliteStore, hooks: &[RawHook], git_dir: &std::path::Path) {
    let reg = AdapterRegistry::with_builtin();
    for raw in hooks {
        ingest_hook(
            raw,
            &reg,
            s,
            git_dir,
            "claude-code",
            "repo-dual",
            raw.occurred_at_ms,
            None,
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn hook_rows_union_bridge_rows_is_a_set_union_noop() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    let hooks = session_hooks();

    // 1. Live hook ingest.
    ingest_all(&s, &hooks, tmp.path()).await;
    let after_hooks = counts(&s).await;
    assert!(
        after_hooks
            .iter()
            .any(|(t, n)| *t == "captured_event" && *n > 0),
        "hook path landed events"
    );

    // 2. The authoritative bridge over the SAME session's archived artifacts.
    let sub = checkpoint_substrate(&full_jsonl_for(&hooks));
    let report = rift_shadow::ingest_from_substrate(&s, &sub, "repo-dual", "claude-code", 999)
        .await
        .unwrap();
    assert_eq!(report.shards_projected, 1);
    assert_eq!(
        report.events_inserted, 0,
        "bridge over hook-fed projection inserts NOTHING (set-union no-op)"
    );
    assert_eq!(counts(&s).await, after_hooks, "no table gained a row");
}

#[tokio::test]
async fn bridge_alone_reproduces_the_hook_fed_row_set() {
    let tmp = tempfile::tempdir().unwrap();
    let hooks = session_hooks();

    // Hook-fed projection A.
    let a = store();
    ingest_all(&a, &hooks, tmp.path()).await;

    // Bridge-fed projection B, from artifacts alone (the delete-and-rebuild
    // story: the projection is never a single point of loss).
    let b = store();
    let sub = checkpoint_substrate(&full_jsonl_for(&hooks));
    rift_shadow::ingest_from_substrate(&b, &sub, "repo-dual", "claude-code", 999)
        .await
        .unwrap();

    // The normalized row sets agree (counts per table + the exact event keys).
    assert_eq!(counts(&a).await, counts(&b).await);
    async fn keys(s: &SqliteStore) -> Vec<String> {
        s.query(
            "SELECT idempotency_key FROM captured_event ORDER BY idempotency_key",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get_text(0).unwrap())
        .collect()
    }
    assert_eq!(keys(&a).await, keys(&b).await, "identical event identities");
}

#[tokio::test]
async fn ingest_is_idempotent_under_duplication_and_permutation() {
    let tmp = tempfile::tempdir().unwrap();
    let hooks = session_hooks();

    let once = store();
    ingest_all(&once, &hooks, tmp.path()).await;
    let baseline = counts(&once).await;

    // Re-deliver everything twice more, the second time in reverse order.
    let noisy = store();
    ingest_all(&noisy, &hooks, tmp.path()).await;
    ingest_all(&noisy, &hooks, tmp.path()).await;
    let reversed: Vec<_> = hooks.iter().rev().cloned().collect();
    ingest_all(&noisy, &reversed, tmp.path()).await;

    assert_eq!(
        counts(&noisy).await,
        baseline,
        "duplication + permutation change nothing (content-addressed ids)"
    );
}
