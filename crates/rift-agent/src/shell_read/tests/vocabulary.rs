//! The published vocabulary: every form the grammar spells, the range each
//! one supports, and which of them is a content search rather than a listing.

use crate::shell_read::{recognise, ShellCall, ShellSpan, MAX_COMMAND_BYTES};
use crate::{HiKind, ReadSpan, SearchKind};

use super::{assert_each_recognised, listing, read, read_of, reads_of};

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
