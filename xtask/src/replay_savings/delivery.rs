//! What one result states about the delivery it answered.
//!
//! A restatement of the binary's own rule, kept apart from the replay so the
//! two can be read side by side.

use rift_agent::window_journal::{Interval, StatedDelivery};
use serde_json::Value;

/// The marker a Claude Code result carries when the client wrote the output to
/// a file and handed the window a pointer and a short preview instead.
///
/// It is the exact string the client writes, and it is the one unambiguous
/// sighting of the case the shell delivery ceiling exists for: the command ran,
/// the lines exist, and what reached the context is a filename.
pub(super) const PERSISTED_OUTPUT: &str = "<persisted-output>";

/// The field a Claude Code `Read` result states its own truncation in, and the
/// fields that say how much of the file it handed over. Spelled here exactly as
/// `bins/rift/src/hooks.rs` spells them, from the same pinned observations of
/// `crates/rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md`.
const TRUNCATED_BY_TOKEN_CAP: &str = "truncatedByTokenCap";
/// The first line handed over, when the result names it.
const DELIVERY_START_LINE: &str = "startLine";
/// How many lines followed it.
const DELIVERY_NUM_LINES: &str = "numLines";
/// How many lines the file has, as the client counts them.
const DELIVERY_TOTAL_LINES: &str = "totalLines";
/// The same truncation fact as a sentence, for a result that carries text.
const TRUNCATION_NOTICE: &str = "[Truncated: PARTIAL view";
/// The clause of that sentence carrying the numbers, spelled `A-B`.
const TRUNCATION_NOTICE_LINES: &str = "showing lines ";
/// The field a command result names the file its output was kept in.
const PERSISTED_OUTPUT_PATH: &str = "persistedOutputPath";

/// What one result states about the delivery it answered.
///
/// **A restatement, not a second opinion.** The rule lives in
/// `bins/rift/src/hooks.rs::stated_delivery`, which is private to the binary
/// and reads a hook payload rather than a transcript record, so a replay
/// cannot call it. Every clause below is the same clause in the same order,
/// against the same field names, and the two are meant to be read side by
/// side: a cut-short reading first, so a result that both sets the flag and
/// carries a contradictory count is read the way that can only cost a saving;
/// a complete reading only where the client says so outright or the run
/// reaches the client's own total; and silence answering `None`, never
/// "complete".
pub(super) fn stated_delivery(response: &Value) -> Option<StatedDelivery> {
    // A command's output is not cut short in place the way a read is: past a
    // threshold in characters the client writes the whole of it to a file and
    // hands the model the path, so nothing states how far the output got.
    let persisted = response
        .get(PERSISTED_OUTPUT_PATH)
        .and_then(Value::as_str)
        .is_some_and(|path| !path.is_empty())
        || response
            .as_str()
            .is_some_and(|text| text.starts_with(PERSISTED_OUTPUT));
    if persisted {
        return Some(StatedDelivery::Unstated);
    }
    let file = response.get("file");
    let stated =
        |file: &Value, name: &str| -> Option<u32> { u32::try_from(file.get(name)?.as_u64()?).ok() };
    let flag = file
        .and_then(|file| file.get(TRUNCATED_BY_TOKEN_CAP))
        .and_then(Value::as_bool);
    let notice = response
        .as_str()
        .or_else(|| response.get("content")?.as_str())
        .filter(|text| text.contains(TRUNCATION_NOTICE));
    let from_object = file.and_then(|file| {
        let lines = stated(file, DELIVERY_NUM_LINES)?;
        let lo = stated(file, DELIVERY_START_LINE).unwrap_or(1);
        Interval::new(lo, lo.checked_add(lines.checked_sub(1)?)?)
    });
    if flag == Some(true) || notice.is_some() {
        let run = from_object.or_else(|| notice.and_then(notice_lines));
        return Some(run.map_or(StatedDelivery::Unstated, StatedDelivery::Truncated));
    }
    let run = from_object?;
    let reaches_the_end = file
        .and_then(|file| stated(file, DELIVERY_TOTAL_LINES))
        .is_some_and(|total| run.hi >= total);
    (flag == Some(false) || reaches_the_end).then_some(StatedDelivery::Complete(run))
}

/// The run of lines a truncation notice names, from its `showing lines A-B`
/// clause. Anything it cannot read outright is no run at all rather than a
/// guess.
fn notice_lines(notice: &str) -> Option<Interval> {
    let clause = notice.split_once(TRUNCATION_NOTICE_LINES)?.1;
    let digits = |text: &str| text.trim().parse::<u32>().ok();
    let (lo, rest) = clause.split_once('-')?;
    let hi = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    Interval::new(digits(lo)?, digits(hi)?)
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::{notice_lines, stated_delivery, Interval, StatedDelivery, PERSISTED_OUTPUT};

    /// The client's own statement, read the way `hooks.rs::stated_delivery`
    /// reads it: silence is not "complete", the cut-short reading wins a
    /// contradiction, and a run that stops short of the file with no flag
    /// beside it says nothing at all.
    #[test]
    fn a_delivery_statement_is_read_the_way_the_gate_reads_it() {
        let file = |body: Value| json!({ "type": "text", "file": body });
        assert_eq!(
            stated_delivery(&file(
                json!({"startLine": 1, "numLines": 574, "totalLines": 574})
            )),
            Some(StatedDelivery::Complete(Interval::new(1, 574).unwrap())),
            "a run reaching the client's own total is a complete delivery"
        );
        assert_eq!(
            stated_delivery(&file(
                json!({"startLine": 1, "numLines": 230, "totalLines": 275})
            )),
            None,
            "short of the total with no flag says nothing"
        );
        assert_eq!(
            stated_delivery(&file(json!({
                "startLine": 1, "numLines": 40, "totalLines": 120, "truncatedByTokenCap": true
            }))),
            Some(StatedDelivery::Truncated(Interval::new(1, 40).unwrap())),
            "the flag names how far it got"
        );
        assert_eq!(
            stated_delivery(&file(
                json!({"startLine": 1, "numLines": 120, "totalLines": 120,
                       "truncatedByTokenCap": true})
            )),
            Some(StatedDelivery::Truncated(Interval::new(1, 120).unwrap())),
            "a contradiction is read the way that can only cost a saving"
        );
        assert_eq!(
            stated_delivery(&json!({"truncatedByTokenCap": true})),
            None,
            "a statement with no file object is no statement"
        );
        assert_eq!(
            stated_delivery(&json!({"persistedOutputPath": "/tmp/out.txt"})),
            Some(StatedDelivery::Unstated),
            "output kept in a file states nothing about lines"
        );
        assert_eq!(
            stated_delivery(&Value::String(format!("{PERSISTED_OUTPUT}\nPreview:"))),
            Some(StatedDelivery::Unstated),
            "the same fact as the envelope the model reads"
        );
        assert_eq!(
            stated_delivery(&Value::String(
                "[Truncated: PARTIAL view showing lines 5-44 of 900]".to_owned()
            )),
            Some(StatedDelivery::Truncated(Interval::new(5, 44).unwrap())),
            "the sentence carries the numbers where the object does not"
        );
        assert_eq!(
            notice_lines("showing lines 12-"),
            None,
            "half a clause is none"
        );
    }
}
