//! The load-bearing invariant: **hook-rows ∪ bridge-rows is a set-union
//! no-op**, and ingest is idempotent under duplication and permutation.
//!
//! The live hook path and the artifact bridge derive identical content-addressed
//! ids (the shared id-derivation module + the shared projector), so feeding the
//! same session through both sources — in either order, any number of times —
//! must land exactly one row set.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use rift_core::Sha256Id;
use rift_hashchain::ChainLink;
use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate, SubstrateReader as _};
use rift_shadow::bridge::ingest_hook;
use rift_shadow::hooks::{dispatch, AdapterRegistry, AgentId, RawHook};
use rift_shadow::legacy_import::{imported_checkpoint_record, parse_shard, Layout};
use rift_store::{
    record_checkpoint, record_imported_checkpoint_if_new, verify_against_source, CanonicalStore,
    ChainSource, CheckpointRecord, SqliteStore,
};

fn store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    s.create_canonical_schema().unwrap();
    s
}

/// The scripted two-turn session with a human edit between turns (the gate
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

// --- the legacy import writes, and writes ONCE --------------------------

/// A pre-Rift shard beside a native one, in the substrate shape the walk yields.
fn mixed_legacy_and_native() -> MemorySubstrate {
    let report = format!(
        r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
        "a".repeat(64)
    );
    let full = |session: &str| {
        format!(
            "{{\"session_id\":\"{session}\",\"turn\":0,\"event\":0,\"kind\":\"session_start\",\
             \"verb\":\"SessionStart\",\"occurred_at\":10}}\n\
             {{\"session_id\":\"{session}\",\"turn\":1,\"event\":0,\"kind\":\"tool_call\",\
             \"verb\":\"PreToolUse\",\"occurred_at\":20}}\n\
             {{\"session_id\":\"{session}\",\"turn\":1,\"event\":1,\"kind\":\"session_end\",\
             \"verb\":\"SessionEnd\",\"occurred_at\":30}}\n"
        )
    };
    let legacy = "ab34cd56ef78";
    let native = "c".repeat(64);
    MemorySubstrate::local()
        .with_shard(CheckpointShard {
            checkpoint_id: legacy.to_owned(),
            dialect: Dialect::Legacy,
            prev: None,
            ordinal: 1,
        })
        .with_artifact(legacy, "redaction_report.json", report.as_bytes())
        .with_artifact(legacy, "full.jsonl", full("01JLEGACYSESSION").as_bytes())
        .with_shard(CheckpointShard {
            checkpoint_id: native.clone(),
            dialect: Dialect::Rift,
            prev: None,
            ordinal: 2,
        })
        .with_artifact(&native, "redaction_report.json", report.as_bytes())
        .with_artifact(&native, "full.jsonl", full("01JNATIVESESSION").as_bytes())
}

/// **The import writes, and it writes only what was imported.**
///
/// `rift import --from-legacy` reported and stopped; "imported history" had no
/// referent. It now ingests through the artifact bridge restricted to the
/// pre-Rift dialect — restricted, because an unfiltered ingest would also sweep
/// the repository's own native checkpoints, which is `rift rebuild`'s job and
/// would make an import report numbers about work nobody imported.
#[tokio::test]
async fn a_legacy_import_writes_only_the_legacy_shards_rows() {
    let store = store();
    let sub = mixed_legacy_and_native();

    let report = rift_shadow::ingest_dialect(
        &store,
        &sub,
        "test-repo",
        "claude-code",
        1_700_000_000,
        Some(Dialect::Legacy),
    )
    .await
    .unwrap();

    assert_eq!(report.shards_seen, 1, "only the pre-Rift shard is in scope");
    assert_eq!(report.shards_projected, 1);
    assert!(report.events_inserted > 0, "the import actually wrote rows");

    // The native shard's session is NOT in the projection: import is import.
    let sessions = store.count("session").await.unwrap();
    assert_eq!(sessions, 1, "one session — the imported one");
}

/// **Idempotence proven by CONTENT, not by a cache.**
///
/// The second run re-reads every artifact and re-derives every event — the
/// `events_derived` count on run two is the proof it did not short-circuit —
/// and the store accepts none of them, because every row id is content-
/// addressed and every writer is `INSERT … OR IGNORE`. The check that matters
/// is the row COUNTS taken from the store itself on both sides, not the number
/// the run reports about itself.
#[tokio::test]
async fn a_second_legacy_import_inserts_no_rows() {
    let store = store();
    let sub = mixed_legacy_and_native();
    let first = rift_shadow::ingest_dialect(
        &store,
        &sub,
        "test-repo",
        "claude-code",
        1_700_000_000,
        Some(Dialect::Legacy),
    )
    .await
    .unwrap();
    let counts_after_first = import_counts(&store).await;
    assert!(first.events_inserted > 0);

    let second = rift_shadow::ingest_dialect(
        &store,
        &sub,
        "test-repo",
        "claude-code",
        1_700_000_000,
        Some(Dialect::Legacy),
    )
    .await
    .unwrap();
    let counts_after_second = import_counts(&store).await;

    assert_eq!(
        second.events_derived, first.events_derived,
        "the second run re-derived everything — it did not short-circuit on a cache"
    );
    assert_eq!(
        second.events_inserted, 0,
        "and the store accepted none of it"
    );
    assert_eq!(
        counts_after_first, counts_after_second,
        "every projected table holds exactly the rows it held before"
    );
}

