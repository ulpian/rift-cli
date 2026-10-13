//! The originals of rewritten results a checkpoint sealed, as `rift
//! checkpoint explain` names them: read off the `originals` manifest beside
//! the session transcript and beside each subagent's, joined to the call by
//! its id, so an orphan or an unsealed original is shown as plainly as a seal.

use rift_recall_substrate::ShardWalker;

use crate::provenance::Label;

/// One original a transcript of the checkpoint carries.
pub(super) struct OriginalRow {
    /// The task subtree it sits in, `None` for the session's own transcript.
    task: Option<Label>,
    tool_use_id: Label,
    /// The rewrite's form, when the record named one.
    form: Option<Label>,
    /// What the client recorded (`spooled_not_delivered`, or the form).
    rewritten: Option<Label>,
    /// The sealed body's size; `None` when no body was sealed.
    bytes: Option<u64>,
    /// The closed word the manifest recorded, when the original is not whole.
    original: Option<Label>,
}

/// The originals the checkpoint's manifests list: the session transcript's,
/// then each listed task's, in manifest order.
pub(super) fn originals_of(
    walker: &ShardWalker,
    id: &str,
    meta: Option<&serde_json::Value>,
) -> Vec<OriginalRow> {
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
fn rows_of(task: Option<&str>, doc: Option<&serde_json::Value>) -> Vec<OriginalRow> {
    doc.and_then(|d| d.get("originals"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let text = |k: &str| {
                entry
                    .get(k)
                    .and_then(serde_json::Value::as_str)
                    .map(Label::of)
            };
            Some(OriginalRow {
                task: task.map(Label::of),
                tool_use_id: text("tool_use_id")?,
                form: text("form"),
                rewritten: text("rewritten"),
                bytes: entry.get("bytes").and_then(serde_json::Value::as_u64),
                original: text("original"),
            })
        })
        .collect()
}

/// One line per original: where it sits, its call, what was delivered, and
/// its size or why it is not whole.
pub(super) fn original_lines(rows: &[OriginalRow], human: &mut String) {
    for r in rows {
        let place = r
            .task
            .as_ref()
            .map_or_else(String::new, |t| format!("tasks/{t}/"));
        let mut words: Vec<String> = Vec::new();
        words.extend(
            r.rewritten
                .as_ref()
                .or(r.form.as_ref())
                .map(ToString::to_string),
        );
        words.extend(r.bytes.map(|n| format!("{n} bytes")));
        words.extend(r.original.as_ref().map(ToString::to_string));
        if r.bytes.is_none() && r.original.is_none() {
            words.push("no body".to_owned());
        }
        human.push_str(&format!(
            "  original {place}tool-originals/{}.json  {}\n",
            r.tool_use_id,
            words.join(", ")
        ));
    }
}

/// The document with its `originals` member, present only when a manifest
/// listed one: a checkpoint without a rewrite renders as it always did.
pub(super) fn with_originals(
    mut json: serde_json::Value,
    rows: &[OriginalRow],
) -> serde_json::Value {
    if rows.is_empty() {
        return json;
    }
    let rows: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "task": r.task, "tool_use_id": r.tool_use_id, "form": r.form,
                "rewritten": r.rewritten, "bytes": r.bytes, "original": r.original,
            })
        })
        .collect();
    if let Some(obj) = json.as_object_mut() {
        obj.insert("originals".into(), rows.into());
    }
    json
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sealed original shows what was delivered and its size, an orphan its
    /// closed word, the subagent's under its subtree; the document gains the
    /// member only when there is a row.
    #[test]
    fn originals_are_named_by_place_delivery_and_state() {
        let meta = serde_json::json!({ "originals": [
            { "tool_use_id": "toolu_a", "form": "folded_output", "rewritten": "folded_output", "bytes": 140, "sha256": "x" },
            { "tool_use_id": "toolu_b", "form": "folded_output", "bytes": 9, "original": "original_line_absent" },
            { "tool_use_id": "toolu_c", "form": "folded_output", "rewritten": "spooled_not_delivered", "bytes": 3 },
            { "bytes": 1 },
        ]});
        let mut rows = rows_of(None, Some(&meta));
        rows.extend(rows_of(
            Some("toolu_t"),
            Some(&serde_json::json!({ "originals": [{ "tool_use_id": "toolu_in", "original": "original_unsealed" }] })),
        ));
        let mut human = String::new();
        original_lines(&rows, &mut human);
        assert_eq!(
            human,
            "  original tool-originals/toolu_a.json  folded_output, 140 bytes\n  \
             original tool-originals/toolu_b.json  folded_output, 9 bytes, original_line_absent\n  \
             original tool-originals/toolu_c.json  spooled_not_delivered, 3 bytes\n  \
             original tasks/toolu_t/tool-originals/toolu_in.json  original_unsealed\n"
        );
        let json = with_originals(serde_json::json!({}), &rows);
        assert_eq!(json["originals"][2]["rewritten"], "spooled_not_delivered");
        assert_eq!(json["originals"][3]["task"], "toolu_t");
        assert_eq!(
            with_originals(serde_json::json!({}), &[]),
            serde_json::json!({})
        );
    }
}
