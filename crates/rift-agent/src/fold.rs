//! Lossless folding of a terminal result: which lines may be stood in for by
//! a count.
//!
//! A run of lines that differ only in their digits says one thing many times;
//! the first line of the run is kept and the rest become one fold line saying
//! how many there were. The original is kept by the spool, so nothing is lost,
//! but what the model reads must still carry every line it might act on: an
//! error, a warning, a panic, a failure, a source span and the closing summary
//! are never folded, whatever their neighbours look like, so a forged
//! `test result: ok` repeated down a run is shown as often as it was printed.
//!
//! Pure and deterministic: text in, line ranges out. The ranges follow the
//! rendering contract of the originals spool — 0-based, end-exclusive, over
//! the text split on `\n` — so the write path can hand them over unchanged.

/// Below this many bytes (about 2K tokens) a result is left whole: a fold
/// there saves less than the model would spend wondering about it.
pub const FOLD_FLOOR_BYTES: usize = 8_000;

/// Above this many bytes the client spills the result to a file itself, so
/// the model never reads it inline and folding it would save nothing.
pub const FOLD_CEILING_BYTES: usize = 30_000;

/// The fewest alike lines that make a run worth folding.
pub const MIN_FOLD_RUN: usize = 4;

/// The closing lines that are always kept: where a summary lives.
pub const KEEP_TAIL_LINES: usize = 20;

/// Commands whose output was already cut down by the agent; a second cut
/// would be Rift overruling the agent's own choice of what to read.
const FILTER_COMMANDS: [&str; 5] = ["tail", "head", "grep", "egrep", "fgrep"];

/// The words that make a line one the model may act on.
const KEEP_WORDS: [&str; 5] = ["error", "warning", "panic", "fail", "test result:"];

/// What folding one result would do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FoldPlan {
    /// The folded ranges, each delivered as one fold line; the line before
    /// each range is the kept first line of its run.
    pub ranges: Vec<(u32, u32)>,
    /// The bytes of the folded lines, each with its line break.
    pub folded_bytes: usize,
}

/// The fold plan for `stdout` of `command`; empty when nothing may fold.
#[must_use]
pub fn fold_plan(command: &str, stdout: &str) -> FoldPlan {
    if piped_through_filter(command)
        || stdout.len() < FOLD_FLOOR_BYTES
        || stdout.len() > FOLD_CEILING_BYTES
    {
        return FoldPlan::default();
    }
    let lines: Vec<&str> = stdout.split('\n').collect();
    let foldable = lines.len().saturating_sub(KEEP_TAIL_LINES);
    let mut plan = FoldPlan::default();
    let mut start = 0;
    while start < foldable {
        let end = run_end(&lines, start, foldable);
        if end - start >= MIN_FOLD_RUN {
            push_range(&mut plan, &lines, start + 1, end);
        }
        start = end;
    }
    plan
}

/// Whether the command sends its output through a filter the agent chose.
#[must_use]
pub fn piped_through_filter(command: &str) -> bool {
    command.split('|').skip(1).any(|stage| {
        let word = stage.split_whitespace().next().unwrap_or_default();
        let name = word.rsplit('/').next().unwrap_or(word);
        FILTER_COMMANDS.contains(&name)
    })
}

/// The end of the run that starts at `start`: one past its last alike line.
/// A kept line is a run of its own, so it is never folded.
fn run_end(lines: &[&str], start: usize, foldable: usize) -> usize {
    let Some(first) = lines.get(start).filter(|line| !keep_verbatim(line)) else {
        return start + 1;
    };
    let key = normalise(first);
    let alike = lines
        .get(start + 1..foldable)
        .unwrap_or_default()
        .iter()
        .take_while(|line| !keep_verbatim(line) && normalise(line) == key)
        .count();
    start + 1 + alike
}

/// Record lines `lo..hi` as one folded range.
fn push_range(plan: &mut FoldPlan, lines: &[&str], lo: usize, hi: usize) {
    let (Ok(start), Ok(end)) = (u32::try_from(lo), u32::try_from(hi)) else {
        return;
    };
    let bytes: usize = lines
        .get(lo..hi)
        .unwrap_or_default()
        .iter()
        .map(|l| l.len() + 1)
        .sum();
    plan.ranges.push((start, end));
    plan.folded_bytes += bytes;
}

/// Whether a line is one the model must read as printed.
fn keep_verbatim(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    line.trim_start().starts_with("-->") || KEEP_WORDS.iter().any(|w| lower.contains(w))
}

