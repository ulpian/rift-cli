//! The witness the fuzz target asserts with: which targets a line names by
//! the value the shell passes for one of its words, and which it never names.

use crate::shell_read::recognise;

use super::shell_read_oracle::names_a_word;

/// Every target is witnessed by its line.
fn assert_witnessed(cases: &[(&str, &str)]) {
    for (target, command) in cases {
        assert!(names_a_word(target, command), "{target:?} in {command:?}");
    }
}

/// No target is witnessed by its line.
fn assert_unwitnessed(cases: &[(&str, &str)]) {
    for (target, command) in cases {
        assert!(!names_a_word(target, command), "{target:?} in {command:?}");
    }
}

/// **A quoted path is witnessed by the word the shell passes, never by its
/// spelling or a piece of it.**
#[test]
fn a_quoted_path_is_witnessed_by_its_value_and_not_its_spelling() {
    assert_witnessed(&[
        ("src/my file.rs", "cat 'src/my file.rs'"),
        ("src/my file.rs", "cat \"src/my file.rs\""),
        ("src/my file.rs", "grep -n \"a\\|b\" \"src/my file.rs\""),
        ("src/lib.rs", "head -n 5 'src/lib.rs'&&cat a.rs"),
        ("my dir", "rg --files 'my dir'"),
        ("my crate/a.rs", "cd \"my crate\" && cat a.rs"),
        ("my crate/my file.rs", "cd 'my crate' && cat 'my file.rs'"),
    ]);
    assert_unwitnessed(&[
        ("'src/my file.rs'", "cat 'src/my file.rs'"),
        ("\"src/my file.rs\"", "cat \"src/my file.rs\""),
        ("src/my", "cat 'src/my file.rs'"),
        ("file.rs", "cat \"src/my file.rs\""),
        ("my crate/a.rs", "cd \"my crate\" && cat '/a.rs'"),
    ]);
}

/// **Inside double quotes a backslash the shell keeps is witnessed as
/// written, and one it removes by the character it escaped.**
#[test]
fn a_double_quoted_backslash_is_witnessed_as_the_shell_reads_it() {
    assert_witnessed(&[
        ("src/a\\b.rs", "cat \"src/a\\b.rs\""),
        ("src/a\\b.rs", "cat \"src/a\\\\b.rs\""),
        ("src/a\"b.rs", "cat \"src/a\\\"b.rs\""),
        ("src/a\\b.rs", "cat 'src/a\\b.rs'"),
        ("dir\\x/a.rs", "cd \"dir\\x\" && cat a.rs"),
    ]);
    assert_unwitnessed(&[
        ("src/a\\\\b.rs", "cat \"src/a\\\\b.rs\""),
        ("src/a\\\"b.rs", "cat \"src/a\\\"b.rs\""),
        ("src/a\\", "cat \"src/a\\\" b.rs\""),
    ]);
}

/// **A double-quoted word holding `$`, a backtick or `!` names nothing**:
/// the shell computes that word, and the witness does not, so neither its
/// spelling nor any guess at its value is a witness — nor a directory a
/// `cd` could join a path under. Single quotes compute nothing.
#[test]
fn a_double_quoted_word_the_shell_computes_witnesses_nothing() {
    assert_unwitnessed(&[
        ("src/$x.rs", "cat \"src/$x.rs\""),
        ("src/.rs", "cat \"src/$x.rs\""),
        ("src/$x.rs", "cat \"src/\\$x.rs\""),
        ("src/`id`.rs", "cat \"src/`id`.rs\""),
        ("src/a!.rs", "cat \"src/a!.rs\""),
        ("$d/a.rs", "cd \"$d\" && cat a.rs"),
        ("/a.rs", "cd \"$d\" && cat a.rs"),
    ]);
    assert_witnessed(&[
        ("src/$x.rs", "cat 'src/$x.rs'"),
        ("src/a!.rs", "cat 'src/a!.rs'"),
    ]);
}

