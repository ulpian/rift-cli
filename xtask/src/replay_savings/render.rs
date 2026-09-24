//! The two surfaces a replay is read through: the human table and the
//! machine-readable document.
//!
//! Aggregates only. No path, no prompt, no command line and no result text
//! reaches either string, which is a property a test asserts.

use rift_agent::window_journal::Limits;
use serde_json::{json, Map, Value};

use super::bucket::Bucket;
use super::report::{share_percent, Report, Tally};
use super::worktree::MAX_INLINE_HASH_BYTES;

/// The version of the machine-readable document this command writes. A later
/// shape gets a later number rather than a reader that tolerates both.
pub const SCHEMA_VERSION: u32 = 2;

/// A number with thousands separators, so a seven-figure token count can be
/// read at a glance.
fn commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (at, ch) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// One line of the table: a label, a call count, a token count and a share.
fn row(label: &str, tally: Tally, denominator: u64) -> String {
    format!(
        "  {label:<48}{:>9}{:>15}{:>11.2} %\n",
        commas(tally.calls),
        commas(tally.tokens),
        share_percent(tally.tokens, denominator)
    )
}

/// The human table.
///
/// Aggregates only: no path, no prompt, no command line and no result text
/// reaches this string.
#[must_use]
pub fn render_table(report: &Report) -> String {
    let denominator = report.repo_content.tokens;
    let mut out = String::new();
    out.push_str(&the_corpus(report));
    out.push_str(&the_buckets(report, denominator));
    out.push_str(&what_the_share_is(report, denominator));
    out.push_str(&the_two_levers(report));
    out.push_str(&what_is_left(report, denominator));
    out.push_str(&the_constants(report));
    out.push_str(READING_IT);
    out
}

/// How to read the share, which is the one sentence of the table that is an
/// argument rather than a figure.
const READING_IT: &str =
    "\n  Reading it: the declined share is an estimated upper bound on what this\n  \
     build would have kept out of context, never a claim that the reads would\n  \
     have happened. Searches are in the denominator and the journal never\n  \
     declines one, so the read-only share is the larger of the two rates.\n";

/// What was replayed: the corpus, and the ceiling every other figure is under.
fn the_corpus(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("replay-savings — what this build's read gate would have decided\n");
    out.push_str(&format!(
        "  schema {SCHEMA_VERSION} · tokenizer {} · aggregates only\n\n",
        report.tokenizer
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "session trees",
        commas(report.trees)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "context windows",
        commas(report.windows)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "tool calls replayed",
        commas(report.tool_calls)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "every tool result (informational ceiling)",
        commas(report.all_results.calls),
        commas(report.all_results.tokens)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "revisions advanced / windows emptied",
        commas(report.edits),
        commas(report.evictions)
    ));
    out
}

/// Every bucket that caught a call, and the two channels they add up to.
///
/// A bucket no call landed in is left out rather than printed as a zero: the
/// table is what happened, and the document below publishes every key.
fn the_buckets(report: &Report, denominator: u64) -> String {
    let mut out = String::new();
    out.push_str("\n  repository-content calls, one bucket each");
    out.push_str(&format!(
        "\n  {:<48}{:>9}{:>15}{:>13}\n",
        "", "calls", "tokens", "of content"
    ));
    for bucket in Bucket::ALL {
        let tally = report.bucket(bucket);
        if tally.calls == 0 {
            continue;
        }
        out.push_str(&row(bucket.label(), tally, denominator));
    }
    out.push_str(&row("read channels", report.reads, denominator));
    out.push_str(&row("search channel", report.searches, denominator));
    out.push_str(&row(
        "repository content (the denominator)",
        report.repo_content,
        denominator,
    ));
    out
}

/// What the journal found held, what this build would have acted on, and what
/// the acting costs.
fn what_the_share_is(report: &Report, denominator: u64) -> String {
    let mut out = String::new();
    out.push_str("\n  what the share is\n");
    out.push_str(&row(
        "declinable by the journal",
        report.declinable,
        denominator,
    ));
    out.push_str(&row("declined by this build", report.declined, denominator));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}{:>11.2} %\n",
        "declined, net of the reason frames",
        commas(report.declined.calls),
        commas(report.declined_net_tokens()),
        share_percent(report.declined_net_tokens(), denominator)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "reason frames cost",
        "",
        commas(report.frame_tokens)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "declines resting on the window's own write",
        commas(report.declines_resting_on_a_write)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "declines lost to an unheld sibling read",
        commas(report.declines_lost_to_a_sibling)
    ));
    out
}

