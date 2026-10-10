//! The published vocabulary: every form the grammar spells, the range each
//! one supports, and which of them is a content search rather than a listing.

use crate::shell_read::{recognise, ShellCall, ShellSpan, MAX_COMMAND_BYTES};
use crate::{HiKind, ReadSpan, SearchKind};

use super::{assert_each_recognised, listing, member, read, read_of, reads_of};

/// **Every form the grammar spells, and the range each one supports.**
///
/// The table is the specification: a command on the published list yields
/// the span that list's semantics allow, and the two forms whose lines
/// depend on the file yield a recognition without a range rather than a
/// range that would be a guess.
#[test]
fn every_grammar_form_yields_the_pinned_range() {
    let file = "src/lib.rs";
    let cases: &[(&str, ShellCall)] = &[
        ("cat src/lib.rs", read(file, ShellSpan::WholeFile)),
        ("nl src/lib.rs", read(file, ShellSpan::DecoratedWholeFile)),
        ("bat src/lib.rs", read(file, ShellSpan::DecoratedWholeFile)),
        (
            "batcat src/lib.rs",
            read(file, ShellSpan::DecoratedWholeFile),
        ),
        (
            "head src/lib.rs",
            read(file, ShellSpan::FromTop { limit: None }),
        ),
        (
            "head -n 40 src/lib.rs",
            read(file, ShellSpan::FromTop { limit: Some(40) }),
        ),
        (
            "tail src/lib.rs",
            read(file, ShellSpan::FromBottom { limit: None }),
        ),
        (
            "tail -n 40 src/lib.rs",
            read(file, ShellSpan::FromBottom { limit: Some(40) }),
        ),
        (
            "sed -n '12,48p' src/lib.rs",
            read(file, ShellSpan::Lines { lo: 12, hi: 48 }),
        ),
        (
            "sed -n 12,48p src/lib.rs",
            read(file, ShellSpan::Lines { lo: 12, hi: 48 }),
        ),
        ("grep needle src/lib.rs", read(file, ShellSpan::Matching)),
        ("egrep 'a|b' src/lib.rs", read(file, ShellSpan::Matching)),
        ("fgrep needle src/lib.rs", read(file, ShellSpan::Matching)),
        ("rg needle src/lib.rs", read(file, ShellSpan::Matching)),
        ("grep -n needle src/lib.rs", read(file, ShellSpan::Matching)),
        (
            "grep -n -A 3 needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        ("rg -n needle src/lib.rs", read(file, ShellSpan::Matching)),
        ("grep -c needle src/lib.rs", read(file, ShellSpan::Matching)),
        ("ls crates", listing("crates", None)),
        ("ls -la crates", listing("crates", None)),
        ("ls -l -a crates", listing("crates", None)),
        ("find crates -name '*.rs'", listing("crates", Some("*.rs"))),
        (
            "find crates -type f -name '*.rs'",
            listing("crates", Some("*.rs")),
        ),
        ("rg --files", listing(".", None)),
        ("rg --files crates", listing("crates", None)),
        // A quoted word is the word the shell passes, in a path as anywhere.
        ("cat 'src/lib.rs'", read(file, ShellSpan::WholeFile)),
        (
            "grep -n \"a\\|b\" \"src/lib.rs\"",
            read(file, ShellSpan::Matching),
        ),
        // A command word spelled from the system's own directory is that
        // command.
        (
            "/usr/bin/sed -n '12,48p' src/lib.rs",
            read(file, ShellSpan::Lines { lo: 12, hi: 48 }),
        ),
        ("/bin/cat src/lib.rs", read(file, ShellSpan::WholeFile)),
        // A search over several named files is a search of each of them.
        (
            "grep -n needle src/lib.rs src/main.rs",
            ShellCall::Read(vec![
                member(file, ShellSpan::Matching),
                member("src/main.rs", ShellSpan::Matching),
            ]),
        ),
    ];
    assert_each_recognised(cases);
}

/// The ranges those spans support, which is what a coverage record is
/// written from — one line where the end is unpinned, the counted line
/// where `-n` or a `sed` script pins it.
#[test]
fn a_pinned_span_yields_the_range_a_record_is_written_from() {
    let file = "src/lib.rs".to_owned();
    assert_eq!(
        read_of("cat src/lib.rs"),
        Some((
            file.clone(),
            ReadSpan::lines(1, HiKind::DefaultWindow(HiKind::END_OF_FILE))
        ))
    );
    assert_eq!(
        read_of("head -n 40 src/lib.rs"),
        Some((file.clone(), ReadSpan::lines(1, HiKind::Explicit(40))))
    );
    // A *counted* `tail` is an exact range whose line numbers only the
    // file's length names, so the count travels and the step that holds
    // the bytes resolves it.
    assert_eq!(
        read_of("tail -n 40 src/lib.rs"),
        Some((file.clone(), ReadSpan::LastLines { count: 40 }))
    );
    assert_eq!(
        read_of("sed -n '12,48p' src/lib.rs"),
        Some((file, ReadSpan::lines(12, HiKind::Explicit(48))))
    );
    // A decorated print and a default window support no record at all, so
    // there is no request to write one from.
    for command in ["nl src/lib.rs", "head src/lib.rs", "tail src/lib.rs"] {
        assert_eq!(read_of(command), None, "{command}");
        assert_eq!(
            reads_of(command),
            vec![("src/lib.rs".to_owned(), ReadSpan::Unknown)],
            "{command} is still a read of that file"
        );
    }
}

/// A content search names its file and reads none of it: the grammar is
/// what names the path, and `tail` — the other command whose lines nobody
/// can state — is not a search and names nothing.
#[test]
fn a_content_search_names_the_one_file_it_searched() {
    for command in [
        "grep needle src/lib.rs",
        "egrep 'a|b' src/lib.rs",
        "fgrep needle src/lib.rs",
        "rg needle src/lib.rs",
        "grep -n needle src/lib.rs",
        "grep -in -C 2 needle src/lib.rs",
        "rg -n needle src/lib.rs",
    ] {
        assert_eq!(
            recognise(command).and_then(|call| call.searched_file().map(str::to_owned)),
            Some("src/lib.rs".to_owned()),
            "{command}"
        );
    }
    for command in [
        "tail -n 40 src/lib.rs",
        "cat src/lib.rs",
        "sed -n '12,48p' src/lib.rs",
        "ls crates",
    ] {
        assert_eq!(
            recognise(command).and_then(|call| call.searched_file().map(str::to_owned)),
            None,
            "{command}"
        );
    }
}

/// **A search over one file with flags that only choose and number its
/// lines is still a search over that file.**
///
/// Every flag here changes which of the file's own lines are printed, or
/// how they are labelled — a line number, a case fold, a whole-word or
/// whole-line match, a fixed string, the extended dialect, a few lines of
/// context around each match, a pattern given with `-e` — and none reads
/// anything but the one file the line names. So each spelling is the same
/// recognition the bare form gets: a content search of that file, with no
/// coverage, because the lines it printed are the file's to decide.
#[test]
fn a_single_file_search_with_line_selecting_flags_is_a_content_search() {
    let file = "src/lib.rs";
    let cases: &[(&str, ShellCall)] = &[
        ("grep -n needle src/lib.rs", read(file, ShellSpan::Matching)),
        (
            "grep -i -w needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        (
            "grep -in needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        ("grep -nF 'a.b' src/lib.rs", read(file, ShellSpan::Matching)),
        ("grep -nE 'a|b' src/lib.rs", read(file, ShellSpan::Matching)),
        ("grep -x needle src/lib.rs", read(file, ShellSpan::Matching)),
        (
            "grep -n -A 3 needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        (
            "grep -n -B 2 -A 8 needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        (
            "grep -A3 -n needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        (
            "grep -nA3 needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        (
            "grep -nC 2 needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        (
            "grep -n -C 0 needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        (
            "grep -n -e needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        ("grep -e '-v' src/lib.rs", read(file, ShellSpan::Matching)),
        ("grep -e a -e b src/lib.rs", read(file, ShellSpan::Matching)),
        (
            "grep -ne needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        ("egrep -n 'a|b' src/lib.rs", read(file, ShellSpan::Matching)),
        (
            "fgrep -in needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        ("rg -n needle src/lib.rs", read(file, ShellSpan::Matching)),
        (
            "rg -n -i -F needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        (
            "rg -n -C 3 needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
    ];
    assert_each_recognised(cases);
    for (command, _) in cases {
        assert!(read_of(command).is_none(), "{command} grants no coverage");
        assert!(
            recognise(command).is_some_and(|call| call.search_request().is_none()),
            "{command} is never a listing"
        );
    }
}

/// **An option written after the pattern or the path is still an option.**
///
/// The search tools an agent's shell runs read their arguments with
/// permutation: an option counts wherever it stands before `--`, and every
/// word after `--` is an operand. So the late spelling is the same search of
/// the same one file as the early one, recognised the same way and with no
/// coverage.
#[test]
fn a_search_reads_its_options_wherever_they_stand_before_the_end_marker() {
    let file = "src/lib.rs";
    let cases: &[(&str, ShellCall)] = &[
        (
            "grep -n needle -A 40 src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        ("grep needle -n src/lib.rs", read(file, ShellSpan::Matching)),
        ("grep needle src/lib.rs -n", read(file, ShellSpan::Matching)),
        (
            "grep needle src/lib.rs -nA3",
            read(file, ShellSpan::Matching),
        ),
        (
            "grep -n 'fn main' src/lib.rs -B 2 -A 8",
            read(file, ShellSpan::Matching),
        ),
        ("grep src/lib.rs -e needle", read(file, ShellSpan::Matching)),
        (
            "grep -n -- needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        (
            "grep needle -i -- src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        ("egrep 'a|b' src/lib.rs -n", read(file, ShellSpan::Matching)),
        ("rg needle src/lib.rs -n", read(file, ShellSpan::Matching)),
        (
            "rg needle -n -C 3 src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
    ];
    assert_each_recognised(cases);
    for (command, _) in cases {
        assert!(read_of(command).is_none(), "{command} grants no coverage");
    }
}

/// A flagged search composes as a bare one does: under a leading `cd`, and
/// narrowed by a trailing pipe, with still no coverage to give.
#[test]
fn a_flagged_search_composes_as_a_bare_one_does() {
    let command = "cd crates && grep -n needle src/lib.rs | head -n 5";
    assert_each_recognised(&[(
        command,
        ShellCall::Read(vec![super::narrowed(
            "crates/src/lib.rs",
            ShellSpan::Matching,
            crate::shell_read::Narrowing::First(5),
        )]),
    )]);
    assert!(read_of(command).is_none(), "{command} grants no coverage");
}

/// **A count of the matches in one file is a search of that file.**
///
/// `-c` prints how many lines matched instead of the lines, which is still a
/// number only the one file's bytes decide, and still no interval of it. So
/// it is recognised exactly as the search it counts, with no coverage, and
/// it combines with the other pinned flags wherever they stand.
#[test]
fn a_count_of_the_matches_in_one_file_is_a_search_of_it() {
    let file = "src/lib.rs";
    let cases: &[(&str, ShellCall)] = &[
        ("grep -c needle src/lib.rs", read(file, ShellSpan::Matching)),
        (
            "grep -ci needle src/lib.rs",
            read(file, ShellSpan::Matching),
        ),
        ("grep -cE 'a|b' src/lib.rs", read(file, ShellSpan::Matching)),
        ("grep needle src/lib.rs -c", read(file, ShellSpan::Matching)),
        ("egrep -c 'a|b' src/lib.rs", read(file, ShellSpan::Matching)),
        ("rg -c needle src/lib.rs", read(file, ShellSpan::Matching)),
    ];
    assert_each_recognised(cases);
    for (command, _) in cases {
        assert!(read_of(command).is_none(), "{command} grants no coverage");
    }
}

/// **A command whose lines depend on the file grants no coverage.**
///
/// `grep` prints what matched and a `tail` with no count prints however
/// much its own default is; neither position follows from the command
/// line. Recording the line the file starts on would say the window holds
/// line one — the line those are least likely to have printed — and a
/// later request for it could then be declined against text nobody was
/// ever handed.
///
/// A **counted** `tail` is deliberately not on this list any more. Its
/// range is exactly as pinned as `head -n N`'s; what it lacked was a way
/// to say "N lines, ending at the last one" to a step that had not yet
/// opened the file, and [`ReadSpan::LastLines`] is that way.
#[test]
fn a_read_whose_lines_depend_on_the_file_grants_no_coverage() {
    for command in [
        "tail src/lib.rs",
        "head src/lib.rs",
        "nl src/lib.rs",
        "grep needle src/lib.rs",
        "rg needle src/lib.rs",
        "grep -n needle src/lib.rs",
        "grep -n -A 3 needle src/lib.rs",
        "rg -n needle src/lib.rs",
    ] {
        assert!(recognise(command).is_some(), "{command} is recognised");
        assert!(read_of(command).is_none(), "{command} grants no coverage");
    }
}

/// **A content search is never a listing**, however it is spelled — the
/// one substitution that would answer a question about content with an
/// answer about names.
#[test]
fn a_content_search_is_never_a_listing() {
    for command in [
        "grep needle src/lib.rs",
        "egrep needle src/lib.rs",
        "fgrep needle src/lib.rs",
        "rg needle src/lib.rs",
    ] {
        assert!(
            recognise(command)
                .and_then(|call| call.search_request())
                .is_none(),
            "{command}"
        );
    }
    // A directory argument, or a recursive flag, is not a listing either:
    // it is simply not in the grammar.
    for command in [
        "grep -r needle crates",
        "grep needle crates",
        "rg needle crates -r",
    ] {
        let call = recognise(command);
        assert!(
            call.as_ref().and_then(ShellCall::search_request).is_none(),
            "{command}"
        );
    }
    let listing = recognise("ls crates").and_then(|c| c.search_request());
    assert_eq!(
        listing.map(|s| s.kind),
        Some(SearchKind::Listing {
            dir: "crates".to_owned(),
            pattern: None
        })
    );
}

/// A command line longer than the cap is refused on its length, before
/// anything in it is classified.
#[test]
fn an_oversized_command_line_is_refused_on_its_length() {
    let long = format!("cat {}", "a".repeat(MAX_COMMAND_BYTES));
    assert!(recognise(&long).is_none());
    let fits = format!("cat {}", "a".repeat(MAX_COMMAND_BYTES - 4));
    assert!(recognise(&fits).is_some());
}

/// **A tilde inside a path word already under way is a letter.** Windows
/// writes a long directory name in its 8.3 short form — `RUNNER~1` — and a
/// shell expands a tilde only where it starts a word or follows the `=` or
/// `:` of an assignment, so in the middle of a name it reads as itself.
#[test]
fn a_tilde_inside_a_started_path_word_is_part_of_the_name() {
    assert_each_recognised(&[
        (
            "cat C:/Users/RUNNER~1/AppData/x.md",
            read("C:/Users/RUNNER~1/AppData/x.md", ShellSpan::WholeFile),
        ),
        (
            "head -n 5 /tmp/RUNNER~1/a.rs",
            read("/tmp/RUNNER~1/a.rs", ShellSpan::FromTop { limit: Some(5) }),
        ),
        ("cat a.rs~", read("a.rs~", ShellSpan::WholeFile)),
        ("ls PROGRA~1", listing("PROGRA~1", None)),
    ]);
}

/// **A command spelled from `/bin` or `/usr/bin` is the command itself.**
///
/// Those two directories hold the system's own copies of the vocabulary, and
/// an agent reaches for the full spelling to step around a shell alias or a
/// function of the same name. The full spelling names the program more
/// exactly than the bare word does, so each line reads exactly as its bare
/// form: the same reads, the same spans, the same coverage.
#[test]
fn a_command_spelled_from_a_system_directory_reads_as_the_bare_one() {
    for (spelled, bare) in [
        ("/bin/cat src/lib.rs", "cat src/lib.rs"),
        ("/usr/bin/head -n 40 src/lib.rs", "head -n 40 src/lib.rs"),
        ("/usr/bin/tail -n 40 src/lib.rs", "tail -n 40 src/lib.rs"),
        (
            "/usr/bin/sed -n '12,48p' src/lib.rs",
            "sed -n '12,48p' src/lib.rs",
        ),
        ("/usr/bin/nl src/lib.rs", "nl src/lib.rs"),
        (
            "/usr/bin/grep -n needle src/lib.rs | head -n 5",
            "grep -n needle src/lib.rs | head -n 5",
        ),
        ("/bin/ls -la crates", "ls -la crates"),
        (
            "/usr/bin/find crates -name '*.rs'",
            "find crates -name '*.rs'",
        ),
        (
            "cd crates && echo '=== a ===' && /usr/bin/sed -n '1,9p' a.rs",
            "cd crates && echo '=== a ===' && sed -n '1,9p' a.rs",
        ),
    ] {
        assert!(recognise(bare).is_some(), "{bare:?}");
        assert_eq!(recognise(spelled), recognise(bare), "{spelled:?}");
        assert_eq!(read_of(spelled), read_of(bare), "{spelled:?}");
    }
}

/// **A search over several named files is a search of each of them.**
///
/// With the same flags that only choose, number or count the lines, every
/// file the line names is searched for the same pattern and nothing else is
/// opened; the tool labels each match with the file it came from. So the
/// line is one content search per file, in the order it names them, each
/// recognised as a one-file search is: with no coverage. It is not the one
/// search an index could stand in for, so it names no single searched file.
#[test]
fn a_search_over_several_named_files_is_a_search_of_each() {
    let each = |paths: &[&str]| {
        ShellCall::Read(
            paths
                .iter()
                .map(|path| member(path, ShellSpan::Matching))
                .collect(),
        )
    };
    let cases: &[(&str, ShellCall)] = &[
        ("grep needle a.rs b.rs", each(&["a.rs", "b.rs"])),
        (
            "grep -n needle a.rs b.rs c.rs",
            each(&["a.rs", "b.rs", "c.rs"]),
        ),
        ("grep -n -A 3 'fn a' a.rs b.rs", each(&["a.rs", "b.rs"])),
        ("grep -c -e x -e y a.rs b.rs", each(&["a.rs", "b.rs"])),
        ("grep needle a.rs -n b.rs -C2", each(&["a.rs", "b.rs"])),
        ("grep -n -- needle a.rs b.rs", each(&["a.rs", "b.rs"])),
        ("egrep -n 'a|b' a.rs b.rs", each(&["a.rs", "b.rs"])),
        ("fgrep -i needle a.rs b.rs", each(&["a.rs", "b.rs"])),
        ("rg -n needle a.rs 'my b.rs'", each(&["a.rs", "my b.rs"])),
        (
            "cd crates && grep -n needle a.rs /r/b.rs",
            each(&["crates/a.rs", "/r/b.rs"]),
        ),
        ("/usr/bin/grep -n needle a.rs b.rs", each(&["a.rs", "b.rs"])),
    ];
    assert_each_recognised(cases);
    for (command, _) in cases {
        let call = recognise(command);
        assert!(read_of(command).is_none(), "{command} grants no coverage");
        assert_eq!(
            call.as_ref().and_then(ShellCall::searched_file),
            None,
            "{command}"
        );
        assert!(
            call.is_some_and(|call| call.search_request().is_none()),
            "{command} is never a listing"
        );
    }
}
