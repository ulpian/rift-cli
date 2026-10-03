//! The `sed -n` print scripts beyond one range: a single line, and several
//! runs joined by `;`, with what each one reads and what it may be credited
//! with.

use crate::shell_read::{recognise, ShellCall, ShellRead, ShellSpan, MAX_CHAIN_READS};
use crate::{HiKind, ReadSpan};

use super::{assert_each_recognised, member, read, read_of, reads_of};

/// One of several runs a script prints, as the table spells it.
fn run(path: &str, lo: u32, hi: u32) -> ShellRead {
    member(path, ShellSpan::OneOfSeveralRuns { lo, hi })
}

/// **A print script of single lines and runs reads each of them, in the
/// order the script names them.**
///
/// `Ap` prints line `A`, and `A,Bp` the lines `A` to `B`. A script of several
/// joined by `;` prints each in the file's order, a line twice where two
/// overlap, and nothing else under `-n` — so the lines it delivered are
/// exactly the runs it names, and it is one read per run.
#[test]
fn a_print_script_reads_each_run_it_names() {
    let file = "src/lib.rs";
    assert_each_recognised(&[
        (
            "sed -n '5p' src/lib.rs",
            read(file, ShellSpan::Lines { lo: 5, hi: 5 }),
        ),
        (
            "sed -n 5p src/lib.rs",
            read(file, ShellSpan::Lines { lo: 5, hi: 5 }),
        ),
        (
            "sed -n '1,5p;9,12p' src/lib.rs",
            ShellCall::Read(vec![run(file, 1, 5), run(file, 9, 12)]),
        ),
        (
            "sed -n '9,12p;1,5p;20p' src/lib.rs",
            ShellCall::Read(vec![run(file, 9, 12), run(file, 1, 5), run(file, 20, 20)]),
        ),
        (
            "cd crates && sed -n \"1,5p;3,8p\" src/lib.rs",
            ShellCall::Read(vec![
                run("crates/src/lib.rs", 1, 5),
                run("crates/src/lib.rs", 3, 8),
            ]),
        ),
    ]);
}

/// **A single line is credited; several runs are recognised and credited with
/// nothing.**
///
/// One run is one slice, held to the terminal threshold like any other.
/// Several runs reach the window as one output, and that output can pass the
/// threshold — and be written to a file the window only gets a pointer to —
/// while each run's own slice stays under it. The gate weighs each read on
/// its own, so a run credited there could be a run the window never held.
#[test]
fn several_runs_are_recognised_and_credited_with_nothing() {
    assert_eq!(
        read_of("sed -n '5p' src/lib.rs"),
        Some((
            "src/lib.rs".to_owned(),
            ReadSpan::lines(5, HiKind::Explicit(5))
        ))
    );
    assert_eq!(read_of("sed -n '1,5p;9,12p' src/lib.rs"), None);
    assert_eq!(
        reads_of("sed -n '1,5p;9,12p' src/lib.rs"),
        vec![
            ("src/lib.rs".to_owned(), ReadSpan::Unknown),
            ("src/lib.rs".to_owned(), ReadSpan::Unknown)
        ]
    );
    assert!(recognise("sed -n '1,5p;9,12p' src/lib.rs")
        .is_some_and(|call| call.searched_file().is_none()));
}

/// **A script one step outside a list of print runs is left alone.**
///
/// An empty element, a space the shell would pass but the grammar does not
/// pin, a command other than `p`, a substitution, a line `$` or a sign only
/// the file's length or the command's own arithmetic could settle, a run
/// that ends before it starts, line zero, a second path, an in-place edit,
/// a missing `-n`, a narrowing of several runs, and more runs than a call
/// may hold.
#[test]
fn a_script_outside_the_print_runs_is_left_alone() {
    let too_many = vec!["1p"; MAX_CHAIN_READS + 1].join(";");
    let too_many = format!("sed -n '{too_many}' src/lib.rs");
    for command in [
        "sed -n '1,5p;' src/lib.rs",
        "sed -n ';1,5p' src/lib.rs",
        "sed -n '1,5p;;9p' src/lib.rs",
        "sed -n '1,5p; 9,12p' src/lib.rs",
        "sed -n '1,5p;9,12d' src/lib.rs",
        "sed -n '1,5p;s/a/b/p' src/lib.rs",
        "sed -n '1,5p;9,12p;q' src/lib.rs",
        "sed -n '1,$p' src/lib.rs",
        "sed -n '$p' src/lib.rs",
        "sed -n '1,5p;$p' src/lib.rs",
        "sed -n '1,5p;9,+3p' src/lib.rs",
        "sed -n '1,5p;12,9p' src/lib.rs",
        "sed -n '0p' src/lib.rs",
        "sed -n 'p' src/lib.rs",
        "sed -n '5' src/lib.rs",
        "sed -n '1,5p;9,12p' src/lib.rs src/main.rs",
        "sed -i -n '1,5p;9,12p' src/lib.rs",
        "sed -n -i '5p' src/lib.rs",
        "sed '1,5p;9,12p' src/lib.rs",
        "sed -n '1,5p;9,12p' src/lib.rs | head -n 3",
        "sed -n '1,5p\n9,12p' src/lib.rs",
        &too_many,
    ] {
        assert!(
            recognise(command).is_none(),
            "{command:?} is not recognised"
        );
    }
}

proptest::proptest! {
    /// ★ Any list of print runs is read as exactly those runs, in order; and
    /// the same list with one element turned into a command that is not a
    /// print is not read at all.
    #[test]
    fn a_list_of_print_runs_is_read_as_exactly_those_runs(
        runs in proptest::collection::vec((1_u32..500, 0_u32..50, proptest::bool::ANY), 2..6),
        bad in proptest::sample::select(&["9,12d", "s/a/b/p", "$p", "1,+3p", "", "q"][..]),
        at in 0_usize..6,
    ) {
        let spell = |(lo, len, single): &(u32, u32, bool)| {
            if *single { format!("{lo}p") } else { format!("{lo},{}p", lo + len) }
        };
        let mut script: Vec<String> = runs.iter().map(spell).collect();
        let call = recognise(&format!("sed -n '{}' src/lib.rs", script.join(";")));
        let expected: Vec<ShellRead> = runs
            .iter()
            .map(|(lo, len, single)| run("src/lib.rs", *lo, if *single { *lo } else { lo + len }))
            .collect();
        proptest::prop_assert_eq!(call, Some(ShellCall::Read(expected)));
        script.insert(at.min(script.len()), (*bad).to_owned());
        let widened = format!("sed -n '{}' src/lib.rs", script.join(";"));
        proptest::prop_assert!(recognise(&widened).is_none(), "{widened:?}");
    }
}