/// The client's own statement, and the calls that are more than one read.
fn the_two_levers(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("\n  the two levers a replay has to model on purpose\n");
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "results with a response object / stating a delivery",
        commas(report.results_with_a_response),
        commas(report.statements_read)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "reads credited from the client's statement",
        commas(report.deliveries_credited),
        commas(report.bucket(Bucket::CreditedFromStatement).tokens)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "deliveries the statement took back",
        commas(report.deliveries_taken_back)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "calls performing several reads / members",
        commas(report.calls_with_several_reads),
        commas(report.read_members)
    ));
    out
}

/// What no decline could have reached, and the bounds on what the harness
/// cannot see.
fn what_is_left(report: &Report, denominator: u64) -> String {
    let mut out = String::new();
    out.push_str("\n  what is left, and why\n");
    out.push_str(&row(
        "first-read floor (never declinable)",
        report.first_read_floor,
        denominator,
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}{:>11.2} %\n",
        "  of it, read again by a sibling window",
        commas(report.cross_window_pairs),
        commas(report.cross_window.tokens),
        share_percent(report.cross_window.tokens, denominator)
    ));
    out.push_str(&row(
        "reads after a non-write edit (not modelled)",
        report.verification_after_edit,
        denominator,
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "invisible shell reads, loose bound",
        commas(report.invisible_shell.calls),
        commas(report.invisible_shell.tokens)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "  of them, naming a file this checkout has",
        commas(report.invisible_shell_named_file.calls),
        commas(report.invisible_shell_named_file.tokens)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "shell calls / of them recognised by the grammar",
        commas(report.shell_calls),
        commas(report.shell_recognised)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "reads outside the work tree (out of scope)",
        commas(report.outside_work_tree.calls),
        commas(report.outside_work_tree.tokens)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "unmeasured remainder (every other tool)",
        "",
        commas(report.remainder_tokens())
    ));
    out
}

/// The numbers the decision was taken with, so a reader can check them.
fn the_constants(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("\n  the constants this build decided with\n");
    for (name, value) in constants() {
        out.push_str(&format!("  {name:<48}{value:>9}\n"));
    }
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "other tool names seen",
        commas(report.other_tools.len() as u64)
    ));
    out
}

/// The numbers the decision was taken with, as a list a reader can check.
fn constants() -> Vec<(&'static str, String)> {
    let limits = Limits::default();
    vec![
        (
            "max slice bytes (decline ceiling)",
            limits.max_slice_bytes.to_string(),
        ),
        (
            "min slice lines (decline floor)",
            limits.min_slice_lines.to_string(),
        ),
        (
            "max delivered bytes, read tool",
            limits.max_delivered_bytes.to_string(),
        ),
        (
            "max delivered bytes, terminal",
            limits.max_shell_delivered_bytes.to_string(),
        ),
        (
            "context lines around a changed site",
            limits.context_lines.to_string(),
        ),
        ("max inline hash bytes", MAX_INLINE_HASH_BYTES.to_string()),
        ("mode", "enforce".to_owned()),
        ("edit echo", "pinned".to_owned()),
    ]
}

/// One tally as the document spells it.
fn tally_json(tally: Tally) -> Value {
    json!({ "calls": tally.calls, "tokens": tally.tokens })
}

/// Every bucket, published whether or not a call landed in it: a key that
/// disappears when a bucket is empty is a key no script can depend on.
fn buckets_json(report: &Report, denominator: u64) -> Value {
    let mut buckets = Map::new();
    for bucket in Bucket::ALL {
        let t = report.bucket(bucket);
        buckets.insert(
            bucket.key().to_owned(),
            json!({
                "calls": t.calls,
                "declinable": bucket.declinable(),
                "share_percent_of_repository_content": share_percent(t.tokens, denominator),
                "tokens": t.tokens,
            }),
        );
    }
    Value::Object(buckets)
}

/// The constants, under keys made from their table names.
fn constants_json() -> Value {
    let mut constants_map = Map::new();
    for (name, value) in constants() {
        constants_map.insert(
            name.replace(' ', "_").replace([',', '(', ')'], ""),
            json!(value),
        );
    }
    Value::Object(constants_map)
}

/// The tool names this replay had nothing else to say about.
fn other_tools_json(report: &Report) -> Value {
    let mut others = Map::new();
    for (name, count) in &report.other_tools {
        others.insert(name.clone(), json!(count));
    }
    Value::Object(others)
}

