//! Quoted words: the spellings the shell resolves to one fixed word, the
//! reads they name, and the spellings left alone because the shell would
//! compute the word rather than pass it.

use crate::shell_read::{recognise, ShellCall, ShellSpan};
use crate::{HiKind, ReadSpan};

use super::{assert_each_recognised, read, read_of};

/// **A quoted path names the file the shell opens, and a read of it is
/// credited exactly as a bare one is.**
///
/// A single-quoted word is its text. A double-quoted word with no `$`, no
/// backtick and no `!` is its text too, with every backslash kept, because
/// a backslash before an ordinary character is passed through as written —
/// which is what lets `grep -n "a\|b"` in.
#[test]
fn a_quoted_word_is_the_word_the_shell_passes() {
    let spaced = "src/my file.rs";
    assert_each_recognised(&[
        ("cat 'src/my file.rs'", read(spaced, ShellSpan::WholeFile)),
        ("cat \"src/my file.rs\"", read(spaced, ShellSpan::WholeFile)),
        (
            "cat 'src/a\\b.rs'",
            read("src/a\\b.rs", ShellSpan::WholeFile),
        ),
        (
            "head -n 5 \"src/lib.rs\"",
            read("src/lib.rs", ShellSpan::FromTop { limit: Some(5) }),
        ),
        (
            "sed -n '1,5p' 'src/my file.rs'",
            read(spaced, ShellSpan::Lines { lo: 1, hi: 5 }),
        ),
        (
            "sed -n \"1,5p\" src/lib.rs",
            read("src/lib.rs", ShellSpan::Lines { lo: 1, hi: 5 }),
        ),
        (
            "nl \"src/lib.rs\"",
            read("src/lib.rs", ShellSpan::DecoratedWholeFile),
        ),
        (
            "grep -n \"a\\|b\" src/lib.rs",
            read("src/lib.rs", ShellSpan::Matching),
        ),
        (
            "grep -n \"fn \\w+(\" \"src/lib.rs\"",
            read("src/lib.rs", ShellSpan::Matching),
        ),
        (
            "rg \"a\\.b\" src/lib.rs",
            read("src/lib.rs", ShellSpan::Matching),
        ),
    ]);
    assert_eq!(
        read_of("cat \"src/my file.rs\""),
        Some((
            spaced.to_owned(),
            ReadSpan::lines(1, HiKind::DefaultWindow(HiKind::END_OF_FILE))
        ))
    );
    assert_eq!(
        read_of("tail -n 5 'src/my file.rs'"),
        Some((spaced.to_owned(), ReadSpan::LastLines { count: 5 }))
    );
    assert_eq!(read_of("grep -n \"a\\|b\" \"src/lib.rs\""), None);
}

/// **A double-quoted word the shell would compute, and a quoted path that
/// spells a pattern, are left alone.**
///
/// An escaped `$`, backtick or line break inside double quotes, a backslash
/// with nothing after it, an expansion, a `!`, and a quoted path with a
/// glob, a `~` or any backslash in it — each is a line whose word the
/// grammar would have to compute, or one whose path is not what the agent
/// most likely meant, so none is named.
#[test]
fn a_quoted_word_the_shell_would_compute_is_left_alone() {
    for command in [
        "grep \"a\\$b\" src/lib.rs",
        "grep \"a\\`b\" src/lib.rs",
        "grep \"a\\\nb\" src/lib.rs",
        "grep \"ab\\\" src/lib.rs",
        "cat \"src/a\\\"b.rs\"",
        "cat \"src/a\\\\b.rs\"",
        "grep needle \"src/a\\\"b.rs\"",
        "echo \"\\\"a\\\"\" && cat src/lib.rs",
        "grep \"a\\\"b src/lib.rs",
        "grep \"a\\\"; cat src/lib.rs",
        "cat \"src/lib.rs\\\"",
        "grep \"a$b\" src/lib.rs",
        "grep \"a`id`b\" src/lib.rs",
        "grep \"a!b\" src/lib.rs",
        "cat \"src/lib!.rs\"",
        "cat \"src/*.rs\"",
        "cat 'src/*.rs'",
        "cat \"src/lib?.rs\"",
        "cat \"src/[ab].rs\"",
        "cat \"~/notes.md\"",
        "cat '-n'",
        "cat \"\"",
        "cat \"src/a\\b.rs\"",
        "cat 'src/lib.rs'x",
        "cat x\"src/lib.rs\"",
        "sed -n \"/a\\|b/p\" src/lib.rs",
    ] {
        assert!(
            recognise(command).is_none(),
            "{command:?} is not recognised"
        );
    }
}

