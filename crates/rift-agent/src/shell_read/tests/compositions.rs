//! The three compositions above the vocabulary — a leading `cd`, a trailing
//! narrowing pipe, a chain of reads — and the reads each one derives.

use crate::shell_read::{leading_cd, recognise, Narrowing, ShellCall, ShellSpan, MAX_CHAIN_READS};
use crate::{HiKind, ReadSpan};

use super::shell_read_oracle::names_a_word;
use super::{assert_each_recognised, listing, member, narrowed, read, read_of, reads_of};

/// **Every way the grammar composes those forms, and what a leading `cd`
/// makes of each path after it.**
///
/// The second half of the same specification: the vocabulary above is a
/// list of commands, and this is the list of shapes a command is written
/// in — the three the census in
/// `rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md`
/// (O8) found agents actually writing, and no fourth. The first shape is the
/// `cd`: every path after it is read from the directory it named, and a path
/// that is already rooted is what it says it is.
#[test]
fn a_leading_cd_puts_every_path_under_its_directory() {
    assert_each_recognised(&[
        // ---- the leading `cd`: every path after it is read from there ----
        (
            "cd crates && cat src/lib.rs",
            read("crates/src/lib.rs", ShellSpan::WholeFile),
        ),
        (
            "cd \"my crate\" && sed -n '12,48p' src/lib.rs",
            read("my crate/src/lib.rs", ShellSpan::Lines { lo: 12, hi: 48 }),
        ),
        (
            "cd 'my crate' && cat src/lib.rs",
            read("my crate/src/lib.rs", ShellSpan::WholeFile),
        ),
        (
            "cd /srv/repo && cat src/lib.rs",
            read("/srv/repo/src/lib.rs", ShellSpan::WholeFile),
        ),
        (
            "cd C:/repo && cat src/lib.rs",
            read("C:/repo/src/lib.rs", ShellSpan::WholeFile),
        ),
        // A rooted path is what it says it is; the `cd` told it nothing.
        (
            "cd crates && cat /etc/hosts",
            read("/etc/hosts", ShellSpan::WholeFile),
        ),
        (
            "cd crates && cat C:/repo/src/lib.rs",
            read("C:/repo/src/lib.rs", ShellSpan::WholeFile),
        ),
        (
            "cd C:/Users/RUNNER~1/repo && cat src/lib.rs",
            read("C:/Users/RUNNER~1/repo/src/lib.rs", ShellSpan::WholeFile),
        ),
        ("cd crates && ls src", listing("crates/src", None)),
        ("cd crates && rg --files", listing("crates/.", None)),
    ]);
}

/// **A leading `cd` joined by `;` is the same `cd`.**
///
/// The two joins differ only when the `cd` fails, and a `cd` fails only
/// where its directory cannot be entered — so every relative path read from
/// it names a file nobody can open, and the caller that resolves it records
/// nothing. Where the `cd` succeeds the lines are the `&&` lines exactly, so
/// each one is read as its `&&` spelling is, labels and all, and a rooted
/// path is still what it says it is.
#[test]
fn a_leading_cd_joined_by_a_semicolon_reads_as_the_and_join() {
    for (semicolon, and) in [
        ("cd crates; cat src/lib.rs", "cd crates && cat src/lib.rs"),
        (
            "cd '/w/my crate'; sed -n '1,40p' a.rs; sed -n '90,120p' b.rs",
            "cd '/w/my crate' && sed -n '1,40p' a.rs; sed -n '90,120p' b.rs",
        ),
        (
            "cd crates ; head -n 5 a.rs && grep -n x b.rs",
            "cd crates && head -n 5 a.rs && grep -n x b.rs",
        ),
        (
            "cd crates; echo '=== a ==='; cat a.rs",
            "cd crates && echo '=== a ===' && cat a.rs",
        ),
        (
            "echo start; cd crates; cat a.rs",
            "echo start && cd crates && cat a.rs",
        ),
        ("cd crates;cat /etc/hosts", "cd crates && cat /etc/hosts"),
        ("cd crates; ls src", "cd crates && ls src"),
    ] {
        assert!(recognise(and).is_some(), "{and:?}");
        assert_eq!(recognise(semicolon), recognise(and), "{semicolon:?}");
        assert_eq!(leading_cd(semicolon), leading_cd(and), "{semicolon:?}");
    }
}

