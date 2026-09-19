//! The labels a chain writes between its reads: an `echo` or `printf` over
//! plain operands contributes no read and disqualifies nothing, and a line
//! that is labels alone is not a read.

use crate::shell_read::{recognise, ShellCall, ShellSpan};

use super::{member, read};

/// **Every way a chain labels itself, and the reads it still is.**
///
/// The shape the second census found: reads separated by the `echo` lines
/// an agent writes to caption its own output. A label contributes no read
/// and disqualifies nothing, so each of these is exactly the chain it
/// would have been with the labels struck out — which is what the table
/// asserts, by pairing every labelled line with that one chain.
#[test]
fn a_label_between_reads_changes_nothing_about_the_reads() {
    let plain = ShellCall::Read(vec![
        member("repo/a.rs", ShellSpan::Lines { lo: 1, hi: 80 }),
        member("repo/b.rs", ShellSpan::Lines { lo: 100, hi: 140 }),
    ]);
    for command in [
        // The measured line, in shape.
        "cd \"repo\" && echo \"=== a ===\" && sed -n '1,80p' a.rs && echo && \
         sed -n '100,140p' b.rs",
        // `sed > echo > sed`, with the directory already on the paths.
        "sed -n '1,80p' repo/a.rs && echo --- && sed -n '100,140p' repo/b.rs",
        // `cd > sed > echo > sed`, and a `printf` in the same position.
        "cd repo && sed -n '1,80p' a.rs && printf '%s' next && sed -n '100,140p' b.rs",
        // A label may open the line, close it, or stand twice over.
        "echo start && cd repo && sed -n '1,80p' a.rs && echo && echo && \
         sed -n '100,140p' b.rs && echo done",
        // …and `;` joins a label exactly as `&&` does. (The `cd` still
        // takes `&&` and only `&&`: that is the join that makes the
        // directory a precondition rather than a separate statement.)
        "cd repo && sed -n '1,80p' a.rs; echo; sed -n '100,140p' b.rs",
    ] {
        assert_eq!(recognise(command).as_ref(), Some(&plain), "{command}");
    }
    // A label opening the line is not a `cd` prefix in disguise: the
    // directory is peeled off a `cd` and off nothing else, so a path after
    // one stays exactly as it was written.
    //
    // Quoting is the whole operand rule: a bare word, a single-quoted
    // literal and a double-quoted one are all plain operands, and a label
    // with no operands at all is the blank line between two reads.
    for label in [
        "echo",
        "echo hi",
        "echo === a ===",
        "echo \"=== a ===\"",
        "echo '=== a ==='",
        "printf",
        "printf '%s\\n' one two",
    ] {
        assert_eq!(
            recognise(&format!("{label} && cat src/lib.rs")).as_ref(),
            Some(&read("src/lib.rs", ShellSpan::WholeFile)),
            "{label}"
        );
    }
}

/// **A line that only labels itself read nothing, and a label never
/// carries a read into the grammar on its back.**
///
/// The first half is what keeps the rule additive: a label contributes no
/// request, so a line made of them contributes none and is not a read at
/// all. The second is its boundary — an operand a shell would act on, a
/// command word that merely resembles one of the two, a pipe out of
/// something that read nothing — and every one of those takes the whole
/// line out, exactly as an unrecognised member always has.
#[test]
fn a_line_that_only_labels_itself_is_not_a_read() {
    for command in [
        // Labels and nothing else.
        "echo hi",
        "echo",
        "printf '%s' hi",
        "cd repo && echo hi",
        "echo one && echo two",
        // An operand a shell would act on is not a plain operand.
        "echo $HOME && cat src/lib.rs",
        "echo \"$HOME\" && cat src/lib.rs",
        "echo `id` && cat src/lib.rs",
        "echo $(id) && cat src/lib.rs",
        "echo hi > out.txt && cat src/lib.rs",
        "echo hi >> out.txt && cat src/lib.rs",
        "echo *.rs && cat src/lib.rs",
        "echo ~ && cat src/lib.rs",
        "echo \"a\\nb\" && cat src/lib.rs",
        "echo a'b' && cat src/lib.rs",
        // A pipe out of a label is a shape nobody has measured.
        "echo hi | head -n 5 && cat src/lib.rs",
        "cat src/lib.rs && echo hi | head -n 5",
        // A near neighbour of the two label commands is not one of them.
        "cat src/lib.rs && print hi",
        "cat src/lib.rs && echoes hi",
        "cat src/lib.rs && /bin/echo hi",
        // A listing still stands alone, labels included.
        "ls crates && echo hi",
        "echo hi && ls crates",
    ] {
        assert!(
            recognise(command).is_none(),
            "{command:?} is not recognised"
        );
    }
}