/// **An unquoted word the shell would expand is no witness**, except the
/// one spelling the grammar reports verbatim: a `~` inside a word and not
/// after `=` or `:`, which the shell passes as the letter it is.
#[test]
fn an_unquoted_word_the_shell_would_expand_witnesses_nothing() {
    assert_unwitnessed(&[
        ("src/*.rs", "cat src/*.rs"),
        ("src/a?.rs", "cat src/a?.rs"),
        ("src/[ab].rs", "cat src/[ab].rs"),
        ("src/{a,b}.rs", "cat src/{a,b}.rs"),
        ("~/a.rs", "cat ~/a.rs"),
        ("a=~/x", "cat a=~/x"),
        ("a:~/x", "cat a:~/x"),
        ("src/$x", "cat src/$x"),
        ("src/a\\ b.rs", "cat src/a\\ b.rs"),
        ("b.rs", "cat a.rs #b.rs"),
        ("c.rs", "cat a.rs # ; cat c.rs"),
    ]);
    let verbatim = "cat RUNNER~1/a.rs";
    assert!(recognise(verbatim).is_some(), "{verbatim:?}");
    assert_witnessed(&[("RUNNER~1/a.rs", verbatim)]);
}

/// **A search's pattern and an option's value are never a file**, quoted or
/// not, wherever the option stands; the file it searches still is.
#[test]
fn a_search_pattern_or_option_value_witnesses_nothing() {
    assert_unwitnessed(&[
        ("a.rs", "grep a.rs b.rs"),
        ("a.rs", "grep 'a.rs' b.rs"),
        ("a.rs", "rg \"a.rs\" b.rs"),
        ("a.rs", "grep -e a.rs b.rs"),
        ("a.rs", "grep -ea.rs b.rs"),
        ("a.rs", "grep -n -- 'a.rs' b.rs"),
        ("3", "grep -A 3 needle b.rs"),
        ("3", "grep needle b.rs -C 3"),
        ("a\\|b", "grep -n \"a\\|b\" \"src/my file.rs\""),
        ("a|b", "grep -n \"a\\|b\" \"src/my file.rs\""),
        ("a.rs", "grep b.rs -e a.rs"),
    ]);
    assert_witnessed(&[
        ("b.rs", "grep -e 'a.rs' -n 'b.rs'"),
        ("b.rs", "grep 'a.rs' 'b.rs' -A 3"),
        ("b.rs", "grep -A 3 'a.rs' b.rs"),
        ("b.rs", "grep -E 'a|b' 'b.rs'"),
        ("-b.rs", "grep -n -- needle -b.rs"),
        ("b.rs", "grep b.rs -e a.rs"),
        ("rep", "grep rep -etai,,,,"),
        (
            "crates/src/l",
            "cd crates && grep -in -e needle src/l| head -n 5",
        ),
    ]);
    // The line the fuzzer found: the pattern rides on a late `-e`, so the
    // one operand is the file the grammar reports.
    let late = "grep rep -etai,,,,";
    let call = recognise(late);
    assert_eq!(
        call.as_ref().and_then(|call| call.searched_file()),
        Some("rep")
    );
}

/// **Spans run together into one word are a shape neither side reads**:
/// the grammar refuses the line, and the witness names no target from it —
/// not the word the shell would pass, and not either span.
#[test]
fn adjacent_quoted_spans_are_refused_by_the_grammar_and_the_witness_alike() {
    for (command, value) in [
        ("cat src/\"a b\".rs", "src/a b.rs"),
        ("cat 'src/a'\"b.rs\"", "src/ab.rs"),
        ("cat \"src/lib.rs\"x", "src/lib.rsx"),
    ] {
        assert!(recognise(command).is_none(), "{command:?}");
        assert!(!names_a_word(value, command), "{command:?}");
    }
}

/// **A quoted command word is no command**: the grammar takes `cd`, `rg
/// --files` and a label only bare, so the witness joins nothing under a
/// quoted `cd` and implies no directory for a quoted `rg --files`.
#[test]
fn a_quoted_command_word_is_read_by_neither_side() {
    for (target, command) in [
        ("dir/a.rs", "\"cd\" dir && cat a.rs"),
        (".", "'rg' --files"),
        (".", "rg '--files'"),
    ] {
        assert!(recognise(command).is_none(), "{command:?}");
        assert!(!names_a_word(target, command), "{target:?} in {command:?}");
    }
}