/// **The trailing narrowing pipe**, the second shape: a `| head -N` or a
/// `| tail -N` in either spelling travels with the one read it follows,
/// whatever span that read had.
#[test]
fn a_trailing_pipe_narrows_the_one_read_it_follows() {
    let file = "src/lib.rs";
    assert_each_recognised(&[
        // ---- the trailing narrowing pipe ----
        (
            "cat src/lib.rs | head -n 5",
            ShellCall::Read(vec![narrowed(
                file,
                ShellSpan::WholeFile,
                Narrowing::First(5),
            )]),
        ),
        (
            "cat src/lib.rs | head -5",
            ShellCall::Read(vec![narrowed(
                file,
                ShellSpan::WholeFile,
                Narrowing::First(5),
            )]),
        ),
        (
            "cat src/lib.rs | tail -n 5",
            ShellCall::Read(vec![narrowed(
                file,
                ShellSpan::WholeFile,
                Narrowing::Last(5),
            )]),
        ),
        (
            "cat src/lib.rs | tail -5",
            ShellCall::Read(vec![narrowed(
                file,
                ShellSpan::WholeFile,
                Narrowing::Last(5),
            )]),
        ),
        (
            "sed -n '12,48p' src/lib.rs | head -n 5",
            ShellCall::Read(vec![narrowed(
                file,
                ShellSpan::Lines { lo: 12, hi: 48 },
                Narrowing::First(5),
            )]),
        ),
        (
            "grep needle src/lib.rs | head -n 5",
            ShellCall::Read(vec![narrowed(
                file,
                ShellSpan::Matching,
                Narrowing::First(5),
            )]),
        ),
    ]);
}

/// **Every chain the grammar joins, and the reads each one is.**
///
/// The third composition, and the one `cat` over several paths that is a
/// chain without a separator — together with the line that carries all
/// three compositions at once, because they are defined to compose.
#[test]
fn every_chain_yields_the_pinned_reads() {
    let file = "src/lib.rs";
    let cases: &[(&str, ShellCall)] = &[
        (
            "cat src/lib.rs src/main.rs",
            ShellCall::Read(vec![
                member(file, ShellSpan::WholeFile),
                member("src/main.rs", ShellSpan::WholeFile),
            ]),
        ),
        (
            "cat src/lib.rs && cat src/main.rs",
            ShellCall::Read(vec![
                member(file, ShellSpan::WholeFile),
                member("src/main.rs", ShellSpan::WholeFile),
            ]),
        ),
        (
            "cat src/lib.rs; cat src/main.rs",
            ShellCall::Read(vec![
                member(file, ShellSpan::WholeFile),
                member("src/main.rs", ShellSpan::WholeFile),
            ]),
        ),
        // ---- all three compositions on one line ----
        (
            "cd crates && head -n 40 src/lib.rs && sed -n '1,9p' src/main.rs | tail -n 2",
            ShellCall::Read(vec![
                member("crates/src/lib.rs", ShellSpan::FromTop { limit: Some(40) }),
                narrowed(
                    "crates/src/main.rs",
                    ShellSpan::Lines { lo: 1, hi: 9 },
                    Narrowing::Last(2),
                ),
            ]),
        ),
    ];
    assert_each_recognised(cases);
}

/// **The arithmetic of a narrowed read**, which is the whole of what the
/// trailing pipe adds: `head` takes a prefix of the run the member
/// produced and `tail` a suffix of it, and the cell that cannot be written
/// as one interval of the file — the *first* `N` of the *last* `M`, whose
/// start only the file's length names — is unknown rather than the nearest
/// interval that could be written.
#[test]
fn a_narrowing_pipe_cuts_the_range_the_member_produced() {
    let file = "src/lib.rs".to_owned();
    let cases: &[(&str, ReadSpan)] = &[
        (
            "cat src/lib.rs | head -n 5",
            ReadSpan::lines(1, HiKind::Explicit(5)),
        ),
        (
            "cat src/lib.rs | tail -n 5",
            ReadSpan::LastLines { count: 5 },
        ),
        // The tighter of the two counts wins, because both are ceilings.
        (
            "head -n 40 src/lib.rs | head -n 5",
            ReadSpan::lines(1, HiKind::Explicit(5)),
        ),
        (
            "head -n 5 src/lib.rs | head -n 40",
            ReadSpan::lines(1, HiKind::Explicit(5)),
        ),
        // The last 5 of the first 40 is a plain interval: 36 through 40.
        (
            "head -n 40 src/lib.rs | tail -n 5",
            ReadSpan::lines(36, HiKind::Explicit(40)),
        ),
        // …and never starts before the first line the member produced.
        (
            "head -n 3 src/lib.rs | tail -n 900",
            ReadSpan::lines(1, HiKind::Explicit(3)),
        ),
        (
            "sed -n '12,48p' src/lib.rs | head -n 5",
            ReadSpan::lines(12, HiKind::Explicit(16)),
        ),
        (
            "sed -n '12,48p' src/lib.rs | tail -n 5",
            ReadSpan::lines(44, HiKind::Explicit(48)),
        ),
        (
            "sed -n '12,48p' src/lib.rs | head -n 900",
            ReadSpan::lines(12, HiKind::Explicit(48)),
        ),
        (
            "tail -n 40 src/lib.rs | tail -n 5",
            ReadSpan::LastLines { count: 5 },
        ),
        (
            "tail -n 5 src/lib.rs | tail -n 40",
            ReadSpan::LastLines { count: 5 },
        ),
    ];
    for (command, expected) in cases {
        assert_eq!(
            read_of(command),
            Some((file.clone(), expected.clone())),
            "{command}"
        );
    }
    // The one cell with no interval: a count in from the end is a start
    // only the file's length names, and no span here says that.
    assert_eq!(read_of("tail -n 40 src/lib.rs | head -n 5"), None);
    // A member whose own window is unpinned stays unpinned however it is
    // cut: a prefix of an unknown run is still unknown.
    for command in [
        "head src/lib.rs | head -n 5",
        "tail src/lib.rs | tail -n 5",
        "nl src/lib.rs | head -n 5",
        "grep needle src/lib.rs | head -n 5",
    ] {
        assert!(recognise(command).is_some(), "{command} is recognised");
        assert_eq!(read_of(command), None, "{command} grants no coverage");
    }
}

