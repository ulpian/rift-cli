//! The output-recognition rows, as the table prints them and as the document
//! carries them.
//!
//! Its own module so the rows can be read beside the match without the
//! match's state in view. Every share is of all tool-result tokens, the
//! denominator these classes are reported against, with the `Bash`-only share
//! beside it; neither is part of the headline share.

use serde_json::{json, Map, Value};

use super::output_recognition::OutputRecognition;
use super::report::{share_percent, Report, Tally};

/// The table's rows.
pub(super) fn output_recognition_rows(report: &Report) -> String {
    let rows = &report.output_recognition;
    let all = report.all_results.tokens;
    let bash = rows.bash.tokens;
    let line = |label: &str, tally: Tally| {
        format!(
            "  {label:<56}{:>9}{:>13}{:>8.2} % of all{:>8.2} % of Bash\n",
            tally.calls,
            tally.tokens,
            share_percent(tally.tokens, all),
            share_percent(tally.tokens, bash),
        )
    };
    let mut out = String::from(
        "\n  output-side recognition (Bash; calls / tokens; a run of 5 distinct lines, a unique line, or a verified prefix)\n",
    );
    out.push_str(&line("every Bash result", rows.bash));
    out.push_str(&line(
        "  booked to a member (calls with coverage / tokens)",
        rows.covered,
    ));
    for (class, tally) in &rows.by_class {
        out.push_str(&line(&format!("    member class {class}"), *tally));
    }
    out.push_str(&line(
        "  grounded and unaligned (books nothing)",
        rows.unaligned,
    ));
    for (label, [all_calls, covered]) in class_rows(rows) {
        out.push_str(&line(label, all_calls));
        out.push_str(&line("    of it booked", covered));
    }
    out.push_str("  booked lines, by what the session tree already held:\n");
    out.push_str(&line("    first sight in this window", rows.first_sight));
    out.push_str(&line("    same-window repeat", rows.same_window));
    out.push_str(&line("    sibling re-read", rows.sibling));
    out
}

/// The same rows for the machine-readable document.
pub(super) fn output_recognition_json(report: &Report) -> Value {
    let rows = &report.output_recognition;
    let tally = |t: Tally| json!({ "calls": t.calls, "tokens": t.tokens });
    let mut classes = Map::new();
    for (class, t) in &rows.by_class {
        classes.insert((*class).to_owned(), tally(*t));
    }
    let mut map = Map::new();
    map.insert(
        "grounding_run".to_owned(),
        json!(rift_agent::window_journal::output_match::GROUNDING_RUN),
    );
    map.insert("bash".to_owned(), tally(rows.bash));
    map.insert("covered".to_owned(), tally(rows.covered));
    map.insert("by_member_class".to_owned(), Value::Object(classes));
    map.insert("unaligned".to_owned(), tally(rows.unaligned));
    for (key, [all_calls, covered]) in [
        ("no_line_count", rows.no_line_count),
        ("invisible_shell", rows.invisible),
        ("invisible_shell_named_file", rows.invisible_named),
        ("recognised", rows.recognised),
    ] {
        map.insert(
            key.to_owned(),
            json!({ "all": tally(all_calls), "covered": tally(covered) }),
        );
    }
    map.insert("first_sight".to_owned(), tally(rows.first_sight));
    map.insert("same_window".to_owned(), tally(rows.same_window));
    map.insert("sibling".to_owned(), tally(rows.sibling));
    Value::Object(map)
}

/// The four call classes, labelled.
fn class_rows(rows: &OutputRecognition) -> [(&'static str, [Tally; 2]); 4] {
    [
        (
            "  Bash in \"no name or line count for the path\"",
            rows.no_line_count,
        ),
        ("  invisible shell reads (loose)", rows.invisible),
        (
            "  invisible shell reads naming a checkout file",
            rows.invisible_named,
        ),
        ("  shell reads the grammar recognises", rows.recognised),
    ]
}
