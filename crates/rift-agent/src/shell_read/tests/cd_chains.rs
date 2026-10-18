//! A `cd` anywhere in a line, and how sure the directory of each read after
//! it is.

use crate::shell_read::recognise;
use crate::ReadSpan;

use super::shell_read_oracle::names_a_word;
use super::{assert_each_recognised, listing, reads_of};

/// Each read of `command` as `(path, sure)`: whether the span is one a caller
/// may take as coverage.
fn sureness(command: &str) -> Vec<(String, bool)> {
    reads_of(command)
        .into_iter()
        .map(|(path, span)| (path, span != ReadSpan::Unknown))
        .collect()
}

/// **A `cd` the line is sure to have run moves every read after it**, the
/// second one under the first, a rooted one from the root; reads before it
/// stay where the shell started. A `;` is admitted as at the front: a `cd`
/// that failed names a directory no joined path can be opened under.
#[test]
fn a_cd_sure_to_have_run_moves_every_read_after_it() {
    for (command, expected) in [
        (
            "cd crates && cd rift-core && cat src/lib.rs",
            vec![("crates/rift-core/src/lib.rs", true)],
        ),
        (
            "cd crates; cd rift-core; cat src/lib.rs",
            vec![("crates/rift-core/src/lib.rs", true)],
        ),
        (
            "cat src/lib.rs && cd crates && cat src/main.rs",
            vec![("src/lib.rs", true), ("crates/src/main.rs", true)],
        ),
        (
            "cat a.rs; cd crates; cat b.rs && cd x && cat c.rs",
            vec![
                ("a.rs", true),
                ("crates/b.rs", true),
                ("crates/x/c.rs", true),
            ],
        ),
        (
            "echo go && cd crates && cd /abs && cat a.rs",
            vec![("/abs/a.rs", true)],
        ),
        (
            "cat a.rs || cat b.rs; cd crates; cat c.rs",
            vec![("a.rs", true), ("b.rs", false), ("crates/c.rs", true)],
        ),
    ] {
        let expected: Vec<(String, bool)> = expected
            .into_iter()
            .map(|(path, sure)| (path.to_owned(), sure))
            .collect();
        assert_eq!(sureness(command), expected, "{command:?}");
        for (path, _) in &expected {
            assert!(names_a_word(path, command), "{path:?} in {command:?}");
        }
    }
    assert_each_recognised(&[(
        "cd crates && cd rift-core && ls src",
        listing("crates/rift-core/src", None),
    )]);
}

/// **A `cd` the line may not reach leaves the directory of every later read
/// unknown**, so each such read is counted and never coverage — even one a
/// `;` would otherwise make sure, because which directory it opened from
/// depends on whether the search matched or the member failed.
#[test]
fn a_cd_the_line_may_not_reach_leaves_every_later_read_unknown() {
    for (command, expected) in [
        (
            "grep x a.rs && cd crates; cat b.rs",
            vec![("a.rs", false), ("crates/b.rs", false)],
        ),
        (
            "cat a.rs || cd crates; cat b.rs",
            vec![("a.rs", true), ("crates/b.rs", false)],
        ),
        (
            "cat c.rs; rg x a.rs && cd crates; cat b.rs; cd y; cat d.rs",
            vec![
                ("c.rs", true),
                ("a.rs", false),
                ("crates/b.rs", false),
                ("crates/y/d.rs", false),
            ],
        ),
    ] {
        let expected: Vec<(String, bool)> = expected
            .into_iter()
            .map(|(path, sure)| (path.to_owned(), sure))
            .collect();
        assert_eq!(sureness(command), expected, "{command:?}");
    }
}

/// **A `cd` whose directory is not a plain spelling, or one a fallback could
/// run around, still takes the whole line out of the grammar** — wherever in
/// the line it stands.
#[test]
fn a_cd_with_no_plain_directory_or_before_a_fallback_is_refused_anywhere() {
    for command in [
        "cat a.rs && cd - && cat b.rs",
        "cat a.rs && cd && cat b.rs",
        "cat a.rs; cd $HOME; cat b.rs",
        "cat a.rs && cd ~ && cat b.rs",
        "cat a.rs && cd -P crates && cat b.rs",
        "cat a.rs && cd crates other && cat b.rs",
        "cat a.rs && cd crates || cat b.rs",
        "cat a.rs; cd crates && cat b.rs || cat c.rs",
        "cd crates && cd rift-core",
        "cat a.rs && cd crates | cat b.rs",
        "cat a.rs && cd crates && ls src",
    ] {
        assert!(
            recognise(command).is_none(),
            "{command:?} is not recognised"
        );
    }
}