/// **A command line that reads several files answers with several reads**,
/// in the order it performs them — and the members whose lines nobody can
/// name are in that list too, as unknown spans, because a caller deciding
/// about the whole line has to see the part it cannot answer for.
#[test]
fn a_composed_line_yields_one_request_per_read() {
    assert_eq!(
        reads_of("cd crates && cat a.rs && grep needle b.rs && sed -n '1,9p' c.rs"),
        vec![
            (
                "crates/a.rs".to_owned(),
                ReadSpan::lines(1, HiKind::DefaultWindow(HiKind::END_OF_FILE))
            ),
            ("crates/b.rs".to_owned(), ReadSpan::Unknown),
            (
                "crates/c.rs".to_owned(),
                ReadSpan::lines(1, HiKind::Explicit(9))
            ),
        ]
    );
    // The singular answer is the first read that supports coverage, which
    // for a one-command line is the only one it ever was.
    assert_eq!(
        read_of("cd crates && grep needle b.rs && sed -n '1,9p' c.rs"),
        Some((
            "crates/c.rs".to_owned(),
            ReadSpan::lines(1, HiKind::Explicit(9))
        ))
    );
    // A listing performs no reads at all, chained or not.
    assert!(reads_of("ls crates").is_empty());
}

/// A chain longer than the cap is not read-shaped: the cap bounds both the
/// work a hostile line can ask for and the length of the sentence a
/// decline of it would have to render.
#[test]
fn a_chain_past_the_cap_is_left_alone() {
    let paths: Vec<String> = (0..MAX_CHAIN_READS).map(|n| format!("f{n}.rs")).collect();
    let fits = format!("cat {}", paths.join(" "));
    assert_eq!(reads_of(&fits).len(), MAX_CHAIN_READS);
    let over = format!("{fits} f.rs");
    assert!(recognise(&over).is_none(), "one read past the cap");
    let chained = (0..=MAX_CHAIN_READS)
        .map(|n| format!("cat f{n}.rs"))
        .collect::<Vec<_>>()
        .join(" && ");
    assert!(recognise(&chained).is_none(), "one member past the cap");
}

/// The paths a recognised read reports, in order.
fn paths_of(command: &str) -> Vec<String> {
    let Some(ShellCall::Read(reads)) = recognise(command) else {
        panic!("{command:?} is recognised as a read");
    };
    reads.into_iter().map(|read| read.path).collect()
}

/// **The two lines the fuzzer once failed on are reads of words they
/// wrote.** Joined with no space around the operator, or spaced around a
/// `;`, each target is a complete word of the line — which the earlier
/// witness, splitting on whitespace alone, could not see.
#[test]
fn the_preserved_fuzz_crashes_report_only_complete_words() {
    for (command, expected) in [
        ("tail lnrg/,&&nl %//", ["lnrg/,", "%//"]),
        ("  nl n::::j;   nl n", ["n::::j", "n"]),
    ] {
        assert_eq!(paths_of(command), expected, "{command:?}");
        for path in expected {
            assert!(names_a_word(path, command), "{path:?} in {command:?}");
        }
        assert!(!names_a_word("invented/n", command), "{command:?}");
    }
}

