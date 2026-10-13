//! The spilled tool outputs a checkpoint sealed, as `rift checkpoint explain`
//! names them: read off the `spills` manifest beside the session transcript
//! and beside each subagent's, so a loss the archive recorded is shown as
//! plainly as a seal.

use rift_recall_substrate::ShardWalker;

use crate::provenance::Label;

/// One spill a transcript of the checkpoint named.
pub(super) struct SpillRow {
    /// The task subtree it sits in, `None` for the session's own transcript.
    task: Option<Label>,
    tool_use_id: Label,
    /// The sealed body's size; `None` when no body was sealed.
    bytes: Option<u64>,
    /// The closed word the manifest recorded, when the spill is not whole.
    spill: Option<Label>,
}

/// The spills the checkpoint's manifests list: the session transcript's,
/// then each listed task's, in manifest order.
pub(super) fn spills_of(
    walker: &ShardWalker,
    id: &str,
    meta: Option<&serde_json::Value>,
) -> Vec<SpillRow> {
    let mut rows = rows_of(None, meta);
    let tasks = meta
        .and_then(|m| m.get("tasks"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str);
    for task in tasks {
        let doc: Option<serde_json::Value> = walker
            .read_artifact_any(id, &format!("tasks/{task}/metadata.json"))
            .ok()
            .flatten()
            .and_then(|b| serde_json::from_slice(&b).ok());
        rows.extend(rows_of(Some(task), doc.as_ref()));
    }
    rows
}

/// The rows one metadata document's manifest lists.
fn rows_of(task: Option<&str>, doc: Option<&serde_json::Value>) -> Vec<SpillRow> {
    doc.and_then(|d| d.get("spills"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let text = |k: &str| entry.get(k).and_then(serde_json::Value::as_str);
            Some(SpillRow {
                task: task.map(Label::of),
                tool_use_id: Label::of(text("tool_use_id")?),
                bytes: entry.get("bytes").and_then(serde_json::Value::as_u64),
                spill: text("spill").map(Label::of),
            })
        })
        .collect()
}

/// One line per spill: where it sits, its call, and its size or why it is
/// not whole.
pub(super) fn spill_lines(spills: &[SpillRow], human: &mut String) {
    for s in spills {
        let place = s
            .task
            .as_ref()
            .map_or_else(String::new, |t| format!("tasks/{t}/"));
        let state = match (s.bytes, &s.spill) {
            (Some(n), None) => format!("{n} bytes"),
            (Some(n), Some(why)) => format!("{n} bytes, {why}"),
            (None, Some(why)) => why.to_string(),
            (None, None) => "no body".to_owned(),
        };
        human.push_str(&format!(
            "  spill {place}tool-results/{}.txt  {state}\n",
            s.tool_use_id
        ));
    }
}

/// The document with its `spills` member, present only when a manifest
/// listed one: a checkpoint without spills renders the document it always
/// did, byte for byte.
pub(super) fn with_spills(mut json: serde_json::Value, spills: &[SpillRow]) -> serde_json::Value {
    if spills.is_empty() {
        return json;
    }
    let rows: Vec<serde_json::Value> = spills
        .iter()
        .map(|s| {
            serde_json::json!({
                "task": s.task, "tool_use_id": s.tool_use_id,
                "bytes": s.bytes, "spill": s.spill,
            })
        })
        .collect();
    if let Some(obj) = json.as_object_mut() {
        obj.insert("spills".into(), rows.into());
    }
    json
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sealed spill shows its size and a lost one its closed word, the
    /// subagent's under its subtree; the document gains the member only when
    /// there is a row.
    #[test]
    fn spills_are_named_by_place_and_state() {
        let meta = serde_json::json!({ "spills": [
            { "tool_use_id": "toolu_a", "bytes": 60000, "sha256": "x" },
            { "tool_use_id": "toolu_b", "spill": "spill_missing" },
            { "tool_use_id": "toolu_c", "bytes": 9, "spill": "spill_truncated" },
            { "bytes": 1 },
        ]});
        let mut rows = rows_of(None, Some(&meta));
        rows.extend(rows_of(
            Some("toolu_t"),
            Some(&serde_json::json!({ "spills": [{ "tool_use_id": "toolu_in" }] })),
        ));
        let mut human = String::new();
        spill_lines(&rows, &mut human);
        assert_eq!(
            human,
            "  spill tool-results/toolu_a.txt  60000 bytes\n  \
             spill tool-results/toolu_b.txt  spill_missing\n  \
             spill tool-results/toolu_c.txt  9 bytes, spill_truncated\n  \
             spill tasks/toolu_t/tool-results/toolu_in.txt  no body\n"
        );
        let json = with_spills(serde_json::json!({}), &rows);
        assert_eq!(json["spills"][1]["spill"], "spill_missing");
        assert_eq!(json["spills"][3]["task"], "toolu_t");
        assert_eq!(
            with_spills(serde_json::json!({}), &[]),
            serde_json::json!({})
        );
    }
}
