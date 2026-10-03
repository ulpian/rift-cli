//! What is left alone: a shell that would do more than read, a composition
//! the grammar does not spell, and the quoted separator that is data.

use crate::shell_read::{recognise, ShellSpan};

use super::{listing, read};

/// The shapes a shell would act on, each one refused.
///
/// They are listed rather than generated so that a reader can see the
/// class each stands for; the property test beside this one covers the
/// space between them.
#[test]
fn a_shell_that_would_do_more_than_read_is_left_alone() {
    for command in [
        "cat src/lib.rs; rm -rf /",
        "cat src/lib.rs > /tmp/out",
        "cat < src/lib.rs",
        "cat $FILE",
        "cat `echo src/lib.rs`",
        "cat $(echo src/lib.rs)",
        "cat ~/src/lib.rs",
        "cat src/*.rs",
        "cat src/lib.rs\nrm -rf /",
        "FOO=bar cat src/lib.rs",
        "sudo cat /etc/shadow",
        "xargs cat",
        "cat -n src/lib.rs",
        "head -5 src/lib.rs",
        "head -n0 src/lib.rs",
        "head -n 0 src/lib.rs",
        "sed -n '12,48d' src/lib.rs",
        "sed -n '48,12p' src/lib.rs",
        "sed -n '0,4p' src/lib.rs",
        "sed '12,48p' src/lib.rs",
        "sed -i s/a/b/ src/lib.rs",
        "cat 'src/lib.rs'",
        "cat \"src/$HOME\"",
        "cat src/lib.rs'",
        "cat 'src/lib.rs",
        "cat a'b'",
        "",
        "   ",
        "cat",
    ] {
        assert!(
            recognise(command).is_none(),
            "{command:?} is not recognised"
        );
    }
}

/// **The compositions stop exactly where they are spelled.**
///
/// Each line here is one step outside one of the three rules — a pipe into
/// something that is not a narrowing, a member that is not a read, a `cd`
/// that is not the only one or not at the front, a separator the grammar
/// does not join with — and every one of them takes the whole command line
/// out of the grammar rather than costing only the part that stepped out.
#[test]
fn a_composition_the_grammar_does_not_spell_is_left_alone() {
    for command in [
        // A pipe target that is not one of the two narrowing commands.
        "cat src/lib.rs | grep needle",
        "cat src/lib.rs | wc -l",
        "cat src/lib.rs | less",
        "cat src/lib.rs | cat",
        // A narrowing with no count, or one that is not a count.
        "cat src/lib.rs | head",
        "cat src/lib.rs | head -n",
        "cat src/lib.rs | head -n0",
        "cat src/lib.rs | head -0",
        "cat src/lib.rs | head -n x",
        "cat src/lib.rs | tail -n -5",
        // One narrowing, not two, and never on a member that is several
        // reads or a listing.
        "cat src/lib.rs | head -n 20 | tail -n 5",
        "cat a.rs b.rs | head -n 5",
        "ls crates | head -n 5",
        "cat src/lib.rs |",
        "| cat src/lib.rs",
        "cat src/lib.rs || cat src/main.rs",
        // A member that is not a read takes the line with it.
        "cat src/lib.rs && cargo build",
        "cat src/lib.rs && touch done",
        "cd crates && cargo test",
        "cat src/lib.rs &&",
        "cat src/lib.rs ;",
        "cat src/lib.rs && && cat src/main.rs",
        // A `cd` is one, and it is first.
        "cd crates",
        "cd crates && cd rift-core && cat src/lib.rs",
        "cat src/lib.rs && cd crates && cat src/main.rs",
        "cd crates; cat src/lib.rs",
        "cd && cat src/lib.rs",
        "cd -P crates && cat src/lib.rs",
        "cd crates other && cat src/lib.rs",
        "cd $HOME && cat src/lib.rs",
        "cd \"$HOME\" && cat src/lib.rs",
        // A background job is not a chain, whatever it looks like.
        "cat src/lib.rs & cat src/main.rs",
        "cat src/lib.rs &",
        // A listing never stands among reads.
        "cat src/lib.rs && ls crates",
        "ls crates && ls bins",
    ] {
        assert!(
            recognise(command).is_none(),
            "{command:?} is not recognised"
        );
    }
}

/// A separator inside a quoted pattern is data, not punctuation — the one
/// place widening the grammar could have quietly cut a recognised line in
/// half.
#[test]
fn a_separator_inside_a_quote_is_part_of_the_pattern() {
    for command in [
        "egrep 'a|b' src/lib.rs",
        "egrep 'a&&b' src/lib.rs",
        "egrep 'a;b' src/lib.rs",
        "grep \"a|b\" src/lib.rs",
    ] {
        assert_eq!(
            recognise(command).as_ref(),
            Some(&read("src/lib.rs", ShellSpan::Matching)),
            "{command}"
        );
    }
    assert_eq!(
        recognise("find crates -name 'a|b.rs'").as_ref(),
        Some(&listing("crates", Some("a|b.rs")))
    );
}

/// **A tilde a shell would expand is still refused.** Only one inside a path
/// word already under way is a letter; at the start of a word, or after the
/// `=` or `:` where an assignment expands one, it names a home directory,
/// and a word the shell would rewrite is not a path this grammar reads.
#[test]
fn a_tilde_the_shell_would_expand_is_left_alone() {
    for command in [
        "cat ~",
        "cat ~/src/lib.rs",
        "cat ~user/src/lib.rs",
        "cat ~+/src/lib.rs",
        "cd ~ && cat src/lib.rs",
        "cd ~/repo && cat src/lib.rs",
        "cat src/lib.rs && cat ~/notes.md",
        "cat a.rs | head -~1",
        "FOO=~/x cat src/lib.rs",
        "cat FOO=~/x",
        "cat PATH=a:~/b",
        "cat C:~/a.rs",
        "cat RUNNER~1/$HOME",
        "cat RUNNER~1/*.rs",
        "cat 'RUNNER~1'",
    ] {
        assert!(
            recognise(command).is_none(),
            "{command:?} is not recognised"
        );
    }
}
