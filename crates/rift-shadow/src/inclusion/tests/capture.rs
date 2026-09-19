//! The document itself: what an opted-in file becomes, what the empty capture
//! is, and how the control file's lines are read into roots.

use crate::inclusion::{capture_document, hard_refusal, IncludeList, Refusal, INCLUDE_FILE};

use super::fixtures::{doc, tree};

#[test]
fn no_include_file_captures_nothing_at_all() {
    let dir = tree(&[("secret.env", "TOKEN=x")], None);
    assert!(
        capture_document(dir.path()).as_slice().is_empty(),
        "a repo that never opted in produces the byte-identical empty \
         capture it produced before this module existed"
    );
}

#[test]
fn an_empty_or_comment_only_list_captures_nothing() {
    let dir = tree(&[("a.txt", "x")], Some("# nothing here\n\n   \n"));
    assert!(capture_document(dir.path()).as_slice().is_empty());
}

#[test]
fn an_opted_in_file_is_captured_line_by_line() {
    let dir = tree(&[(".env", "A=1\nB=2\n")], Some(".env\n"));
    assert_eq!(doc(dir.path()), "rift-capture/1\nf .env\n A=1\n B=2\n \n");
}

#[test]
fn content_lines_rejoin_to_the_exact_bytes() {
    for body in ["", "a", "a\n", "a\nb", "a\nb\n", "\n\n"] {
        let dir = tree(&[("f.txt", body)], Some("f.txt\n"));
        let text = doc(dir.path());
        let rejoined: Vec<&str> = text
            .lines()
            .skip(2)
            .map(|l| l.strip_prefix(' ').unwrap())
            .collect();
        assert_eq!(rejoined.join("\n"), body, "round-trip for {body:?}");
    }
}

#[test]
fn a_binary_file_is_refused_rather_than_transcribed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("blob.bin"), [0xff_u8, 0xfe, 0x00, 0x01]).unwrap();
    std::fs::write(dir.path().join(INCLUDE_FILE), "blob.bin\n").unwrap();
    assert_eq!(doc(dir.path()), "rift-capture/1\nx blob.bin binary\n");
}

#[test]
fn a_missing_or_unsafe_entry_is_named_not_dropped() {
    let dir = tree(&[], Some("gone.txt\n../escape\n/abs\n"));
    let text = doc(dir.path());
    assert!(text.contains("x gone.txt missing"), "{text}");
    assert!(text.contains("x ../escape unsafe-path"), "{text}");
    assert!(text.contains("x /abs unsafe-path"), "{text}");
}

/// An included directory speaks only through its children, so an empty one
/// says nothing — and a repository whose include file names only empty
/// directories keeps the exact capture, and the exact checkpoint id, it had
/// before it opted in. Pinned in both shapes so the silence cannot be read
/// as the walk having failed to look.
#[test]
fn an_include_naming_only_an_empty_directory_records_nothing() {
    let alone = tree(&[], Some("empty/\n"));
    std::fs::create_dir_all(alone.path().join("empty")).unwrap();
    assert!(
        capture_document(alone.path()).as_slice().is_empty(),
        "an empty directory is not a refusal, and not a record either"
    );

    let beside = tree(&[("a.txt", "hi")], Some("a.txt\nempty/\n"));
    std::fs::create_dir_all(beside.path().join("empty")).unwrap();
    assert_eq!(
        doc(beside.path()),
        "rift-capture/1\nf a.txt\n hi\n",
        "and it stays silent in a document that has something to say"
    );
}

#[test]
fn the_document_is_deterministic_and_order_independent() {
    let a = tree(
        &[("d/b.txt", "bee"), ("d/a.txt", "ay")],
        Some("d/b.txt\nd/a.txt\n"),
    );
    let b = tree(
        &[("d/b.txt", "bee"), ("d/a.txt", "ay")],
        Some("d/a.txt\nd/b.txt\n"),
    );
    assert_eq!(doc(a.path()), doc(b.path()));
    assert_eq!(doc(a.path()), doc(a.path()), "and stable across runs");
    assert!(
        doc(a.path()).find("a.txt") < doc(a.path()).find("b.txt"),
        "sorted, not as typed"
    );
}

/// `./a.txt` is the same file as `a.txt` — to git, to the shell, and now
/// here. It used to earn `x ./a.txt unsafe-path` (the `.` segment), which
/// told a developer their line was dangerous when it was merely spelled the
/// long way. A bare `.` still refuses: that one really does mean "the whole
/// tree", which is the thing this module's default declines to capture.
#[test]
fn a_leading_dot_slash_is_stripped_but_a_bare_dot_still_refuses() {
    assert_eq!(
        IncludeList::parse("./a.txt\n.\\b.txt\n././c/\n.\n").roots(),
        [".", ".\\b.txt", "a.txt", "c"],
        "`./` is the OS's own no-op on both platforms and is stripped; \
         `.\\` is one on Windows only, so it is left to be refused rather \
         than rewritten into a file Unix would have read instead"
    );

    let dir = tree(&[("a.txt", "hi")], Some("./a.txt\n.\n"));
    let text = doc(dir.path());
    assert!(
        text.contains("f a.txt\n hi\n"),
        "captured under its real name, \
         not refused for how it was typed: {text}"
    );
    assert!(text.contains("x . unsafe-path"), "{text}");
}

/// A refusal record echoes the entry, so a spelling nobody typed must not be
/// the one that lands in a durable document: `.//a.txt` used to normalize to
/// `/a.txt` and be recorded as an absolute path the developer never wrote.
#[test]
fn repeated_separators_collapse_so_no_invented_path_is_echoed() {
    assert_eq!(
        IncludeList::parse(".//a.txt\nsrc//lib//x.rs\n").roots(),
        ["a.txt", "src/lib/x.rs"],
    );

    let dir = tree(&[("a.txt", "hi")], Some(".//a.txt\n"));
    let text = doc(dir.path());
    assert!(
        !text.contains("/a.txt unsafe-path"),
        "the shard must not echo a path nobody typed: {text}"
    );
    assert_eq!(text, "rift-capture/1\nf a.txt\n hi\n");
}

#[test]
fn parse_normalizes_comments_blanks_dedup_and_trailing_slashes() {
    // A directory written with and without its trailing `/` is one root, and
    // a repeated file is one entry. `src\` is NOT a third spelling of the
    // same root: it is a name Unix would resolve to a different file, so it
    // survives normalization and earns its own refusal.
    let list = IncludeList::parse("# a comment\n\n  b.txt  \nsrc/\nsrc\\\nb.txt\ncfg/\n");
    assert_eq!(
        list.roots(),
        ["b.txt", "cfg", "src", "src\\"],
        "trimmed, the portable separator stripped, deduplicated, sorted"
    );
    assert_eq!(hard_refusal("src\\"), Some(Refusal::UnsafePath));
}
