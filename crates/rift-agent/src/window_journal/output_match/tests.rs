//! The whole match, end to end, and the red-team rows: a forged prefix, a
//! line found in many files, and a short output that matches by chance.

use std::fmt::Write as _;

use super::*;
use crate::window_journal::Interval;

/// Thirty lines of a made-up source file, every one distinct.
fn source(tag: &str) -> String {
    (1..=30).fold(String::new(), |mut out, n| {
        let _ = writeln!(out, "let {tag}_{n} = compute_{tag}({n});");
        out
    })
}

fn run(lo: u32, hi: u32) -> Interval {
    Interval::new(lo, hi).unwrap()
}

/// Match `output` against `files`, each named by the member at the same
/// index, none of them a `cd`.
fn book(files: &[String], output: &str) -> Vec<Booking> {
    let hashes: Vec<Vec<u64>> = files
        .iter()
        .map(|f| file_line_hashes(f.as_bytes()))
        .collect();
    let slices: Vec<&[u64]> = hashes.iter().map(Vec::as_slice).collect();
    let named_by: Vec<Vec<usize>> = (0..files.len()).map(|i| vec![i]).collect();
    let cd = vec![false; files.len()];
    let matching = Matching {
        files: &slices,
        owners: Owners {
            named_by: &named_by,
            cd: &cd,
        },
        sole_file: (files.len() == 1).then_some(0),
    };
    let names: Vec<String> = (0..files.len()).map(|i| format!("f{i}.rs")).collect();
    match_delivery(&matching, output, &|path| {
        names.iter().position(|n| n == path)
    })
}

fn lines_of(text: &str, lo: usize, hi: usize) -> String {
    text.lines()
        .skip(lo - 1)
        .take(hi - lo + 1)
        .flat_map(|l| [l, "\n"])
        .collect()
}

/// ★ Two members' output, in order, books each member the lines it
/// printed: a plain slice and a `grep -n` hit list.
#[test]
fn a_chain_books_each_member_what_it_printed() {
    let files = [source("a"), source("b")];
    let mut output = lines_of(&files[0], 3, 12);
    output.push_str("--- separator the agent wrote ---\n");
    output.push_str("4:let b_4 = compute_b(4);\n9:let b_9 = compute_b(9);\n");
    let booked = book(&files, &output);
    assert_eq!(booked.len(), 2, "{booked:?}");
    assert_eq!(booked[0].runs, vec![run(3, 12)]);
    assert_eq!(booked[1].member, 1);
    assert_eq!(booked[1].runs, vec![run(4, 4), run(9, 9)]);
}

/// ★ A `grep -rn` line names its own file, and its content is checked
/// against that file's line, not trusted.
#[test]
fn a_printed_path_and_line_are_checked_against_the_file() {
    let files = [source("a"), source("b")];
    let booked = book(&files, "f1.rs:7:let b_7 = compute_b(7);\n");
    assert_eq!(booked.len(), 1);
    assert_eq!(
        (booked[0].file, booked[0].runs.clone()),
        (1, vec![run(7, 7)])
    );
}

/// ★ Red team: a forged prefix — the right file, the wrong line — and a
/// forged `cat -n` number book nothing.
#[test]
fn a_forged_prefix_books_nothing() {
    let files = [source("a"), source("b")];
    assert!(book(&files, "f1.rs:3:let b_7 = compute_b(7);\n").is_empty());
    assert!(book(&files, "f1.rs:7:let b_7 = compute_b(8);\n").is_empty());
    let one = [source("a")];
    assert!(book(&one, "     3\tlet a_7 = compute_a(7);\n").is_empty());
}

/// ★ Red team: a line found in many files grounds nothing alone, and a line
/// found in two is placed in neither.
#[test]
fn a_line_in_many_files_grounds_nothing_alone() {
    let common = "use std::collections::HashMap;\n";
    let many: Vec<String> = (0..60)
        .map(|i| format!("{common}{}", source(&format!("m{i}"))))
        .collect();
    assert!(book(&many, common).is_empty());
    let two = [
        format!("{common}{}", source("a")),
        format!("{common}{}", source("b")),
    ];
    assert!(book(&two, common).is_empty());
}

/// ★ Red team: a short output that matches by chance — a trivial line, or
/// four common lines in a row that also occur elsewhere — books nothing.
#[test]
fn a_short_output_that_matches_by_chance_books_nothing() {
    let shared = "    }\n    Ok(())\n}\n\n";
    let files = [
        format!("{shared}{}", source("a")),
        format!("{}{shared}", source("b")),
    ];
    assert!(book(&files, "}\n").is_empty());
    assert!(book(&files, shared).is_empty());
}

/// Output the member printed after a `cd` is the next member's; the `cd`
/// itself owns nothing.
#[test]
fn a_cd_member_owns_no_output() {
    let file = source("a");
    let hashes = file_line_hashes(file.as_bytes());
    let slices = [hashes.as_slice()];
    let named_by = [vec![0]];
    let matching = Matching {
        files: &slices,
        owners: Owners {
            named_by: &named_by,
            cd: &[true],
        },
        sole_file: None,
    };
    let output = lines_of(&file, 1, 10);
    assert!(match_delivery(&matching, &output, &|_| None).is_empty());
}

/// A unified diff's body lines are read through their markers.
#[test]
fn a_diff_body_is_matched_through_its_markers() {
    let files = [source("a")];
    let body: String = lines_of(&files[0], 5, 11)
        .lines()
        .flat_map(|l| [" ", l, "\n"])
        .collect();
    let output = format!("--- a/f0.rs\n+++ b/f0.rs\n@@ -5,7 +5,7 @@\n{body}");
    let booked = book(&files, &output);
    assert_eq!(booked[0].runs, vec![run(5, 11)]);
}