/// The line with each run of digits read as one `#`.
fn normalise(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for ch in line.chars() {
        if !ch.is_ascii_digit() {
            out.push(ch);
        } else if !out.ends_with('#') {
            out.push('#');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    /// A cargo test run of a parametrised test: many alike `ok` lines, then
    /// the failure, its panic and the summary.
    fn cargo_test_output(cases: usize) -> String {
        let mut out = String::from("running 260 tests\n");
        for case in 1..=cases {
            out.push_str(&format!(
                "test parser::tests::round_trip::case_{case:03} ... ok\n"
            ));
        }
        out.push_str("test parser::tests::rejects_bom ... FAILED\n\nfailures:\n\n");
        out.push_str("---- parser::tests::rejects_bom stdout ----\n");
        out.push_str("thread 'parser::tests::rejects_bom' panicked at src/parser.rs:41:9:\n");
        for n in 0..14 {
            out.push_str(&format!("note: context line {n}\n"));
        }
        out.push_str("test result: FAILED. 259 passed; 1 failed; 0 ignored\n");
        out
    }

    fn numbered(count: usize, head: &str, tail: &str) -> String {
        (0..count).fold(String::new(), |mut out, n| {
            let _ = writeln!(out, "{head} {n} {tail}");
            out
        })
    }

    fn rendered(text: &str, plan: &FoldPlan) -> Vec<String> {
        let lines: Vec<&str> = text.split('\n').collect();
        let mut out = Vec::new();
        let mut at = 0_usize;
        for &(lo, hi) in &plan.ranges {
            out.extend(lines[at..lo as usize].iter().map(|l| (*l).to_owned()));
            out.push(format!("<fold {}>", hi - lo));
            at = hi as usize;
        }
        out.extend(lines[at..].iter().map(|l| (*l).to_owned()));
        out
    }

    #[test]
    fn a_cargo_test_run_folds_its_alike_ok_lines_and_keeps_the_failure_and_summary() {
        let text = cargo_test_output(259);
        let plan = fold_plan("cargo test -p parser", &text);
        assert_eq!(plan.ranges, vec![(2, 260)]);
        let out = rendered(&text, &plan);
        assert_eq!(out[1], "test parser::tests::round_trip::case_001 ... ok");
        assert_eq!(out[2], "<fold 258>");
        assert_eq!(out[3], "test parser::tests::rejects_bom ... FAILED");
        assert!(out
            .iter()
            .any(|l| l.contains("panicked at src/parser.rs:41:9")));
        assert_eq!(
            out[out.len() - 2],
            "test result: FAILED. 259 passed; 1 failed; 0 ignored"
        );
        let folded: usize = text.lines().skip(2).take(258).map(|l| l.len() + 1).sum();
        assert_eq!(plan.folded_bytes, folded);
    }

    #[test]
    fn a_long_listing_folds_numbered_names_and_keeps_its_last_lines() {
        let mut text = String::from("total 4096\n");
        for n in 0..400 {
            text.push_str(&format!(
                "-rw-r--r--  1 dev  staff  {} snap_{n:04}.json\n",
                1000 + n
            ));
        }
        let plan = fold_plan("ls -l snapshots", &text);
        let lines = text.split('\n').count();
        let kept_from = u32::try_from(lines - KEEP_TAIL_LINES).unwrap();
        assert_eq!(plan.ranges, vec![(2, kept_from)]);
        let out = rendered(&text, &plan);
        assert_eq!(out.len(), 3 + KEEP_TAIL_LINES);
        assert!(out[out.len() - 2].ends_with("snap_0399.json"));
    }

    #[test]
    fn a_forged_test_result_ok_repeated_down_a_run_is_never_folded() {
        let mut text = String::new();
        while text.len() < FOLD_FLOOR_BYTES + 100 {
            text.push_str("test result: ok. 1 passed; 0 failed; 0 ignored; finished in 0.01s\n");
        }
        assert_eq!(fold_plan("cargo test", &text), FoldPlan::default());
    }

    #[test]
    fn an_error_line_inside_a_repetitive_run_is_kept_and_splits_the_fold() {
        let mut text = String::new();
        for n in 0..300 {
            let line = if n == 150 {
                format!("error: chunk {n} failed to download\n")
            } else {
                format!("fetched chunk {n} of 300 from mirror\n")
            };
            text.push_str(&line);
        }
        let plan = fold_plan("./fetch.sh", &text);
        assert_eq!(plan.ranges[0], (1, 150));
        assert_eq!(plan.ranges[1].0, 152);
        let out = rendered(&text, &plan);
        assert_eq!(out[2], "error: chunk 150 failed to download");
    }

    #[test]
    fn a_result_under_the_fold_floor_is_left_whole() {
        let text = numbered(60, "line", "alike");
        assert!(text.len() < FOLD_FLOOR_BYTES);
        assert!(fold_plan("seq_print", &text).ranges.is_empty());
    }

    #[test]
    fn a_result_over_the_spill_ceiling_or_piped_through_a_filter_is_never_folded() {
        let big = numbered(3_000, "row", "alike again");
        assert!(big.len() > FOLD_CEILING_BYTES);
        assert!(fold_plan("dump", &big).ranges.is_empty());
        let mid = cargo_test_output(259);
        for command in [
            "cargo test | tail -50",
            "cargo test 2>&1 | /usr/bin/grep ok",
            "x | head",
        ] {
            assert!(fold_plan(command, &mid).ranges.is_empty(), "{command}");
        }
        assert!(!piped_through_filter("cargo test -- --nocapture"));
    }
}