/// The four rates, each named for its numerator and its denominator.
fn shares_json(report: &Report, denominator: u64) -> Value {
    json!({
        "declinable_of_repository_content": share_percent(
            report.declinable.tokens, denominator),
        "declined_net_of_frames_of_reads": share_percent(
            report.declined_net_tokens(), report.reads.tokens),
        "declined_net_of_frames_of_repository_content": share_percent(
            report.declined_net_tokens(), denominator),
        "declined_of_repository_content": share_percent(
            report.declined.tokens, denominator),
    })
}

/// The machine-readable document: sorted keys, versioned, no free text from
/// the corpus anywhere in it.
#[must_use]
pub fn render_json(report: &Report) -> String {
    let denominator = report.repo_content.tokens;
    let document = json!({
        "buckets": buckets_json(report, denominator),
        "constants": constants_json(),
        "corpus": {
            "context_windows": report.windows,
            "revisions_advanced": report.edits,
            "calls_performing_several_reads": report.calls_with_several_reads,
            "context_windows_read_members": report.read_members,
            "session_trees": report.trees,
            "shell_calls": report.shell_calls,
            "shell_calls_the_grammar_recognised": report.shell_recognised,
            "tool_calls_replayed": report.tool_calls,
            "windows_emptied": report.evictions,
        },
        "delivery_statement": {
            "credited": report.deliveries_credited,
            "credited_tokens": report.bucket(Bucket::CreditedFromStatement).tokens,
            "results_with_a_response_object": report.results_with_a_response,
            "results_stating_a_delivery": report.statements_read,
            "taken_back": report.deliveries_taken_back,
        },
        "cross_window_repeats": {
            "pairs": report.cross_window_pairs,
            "tokens": report.cross_window.tokens,
        },
        "declined": {
            "calls": report.declined.calls,
            "frame_tokens": report.frame_tokens,
            "net_tokens": report.declined_net_tokens(),
            "lost_to_an_unheld_sibling": report.declines_lost_to_a_sibling,
            "resting_on_a_write": report.declines_resting_on_a_write,
            "tokens": report.declined.tokens,
        },
        "declinable": tally_json(report.declinable),
        "first_read_floor": tally_json(report.first_read_floor),
        "invisible_shell_reads": {
            "loose_bound": tally_json(report.invisible_shell),
            "naming_a_work_tree_file": tally_json(report.invisible_shell_named_file),
        },
        "other_tools": other_tools_json(report),
        "outside_the_work_tree": tally_json(report.outside_work_tree),
        "reads_after_a_non_write_edit": tally_json(report.verification_after_edit),
        "repository_content": tally_json(report.repo_content),
        "repository_content_reads": tally_json(report.reads),
        "repository_content_searches": tally_json(report.searches),
        "schema_version": SCHEMA_VERSION,
        "share_estimated_upper_bound_percent": shares_json(report, denominator),
        "tokenizer": report.tokenizer,
        "tool_results_all": tally_json(report.all_results),
        "unmeasured_remainder_tokens": report.remainder_tokens(),
    });
    serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::super::replay::replay;
    use super::super::tests::{fixture, SENTINEL};
    use super::{commas, render_json, render_table, SCHEMA_VERSION};

    #[test]
    fn no_prompt_text_reaches_the_output() {
        let tmp = tempfile::tempdir().unwrap();
        let options = fixture(tmp.path());
        let corpus = std::fs::read_to_string(options.transcripts.join("sess-0001.jsonl")).unwrap();
        assert!(
            corpus.contains(SENTINEL),
            "the fixture carries the sentinel"
        );
        let report = replay(&options).unwrap();
        let table = render_table(&report);
        let document = render_json(&report);
        for rendered in [&table, &document] {
            assert!(!rendered.contains(SENTINEL), "a prompt reached the output");
            assert!(!rendered.contains("lib.rs"), "a path reached the output");
            assert!(!rendered.contains("notes.md"), "a path reached the output");
            assert!(
                !rendered.contains("/repo"),
                "a directory reached the output"
            );
        }
    }

    #[test]
    fn the_document_is_stable_and_versioned() {
        let tmp = tempfile::tempdir().unwrap();
        let options = fixture(tmp.path());
        let first = render_json(&replay(&options).unwrap());
        let second = render_json(&replay(&options).unwrap());
        assert_eq!(first, second, "two replays of one corpus agree");
        let parsed: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(parsed["schema_version"], json!(SCHEMA_VERSION));
        // serde_json's map is ordered, so the document's keys are sorted as
        // written: assert the property a script would depend on.
        let keys: Vec<&String> = parsed.as_object().unwrap().keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "top-level keys are sorted");
    }

    #[test]
    fn a_number_is_readable() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_234_567), "1,234,567");
    }
}
