//! End-to-end: two Stops through the real binary seal a live session turn by
//! turn. Each mints a `turn-end` checkpoint; the second carries the first's
//! full and compact parts by the blob oids the first shard wrote, and the
//! fixture repository still verifies.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{run_rift_stdout, seed_live_session_repo, sync_env, DetachedHooks};
use rift_recall_substrate::parse::segments::{compact_segment_name, segment_name};

const SESSION: &str = "01JTURNSEALTWOSTOPS00000001";

/// `count` alternating user and assistant records, each a whole line, so
/// both the full and the compact form have something to seal.
fn write_records(path: &Path, count: usize) {
    let body: String = (0..count)
        .map(|i| {
            let record = if i % 2 == 0 {
                serde_json::json!({"type":"user","uuid":format!("u{i}"),
                    "message":{"role":"user","content":format!("step {i}")}})
            } else {
                serde_json::json!({"type":"assistant","uuid":format!("a{i}"),
                    "message":{"id":format!("m{i}"),"role":"assistant",
                        "content":[{"type":"text","text":format!("done {i}")}]}})
            };
            record.to_string() + "\n"
        })
        .collect();
    std::fs::write(path, body).unwrap();
}

/// One hook for the session through the real binary; a capture hook never
/// fails the agent.
fn hook(
    hooks: &DetachedHooks,
    repo: &Path,
    env: &[(&str, &str)],
    verb: &str,
    extra: serde_json::Value,
) {
    let mut body = extra;
    body["session_id"] = SESSION.into();
    body["hook_event_name"] = verb.into();
    let (code, stderr) = hooks.run(
        repo,
        &["hooks", "claude-code", verb],
        &body.to_string(),
        env,
    );
    assert_eq!(code, 0, "{verb}: {stderr}");
}

/// The `turn-end` checkpoints the fixture's projection holds, oldest first.
fn turn_end_checkpoints(cache: &Path, repo: &Path) -> Vec<String> {
    let repo_id = rift_git::repo_id_for(repo).unwrap();
    let db = rift_store::projection_db_under(cache, &repo_id);
    let store = rift_store::open_capture_projection(db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let rows = rt
        .block_on(rift_store::CanonicalStore::query(
            &store,
            "SELECT checkpoint_id FROM archive_decision WHERE reason = 'turn-end' \
             AND archived = 1 ORDER BY decided_at, decision_id",
            &[],
        ))
        .unwrap();
    rows.iter().map(|r| r.get_text(0).unwrap()).collect()
}

#[test]
fn two_stops_mint_two_turn_end_checkpoints_and_the_second_carries_the_first_parts_by_oid() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let transcript = tmp.path().join(format!("{SESSION}.jsonl"));
    write_records(&transcript, 4);
    let hooks = DetachedHooks::default();
    let path = transcript.display().to_string();
    hook(
        &hooks,
        &repo,
        &env,
        "SessionStart",
        serde_json::json!({"source":"startup","transcript_path":path}),
    );
    hook(
        &hooks,
        &repo,
        &env,
        "UserPromptSubmit",
        serde_json::json!({"prompt":"step one"}),
    );
    hook(
        &hooks,
        &repo,
        &env,
        "Stop",
        serde_json::json!({"stop_hook_active":false}),
    );
    hooks.await_workers(|| turn_end_checkpoints(&cache, &repo).len() == 1);
    write_records(&transcript, 8);
    hook(
        &hooks,
        &repo,
        &env,
        "UserPromptSubmit",
        serde_json::json!({"prompt":"step two"}),
    );
    hook(
        &hooks,
        &repo,
        &env,
        "Stop",
        serde_json::json!({"stop_hook_active":false}),
    );
    hooks.await_workers(|| turn_end_checkpoints(&cache, &repo).len() == 2);

    let ids = turn_end_checkpoints(&cache, &repo);
    assert_eq!(ids.len(), 2, "one turn-end checkpoint per Stop: {ids:?}");
    let walker = rift_recall_substrate::ShardWalker::open(&repo).unwrap();
    let entry = |id: &str, name: &str| walker.read_native_entry(id, name).unwrap();
    let full = entry(&ids[0], &segment_name(0)).expect("the first turn is segmented");
    assert_eq!(
        entry(&ids[1], &segment_name(0)).unwrap().0,
        full.0,
        "full part 0 by oid"
    );
    assert!(
        entry(&ids[1], &segment_name(1)).is_some(),
        "turn two appends a full part"
    );
    let compact = entry(&ids[0], &compact_segment_name(0)).expect("the compact form is segmented");
    let carried = entry(&ids[1], &compact_segment_name(0)).expect("turn two keeps compact part 0");
    assert_eq!(carried.0, compact.0, "compact part 0 by oid");

    let (code, out) = run_rift_stdout(&repo, &["verify"], &env);
    assert_eq!(code, 0, "the fixture repository verifies: {out}");
}
