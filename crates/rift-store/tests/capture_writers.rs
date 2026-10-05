//! Integration test: the capture-projection writers and readers as a dependent
//! crate (`rift-shadow`, the hook worker) drives them, through the public API
//! only.
//!
//! Every writer skips a conflict on its content-addressed key, so the
//! property each test pins is the same one the event bridge relies on: the
//! first delivery lands one row, a redelivery is a no-op that reports `false`,
//! and the readers give back exactly what was written. The clock is a literal;
//! nothing here reads the wall clock.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use rift_store::{
    content_blob_oid_map, latest_archive_ms, latest_turn_boundary_files, record_agent_enablement,
    record_archive_decision, record_capture_session_identity, record_captured_event,
    record_file_read_tokens, record_file_revision, record_main_context_observation,
    record_model_observation, record_session, record_session_event, record_session_in_checkpoint,
    record_session_transition, record_subagent_session, record_tool_call, record_tool_reads_file,
    record_tool_result_tokens, record_turn, record_turn_boundary_file,
    record_turn_boundary_snapshot, record_verification_divergence, record_verification_run,
    record_verification_witness, session_read_facts, turn_boundary_files_for_session_path,
    AgentEnablementRow, ArchiveDecisionRow, CanonicalStore, CaptureSessionIdentityRow,
    CapturedEventRow, FileRevisionRow, MainContextObservationRow, ModelObservationRow,
    SessionEventRow, SessionInCheckpointRow, SessionReadFact, SessionRow, SessionTransitionRow,
    SqliteStore, SubagentSessionRow, ToolCallRow, TurnBoundaryFileRow, TurnBoundarySnapshotRow,
    TurnRow, Value, VerificationDivergenceRow, VerificationRunRow, VerificationWitnessRow,
};

/// A fixed instant every row is stamped with, so no verdict depends on a clock.
const T0: i64 = 1_700_000_000_000;

fn store() -> SqliteStore {
    let store = SqliteStore::open_in_memory().unwrap();
    store.create_canonical_schema().unwrap();
    store
}

/// The first delivery reports a new row, the redelivery reports a no-op, and
/// the relation holds exactly one row either way.
async fn assert_lands_once(store: &SqliteStore, table: &str, first: bool, replay: bool) {
    assert!(first, "the first delivery into {table} must land a row");
    assert!(!replay, "a redelivery into {table} must be a no-op");
    assert_eq!(
        store.count(table).await.unwrap(),
        1,
        "{table} holds one row"
    );
}

fn tool_call(id: &str, turn_id: &str, started_at: i64) -> ToolCallRow {
    ToolCallRow {
        tool_call_id: id.to_owned(),
        turn_id: turn_id.to_owned(),
        tool_name: "Read".to_owned(),
        started_at,
        schema_version: 1,
    }
}

fn file_revision(id: &str, path: &str, blob_oid: &str, content_sha256: &str) -> FileRevisionRow {
    FileRevisionRow {
        file_revision_id: id.to_owned(),
        path: path.to_owned(),
        blob_oid: blob_oid.to_owned(),
        content_sha256: content_sha256.to_owned(),
        schema_version: 1,
    }
}

fn boundary(session_id: &str, turn_ordinal: i64, boundary: &str) -> TurnBoundarySnapshotRow {
    TurnBoundarySnapshotRow {
        snapshot_id: format!("snap-{session_id}-{turn_ordinal}-{boundary}"),
        session_id: session_id.to_owned(),
        turn_ordinal,
        boundary: boundary.to_owned(),
        actor: if boundary == "start" {
            "human"
        } else {
            "agent"
        }
        .to_owned(),
        worktree_hash: format!("wt-{turn_ordinal}-{boundary}"),
        captured_at: T0 + turn_ordinal,
        schema_version: 1,
    }
}

fn boundary_file(snapshot_id: &str, path: &str, blob_oid: &str) -> TurnBoundaryFileRow {
    TurnBoundaryFileRow {
        snapshot_id: snapshot_id.to_owned(),
        path: path.to_owned(),
        blob_oid: blob_oid.to_owned(),
        content_sha256: format!("sha-{blob_oid}"),
        schema_version: 1,
    }
}

fn archive_decision(id: &str, session_id: &str, archived: bool, at: i64) -> ArchiveDecisionRow {
    ArchiveDecisionRow {
        decision_id: id.to_owned(),
        session_id: Some(session_id.to_owned()),
        strategy: "scheduled".to_owned(),
        reason: "schedule".to_owned(),
        archived,
        checkpoint_id: if archived {
            Some(format!("cp-{id}"))
        } else {
            None
        },
        decided_at: at,
        schema_version: 1,
    }
}

