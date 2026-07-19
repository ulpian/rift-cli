//! Process-level MCP audit test: real rmcp stdio, real daemon, real sibling hook.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use rift_store::CanonicalStore;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::ServiceExt;

const MEMD: &str = env!("CARGO_BIN_EXE_rift-memd");

fn sibling_rift(memd: &Path) -> PathBuf {
    memd.parent()
        .expect("rift-memd binary has a parent")
        .join(format!("rift{}", std::env::consts::EXE_SUFFIX))
}

#[tokio::test]
async fn real_stdio_calls_persist_one_thread_and_distinct_call_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&repo).unwrap();

    // The production handoff resolves `rift` beside `rift-memd`. The workspace
    // gate builds all targets before running tests, so this is the real hook
    // binary rather than a fixture or PATH shim.
    let rift = sibling_rift(Path::new(MEMD));
    assert!(
        rift.is_file(),
        "process test requires the workspace sibling binary at {}",
        rift.display()
    );

    let transport = TokioChildProcess::new(tokio::process::Command::new(MEMD).configure(|cmd| {
        cmd.args(["serve", "--mcp", "--repo"])
            .arg(&repo)
            .current_dir(&repo)
            .env("RIFT_CACHE_DIR", &cache);
    }))
    .unwrap();
    let client = ().serve(transport).await.unwrap();

    let mut arguments = serde_json::Map::new();
    arguments.insert("task".into(), "PRIVATE_ARGUMENT_SENTINEL".into());
    arguments.insert(
        "files_in_focus".into(),
        serde_json::json!(["PRIVATE_ARGUMENT_SENTINEL.rs"]),
    );
    for _ in 0..2 {
        client
            .call_tool(CallToolRequestParams {
                meta: None,
                name: "rift_known_state".into(),
                arguments: Some(arguments.clone()),
                task: None,
            })
            .await
            .unwrap();
    }
    client.cancel().await.unwrap();

    let repo_id = rift_store::recall_repo_id(&repo).unwrap();
    let db = rift_store::recall_db_under(&cache, &repo_id);
    let mut rows = Vec::new();
    // The audit subprocess is intentionally detached from the response path;
    // give both child handoffs a short bounded opportunity to commit.
    for _ in 0..100 {
        if db.is_file() {
            let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
            rows = store
                .query(
                    "SELECT call_id, thread_id, tool_name, terminal_verb, schema_version FROM memd_call_event ORDER BY call_id",
                    &[],
                )
                .await
                .unwrap();
            if rows.len() == 2 {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(rows.len(), 2, "one terminal row per decoded invocation");
    let [first, second] = rows.as_slice() else {
        panic!("expected exactly two persisted rows");
    };
    let first_call = first.get_text(0).unwrap();
    let second_call = second.get_text(0).unwrap();
    assert_ne!(first_call, second_call, "identical calls remain distinct");
    assert_eq!(first.get_text(1).unwrap(), second.get_text(1).unwrap());
    for row in &rows {
        assert_eq!(row.get_text(2).unwrap(), "rift_known_state");
        assert!(row.get_text(3).unwrap().starts_with("memd."));
        assert_eq!(row.get_i64(4).unwrap(), 2);
        for index in 0..4 {
            assert!(!row
                .get_text(index)
                .unwrap()
                .contains("PRIVATE_ARGUMENT_SENTINEL"));
        }
    }
}
