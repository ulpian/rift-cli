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
/// An escape the shell resolves inside double quotes (`\$`, `` \` ``, `\"`,
/// `\\`), a backslash with nothing after it, an expansion, a `!`, and a
/// quoted path with a glob or a `~` in it — each is a line whose word the
/// grammar would have to compute, or one whose path is not what the agent
/// most likely meant, so none is named.
#[test]
fn a_quoted_word_the_shell_would_compute_is_left_alone() {
    for command in [
        "grep \"a\\$b\" src/lib.rs",
        "grep \"a\\`b\" src/lib.rs",
        "grep \"a\\\"b\" src/lib.rs",
        "grep \"a\\\\b\" src/lib.rs",
        "grep \"ab\\\" src/lib.rs",
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
    let piece = proptest::sample::select(
        &[
            "a", "b.rs", "/", " ", "\\", "\\|", "\\w", "\\$", "\\\"", "\\\\", "\\`", "$", "`", "!",
            "'", "\"", "*", "~", "-", "|", ";",
        ][..],
    );
    (
        proptest::sample::select(&["'", "\""][..]),
        proptest::collection::vec(piece, 1..6),
    )
        .prop_map(|(mark, pieces)| format!("{mark}{}{mark}", pieces.concat()))
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
}
