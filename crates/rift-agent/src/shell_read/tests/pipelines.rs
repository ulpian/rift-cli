//! A pipeline composed over its stages: a read piped into a filter, or past
//! more than one narrowing, is counted and never coverage, and a stage that
//! is neither still takes the whole line out of the grammar.

use crate::shell_read::{recognise, ShellCall, ShellSpan};
use crate::ReadSpan;

use super::{member, read};

/// **A read piped into a filter is counted as a read of that file.**
///
/// The census's largest named refusal was a read piped into `grep`; the
/// shell opened the file whatever the filter kept, so it is a read, and
/// which lines arrived is nothing the line names, so it is filtered.
#[test]
fn a_read_piped_into_a_filter_is_a_filtered_read_of_that_file() {
    for command in [
        "cat src/lib.rs | grep needle",
        "cat src/lib.rs | grep -n needle",
        "cat src/lib.rs | grep -v '^$'",
        "cat src/lib.rs | egrep 'a|b'",
        "cat src/lib.rs | grep -A 3 needle",
        "cat src/lib.rs | grep -C2 -e needle",
        "sed -n '1,80p' src/lib.rs | grep -n fn",
        "head -n 40 src/lib.rs | cut -c1-80",
        "cat src/lib.rs | cut -d: -f1",
        "cat src/lib.rs | cut -d ' ' -f 2,3",
        "cat src/lib.rs | sort",
        "cat src/lib.rs | sort -k2n -t:",
        "cat src/lib.rs | sort | uniq -c",
        "cat src/lib.rs | grep needle | head -n 20",
        "cd crates && cat src/lib.rs | grep needle",
    ] {
        let expected = if command.starts_with("cd ") {
            "crates/src/lib.rs"
        } else {
            "src/lib.rs"
        };
        assert_eq!(
            recognise(command),
            Some(read(expected, ShellSpan::Filtered)),
            "{command}"
        );
    }
}

/// **Two narrowings, or one over several reads, are filtered too.**
///
/// A narrowing over files `cat` joined cuts the joined run, a run of
/// neither file; two narrowings compose into a range this slice does not
/// name. Each file is still counted, which is the denominator the census
/// was missing.
#[test]
fn a_narrowing_the_arithmetic_cannot_place_leaves_each_read_filtered() {
    let both = ShellCall::Read(vec![
        member("a.rs", ShellSpan::Filtered),
        member("b.rs", ShellSpan::Filtered),
    ]);
    for command in [
        "cat a.rs b.rs | head -n 5",
        "grep -n needle a.rs b.rs | head -n 5",
        "cat a.rs b.rs | grep needle",
    ] {
        assert_eq!(recognise(command).as_ref(), Some(&both), "{command}");
    }
    for command in [
        "cat src/lib.rs | head -n 20 | tail -n 5",
        "cat src/lib.rs | sed -n '1,5p' | head -n 2",
        "awk 'NR>=1 && NR<=5' src/lib.rs | grep x",
    ] {
        assert_eq!(
            recognise(command),
            Some(read("src/lib.rs", ShellSpan::Filtered)),
            "{command}"
        );
    }
    assert_eq!(
        recognise("sed -n '1,5p;9,12p' src/lib.rs | head -n 3"),
        Some(ShellCall::Read(vec![
            member("src/lib.rs", ShellSpan::Filtered),
            member("src/lib.rs", ShellSpan::Filtered),
        ]))
    );
}

/// **The rows the single-line grammar refused that the pipeline composes.**
///
/// Each one stood in a refusal table before this slice; it is pinned here
/// so the move is visible: every file it names is counted, none is
/// coverage.
#[test]
fn the_refused_pipelines_this_slice_admits_are_each_counted() {
    for (command, files) in [
        ("grep -n needle src/lib.rs src/main.rs | tail -5", 2),
        ("rg -n needle src/lib.rs src/main.rs | head -n 5", 2),
        ("cat a.rs b.rs | sed -n '1,5p'", 2),
    ] {
        let Some(ShellCall::Read(reads)) = recognise(command) else {
            panic!("{command:?} is a counted pipeline");
        };
        assert_eq!(reads.len(), files, "{command}");
        assert!(
            reads.iter().all(|read| read.span == ShellSpan::Filtered),
            "{command}"
        );
    }
}

/// **A filtered read supports no coverage and is no search** (rules 2, 3).
#[test]
fn a_filtered_read_records_nothing_and_answers_no_search() {
    let call = recognise("cat src/lib.rs | grep needle && sed -n '1,9p' src/main.rs")
        .expect("both members are read");
    let spans: Vec<ReadSpan> = call.read_requests().into_iter().map(|r| r.span).collect();
    assert_eq!(spans.first(), Some(&ReadSpan::Unknown));
    assert_eq!(
        call.read_request().map(|r| r.path),
        Some("src/main.rs".to_owned())
    );
    let alone = recognise("cat src/lib.rs | grep needle").expect("a filtered read");
    assert_eq!(alone.read_request(), None);
    assert_eq!(alone.searched_file(), None);
    assert_eq!(alone.search_request(), None);
}

/// **A filter that could open, write or recurse is not a filter.**
///
/// Each line is one step outside the admitted forms: an operand the filter
/// would read in place of the pipe, an option that reads a pattern file,
/// recurses, writes an output file or names inputs, a long option, a quoted
/// pattern the tool reads as an option, and the stages that are no filter at
/// all. A listing or a label piped onward stays refused.
#[test]
fn a_stage_that_is_no_filter_still_refuses_the_line() {
    for command in [
        "cat src/lib.rs | grep needle other.rs",
        "cat src/lib.rs | grep -f patterns",
        "cat src/lib.rs | grep -r needle",
        "cat src/lib.rs | grep -l needle",
        "cat src/lib.rs | grep --count needle",
        "cat src/lib.rs | grep -- needle",
        "cat src/lib.rs | grep '-v'",
        "cat src/lib.rs | grep -A x needle",
        "cat src/lib.rs | grep -n",
        "cat src/lib.rs | sort -o out.txt",
        "cat src/lib.rs | sort -m other.txt",
        "cat src/lib.rs | sort other.txt",
        "cat src/lib.rs | uniq in.txt out.txt",
        "cat src/lib.rs | cut -c1-80 other.rs",
        "cat src/lib.rs | cut -d:",
        "cat src/lib.rs | wc -l",
        "cat src/lib.rs | less",
        "cat src/lib.rs | cat",
        "cat src/lib.rs | xargs grep x",
        "cat src/lib.rs | /usr/bin/grep x",
        "cat src/lib.rs | grep x > out.txt",
        "cat src/lib.rs | grep $X",
        "cat src/lib.rs | grep x |",
        "cat src/lib.rs || grep x",
        "ls crates | grep rift",
        "ls crates | head -n 5",
        "echo hi | grep h",
    ] {
        assert!(
            recognise(command).is_none(),
            "{command:?} is not recognised"
        );
    }
}
