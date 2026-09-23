//! Reading a client's recorded sessions back into replayable events.
//!
//! Nothing here decides anything: it turns the files under a project directory
//! into calls, results and window boundaries, in one order, and hands them on.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// One tool call, with the result that came back for it.
pub(super) struct Call {
    /// The tool's own name, as the client spelled it.
    pub(super) name: String,
    /// The tool's arguments.
    pub(super) input: Value,
    /// The working directory the record states, which a relative path is
    /// resolved against and against nothing else.
    pub(super) cwd: Option<String>,
    /// The result text, or `None` when the transcript holds no result.
    pub(super) result: Option<String>,
    /// The client's own result object for this call — `toolUseResult` in a
    /// transcript, `tool_response` on the wire the hook reads. It is what the
    /// delivery statement of [`stated_delivery`] is built from.
    pub(super) response: Option<Value>,
}

/// One replayable event in a window, in the order it happened.
pub(super) enum Event {
    /// A tool call.
    Call(Box<Call>),
    /// The window stopped holding what it had been given.
    Evict,
    /// A new turn started.
    Turn,
}

/// One context window's events, with the sort key that merges it with its
/// siblings into one tree-wide order.
pub(super) struct Window {
    /// The window's own events.
    pub(super) events: Vec<(String, usize, Event)>,
}

/// The text of one `tool_result` block's content, which is a string or a list
/// of typed parts.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                }
            }
            out
        }
        _ => String::new(),
    }
}

/// The blocks of one record's message, when it has any.
fn blocks(record: &Value) -> &[Value] {
    record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Whether this record is a context-window boundary the client announced.
fn is_eviction(record: &Value) -> bool {
    let field = |key: &str| record.get(key).and_then(Value::as_str);
    record.get("isCompactSummary") == Some(&Value::Bool(true))
        || (field("type") == Some("system") && field("subtype") == Some("compact_boundary"))
}

/// Whether this record is a person's own prompt, which starts a turn.
fn is_prompt(record: &Value) -> bool {
    if record.get("type").and_then(Value::as_str) != Some("user") {
        return false;
    }
    let content = record.get("message").and_then(|m| m.get("content"));
    match content {
        Some(Value::String(_)) => true,
        Some(Value::Array(parts)) => !parts
            .iter()
            .any(|part| part.get("type").and_then(Value::as_str) == Some("tool_result")),
        _ => false,
    }
}

/// Read one window file into the events it replays.
///
/// Results are collected first and then paired to their calls by id, because a
/// client may write a result many records after the call and a replay must not
/// depend on how far.
pub(super) fn load_window(path: &Path) -> Result<Window, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("replay-savings: read a transcript: {e}"))?;
    let mut records: Vec<(String, usize, Value)> = Vec::new();
    let mut last_stamp = String::new();
    for (at, row) in text.lines().enumerate() {
        let Ok(record) = serde_json::from_str::<Value>(row) else {
            continue;
        };
        if let Some(stamp) = record.get("timestamp").and_then(Value::as_str) {
            stamp.clone_into(&mut last_stamp);
        }
        records.push((last_stamp.clone(), at, record));
    }
    let mut results: BTreeMap<String, String> = BTreeMap::new();
    // The client's own object for the same call, off the record the result
    // block arrived on. It is the transcript's spelling of the field the
    // result hook reads, and it is the only place a delivery larger than
    // density can vouch for is ever stated.
    let mut responses: BTreeMap<String, Value> = BTreeMap::new();
    for (_, _, record) in &records {
        for block in blocks(record) {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
                continue;
            };
            if let Some(response) = record.get("toolUseResult") {
                responses.insert(id.to_owned(), response.clone());
            }
            let Some(content) = block.get("content") else {
                continue;
            };
            results.insert(id.to_owned(), result_text(content));
        }
    }
    let mut events = Vec::new();
    for (stamp, at, record) in &records {
        if is_eviction(record) {
            events.push((stamp.clone(), *at, Event::Evict));
            continue;
        }
        if is_prompt(record) {
            events.push((stamp.clone(), *at, Event::Turn));
            continue;
        }
        let cwd = record.get("cwd").and_then(Value::as_str).map(str::to_owned);
        for block in blocks(record) {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let (Some(name), Some(id)) = (
                block.get("name").and_then(Value::as_str),
                block.get("id").and_then(Value::as_str),
            ) else {
                continue;
            };
            events.push((
                stamp.clone(),
                *at,
                Event::Call(Box::new(Call {
                    name: name.to_owned(),
                    input: block.get("input").cloned().unwrap_or(Value::Null),
                    cwd: cwd.clone(),
                    result: results.get(id).cloned(),
                    response: responses.get(id).cloned(),
                })),
            ));
        }
    }
    Ok(Window { events })
}

/// The window files of one session tree: the main session, then each subagent.
pub(super) fn tree_windows(dir: &Path, stem: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let main = dir.join(format!("{stem}.jsonl"));
    if main.is_file() {
        out.push(main);
    }
    let subagents = dir.join(stem).join("subagents");
    if let Ok(entries) = std::fs::read_dir(&subagents) {
        let mut agents: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
            .collect();
        agents.sort();
        out.extend(agents);
    }
    out
}

/// Every session tree under a client project directory, by stem, sorted.
pub(super) fn tree_stems(dir: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("replay-savings: read {}: {e}", dir.display()))?;
    let mut stems = BTreeSet::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "jsonl") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                stems.insert(stem.to_owned());
            }
        } else if path.join("subagents").is_dir() {
            if let Some(stem) = path.file_name().and_then(|s| s.to_str()) {
                stems.insert(stem.to_owned());
            }
        }
    }
    Ok(stems.into_iter().collect())
}
