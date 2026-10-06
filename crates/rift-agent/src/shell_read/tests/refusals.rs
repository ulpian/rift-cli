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

/// **A count with a sign is a different instruction, not a count.**
///
/// `tail -n +5` prints from line five to the end, and GNU `sed` reads
/// `1,+3p` as line one and the three after it. Rust's integer parser
/// accepts a leading `+` and would read both as a plain five or three — a
/// window the command never printed, credited as held. Every count position
/// the grammar has is listed, because each one used to reach that parser.
#[test]
fn a_count_with_a_sign_is_never_read_as_a_count() {
    for command in [
        // `tail` and `head` in the command position.
        "tail -n +5 src/lib.rs",
        "tail -n+5 src/lib.rs",
        "tail +5 src/lib.rs",
        "tail -n -5 src/lib.rs",
        "head -n +5 src/lib.rs",
        "head -n -5 src/lib.rs",
        // The narrowing position, in both spellings.
        "cat src/lib.rs | tail -n +5",
        "cat src/lib.rs | tail -+5",
        "cat src/lib.rs | tail +5",
        "cat src/lib.rs | head -n +5",
        "cat src/lib.rs | head -+5",
        "cat src/lib.rs | head --5",
        "cd crates && cat src/lib.rs | tail -n +5",
        // Either end of a `sed` range.
        "sed -n '1,+3p' src/lib.rs",
        "sed -n '+1,3p' src/lib.rs",
        "sed -n '+1,+3p' src/lib.rs",
        "sed -n '2,-3p' src/lib.rs",
    ] {
        assert!(
            recognise(command).is_none(),
            "{command:?} is not recognised"
        );
    }
}

/// **A search stays a search over one file, and only the flags that choose
/// its lines come with it.**
///
/// Each line here is one step outside the flagged single-file search: a
/// flag that reaches past the one file (a recursion, a pattern file, a
/// glob), one that prints something other than the file's lines or their
/// count (a file name, a part of a line, the lines that did not match), a second
/// path or none, a context count that is not one, a long option, an option
/// written after the pattern — which one `grep` permutes and another reads
/// as a file — and a pattern a search tool would take for an option.
#[test]
fn a_search_flag_the_grammar_does_not_pin_is_left_alone() {
    for command in [
        // Past the one file.
        "grep -r needle src",
        "grep -rn needle src",
        "grep -nr needle src",
        "grep -R needle src",
        "grep -f patterns.txt src/lib.rs",
        "grep -n --include '*.rs' needle src",
        "rg -n -g '*.rs' needle src",
        "rg -n -t rust needle src",
        // Something other than the file's lines.
        "grep -l needle src/lib.rs",
        "grep -cl needle src/lib.rs",
        "grep -c needle src/lib.rs src/main.rs",
        "grep -rc needle src",
        "grep needle src/lib.rs -cv",
        "grep -L needle src/lib.rs",
        "grep -q needle src/lib.rs",
        "grep -o needle src/lib.rs",
        "grep -v needle src/lib.rs",
        "grep -nv needle src/lib.rs",
        "rg -r x needle src/lib.rs",
        // One path, not two and not none.
        "grep -n needle src/lib.rs src/main.rs",
        "grep -n -e needle src/lib.rs src/main.rs",
        "grep -n needle",
        "grep -n -e needle",
        "grep -n -e src/lib.rs",
        "rg -n needle",
        // A context count that is not one.
        "grep -A needle src/lib.rs",
        "grep -An needle src/lib.rs",
        "grep -nA needle src/lib.rs",
        "grep -A -3 needle src/lib.rs",
        "grep -A +3 needle src/lib.rs",
        "grep -A+3 needle src/lib.rs",
        "grep -A 3x needle src/lib.rs",
        "grep -A '3' needle src/lib.rs",
        // A long option, or a bare dash.
        "grep --line-number needle src/lib.rs",
        "grep - needle src/lib.rs",
        "grep -n - src/lib.rs",
        // A letter one tool pins and another does not.
        "grep -P needle src/lib.rs",
        "rg -E latin1 needle src/lib.rs",
        "egrep -F needle src/lib.rs",
        "fgrep -E 'a|b' src/lib.rs",
        // A pattern the tool would read as an option, quoted or not.
        "grep '-v' src/lib.rs",
        "grep -n '-v' src/lib.rs",
        "rg \"-n\" src/lib.rs",
        "grep -e -v src/lib.rs",
        // The shell's own characters, as everywhere else.
        "grep -n $PAT src/lib.rs",
        "grep -n needle src/*.rs",
        "grep -n -A $N needle src/lib.rs",
    ] {
        assert!(
            recognise(command).is_none(),
            "{command:?} is not recognised"
        );
    }
}

