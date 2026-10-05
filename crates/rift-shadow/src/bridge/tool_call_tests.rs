//! One `tool_call` row per tool call: a turn's tool calls are told apart by
//! the agent's own tool-use id, never by the hook-local event ordinal, which
//! restarts at 0 in every hook and so named every call of a turn the same.

use rift_shadow_hooks::AdapterRegistry;
use rift_store::{CanonicalStore, SqliteStore};
use serde_json::{json, Value};

use super::fixtures::{raw, store};
use super::ingest_hook;
use crate::project::ReadContent;

/// The session row every hook in this module lands on.
fn session_row() -> String {
    crate::ids::session_row_id("claude-code", "01JSESSION")
}

/// Deliver one hook of `verb` with `payload` in turn 1, resolving any read to
/// a fixed revision so a `PostToolUse` of `Read` records its read row.
async fn deliver(s: &SqliteStore, git_dir: &std::path::Path, verb: &str, payload: Value) {
    let reg = AdapterRegistry::with_builtin();
    let hasher = |path: &str| {
        Some(ReadContent {
            rel_path: path.to_owned(),
            blob_oid: "oid-1".into(),
            content_sha256: "aa11".into(),
            tokens: None,
        })
    };
    let mut hook = raw(verb, 1);
    hook.payload = payload;
    ingest_hook(
        &hook,
        &reg,
        s,
        git_dir,
        "claude-code",
        "repo-1",
        10,
        Some(&hasher),
    )
    .await
    .unwrap();
}

fn pre_tool(tool: &str, tool_use_id: &str) -> Value {
    json!({"tool_name": tool, "tool_use_id": tool_use_id, "tool_input": {}})
}

/// Rows that record a tool having run — every `tool_call` row no read links.
async fn tool_ran_names(s: &SqliteStore) -> Vec<String> {
    s.query(
        "SELECT tool_name FROM tool_call \
         WHERE tool_call_id NOT IN (SELECT tool_call_id FROM tool_reads_file) \
         ORDER BY tool_name",
        &[],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| r.get_text(0).unwrap())
    .collect()
}

async fn tool_call_ids(s: &SqliteStore) -> Vec<String> {
    s.query(
        "SELECT tool_call_id FROM tool_call ORDER BY tool_call_id",
        &[],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| r.get_text(0).unwrap())
    .collect()
}

#[tokio::test]
async fn every_tool_call_of_a_turn_lands_its_own_row_beside_the_reads_row() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    for (tool, id) in [
        ("Bash", "toolu_01BASHA"),
        ("Read", "toolu_01READA"),
        ("Grep", "toolu_01GREPA"),
        ("Bash", "toolu_01BASHB"),
    ] {
        deliver(&s, tmp.path(), "PreToolUse", pre_tool(tool, id)).await;
    }
    let post_read = json!({
        "tool_name": "Read",
        "tool_use_id": "toolu_01READA",
        "tool_input": {"file_path": "src/lib.rs"},
    });
    deliver(&s, tmp.path(), "PostToolUse", post_read).await;

    assert_eq!(tool_ran_names(&s).await, ["Bash", "Bash", "Grep", "Read"]);
    assert_eq!(s.count("tool_reads_file").await.unwrap(), 1);
    assert_eq!(s.count("tool_call").await.unwrap(), 5);
    let native = crate::ids::tool_call_native_row_id(&session_row(), "toolu_01GREPA");
    assert!(tool_call_ids(&s).await.contains(&native));
}

#[tokio::test]
async fn a_payload_without_a_tool_use_id_keeps_todays_slot_id() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    deliver(&s, tmp.path(), "PreToolUse", json!({"tool_name": "Bash"})).await;

    let slot = crate::ids::tool_call_row_id(&session_row(), 1, 0);
    // Pinned bytes: the fallback id is the one every existing row carries.
    assert_eq!(
        slot,
        "3ddfb7b0b8f75096ab9abdcaec458ab2bd33e9e3573412fbc24325ece9444764"
    );
    assert_eq!(tool_call_ids(&s).await, [slot]);
}

#[tokio::test]
async fn a_redelivered_tool_call_is_one_row() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        deliver(
            &s,
            tmp.path(),
            "PreToolUse",
            pre_tool("Bash", "toolu_01SAME"),
        )
        .await;
    }
    assert_eq!(tool_ran_names(&s).await, ["Bash"]);
}

#[tokio::test]
async fn an_unbounded_tool_use_id_is_treated_as_absent() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    for hostile in ["toolu_\u{7}bell", "", &"x".repeat(257)] {
        deliver(&s, tmp.path(), "PreToolUse", pre_tool("Bash", hostile)).await;
    }
    let slot = crate::ids::tool_call_row_id(&session_row(), 1, 0);
    assert_eq!(tool_call_ids(&s).await, [slot]);
}
