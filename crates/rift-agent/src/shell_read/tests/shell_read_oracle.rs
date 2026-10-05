//! An independent witness for the targets the shell recogniser reports.
//!
//! The recogniser's one promise worth fuzzing is that it never invents a
//! target: every path or directory it names was written on the line. This
//! file checks that promise without calling the recogniser's own lexer or
//! its reading of a quote, so a mistake there cannot also hide here — and it
//! is std-only, because the fuzz target includes this same file by path
//! rather than keeping a second copy that could drift from the one the unit
//! tests exercise.
//!
//! A word is compared by the value the shell passes for it, worked out here
//! from the POSIX rules: a single-quoted word is its text; a double-quoted
//! one is its text with a backslash taken off before `"`, `\` or a newline
//! and kept before anything else; an unquoted one is its text. Where the
//! shell would compute the word instead — a `$`, a backtick or a `!` inside
//! double quotes, or an unquoted backslash, glob, brace, `$` or leading `~` —
//! the word has no value the witness knows, and it witnesses nothing. The
//! one unquoted spelling the grammar reports verbatim that looks like an
//! expansion is a `~` inside a word and not after `=` or `:`, and the shell
//! passes that as the letter it is, so it is a value here too.
//!
//! A target is witnessed in exactly three ways: it is the value of one
//! whole word of the line; it is the `.` of a member that is exactly
//! `rg --files`; or it is the lexical join of the directory a genuinely
//! leading `cd` named with one of those. A substring, a suffix of a word, a
//! piece of a quoted pattern or an operator is never a witness — the earlier
//! check accepted any target that ended in `/` plus a word, which let
//! `invented/a.rs` pass for `cat a.rs`. Neither is a search's pattern or the
//! value of one of its options: a search reads one file, and a pattern
//! reported as that file is exactly the mix-up a quoted pattern invites.
//!
//! Quoted spans run together into one word (`src/"a b".rs`) are a shape the
//! grammar refuses, so the witness refuses the whole line rather than
//! deciding what such a word would be.

/// One lexical token of a command line.
#[derive(Debug, PartialEq, Eq)]
enum Token {
    /// A whole word: the value the shell passes for it, or [`None`] when the
    /// shell would compute it, and whether quotes produced it.
    Word { value: Option<String>, quoted: bool },
    /// An unquoted `&&`.
    And,
    /// An unquoted `;`.
    Semi,
    /// An unquoted `|`.
    Pipe,
}

/// What the shell makes of a double-quoted span.
enum DoubleQuoted {
    /// The word it passes as it is.
    Passed(String),
    /// A word it computes, which the witness does not.
    Computed,
}

/// The short options of `grep`, `egrep` and `fgrep` that take a value.
const GREP_VALUE_LETTERS: &str = "efmABCdD";

/// The short options of `rg` that take a value; `-E` names an encoding there.
const RG_VALUE_LETTERS: &str = "efmABCEgtTMjrd";

/// The short options of every search that give the pattern itself.
const PATTERN_LETTERS: &str = "ef";

impl DoubleQuoted {
    /// The word's value, when the witness knows it.
    fn known(self) -> Option<String> {
        match self {
            DoubleQuoted::Passed(text) => Some(text),
            DoubleQuoted::Computed => None,
        }
    }
}

/// Whether a reported target is one the line itself wrote.
pub(super) fn names_a_word(target: &str, command: &str) -> bool {
    let Some(tokens) = tokenize(command) else {
        return false;
    };
    let witnesses = witness_values(&tokens);
    if witnesses.contains(&target) || (target == "." && lists_implicitly(&tokens)) {
        return true;
    }
    let Some(dir) = leading_directory(&tokens) else {
        return false;
    };
    let under = |path: &str| format!("{}/{path}", dir.trim_end_matches('/'));
    if lists_implicitly(&tokens) && target == under(".") {
        return true;
    }
    witnesses
        .into_iter()
        .any(|path| !is_rooted(path) && target == under(path))
}

/// Split a line into whole words and the three unquoted operators, or
/// [`None`] when it is not a line of that shape — a lone `&`, a quote that
/// never closes, or a quote that does not wrap a whole word. An unquoted
/// `#` that starts a word ends the line: the rest is a comment.
fn tokenize(command: &str) -> Option<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' && word.is_empty() {
            return Some(tokens);
        }
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
        let value = match c {
            '\'' => Some(single_quoted(&mut chars)?),
            _ => double_quoted(&mut chars)?.known(),
        };
        if !chars.peek().copied().is_none_or(is_boundary) {
            return None;
        }
        tokens.push(Token::Word {
            value,
            quoted: true,
        });
    }
    end_word(&mut tokens, &mut word);
    Some(tokens)
}

/// The text up to the closing `'`, consuming it, or [`None`] when the line
/// ends first. Nothing inside single quotes means anything to the shell.
fn single_quoted(chars: &mut impl Iterator<Item = char>) -> Option<String> {
    let mut text = String::new();
    for c in chars {
        if c == '\'' {
            return Some(text);
        }
        text.push(c);
    }
    None
}

