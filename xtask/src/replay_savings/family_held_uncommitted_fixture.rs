//! The family-held share of the unchanged re-reads of files no commit holds,
//! pinned over two windows of one session tree reading a file that no commit
//! and no checkout holds.

use std::path::Path;

use serde_json::{json, Value};

use super::render::{render_json, render_table};
use super::replay::replay;
use super::tests::fixture;

const FILE: &str = "/repo/scratch/u.md";

fn lines() -> String {
    (1..=30)
        .map(|i| format!("{i:>6}\u{2192}line {i} of plain prose text"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One whole read of the file at `clock`, as two records.
fn whole_read(id: &str, clock: &str) -> Vec<String> {
    let call = json!({"type": "assistant", "message": {"role": "assistant", "content": [
        {"type": "tool_use", "id": id, "name": "Read", "input": {"file_path": FILE}}
    ]}});
    let result = json!({"type": "user", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": id, "content": lines()}
    ]}});
    [call, result]
        .into_iter()
        .map(|mut record| {
            record["cwd"] = json!("/repo");
            record["timestamp"] = json!(format!("2026-03-01T{clock}.000Z"));
            record.to_string()
        })
        .collect()
}

/// Replay a session whose main window and one subagent each read the file
/// whole, at the clock times given, returning the document.
fn replayed(main_at: &str, agent_at: &str) -> (Value, String) {
    let tmp = tempfile::tempdir().unwrap();
    let transcripts = tmp.path().join("transcripts");
    let agents = transcripts.join("sess-family").join("subagents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        transcripts.join("sess-family.jsonl"),
        whole_read("m1", main_at).join("\n"),
    )
    .unwrap();
    std::fs::write(
        agents.join("agent-a.jsonl"),
        whole_read("a1", agent_at).join("\n"),
    )
    .unwrap();
    let mut opts = fixture(tmp.path());
    opts.transcripts = Path::new(&transcripts).to_path_buf();
    let report = replay(&opts).unwrap();
    let doc = serde_json::from_str(&render_json(&report)).unwrap();
    (doc, render_table(&report))
}

fn calls(doc: &Value, path: &[&str]) -> u64 {
    let mut at = &doc["family_held_uncommitted"];
    for key in path {
        at = &at[*key];
    }
    at["calls"].as_u64().unwrap_or(u64::MAX)
}

/// ★ A subagent's first read of bytes the live session window read whole is a
/// family-held case on the working-tree model, and the family-held rows,
/// which need a resolved line count, do not count it.
#[test]
fn a_first_read_of_uncommitted_bytes_a_live_window_held_is_a_case() {
    let (doc, table) = replayed("00:00:01", "00:00:10");
    assert_eq!(calls(&doc, &["first_reads"]), 1, "{doc}");
    assert_eq!(calls(&doc, &["cases"]), 1, "{doc}");
    assert_eq!(calls(&doc, &["counted_by_family_held_rows"]), 0, "{doc}");
    let today: u64 = ["outline", "held", "runs"]
        .iter()
        .map(|route| doc["family_held"][route]["calls"].as_u64().unwrap())
        .sum();
    assert_eq!(today, 0, "no resolved line count, so no case today: {doc}");
    assert!(
        table.contains("files no commit holds: first reads of bytes another window held"),
        "{table}"
    );
}

/// A subagent holder whose journal has been idle past the floor holds
/// nothing a decline may rest on; one within the floor does.
#[test]
fn a_quiet_subagent_holder_of_uncommitted_bytes_is_no_case() {
    let (quiet, _) = replayed("00:06:00", "00:00:01");
    assert_eq!(calls(&quiet, &["first_reads"]), 1, "{quiet}");
    assert_eq!(calls(&quiet, &["cases"]), 0, "{quiet}");
    assert_eq!(
        calls(&quiet, &["no_case", "quiet_subagent_holder"]),
        1,
        "{quiet}"
    );
    let (live, _) = replayed("00:04:00", "00:00:01");
    assert_eq!(calls(&live, &["cases"]), 1, "{live}");
}