/// **An escaped `"` or `\\` inside double quotes is the one character it
/// escapes.**
///
/// The shell resolves both the same way every time, so a pattern spelled
/// with them is one fixed word, and the search it belongs to is recognised as
/// any search is: with no coverage. An escaped quote does not close the
/// quote, so a separator after it is part of the pattern and never splits
/// the line.
#[test]
fn an_escaped_quote_or_backslash_is_the_character_it_escapes() {
    let lib = "src/lib.rs";
    assert_each_recognised(&[
        (
            "grep -n \"\\\"decision\\\"\" src/lib.rs",
            read(lib, ShellSpan::Matching),
        ),
        ("grep \"a\\\\b\" src/lib.rs", read(lib, ShellSpan::Matching)),
        (
            "rg -n \"fn a\\(\\\"x\\\"\\|\\\"y\\\"\" src/lib.rs | head -n 5",
            ShellCall::Read(vec![super::narrowed(
                lib,
                ShellSpan::Matching,
                crate::shell_read::Narrowing::First(5),
            )]),
        ),
        (
            "grep -n \"a\\\"; cat x\" src/lib.rs; cat src/main.rs",
            ShellCall::Read(vec![
                super::member(lib, ShellSpan::Matching),
                super::member("src/main.rs", ShellSpan::WholeFile),
            ]),
        ),
        (
            "cd \"my \\\"x\\\" dir\" && sed -n '1,5p' a.rs",
            read("my \"x\" dir/a.rs", ShellSpan::Lines { lo: 1, hi: 5 }),
        ),
    ]);
    assert_eq!(read_of("grep -n \"\\\"decision\\\"\" src/lib.rs"), None);
}

/// **A reference reading of one shell word, written from the POSIX rules
/// and not from the grammar.**
///
/// Outside quotes a backslash takes the next character as it is; inside
/// single quotes everything is literal; inside double quotes a backslash
/// escapes only `$`, a backtick, `"`, `\` and a newline and is kept before
/// anything else. Any `$` or backtick outside single quotes is an
/// expansion, for which there is no fixed word, so the answer is [`None`].
fn posix_word(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = word.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    inner => out.push(inner),
                }
            },
            '"' => loop {
                match chars.next()? {
                    '"' => break,
                    '$' | '`' => return None,
                    '\\' => match chars.peek() {
                        Some('$' | '`' | '"' | '\\' | '\n') => out.push(chars.next()?),
                        _ => out.push('\\'),
                    },
                    inner => out.push(inner),
                }
            },
            '\\' => out.push(chars.next()?),
            '$' | '`' => return None,
            _ => out.push(c),
        }
    }
    Some(out)
}

/// The pieces one quoted word is built from: every character that means
/// something inside a quote, and some that do not.
fn quoted_word() -> impl proptest::strategy::Strategy<Value = String> {
    const PIECES: [&str; 21] = [
        "a", "b.rs", "/", " ", "\\", "\\|", "\\w", "\\$", "\\\"", "\\\\", "\\`", "$", "`", "!",
        "'", "\"", "*", "~", "-", "|", ";",
    ];
    // The enclosing mark is not a piece of its own word: a bare `'` inside
    // `'…'` (or `"` inside `"…"`) ends the word there, and what follows is a
    // second word. The other mark stays, since it is literal inside.
    proptest::sample::select(&["'", "\""][..]).prop_flat_map(|mark| {
        let inside: Vec<&'static str> = PIECES.into_iter().filter(|piece| *piece != mark).collect();
        proptest::collection::vec(proptest::sample::select(inside), 1..6)
            .prop_map(move |pieces| format!("{mark}{}{mark}", pieces.concat()))
    })
}

/// A double-quoted pattern built only of escapes the grammar resolves and
/// of characters a shell would split on outside a quote.
fn resolvable_pattern() -> impl proptest::strategy::Strategy<Value = String> {
    let piece = proptest::sample::select(
        &[
            "a", " ", ";", "&&", "|", "'", "\\\"", "\\\\", "\\w", "\\|", "#",
        ][..],
    );
    proptest::collection::vec(piece, 1..8).prop_map(|pieces| format!("\"{}\"", pieces.concat()))
}

use proptest::strategy::Strategy as _;

proptest::proptest! {
    /// ★ Whatever path or directory the grammar reads off a quoted word is
    /// the word the POSIX rules say the shell passes — in the path position
    /// of a read, of a search, and in a leading `cd`.
    #[test]
    fn a_quoted_word_resolves_to_the_word_the_shell_passes(word in quoted_word()) {
        let reference = posix_word(&word);
        let under = |dir: &str| format!("{}/lib.rs", dir.trim_end_matches('/'));
        for (command, in_cd) in [
            (format!("cat {word}"), false),
            (format!("grep needle {word}"), false),
            (format!("sed -n '1,5p' {word}"), false),
            (format!("cd {word} && cat lib.rs"), true),
        ] {
            let Some(ShellCall::Read(reads)) = recognise(&command) else { continue };
            proptest::prop_assert!(
                reference.is_some(),
                "{command:?} was read, but the shell would expand it"
            );
            let expected = reference.as_deref().map(|r| if in_cd { under(r) } else { r.to_owned() });
            proptest::prop_assert_eq!(Some(reads[0].path.clone()), expected, "{}", command);
        }
    }

    /// ★ A pattern of resolvable escapes is one word wherever its quotes and
    /// separators fall: the search it belongs to is read, and so is the read
    /// chained after it — no more and no fewer.
    #[test]
    fn an_escaped_quote_never_moves_a_separator(pattern in resolvable_pattern()) {
        let command = format!("grep -n {pattern} src/lib.rs; cat src/main.rs");
        let expected = ShellCall::Read(vec![
            super::member("src/lib.rs", ShellSpan::Matching),
            super::member("src/main.rs", ShellSpan::WholeFile),
        ]);
        proptest::prop_assert_eq!(recognise(&command), Some(expected), "{}", command);
    }
}
