//! The one `awk` program the grammar reads: a line pick, `NR>=A && NR<=B`,
//! which prints what `sed -n 'A,Bp'` prints and is credited the same.

use crate::shell_read::{recognise, Narrowing, ShellCall, ShellSpan};
use crate::{HiKind, ReadSpan};

use super::{assert_each_recognised, narrowed, read, read_of};

/// **A line pick written in `awk` is the `sed` range it prints.**
///
/// A pattern with no action prints each record it matches, unchanged, and
/// this pattern matches records `A` to `B`, so the lines delivered follow
/// from the line alone: the same span as `sed -n 'A,Bp'`, with the same
/// coverage, under a `cd`, after a label and narrowed by a pipe alike.
#[test]
fn a_line_pick_written_in_awk_reads_as_the_sed_range() {
    let lines = ShellSpan::Lines { lo: 60, hi: 110 };
    assert_each_recognised(&[
        ("awk 'NR>=60 && NR<=110' a.rs", read("a.rs", lines)),
        ("awk 'NR>=60&&NR<=110' a.rs", read("a.rs", lines)),
        ("awk \"NR >= 60 && NR <= 110\" a.rs", read("a.rs", lines)),
        ("/usr/bin/awk 'NR>=60 && NR<=110' a.rs", read("a.rs", lines)),
        (
            "cd crates && echo '== a ==' && awk 'NR>=60 && NR<=110' a.rs",
            read("crates/a.rs", lines),
        ),
        (
            "awk 'NR>=60 && NR<=110' a.rs | head -n 5",
            ShellCall::Read(vec![narrowed("a.rs", lines, Narrowing::First(5))]),
        ),
        (
            "awk 'NR>=7 && NR<=7' a.rs",
            read("a.rs", ShellSpan::Lines { lo: 7, hi: 7 }),
        ),
    ]);
    assert_eq!(
        read_of("awk 'NR>=60 && NR<=110' a.rs"),
        Some((
            "a.rs".to_owned(),
            ReadSpan::lines(60, HiKind::Explicit(110))
        ))
    );
    assert_eq!(
        read_of("awk 'NR>=60 && NR<=110' a.rs"),
        read_of("sed -n '60,110p' a.rs")
    );
}

/// **Every other `awk` is left alone.**
///
/// An action decides what is printed, so a program with one — even one
/// that prints the line number beside the line — is the program's to
/// describe, not the file's. So are a range the wrong way round or from line
/// zero, a signed or computed number, a number with a leading zero (`gawk`
/// reads `010` as eight), a token split by a space, a tab, a second
/// condition, an option, a second file, an operand `awk` would read as an
/// assignment, and standard input.
#[test]
fn an_awk_program_that_is_not_a_line_pick_is_left_alone() {
    for command in [
        "awk '{print}' a.rs",
        "awk 'NR>=1 && NR<=5 {print NR\": \"$0}' a.rs",
        "awk 'NR>=1 && NR<=5 {print}' a.rs",
        "awk 'NR>=1 && NR<=5;' a.rs",
        "awk 'NR>=5 && NR<=1' a.rs",
        "awk 'NR>=0 && NR<=5' a.rs",
        "awk 'NR>=+1 && NR<=5' a.rs",
        "awk 'NR>=1 && NR<=5e1' a.rs",
        "awk 'NR>=1 && NR<=X' a.rs",
        "awk 'NR>=010 && NR<=20' a.rs",
        "awk 'NR>=1 && NR<=08' a.rs",
        "awk 'N R>=1 && NR<=5' a.rs",
        "awk 'NR> =1 && NR<=5' a.rs",
        "awk 'NR>=1\t&& NR<=5' a.rs",
        "awk 'NR>=1 || NR<=5' a.rs",
        "awk 'NR<=5 && NR>=1' a.rs",
        "awk 'NR>=1 && NR<=5 && /x/' a.rs",
        "awk 'NR==3' a.rs",
        "awk -F: 'NR>=1 && NR<=5' a.rs",
        "awk 'NR>=1 && NR<=5' a.rs b.rs",
        "awk 'NR>=1 && NR<=5' x=1",
        "awk 'NR>=1 && NR<=5' -",
        "awk 'NR>=1 && NR<=5'",
        "awk \"NR>=1 && NR<=$N\" a.rs",
        "gawk 'NR>=1 && NR<=5' a.rs",
        "/usr/local/bin/awk 'NR>=1 && NR<=5' a.rs",
    ] {
        assert!(recognise(command).is_none(), "{command:?}");
    }
}

proptest::proptest! {
    /// ★ Whatever its spacing, a line pick in `awk` reads exactly as the
    /// `sed` range over the same numbers, and is refused exactly where that
    /// range is.
    #[test]
    fn an_awk_line_pick_reads_as_the_sed_range_over_the_same_numbers(
        lo in 0_u32..400,
        hi in 0_u32..400,
        gaps in proptest::collection::vec(
            proptest::sample::select(&["", " ", "  "][..]),
            8,
        ),
    ) {
        let (lo_text, hi_text) = (lo.to_string(), hi.to_string());
        let tokens = ["NR", ">=", &lo_text, "&&", "NR", "<=", &hi_text, ""];
        let program: String = gaps
            .iter()
            .zip(tokens)
            .flat_map(|(gap, token)| [*gap, token])
            .collect();
        let awk = recognise(&format!("awk '{program}' a.rs"));
        let sed = recognise(&format!("sed -n '{lo},{hi}p' a.rs"));
        proptest::prop_assert_eq!(awk, sed, "{}", program);
    }
}
