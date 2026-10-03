//! An independent witness for the targets the shell recogniser reports.
//!
//! The recogniser's one promise worth fuzzing is that it never invents a
//! target: every path or directory it names was written on the line. This
//! file checks that promise without calling the recogniser's own lexer, so a
//! mistake there cannot also hide here — and it is std-only, because the fuzz
//! target includes this same file by path rather than keeping a second copy
//! that could drift from the one the unit tests exercise.
//!
//! A target is witnessed in exactly three ways: it is one complete unquoted
//! word of the line; it is the `.` of a member that is exactly `rg --files`;
//! or it is the lexical join of the directory a genuinely leading `cd` named
//! with one of those. A substring, a suffix of a word, a piece of a quoted
//! pattern or an operator is never a witness — the earlier check accepted
//! any target that ended in `/` plus a word, which let `invented/a.rs` pass
//! for `cat a.rs`.

/// One lexical token of a command line.
#[derive(Debug, PartialEq, Eq)]
enum Token {
    /// A whole word, and whether quotes produced it.
    Word { text: String, quoted: bool },
    /// An unquoted `&&`.
    And,
    /// An unquoted `;`.
    Semi,
    /// An unquoted `|`.
    Pipe,
}

/// Whether a reported target is one the line itself wrote.
pub(super) fn names_a_word(target: &str, command: &str) -> bool {
    let Some(tokens) = tokenize(command) else {
        return false;
    };
    let bare: Vec<&str> = tokens.iter().filter_map(bare_word).collect();
    if bare.contains(&target) || (target == "." && lists_implicitly(&tokens)) {
        return true;
    }
    let Some(dir) = leading_directory(&tokens) else {
        return false;
    };
    let under = |path: &str| format!("{}/{path}", dir.trim_end_matches('/'));
    if lists_implicitly(&tokens) && target == under(".") {
        return true;
    }
    bare.into_iter()
        .any(|path| !is_rooted(path) && target == under(path))
}

/// Split a line into whole words and the three unquoted operators, or
/// [`None`] when it is not a line of that shape — a lone `&`, a quote that
/// never closes, or a quote that does not wrap a whole word.
fn tokenize(command: &str) -> Option<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if is_boundary(c) {
            end_word(&mut tokens, &mut word);
            match c {
                '&' if chars.next_if_eq(&'&').is_some() => tokens.push(Token::And),
                '&' => return None,
                ';' => tokens.push(Token::Semi),
                '|' => tokens.push(Token::Pipe),
                _ => {}
            }
            continue;
        }
        if !matches!(c, '\'' | '"') {
            word.push(c);
            continue;
        }
        if !word.is_empty() {
            return None;
        }
        let text = quoted_text(&mut chars, c)?;
        if !chars.peek().copied().is_none_or(is_boundary) {
            return None;
        }
        tokens.push(Token::Word { text, quoted: true });
    }
    end_word(&mut tokens, &mut word);
    Some(tokens)
}

/// The text up to the closing `quote`, consuming it, or [`None`] when the
/// line ends first.
fn quoted_text(chars: &mut impl Iterator<Item = char>, quote: char) -> Option<String> {
    let mut text = String::new();
    for c in chars {
        if c == quote {
            return Some(text);
        }
        text.push(c);
    }
    None
}

/// Whether a character ends a word: whitespace, or the start of an operator.
fn is_boundary(c: char) -> bool {
    matches!(c, ' ' | '\t' | '&' | ';' | '|')
}

/// Push the word being built, if there is one.
fn end_word(tokens: &mut Vec<Token>, word: &mut String) {
    if !word.is_empty() {
        tokens.push(Token::Word {
            text: std::mem::take(word),
            quoted: false,
        });
    }
}

/// The text of an unquoted word token.
fn bare_word(token: &Token) -> Option<&str> {
    match token {
        Token::Word {
            text,
            quoted: false,
        } => Some(text),
        _ => None,
    }
}

/// The directory a genuinely leading `cd` named, stepping over the `echo`
/// and `printf` labels that may stand in front of it, each joined by `&&`.
fn leading_directory(tokens: &[Token]) -> Option<&str> {
    let mut rest = tokens;
    loop {
        let end = rest
            .iter()
            .position(|token| !matches!(token, Token::Word { .. }))?;
        let (head, tail) = rest.split_at(end);
        if tail.first() != Some(&Token::And) {
            return None;
        }
        let command = head.first().and_then(bare_word);
        match (command, head.get(1..)) {
            (Some("cd"), Some([Token::Word { text, .. }]))
                if !text.is_empty() && !text.starts_with('-') =>
            {
                return Some(text);
            }
            (Some("echo" | "printf"), _) => rest = tail.get(1..)?,
            _ => return None,
        }
    }
}

/// Whether some member of the line is exactly `rg --files`, the one form
/// whose directory is implied rather than written.
fn lists_implicitly(tokens: &[Token]) -> bool {
    tokens
        .split(|token| !matches!(token, Token::Word { .. }))
        .any(|member| {
            let words: Vec<_> = member.iter().map(bare_word).collect();
            words == [Some("rg"), Some("--files")]
        })
}

/// Whether a path is rooted, so a leading `cd` says nothing about it: a
/// POSIX root, or an ASCII drive letter and a colon.
fn is_rooted(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.first() == Some(&b'/')
        || (bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':'))
}