/// Write one session's turn, two reads (the later one with a token count, the
/// earlier one recorded before counts existed), and the revisions they read.
async fn seed_two_reads(store: &SqliteStore) {
    let turn = TurnRow {
        turn_id: "turn-1".to_owned(),
        session_id: "sess-1".to_owned(),
        ordinal: 3,
        started_at: T0,
        schema_version: 1,
    };
    record_turn(store, &turn).await.unwrap();
    for (call, at, rev, path, oid) in [
        ("call-b", T0 + 20, "rev-b", "src/b.rs", "oid-b"),
        ("call-a", T0 + 10, "rev-a", "src/a.rs", "oid-a"),
    ] {
        record_tool_call(store, &tool_call(call, "turn-1", at))
            .await
            .unwrap();
        let revision = file_revision(rev, path, oid, &format!("sha-{oid}"));
        record_file_revision(store, &revision).await.unwrap();
        record_tool_reads_file(store, call, rev).await.unwrap();
    }
    record_file_read_tokens(store, "call-b", 42, "cl100k_base", 1)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_sessions_read_facts_come_back_oldest_first_with_tokens_only_where_counted() {
    let store = store();
    seed_two_reads(&store).await;

    let facts = session_read_facts(&store, "sess-1").await.unwrap();

    let expected = [
        SessionReadFact {
            tool_call_id: "call-a".to_owned(),
            tool_name: "Read".to_owned(),
            path: "src/a.rs".to_owned(),
            content_sha256: "sha-oid-a".to_owned(),
            input_tokens: None,
            tokenizer: None,
            started_at: T0 + 10,
            turn_ordinal: 3,
            blob_oid: "oid-a".to_owned(),
        },
        SessionReadFact {
            tool_call_id: "call-b".to_owned(),
            tool_name: "Read".to_owned(),
            path: "src/b.rs".to_owned(),
            content_sha256: "sha-oid-b".to_owned(),
            input_tokens: Some(42),
            tokenizer: Some("cl100k_base".to_owned()),
            started_at: T0 + 20,
            turn_ordinal: 3,
            blob_oid: "oid-b".to_owned(),
        },
    ];
    assert_eq!(facts, expected);
    assert!(
        session_read_facts(&store, "sess-other")
            .await
            .unwrap()
            .is_empty(),
        "another session's reads never leak into this one"
    );
}

#[tokio::test]
async fn a_replayed_tool_call_read_link_and_token_count_each_write_nothing() {
    let store = store();
    let call = tool_call("call-1", "turn-1", T0);
    let first = record_tool_call(&store, &call).await.unwrap();
    let replay = record_tool_call(&store, &call).await.unwrap();
    assert_lands_once(&store, "tool_call", first, replay).await;

    let first = record_tool_reads_file(&store, "call-1", "rev-1")
        .await
        .unwrap();
    let replay = record_tool_reads_file(&store, "call-1", "rev-1")
        .await
        .unwrap();
    assert_lands_once(&store, "tool_reads_file", first, replay).await;

    let first = record_file_read_tokens(&store, "call-1", 7, "cl100k_base", 1)
        .await
        .unwrap();
    let replay = record_file_read_tokens(&store, "call-1", 9, "cl100k_base", 1)
        .await
        .unwrap();
    assert_lands_once(&store, "file_read_tokens", first, replay).await;
    let tokens = store
        .query_opt_i64(
            "SELECT input_tokens FROM file_read_tokens WHERE tool_call_id = ?1",
            &[Value::text("call-1")],
        )
        .await
        .unwrap();
    assert_eq!(tokens, Some(7), "a replay never overwrites the first count");
}

#[tokio::test]
async fn the_blob_oid_map_names_one_oid_per_content_hash_whatever_the_path() {
    let store = store();
    for (rev, path) in [("rev-2", "copy/a.rs"), ("rev-1", "src/a.rs")] {
        let revision = file_revision(rev, path, "oid-a", "sha-a");
        record_file_revision(&store, &revision).await.unwrap();
    }
    let revision = file_revision("rev-3", "src/b.rs", "oid-b", "sha-b");
    record_file_revision(&store, &revision).await.unwrap();

    let map = content_blob_oid_map(&store).await.unwrap();

    let entries: Vec<(&str, &str)> = map.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    assert_eq!(entries, [("sha-a", "oid-a"), ("sha-b", "oid-b")]);
    let empty = self::store();
    assert!(content_blob_oid_map(&empty).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_tool_result_measurement_lands_once_and_its_channel_is_a_closed_vocabulary() {
    let store = store();
    let record = |id: &'static str, channel: &'static str| {
        let store = &store;
        async move {
            record_tool_result_tokens(
                store,
                id,
                "sess-1",
                "Grep",
                channel,
                120,
                "cl100k_base",
                "reexecuted",
                T0,
                1,
            )
            .await
        }
    };
    let first = record("result-1", "search").await.unwrap();
    let replay = record("result-1", "search").await.unwrap();
    assert_lands_once(&store, "tool_result_tokens", first, replay).await;

    // A channel outside the closed vocabulary is refused with a typed error
    // naming the relation and the column — never the `false` a redelivery
    // earns — and the row is not written.
    let refused = record("result-2", "telepathy").await;
    assert!(
        matches!(
            &refused,
            Err(rift_core::RiftError::ConstraintRefused { table, constraint })
                if table == "tool_result_tokens" && constraint == "check(channel)"
        ),
        "{refused:?}"
    );
    assert_eq!(store.count("tool_result_tokens").await.unwrap(), 1);
}

#[tokio::test]
async fn an_unmeasured_tool_result_is_counted_with_zero_tokens() {
    let store = store();
    let landed = record_tool_result_tokens(
        &store,
        "result-1",
        "sess-1",
        "Bash",
        "shell_read",
        0,
        "cl100k_base",
        "unmeasured",
        T0,
        1,
    )
    .await
    .unwrap();
    assert!(landed);
    let tokens = store
        .query_opt_i64(
            "SELECT output_tokens FROM tool_result_tokens WHERE basis = 'unmeasured'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(tokens, Some(0));
    let landed = record_tool_result_tokens(
        &store,
        "result-2",
        "sess-1",
        "Bash",
        "read_tool",
        5,
        "cl100k_base",
        "guessed",
        T0,
        1,
    )
    .await;
    assert!(
        matches!(
            &landed,
            Err(rift_core::RiftError::ConstraintRefused { table, constraint })
                if table == "tool_result_tokens" && constraint == "check(basis)"
        ),
        "a basis outside the vocabulary is refused: {landed:?}"
    );
    assert_eq!(store.count("tool_result_tokens").await.unwrap(), 1);
}

#[tokio::test]
async fn a_session_and_its_identity_events_and_transitions_each_land_once() {
    let store = store();
    let session = SessionRow {
        session_id: "sess-1".to_owned(),
        agent: "claude-code".to_owned(),
        model_primary: String::new(),
        repo_id: "repo-1".to_owned(),
        started_at: T0,
        schema_version: 1,
    };
    let first = record_session(&store, &session).await.unwrap();
    let replay = record_session(&store, &session).await.unwrap();
    assert_lands_once(&store, "session", first, replay).await;

    let identity = CaptureSessionIdentityRow {
        session_id: "sess-1".to_owned(),
        agent: "claude-code".to_owned(),
        native_session_id: "native-1".to_owned(),
        schema_version: 1,
    };
    let first = record_capture_session_identity(&store, &identity)
        .await
        .unwrap();
    let replay = record_capture_session_identity(&store, &identity)
        .await
        .unwrap();
    assert_lands_once(&store, "capture_session_identity", first, replay).await;

    let event = SessionEventRow {
        event_id: "event-1".to_owned(),
        session_id: "sess-1".to_owned(),
        kind: "start".to_owned(),
        occurred_at: T0,
        schema_version: 1,
    };
    let first = record_session_event(&store, &event).await.unwrap();
    let replay = record_session_event(&store, &event).await.unwrap();
    assert_lands_once(&store, "session_event", first, replay).await;

    let transition = SessionTransitionRow {
        transition_id: "transition-1".to_owned(),
        session_id: "sess-1".to_owned(),
        from_state: "idle".to_owned(),
        to_state: "active".to_owned(),
        trigger_kind: "session_start".to_owned(),
        occurred_at: T0,
        schema_version: 1,
    };
    let first = record_session_transition(&store, &transition)
        .await
        .unwrap();
    let replay = record_session_transition(&store, &transition)
        .await
        .unwrap();
    assert_lands_once(&store, "session_transition", first, replay).await;
}

#[tokio::test]
async fn a_subagent_link_and_a_checkpoint_membership_each_land_once() {
    let store = store();
    let subagent = SubagentSessionRow {
        child_session_id: "sess-child".to_owned(),
        parent_turn_id: "turn-1".to_owned(),
        schema_version: 1,
    };
    let first = record_subagent_session(&store, &subagent).await.unwrap();
    let replay = record_subagent_session(&store, &subagent).await.unwrap();
    assert_lands_once(&store, "subagent_session", first, replay).await;

    let membership = SessionInCheckpointRow {
        checkpoint_id: "cp-1".to_owned(),
        session_id: "sess-1".to_owned(),
        ordinal: 0,
        schema_version: 1,
    };
    let first = record_session_in_checkpoint(&store, &membership)
        .await
        .unwrap();
    let replay = record_session_in_checkpoint(&store, &membership)
        .await
        .unwrap();
    assert_lands_once(&store, "session_in_checkpoint", first, replay).await;
}

#[tokio::test]
async fn a_context_observation_and_a_model_observation_each_land_once() {
    let store = store();
    let context = MainContextObservationRow {
        observation_id: "obs-1".to_owned(),
        session_id: "sess-1".to_owned(),
        model: "claude-sonnet".to_owned(),
        observed_at: T0,
        uncached_input: 10,
        cached_input: 20,
        cache_creation: 30,
        output: 40,
        counter_method: "provider_reported".to_owned(),
        schema_version: 1,
    };
    let first = record_main_context_observation(&store, &context)
        .await
        .unwrap();
    let replay = record_main_context_observation(&store, &context)
        .await
        .unwrap();
    assert_lands_once(&store, "main_context_observation", first, replay).await;

    let model = ModelObservationRow {
        observation_id: "obs-1".to_owned(),
        session_id: "sess-1".to_owned(),
        model: "claude-sonnet".to_owned(),
        source: "runtime_hook".to_owned(),
        observed_at: T0,
        schema_version: 1,
    };
    let first = record_model_observation(&store, &model).await.unwrap();
    let replay = record_model_observation(&store, &model).await.unwrap();
    assert_lands_once(&store, "model_observation", first, replay).await;
}

#[tokio::test]
async fn a_verification_run_its_divergence_and_its_witness_each_land_once() {
    let store = store();
    let run = VerificationRunRow {
        run_id: "run-1".to_owned(),
        branch: "rift/checkpoints/v1".to_owned(),
        exit_code: 1,
        checked_at: T0,
        outcome_sha256: "outcome".to_owned(),
        schema_version: 1,
    };
    let first = record_verification_run(&store, &run).await.unwrap();
    let replay = record_verification_run(&store, &run).await.unwrap();
    assert_lands_once(&store, "verification_run", first, replay).await;

    let divergence = VerificationDivergenceRow {
        run_id: "run-1".to_owned(),
        ordinal: 0,
        checkpoint_id: "cp-1".to_owned(),
        layer: "Leaf".to_owned(),
        site: "GitVsRecomputed".to_owned(),
        detail: "leaf hash differs".to_owned(),
        schema_version: 1,
    };
    let first = record_verification_divergence(&store, &divergence)
        .await
        .unwrap();
    let replay = record_verification_divergence(&store, &divergence)
        .await
        .unwrap();
    assert_lands_once(&store, "verification_divergence", first, replay).await;
}

#[tokio::test]
async fn an_unreadable_chain_head_is_stored_as_null_never_as_an_empty_string() {
    let store = store();
    let witness = |checkpoint_id: &str, chain_head: Option<&str>| VerificationWitnessRow {
        run_id: "run-1".to_owned(),
        checkpoint_id: checkpoint_id.to_owned(),
        chain_head: chain_head.map(str::to_owned),
        schema_version: 1,
    };
    let first = record_verification_witness(&store, &witness("cp-1", None))
        .await
        .unwrap();
    let replay = record_verification_witness(&store, &witness("cp-1", Some("head")))
        .await
        .unwrap();
    assert_lands_once(&store, "verification_witness", first, replay).await;
    record_verification_witness(&store, &witness("cp-2", Some("head")))
        .await
        .unwrap();

    let rows = store
        .query(
            "SELECT checkpoint_id, chain_head FROM verification_witness ORDER BY checkpoint_id",
            &[],
        )
        .await
        .unwrap();
    let heads: Vec<(String, Option<String>)> = rows
        .iter()
        .map(|r| (r.get_text(0).unwrap(), r.get_opt_text(1).unwrap()))
        .collect();
    assert_eq!(
        heads,
        [
            ("cp-1".to_owned(), None),
            ("cp-2".to_owned(), Some("head".to_owned())),
        ]
    );
}

#[tokio::test]
async fn a_duplicate_captured_event_delivery_is_a_true_no_op() {
    let store = store();
    let event = CapturedEventRow {
        idempotency_key: "key-1".to_owned(),
        session_id: "sess-1".to_owned(),
        turn_ordinal: 0,
        event_ordinal: 0,
        kind: "prompt".to_owned(),
        trust: "untrusted".to_owned(),
        payload_sha256: "payload".to_owned(),
        occurred_at_ms: T0,
        schema_version: 1,
    };
    let first = record_captured_event(&store, &event).await.unwrap();
    let replay = record_captured_event(&store, &event).await.unwrap();
    assert_lands_once(&store, "captured_event", first, replay).await;
}

#[tokio::test]
async fn enable_and_disable_are_separate_events_and_a_rerun_of_either_is_a_no_op() {
    let store = store();
    let change = |id: &str, action: &str, at: i64| AgentEnablementRow {
        change_id: id.to_owned(),
        agent: "codex".to_owned(),
        action: action.to_owned(),
        changed_at: at,
        wired_hooks_hash: "hooks".to_owned(),
        schema_version: 1,
    };
    assert!(record_agent_enablement(&store, &change("c1", "enable", T0))
        .await
        .unwrap());
    assert!(
        !record_agent_enablement(&store, &change("c1", "enable", T0))
            .await
            .unwrap()
    );
    assert!(
        record_agent_enablement(&store, &change("c2", "disable", T0 + 1))
            .await
            .unwrap()
    );
    let rows = store
        .query(
            "SELECT action FROM agent_enablement ORDER BY changed_at",
            &[],
        )
        .await
        .unwrap();
    let actions: Vec<String> = rows.iter().map(|r| r.get_text(0).unwrap()).collect();
    assert_eq!(actions, ["enable", "disable"]);
}

#[tokio::test]
async fn the_last_archive_is_the_latest_decision_that_actually_archived() {
    let store = store();
    assert_eq!(latest_archive_ms(&store, "sess-1").await.unwrap(), None);
    for decision in [
        archive_decision("d1", "sess-1", true, T0),
        archive_decision("d2", "sess-1", true, T0 + 50),
        archive_decision("d3", "sess-1", false, T0 + 90),
        archive_decision("d4", "sess-2", true, T0 + 99),
    ] {
        record_archive_decision(&store, &decision).await.unwrap();
    }

    let latest = latest_archive_ms(&store, "sess-1").await.unwrap();

    assert_eq!(
        latest,
        Some(T0 + 50),
        "a skipped decision is not an archive"
    );
    assert_eq!(latest_archive_ms(&store, "sess-3").await.unwrap(), None);
}

#[tokio::test]
async fn a_paths_boundary_history_replays_turns_in_order_with_start_before_end() {
    let store = store();
    for (turn, edge, oid) in [(2, "end", "o4"), (1, "end", "o2"), (2, "start", "o3")] {
        let snapshot = boundary("sess-1", turn, edge);
        record_turn_boundary_snapshot(&store, &snapshot)
            .await
            .unwrap();
        let file = boundary_file(&snapshot.snapshot_id, "src/a.rs", oid);
        record_turn_boundary_file(&store, &file).await.unwrap();
    }
    let start = boundary("sess-1", 1, "start");
    record_turn_boundary_snapshot(&store, &start).await.unwrap();
    for (path, oid) in [("src/a.rs", "o1"), ("src/other.rs", "x1")] {
        let file = boundary_file(&start.snapshot_id, path, oid);
        record_turn_boundary_file(&store, &file).await.unwrap();
    }
    let elsewhere = boundary("sess-2", 1, "start");
    record_turn_boundary_snapshot(&store, &elsewhere)
        .await
        .unwrap();
    let file = boundary_file(&elsewhere.snapshot_id, "src/a.rs", "z1");
    record_turn_boundary_file(&store, &file).await.unwrap();

    let history = turn_boundary_files_for_session_path(&store, "sess-1", "src/a.rs")
        .await
        .unwrap();

    let expected = [
        (1, "start", "o1"),
        (1, "end", "o2"),
        (2, "start", "o3"),
        (2, "end", "o4"),
    ]
    .map(|(turn, edge, oid)| (turn, edge.to_owned(), oid.to_owned()));
    assert_eq!(history, expected);
    let never_seen = turn_boundary_files_for_session_path(&store, "sess-1", "src/none.rs")
        .await
        .unwrap();
    assert!(never_seen.is_empty());
}

#[tokio::test]
async fn a_session_with_no_boundary_has_no_latest_files() {
    let store = store();
    let files = latest_turn_boundary_files(&store, "sess-1").await.unwrap();
    assert!(files.is_empty());
}