/// Row counts of every table an import can touch — the content comparison.
/// Named tuples rather than bare numbers so a mismatch says WHICH table moved.
async fn import_counts(store: &SqliteStore) -> Vec<(&'static str, i64)> {
    let mut out = Vec::new();
    for table in [
        "captured_event",
        "session",
        "session_event",
        "turn",
        "tool_call",
        "model_call",
        "checkpoint",
        "session_in_checkpoint",
    ] {
        out.push((table, store.count(table).await.unwrap_or(-1)));
    }
    out
}

/// **An imported checkpoint gets NO chain position, and that is deliberate.**
///
/// Its id is not a SHA-256, so it cannot be one; and minting a Rift id for it
/// would put a `checkpoint` row in the projection that the git side cannot
/// produce, which `rift verify` reads as tamper on every run afterwards. The
/// rule "imported checkpoints are claims, not chain anchors" is that fact, and
/// this pins it so a later change cannot quietly grant one.
#[tokio::test]
async fn an_imported_checkpoint_takes_no_chain_position() {
    let store = store();
    let sub = mixed_legacy_and_native();
    rift_shadow::ingest_dialect(
        &store,
        &sub,
        "test-repo",
        "claude-code",
        1_700_000_000,
        Some(Dialect::Legacy),
    )
    .await
    .unwrap();
    assert_eq!(
        store.count("checkpoint").await.unwrap(),
        0,
        "an imported pre-Rift shard is a claim, never an anchor"
    );
}

/// A `ChainSource` fixture over a fixed link list — the same shape
/// `crate::integrity`'s own tests use, reproduced here because it is
/// crate-private there.
struct Fixture(Vec<ChainLink>);
impl ChainSource for Fixture {
    fn links(&self) -> rift_core::Result<Vec<ChainLink>> {
        Ok(self.0.clone())
    }
}

/// **The durable table grants a home, not a chain position — and
/// `rift verify`'s own comparator is the proof, not just a row count.**
///
/// This is the "rift verify rule" the table's own doc comment promises:
/// `verify_against_source` reads `checkpoint` alone (`read_stored_links`), so
/// an `imported_checkpoint` row sitting beside a real chain is invisible to
/// it — never counted, never compared, never a source of a false divergence.
/// Pinned by construction (writing the import row changes nothing the
/// comparator reads) rather than by argument.
#[tokio::test]
async fn an_imported_checkpoint_claim_never_perturbs_verify_against_source() {
    let store = store();

    // One real, native checkpoint — the chain `rift verify` compares against.
    let native = CheckpointRecord {
        checkpoint_id: Sha256Id::from_bytes([7; 32]),
        strategy: "manual-commit".into(),
        created_at: 1,
        merkle_root: Sha256Id::from_bytes([9; 32]),
        prev: None,
        schema_version: 1,
    };
    record_checkpoint(&store, &native).await.unwrap();

    // A legacy claim recorded ALONGSIDE it, exactly as `rift import
    // --from-legacy` would leave one.
    let sub = mixed_legacy_and_native();
    let report_bytes = sub
        .read_artifact("ab34cd56ef78", "redaction_report.json")
        .unwrap()
        .unwrap();
    let full_bytes = sub.read_artifact("ab34cd56ef78", "full.jsonl").unwrap();
    let mut artifacts = std::collections::BTreeMap::new();
    artifacts.insert("redaction_report.json".to_owned(), report_bytes);
    if let Some(b) = full_bytes {
        artifacts.insert("full.jsonl".to_owned(), b);
    }
    let shard = parse_shard("ab34cd56ef78", Layout::ShardedBranch, &artifacts);
    let claim = imported_checkpoint_record(&shard, 1_700_000_000);
    assert!(record_imported_checkpoint_if_new(&store, &claim)
        .await
        .unwrap());
    assert_eq!(store.count("imported_checkpoint").await.unwrap(), 1);

    // `rift verify`'s comparator, given the SAME single-link chain git would
    // report: green, and the count it reports is the native chain's, not the
    // native chain plus the claim sitting beside it.
    let source = Fixture(vec![ChainLink {
        checkpoint_id: native.checkpoint_id,
        merkle_root: native.merkle_root,
        prev: native.prev,
    }]);
    let report = verify_against_source(&store, &source).await.unwrap();
    assert!(report.ok);
    assert_eq!(
        report.checkpoints, 1,
        "the imported claim must not be counted as a verified checkpoint"
    );
}