/// The value of a double-quoted span up to its closing `"`, consuming it,
/// or [`None`] when the line ends first.
///
/// Any `$`, backtick or `!` makes the word computed, escaped or not — the
/// witness knows the value only of a span that holds none of them, which is
/// stricter than the shell and never looser. A backslash escapes `"`, `\`
/// and a newline (a line continuation, so both go) and is kept before
/// anything else.
fn double_quoted(chars: &mut impl Iterator<Item = char>) -> Option<DoubleQuoted> {
    let mut text = String::new();
    let mut computed = false;
    while let Some(c) = chars.next() {
        match c {
            '"' if computed => return Some(DoubleQuoted::Computed),
            '"' => return Some(DoubleQuoted::Passed(text)),
            '\\' => match chars.next()? {
                '\n' => {}
                escaped @ ('"' | '\\') => text.push(escaped),
                kept => {
                    computed |= matches!(kept, '$' | '`' | '!');
                    text.push('\\');
                    text.push(kept);
                }
            },
            _ => {
                computed |= matches!(c, '$' | '`' | '!');
                text.push(c);
            }
        }
    }
    None
}

/// Whether a character ends a word: whitespace, or the start of an operator.
fn is_boundary(c: char) -> bool {
    matches!(c, ' ' | '\t' | '&' | ';' | '|')
}

/// Push the word being built, if there is one.
fn end_word(tokens: &mut Vec<Token>, word: &mut String) {
    if word.is_empty() {
        return;
    }
    tokens.push(Token::Word {
        value: unquoted_value(std::mem::take(word)),
        quoted: false,
    });
}

/// An unquoted word's text, when the shell passes it as it is written: no
/// character it would expand, substitute, redirect or treat as a pattern,
/// and no `~` where a tilde expands — at the start, or after `=` or `:`.
fn unquoted_value(text: String) -> Option<String> {
    let mut previous = None;
    for c in text.chars() {
        let expanding_tilde = c == '~' && previous.is_none_or(|last| matches!(last, '=' | ':'));
        let active = c.is_control()
            || matches!(
                c,
                '\\' | '$' | '`' | '*' | '?' | '[' | '{' | '(' | ')' | '<' | '>' | '!' | '#'
            );
        if active || expanding_tilde {
            return None;
        }
        previous = Some(c);
    }
    Some(text)
}

/// The value of a word token, quoted or not, when the witness knows it.
fn word_value(token: &Token) -> Option<&str> {
    match token {
        Token::Word {
            value: Some(value), ..
        } => Some(value),
        _ => None,
    }
}

/// The value of an unquoted word token: the only spelling of a command word
/// the grammar reads.
fn bare_word(token: &Token) -> Option<&str> {
    match token {
        Token::Word {
            value: Some(value),
            quoted: false,
        } => Some(value),
        _ => None,
    }
}

/// Every value on the line that may stand for a file: each known word value,
/// less a search's pattern and its options' values.
fn witness_values(tokens: &[Token]) -> Vec<&str> {
    let mut out = Vec::new();
    for member in tokens.split(|token| !matches!(token, Token::Word { .. })) {
        let not_paths = search_non_paths(member);
        let values = member
            .iter()
            .enumerate()
            .filter(|(at, _)| !not_paths.contains(at));
        out.extend(values.filter_map(|(_, token)| word_value(token)));
    }
    out
}

/// The positions in a search member that hold its pattern or an option's
/// value, which no file can be: nothing when the member is not a search.
///
/// Options are read wherever they stand, as GNU `grep` and `rg` read them,
/// up to a `--`, and all of them before the operands: the first operand is
/// the pattern only when no option gave one. Only short options take a value here; a long one is taken
/// as a flag, which can only leave a pattern counted as a file — a looser
/// witness, never a stricter one — and the grammar admits none.
fn search_non_paths(member: &[Token]) -> Vec<usize> {
    let Some((head, rest)) = member.split_first() else {
        return Vec::new();
    };
    let value_letters = match bare_word(head) {
        Some("grep" | "egrep" | "fgrep") => GREP_VALUE_LETTERS,
        Some("rg") if rest.first().and_then(bare_word) != Some("--files") => RG_VALUE_LETTERS,
        _ => return Vec::new(),
    };
    let mut not_paths = Vec::new();
    let mut operands = Vec::new();
    let mut pattern_given = false;
    let mut options_end = false;
    let mut words = rest.iter().map(word_value).zip(1..);
    while let Some((value, at)) = words.next() {
        match value {
            Some("--") if !options_end => options_end = true,
            Some(option) if !options_end && option.len() > 1 && option.starts_with('-') => {
                let Some((letter, takes_next)) = value_option(option, value_letters) else {
                    continue;
                };
                pattern_given |= PATTERN_LETTERS.contains(letter);
                if takes_next {
                    not_paths.extend(words.next().map(|(_, at)| at));
                }
            }
            _ => operands.push(at),
        }
    }
    // Every option is read before any operand, so a `-e` after the first
    // operand still means that operand is a file, not the pattern.
    if !pattern_given {
        not_paths.extend(operands.first());
    }
    not_paths
}

/// The first letter of a short-option word that takes a value, and whether
/// that value is the next word rather than the rest of this one — or
/// [`None`] when every letter is a flag, or the word is a long option.
fn value_option(option: &str, value_letters: &str) -> Option<(char, bool)> {
    let letters = option.strip_prefix('-')?;
    if letters.starts_with('-') {
        return None;
    }
    let (at, letter) = letters
        .char_indices()
        .find(|(_, letter)| value_letters.contains(*letter))?;
    Some((letter, letters.len() == at + letter.len_utf8()))
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
            (Some("cd"), Some([dir])) => {
                let dir = word_value(dir)?;
                if dir.is_empty() || dir.starts_with('-') {
                    return None;
                }
                return Some(dir);
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
