//! A trailing `| sed -n 'A,Bp'`: the third narrowing pipe, a line pick on
//! the run the read before it produced.

use crate::shell_read::{recognise, Narrowing, ShellCall, ShellSpan};
use crate::{HiKind, ReadSpan};

use super::{assert_each_recognised, narrowed, read_of, reads_of};

/// **A line pick after a pipe narrows the one read it follows.**
///
/// `sed -n` with one print element reads standard input when no file
/// follows the script, and prints lines `A` to `B` of it and nothing else,
/// so after a pipe it does to the run the read produced exactly what `head`
/// and `tail` do there: it keeps a stretch of it by count. Quoted or bare,
/// one line or a range, under a `cd` and after a label alike.
#[test]
fn a_trailing_line_pick_narrows_the_one_read_it_follows() {
    let pick = |lo, hi| Narrowing::Pick { lo, hi };
    let one = |span, narrowing| ShellCall::Read(vec![narrowed("a.rs", span, narrowing)]);
    assert_each_recognised(&[
        (
            "cat a.rs | sed -n '1,40p'",
            one(ShellSpan::WholeFile, pick(1, 40)),
        ),
        (
            "cat a.rs | sed -n 5,9p",
            one(ShellSpan::WholeFile, pick(5, 9)),
        ),
        (
            "cat a.rs | sed -n '7p'",
            one(ShellSpan::WholeFile, pick(7, 7)),
        ),
        (
            "grep -n needle a.rs | sed -n '1,75p'",
            one(ShellSpan::Matching, pick(1, 75)),
        ),
        (
            "sed -n '100,200p' a.rs | sed -n '1,10p'",
            one(ShellSpan::Lines { lo: 100, hi: 200 }, pick(1, 10)),
        ),
        (
            "cd crates && echo '== a ==' && cat a.rs | sed -n '1,40p'",
            ShellCall::Read(vec![narrowed(
                "crates/a.rs",
                ShellSpan::WholeFile,
                pick(1, 40),
            )]),
        ),
    ]);
}

/// **The arithmetic of a line pick**, run on what the member produced.
///
/// Lines `A` to `B` of a run that starts at line `L` of the file are lines
/// `L+A-1` to `L+B-1`, cut at the run's own end. A pick that starts past
/// that end prints nothing, and a run counted from the end of the file
/// starts at a line only the file's length names: both are unknown. So is
/// any pick of what a search matched.
#[test]
fn a_line_pick_cuts_the_range_the_member_produced() {
    let file = "a.rs".to_owned();
    let lines = |lo, hi| Some((file.clone(), ReadSpan::lines(lo, HiKind::Explicit(hi))));
    assert_eq!(read_of("cat a.rs | sed -n '1,40p'"), lines(1, 40));
    assert_eq!(read_of("cat a.rs | sed -n '7p'"), lines(7, 7));
    assert_eq!(read_of("head -n 40 a.rs | sed -n '5,9p'"), lines(5, 9));
    assert_eq!(read_of("head -n 40 a.rs | sed -n '30,90p'"), lines(30, 40));
    assert_eq!(
        read_of("sed -n '100,200p' a.rs | sed -n '1,10p'"),
        lines(100, 109)
    );
    assert_eq!(
        read_of("sed -n '100,200p' a.rs | sed -n '90,150p'"),
        lines(189, 200)
    );
    for command in [
        "head -n 40 a.rs | sed -n '41,50p'",
        "sed -n '100,200p' a.rs | sed -n '102,110p'",
        "tail -n 40 a.rs | sed -n '1,5p'",
        "grep -n needle a.rs | sed -n '1,75p'",
        "nl a.rs | sed -n '1,5p'",
    ] {
        let spans: Vec<ReadSpan> = reads_of(command)
            .into_iter()
            .map(|(_, span)| span)
            .collect();
        assert_eq!(spans, vec![ReadSpan::Unknown], "{command:?}");
    }
}

/// **Every other `sed` after a pipe is left alone.**
///
/// Without `-n` every line is printed as well; a second element, a pattern,
/// a relative or reversed or zero range, an option, and a file after the
/// script — which `sed` would read in place of the pipe — each make it a
/// different program. A pick cannot narrow a listing, a label or several
/// reads at once, and one pipe is all a member may carry.
#[test]
fn a_piped_sed_that_is_not_one_line_pick_is_left_alone() {
    for command in [
        "cat a.rs | sed '1,5p'",
        "cat a.rs | sed -n '1,5p;9p'",
        "cat a.rs | sed -n '/fn/p'",
        "cat a.rs | sed -n '1,+3p'",
        "cat a.rs | sed -n '5,1p'",
        "cat a.rs | sed -n '0,5p'",
        "cat a.rs | sed -n '$p'",
        "cat a.rs | sed -n '1,5d'",
        "cat a.rs | sed -n -e '1,5p'",
        "cat a.rs | sed -E -n '1,5p'",
        "cat a.rs | sed -n '1,5p' b.rs",
        "cat a.rs | sed -n '1,5p' -",
        "cat a.rs | sed 's/a/b/'",
        "cat a.rs | /usr/bin/sed -n '1,5p'",
        "cat a.rs | sed -n '1,5p' | head -n 2",
        "cat a.rs b.rs | sed -n '1,5p'",
        "ls src | sed -n '1,5p'",
        "echo a.rs | sed -n '1,5p'",
    ] {
        assert!(recognise(command).is_none(), "{command:?}");
    }
}

proptest::proptest! {
    /// ★ A line pick delivers exactly the lines a run of the commands would
    /// print, on a file longer than any number here: the stretch from the
    /// first line it printed to the last, or unknown where it printed none.
    #[test]
    fn a_line_pick_delivers_the_lines_the_commands_print(
        reader in 0_usize..3,
        lo in 1_u32..300,
        len in 0_u32..300,
        pick_lo in 1_u32..400,
        pick_len in 0_u32..400,
    ) {
        let hi = lo + len;
        let (inner, run_lo, run_hi) = match reader {
            0 => ("cat a.rs".to_owned(), 1, u32::MAX),
            1 => (format!("head -n {hi} a.rs"), 1, hi),
            _ => (format!("sed -n '{lo},{hi}p' a.rs"), lo, hi),
        };
        let pick_hi = pick_lo + pick_len;
        let command = format!("{inner} | sed -n '{pick_lo},{pick_hi}p'");
        let first = run_lo + (pick_lo - 1);
        let last = run_hi.min(run_lo + (pick_hi - 1));
        let expected = if first <= last {
            ReadSpan::lines(first, HiKind::Explicit(last))
        } else {
            ReadSpan::Unknown
        };
        let spans: Vec<ReadSpan> = reads_of(&command).into_iter().map(|(_, span)| span).collect();
        proptest::prop_assert_eq!(spans, vec![expected], "{}", command);
    }
}