/// **A target is witnessed by a whole word, the implicit `.` of exactly
/// `rg --files`, or a join under a genuinely leading `cd` — never by a
/// prefix, a suffix, a quoted piece or an operator.**
#[test]
fn the_witness_needs_a_complete_word_or_a_named_leading_directory() {
    for (target, command) in [
        ("a.rs", "cat a.rs&&nl b.rs"),
        ("b.rs", "cat a.rs;nl b.rs"),
        ("a.rs", "cat a.rs|head -n 2"),
        ("my crate/a.rs", "cd 'my crate'&&cat a.rs"),
        ("dir/a.rs", "echo label&&cd dir/&&cat a.rs"),
        ("dir/a.rs", "cd dir;cat a.rs"),
        ("dir/a.rs", "echo label;cd dir;cat a.rs"),
        ("/a.rs", "cd dir&&cat /a.rs"),
        ("C:/a.rs", "cd dir&&cat C:/a.rs"),
        ("dir/1:a.rs", "cd dir&&cat 1:a.rs"),
        (".", "rg --files"),
        ("dir/.", "cd dir&&rg --files"),
    ] {
        assert!(names_a_word(target, command), "{target:?} in {command:?}");
    }
    for (target, command) in [
        ("invented/a.rs", "cat a.rs"),
        ("invented/.", "rg --files"),
        ("dir/a.rs", "cat first&&cd dir&&cat a.rs"),
        ("dir/a.rs", "cat first;cd dir;cat a.rs"),
        ("dir/a.rs", "cd dir|cat a.rs"),
        ("dir//a.rs", "cd dir&&cat /a.rs"),
        ("dir/C:/a.rs", "cd dir&&cat C:/a.rs"),
        ("a.rs", "cat larger-a.rs"),
        ("a.rs", "cat a.rs.bak"),
        ("a.rs", "grep 'a.rs' b.rs"),
        ("a.rs", "cat 'a.rs"),
        ("&&", "grep '&&' b.rs"),
        (".", "rg --files --hidden"),
        (".", "rg --files dir"),
        (".", "rg --files-with-matches needle"),
        ("dir/.", "cd dir&&rg --files --hidden"),
    ] {
        assert!(!names_a_word(target, command), "{target:?} in {command:?}");
    }
}

/// **A drive letter is ASCII and a colon, and only that is rooted.** The
/// recogniser's reading and the witness's agree on both sides of it, and
/// the two `rg` shapes with extra flags stay out of the grammar.
#[test]
fn drive_like_paths_join_exactly_when_the_letter_is_not_a_drive() {
    for (command, expected) in [
        ("cd dir&&cat C:/a.rs", "C:/a.rs"),
        ("cd dir&&cat C:a.rs", "C:a.rs"),
        ("cd dir&&cat 1:a.rs", "dir/1:a.rs"),
        ("cd dir&&cat _:a.rs", "dir/_:a.rs"),
    ] {
        assert_eq!(paths_of(command), [expected], "{command:?}");
        assert!(names_a_word(expected, command), "{command:?}");
    }
    for command in [
        "rg --files --hidden",
        "rg --files --hidden dir",
        "rg --hidden --files",
    ] {
        assert!(recognise(command).is_none(), "{command:?}");
    }
}

/// **An operator inside a quoted pattern splits nothing.** The pattern
/// stays one quoted word, the file is the one bare witness, and no piece of
/// the pattern names a target.
#[test]
fn quoted_operators_in_a_pattern_witness_no_target() {
    for command in [
        "grep 'invented/a.rs&&x;y|z' real.rs",
        "rg 'invented/a.rs&&x;y|z' real.rs",
    ] {
        assert_eq!(paths_of(command), ["real.rs"], "{command:?}");
        assert!(names_a_word("real.rs", command), "{command:?}");
        for invented in ["invented/a.rs", "x", "y", "z", "&&", ";", "|"] {
            assert!(!names_a_word(invented, command), "{invented:?}");
        }
    }
}

/// **The directory a leading `cd` moved to is answered only for a line the
/// grammar reads.** It is spelled exactly as written, quoted or relative, and
/// a label in front of the `cd` does not hide it; a line the grammar leaves
/// alone, or one with no `cd`, names no directory at all.
#[test]
fn a_leading_cd_is_named_only_on_a_recognised_line() {
    for (command, expected) in [
        ("cd /w/member && sed -n '1,40p' src/a.rs", "/w/member"),
        ("cd ../x && cat a.rs", "../x"),
        ("cd '/w/a b' && ls src", "/w/a b"),
        ("echo one && cd /w/member && cat a.rs", "/w/member"),
    ] {
        assert_eq!(
            leading_cd(command).as_deref(),
            Some(expected),
            "{command:?}"
        );
    }
    for command in [
        "cd /w/member && cargo test",
        "cd /w/member",
        "sed -n '1,40p' src/a.rs",
        "cat a.rs && cd /w/member && cat b.rs",
    ] {
        assert_eq!(leading_cd(command), None, "{command:?}");
    }
}