/// **An option may sit anywhere before `--`, and nothing after it is one.**
///
/// The permuted spelling is read the way the search tool reads it, so each
/// line here is one step outside that reading: a flag after the pattern
/// that the set does not pin, a second path written after the options, a
/// word after `--` that would have been an option before it, a pattern
/// after `--` that the tool would still not take for a file, and a quoted
/// word that the tool reads as an option wherever it stands.
#[test]
fn a_late_option_the_grammar_does_not_pin_is_left_alone() {
    for command in [
        "grep -n needle -r src",
        "grep needle -A src/lib.rs",
        "grep needle src/lib.rs -A",
        "grep needle src/lib.rs -A -3",
        "grep needle src/lib.rs --line-number",
        "rg needle src/lib.rs -g '*.rs'",
        "grep -n needle src/lib.rs -A 3 src/main.rs",
        "grep needle -n src/lib.rs src/main.rs",
        "grep -e needle src/lib.rs -n src/main.rs",
        "grep needle -- -n src/lib.rs",
        "grep -- needle src/lib.rs -n",
        "grep -n -- needle -- src/lib.rs",
        "grep -n -- -v src/lib.rs",
        "grep -n -- needle",
        "grep needle src/lib.rs '-n'",
        "grep needle '-A' 3 src/lib.rs",
        "grep needle src/lib.rs -",
    ] {
        assert!(
            recognise(command).is_none(),
            "{command:?} is not recognised"
        );
    }
}

/// **Only `/bin` and `/usr/bin` name a command, and only in the command
/// position.**
///
/// Every other directory is one a user or a package manager writes, so a
/// program found there is whatever was put there; a relative spelling, an
/// empty name, a nested one or a path through `..` names no command the
/// grammar knows. `cd` is a shell builtin, and the `/usr/bin/cd` a system
/// ships runs in a child process and moves nobody, so it is never a leading
/// `cd`. A label and the narrowing commands after a pipe keep their bare
/// spelling.
#[test]
fn a_command_spelled_from_any_other_directory_is_left_alone() {
    for command in [
        "/usr/local/bin/sed -n '1,9p' src/lib.rs",
        "/opt/homebrew/bin/grep -n needle src/lib.rs",
        "/usr/local/bin/ggrep -n needle src/lib.rs",
        "/usr/bin/ggrep -n needle src/lib.rs",
        "./cat src/lib.rs",
        "bin/cat src/lib.rs",
        "usr/bin/cat src/lib.rs",
        "/usr/bin/ src/lib.rs",
        "/bin/ src/lib.rs",
        "/usr/bin/x/cat src/lib.rs",
        "/usr/bin/../bin/cat src/lib.rs",
        "/usr//bin/cat src/lib.rs",
        "//bin/cat src/lib.rs",
        "/sbin/cat src/lib.rs",
        "'/bin/cat' src/lib.rs",
        "\"/usr/bin/sed\" -n '1,9p' src/lib.rs",
        "/usr/bin/cd crates && cat src/lib.rs",
        "/bin/cd crates && cat src/lib.rs",
        "cat src/lib.rs | /usr/bin/head -n 5",
        "cat src/lib.rs | /usr/bin/tail -5",
        "/bin/echo hi && cat src/lib.rs",
        "cat src/lib.rs && /usr/bin/printf x",
        "/usr/bin/awk 'NR<5' src/lib.rs",
        "/usr/bin/sed 's/a/b/' src/lib.rs",
    ] {
        assert!(
            recognise(command).is_none(),
            "{command:?} is not recognised"
        );
    }
}
