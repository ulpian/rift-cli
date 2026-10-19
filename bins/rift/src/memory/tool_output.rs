//! The tool-output classes on `rift savings`: what the hook held back from a
//! repeat and what it folded, each split into what it would save and what it
//! saved.
//!
//! The split is the whole point of this module. An observing row is a
//! counterfactual, so it is published beside the share under `would_save`
//! and never added to anything. An acting row's avoided tokens are already
//! inside `share_pct`, because the share's numerator is the audit relation's
//! whole total, so they are published under `realised` to say which part of
//! the share the class is, never to add to it a second time.

use std::path::Path;

use rift_recall_ledger::savings::{tool_output_totals, ToolOutputClass, ToolOutputTotals};
use rift_store::SqliteStore;

use super::savings::SavingsReport;

/// The tool-output classes over the report's window, or `None` where the
/// recall projection cannot say.
pub(super) fn family_tool_output(report: &SavingsReport) -> Option<ToolOutputTotals> {
    let store = SqliteStore::open_read_only(Path::new(&report.recall_db)).ok()?;
    let rt = tokio::runtime::Builder::new_current_thread().build().ok()?;
    rt.block_on(tool_output_totals(&store, report.window.cut_ms()))
        .ok()
}

/// One line per class, the counterfactual first and labelled as such.
pub(super) fn render_tool_output(totals: Option<&ToolOutputTotals>) -> String {
    let Some(t) = totals else {
        return "  tool output:           not recorded by this projection\n".to_owned();
    };
    let line = |label: &str, c: &ToolOutputClass| {
        format!(
            "  {label:<22} would save on {} results ({} lines, beside the share); \
             {} acted, {} tok avoided (in the share)\n",
            c.observed, c.observed_lines, c.acted, c.avoided
        )
    };
    let mut out = line("tool output held:", &t.held);
    out.push_str(&line("tool output folded:", &t.folded));
    out
}

/// A document with the classes added under one new key, `null` where the
/// projection cannot say, so every key it already had stays as it was.
pub(super) fn with_tool_output(
    mut json: serde_json::Value,
    totals: Option<&ToolOutputTotals>,
) -> serde_json::Value {
    let Some(map) = json.as_object_mut() else {
        return json;
    };
    let value = totals.map_or(serde_json::Value::Null, |t| {
        serde_json::json!({
            "tool_output_held": class_json(&t.held),
            "tool_output_folded": class_json(&t.folded),
        })
    });
    map.insert("tool_output".to_owned(), value);
    json
}

/// One class's two halves. The would-save bytes are `null` because the audit
/// relation keeps the row and drops the figure: a zero would read as a
/// measurement.
fn class_json(c: &ToolOutputClass) -> serde_json::Value {
    serde_json::json!({
        "would_save": {
            "in_share": false,
            "results": c.observed,
            "lines": c.observed_lines,
            "bytes": serde_json::Value::Null,
        },
        "realised": {
            "in_share": true,
            "results": c.acted,
            "tokens_avoided": c.avoided,
            "tokens_returned": c.returned,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ The would-save half says it is outside the share and carries no
    /// token figure; the realised half says it is inside it; a projection
    /// that cannot say publishes `null`, never zeroes.
    #[test]
    fn the_savings_json_keeps_would_save_apart_from_realised() {
        let totals = ToolOutputTotals {
            held: ToolOutputClass {
                observed: 3,
                observed_lines: 60,
                acted: 1,
                avoided: 900,
                returned: 40,
            },
            folded: ToolOutputClass::default(),
        };
        let base = serde_json::json!({ "overall": { "share_pct": 10.0 } });
        let json = with_tool_output(base.clone(), Some(&totals));
        assert_eq!(
            json["overall"], base["overall"],
            "the headline is untouched"
        );
        let held = &json["tool_output"]["tool_output_held"];
        assert_eq!(held["would_save"]["in_share"], false);
        assert_eq!(held["would_save"]["results"], 3);
        assert_eq!(held["would_save"]["lines"], 60);
        assert!(held["would_save"]["bytes"].is_null());
        assert!(held["would_save"].get("tokens_avoided").is_none());
        assert_eq!(held["realised"]["in_share"], true);
        assert_eq!(held["realised"]["tokens_avoided"], 900);
        let folded = &json["tool_output"]["tool_output_folded"];
        assert_eq!(folded["would_save"]["results"], 0);
        assert!(with_tool_output(base, None)["tool_output"].is_null());
    }

    #[test]
    fn the_savings_lines_label_the_counterfactual_beside_the_share() {
        let totals = ToolOutputTotals::default();
        let text = render_tool_output(Some(&totals));
        assert!(
            text.contains("tool output held:      would save on 0 results"),
            "{text}"
        );
        assert!(text.contains("tool output folded:"), "{text}");
        assert!(render_tool_output(None).contains("not recorded"));
    }
}
