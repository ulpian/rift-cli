//! The size ceilings: no tracked `.rs` file over 1,500 lines, no function
//! over 80, and the lexer that makes the second a real parse of item spans.
//!
//! The lexer lives here rather than in a shared module because this screen
//! is what it exists for; the `#[ignore]` screen borrows it so that a doc
//! comment spelling an attribute is never read as one.
use std::path::Path;

use super::tracked_paths;

/// A file may hold at most this many lines, as `wc -l` counts them.
const FILE_CEILING: usize = 1_500;
/// A function may span at most this many lines, signature to closing brace.
const FUNCTION_CEILING: usize = 80;

/// How much the ceiling screen measured, printed on the `ci-meta` ok line so
/// a screen over an empty corpus is visible rather than green.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Measured {
    /// Tracked `.rs` files read.
    pub files: usize,
    /// Functions with a body found in them.
    pub functions: usize,
}

/// One function's span: its name, the line holding its `fn` keyword, and its
/// length in lines from that line to its closing brace, both inclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionSpan {
    name: String,
    line: usize,
    lines: usize,
}

/// No tracked `.rs` file is over 1,500 lines and no function over 80.
///
/// The corpus is every tracked `.rs` file with nothing excluded: benches,
/// fuzz targets, this tool, every `tests/` path and every inline test module
/// are held to the same ceilings as shipped code. A function is every `fn`
/// with a body — free, in an `impl`, a trait default, nested, test — measured
/// from the line of its `fn` keyword to its closing brace. The body is found
/// by brace matching after comments, strings and character literals are
/// masked, so a comment or a string can neither shrink nor grow a function.
///
/// # Errors
/// If `git ls-files` fails, a tracked file cannot be read, or the walk found
/// no `.rs` file at all.
pub fn ceilings_hold(root: &Path) -> Result<(Measured, Vec<String>), String> {
    screen_files(root, &tracked_rust_paths(root)?)
}

/// Every tracked path ending in `.rs`, the corpus of both source screens.
///
/// # Errors
/// If `git ls-files` fails.
pub(super) fn tracked_rust_paths(root: &Path) -> Result<Vec<String>, String> {
    let paths = tracked_paths(root)?;
    Ok(paths
        .into_iter()
        .filter(|rel| {
            Path::new(rel)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
        })
        .collect())
}

/// The screen over an explicit path list — the seam the planting tests use,
/// so they need no repository of their own.
fn screen_files(root: &Path, paths: &[String]) -> Result<(Measured, Vec<String>), String> {
    if paths.is_empty() {
        return Err("ceiling screen: the tracked walk found no .rs file".to_owned());
    }
    let mut measured = Measured::default();
    let mut v = Vec::new();
    for rel in paths {
        let bytes =
            std::fs::read(root.join(rel)).map_err(|e| format!("read tracked file {rel}: {e}"))?;
        measured.files += 1;
        let code = masked(&bytes);
        let starts = line_starts(&code);
        // Masking keeps every newline, so this is the count `wc -l` prints.
        let lines = starts.len() - 1;
        if lines > FILE_CEILING {
            v.push(format!(
                "{rel} is {} lines (ceiling {})",
                thousands(lines),
                thousands(FILE_CEILING)
            ));
        }
        let spans = function_spans(&code, &starts);
        measured.functions += spans.len();
        for span in spans.iter().filter(|span| span.lines > FUNCTION_CEILING) {
            v.push(format!(
                "{rel}:{}: fn `{}` is {} lines (ceiling {FUNCTION_CEILING})",
                span.line, span.name, span.lines
            ));
        }
    }
    Ok((measured, v))
}

/// `1533` as `1,533`, the way the ceilings are written everywhere else.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (idx, digit) in digits.chars().enumerate() {
        if idx > 0 && (digits.len() - idx).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `src` with every comment, string and character literal blanked to spaces,
/// newlines kept, so the result has the same length and the same lines.
///
/// Recognised: `//` comments, nested `/* */` comments, ordinary, byte and C
/// strings with their escapes, raw strings with any number of `#`s, and
/// character literals — told apart from lifetimes and labels by the closing
/// quote. A literal or comment left open at the end of the file blanks to the
/// end: a malformed file loses its tail, it never panics.
pub(super) fn masked(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    let mut at = 0;
    while let Some(byte) = src.get(at).copied() {
        let (word_end, literal) = if is_ident_start(byte) {
            let word_end = ident_end(src, at);
            (word_end, prefixed_literal_end(src, at, word_end))
        } else {
            (at + 1, literal_end(src, at))
        };
        let end = literal.unwrap_or(word_end);
        let span = src.get(at..end).unwrap_or_default();
        match literal {
            Some(_) => blank(&mut out, span),
            None => out.extend_from_slice(span),
        }
        at = end;
    }
    out
}

/// Push `span` as spaces, keeping its newlines.
fn blank(out: &mut Vec<u8>, span: &[u8]) {
    for byte in span {
        out.push(if *byte == b'\n' { b'\n' } else { b' ' });
    }
}

/// The end of a literal whose prefix is the identifier `src[at..word_end]`:
/// `r"…"`, `br#"…"#`, `cr"…"`, `b"…"`, `c"…"`. `None` for any other word,
/// including a raw identifier such as `r#type`.
fn prefixed_literal_end(src: &[u8], at: usize, word_end: usize) -> Option<usize> {
    match src.get(at..word_end)? {
        b"r" | b"br" | b"cr" => raw_string_end(src, word_end),
        b"b" | b"c" if src.get(word_end) == Some(&b'"') => Some(string_end(src, word_end)),
        _ => None,
    }
}

/// The end of the comment, string or character literal starting at `at`, or
/// `None` when none starts there.
fn literal_end(src: &[u8], at: usize) -> Option<usize> {
    match (src.get(at)?, src.get(at + 1)) {
        (b'/', Some(b'/')) => Some(line_end(src, at)),
        (b'/', Some(b'*')) => Some(block_comment_end(src, at)),
        (b'"', _) => Some(string_end(src, at)),
        (b'\'', _) => char_end(src, at),
        _ => None,
    }
}

/// The offset of the newline ending the line `at` is on, or the end.
fn line_end(src: &[u8], at: usize) -> usize {
    src.get(at..)
        .and_then(|rest| rest.iter().position(|byte| *byte == b'\n'))
        .map_or(src.len(), |offset| at + offset)
}

/// The end of the (possibly nested) block comment opening at `at`.
fn block_comment_end(src: &[u8], at: usize) -> usize {
    let mut depth = 0_usize;
    let mut idx = at;
    while idx < src.len() {
        match (src.get(idx), src.get(idx + 1)) {
            (Some(b'/'), Some(b'*')) => {
                depth += 1;
                idx += 2;
            }
            (Some(b'*'), Some(b'/')) => {
                depth -= 1;
                idx += 2;
                if depth == 0 {
                    return idx;
                }
            }
            _ => idx += 1,
        }
    }
    src.len()
}

/// The end of the string whose opening quote is at `quote`, escapes skipped.
fn string_end(src: &[u8], quote: usize) -> usize {
    let mut idx = quote + 1;
    while let Some(byte) = src.get(idx) {
        match byte {
            b'\\' => idx += 2,
            b'"' => return idx + 1,
            _ => idx += 1,
        }
    }
    src.len()
}

/// The end of the raw string whose hashes start at `from` (just after its
/// `r`), or `None` when no quote follows the hashes.
fn raw_string_end(src: &[u8], from: usize) -> Option<usize> {
    let rest = src.get(from..)?;
    let hashes = rest.iter().take_while(|byte| **byte == b'#').count();
    if rest.get(hashes) != Some(&b'"') {
        return None;
    }
    let mut closing = vec![b'"'];
    closing.resize(hashes + 1, b'#');
    let body = from + hashes + 1;
    let found = src
        .get(body..)
        .and_then(|tail| tail.windows(closing.len()).position(|w| w == closing));
    Some(found.map_or(src.len(), |offset| body + offset + closing.len()))
}

/// The end of the character literal opening at `at`, or `None` when the
/// quote opens a lifetime or a label instead.
///
/// A literal is one character (one to four UTF-8 bytes) and a closing quote,
/// or a backslash escape closed within the ten bytes `\u{10FFFF}` needs.
fn char_end(src: &[u8], at: usize) -> Option<usize> {
    let first = *src.get(at + 1)?;
    if first == b'\\' {
        let limit = (at + 13).min(src.len());
        let close = (at + 3..limit).find(|idx| matches!(src.get(*idx), Some(b'\'' | b'\n')))?;
        return (src.get(close) == Some(&b'\'')).then_some(close + 1);
    }
    if matches!(first, b'\'' | b'\n') {
        return None;
    }
    let width = utf8_width(first);
    (src.get(at + 1 + width) == Some(&b'\'')).then_some(at + 2 + width)
}

/// The byte length of the UTF-8 character led by `lead`.
fn utf8_width(lead: u8) -> usize {
    match lead {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

/// The end of the identifier starting at `at`.
pub(super) fn ident_end(src: &[u8], at: usize) -> usize {
    src.get(at..)
        .and_then(|rest| rest.iter().position(|byte| !is_ident_continue(*byte)))
        .map_or(src.len(), |offset| at + offset)
}

/// The offset of the first byte at or after `at` that is not whitespace.
pub(super) fn skip_whitespace(src: &[u8], at: usize) -> usize {
    src.get(at..)
        .and_then(|rest| rest.iter().position(|byte| !byte.is_ascii_whitespace()))
        .map_or(src.len(), |offset| at + offset)
}

/// Every function with a body in a masked file, in source order, nested ones
/// included.
fn function_spans(masked: &[u8], line_starts: &[usize]) -> Vec<FunctionSpan> {
    let mut spans = Vec::new();
    let mut at = 0;
    while let Some(byte) = masked.get(at).copied() {
        if !is_ident_start(byte) {
            at += 1;
            continue;
        }
        let word_end = ident_end(masked, at);
        if masked.get(at..word_end) == Some(b"fn") {
            spans.extend(function_at(masked, line_starts, at, word_end));
        }
        at = word_end;
    }
    spans
}

/// The span of the function whose `fn` keyword is `masked[at..after_fn]`, or
/// `None` for a `fn` type, a declaration without a body, or a `fn` the file
/// ends inside of before its body opens.
fn function_at(
    masked: &[u8],
    line_starts: &[usize],
    at: usize,
    after_fn: usize,
) -> Option<FunctionSpan> {
    let name_at = skip_whitespace(masked, after_fn);
    if name_at == after_fn || !masked.get(name_at).is_some_and(|b| is_ident_start(*b)) {
        return None;
    }
    let name_end = ident_end(masked, name_at);
    let open = body_open(masked, name_end)?;
    let close = matching_close(masked, open);
    let first = line_of(line_starts, at);
    Some(FunctionSpan {
        name: String::from_utf8_lossy(masked.get(name_at..name_end)?).into_owned(),
        line: first,
        lines: line_of(line_starts, close) - first + 1,
    })
}

/// The offset of the brace opening the body of a function whose signature
/// continues at `from`, skipping parameters and generics; `None` when a `;`
/// ends the declaration first.
fn body_open(masked: &[u8], from: usize) -> Option<usize> {
    let mut depth = 0_usize;
    let mut idx = from;
    while let Some(byte) = masked.get(idx).copied() {
        let arrow = matches!(byte, b'-' | b'=') && masked.get(idx + 1) == Some(&b'>');
        match byte {
            _ if arrow => idx += 1,
            b'(' | b'[' | b'<' => depth += 1,
            b')' | b']' | b'>' => depth = depth.saturating_sub(1),
            b'{' if depth == 0 => return Some(idx),
            b';' if depth == 0 => return None,
            _ => {}
        }
        idx += 1;
    }
    None
}

/// The offset of the brace closing the one at `open`; the file's last byte
/// when it never closes, so an unbalanced body runs to the last line.
fn matching_close(masked: &[u8], open: usize) -> usize {
    let mut depth = 0_usize;
    for (idx, byte) in masked.iter().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return idx;
                }
            }
            _ => {}
        }
    }
    masked.len().saturating_sub(1)
}

/// The offset each line starts at, the first line at `0`.
pub(super) fn line_starts(src: &[u8]) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        src.iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'\n')
            .map(|(idx, _)| idx + 1),
    );
    starts
}

/// The 1-based line of the byte at `offset`.
pub(super) fn line_of(line_starts: &[usize], offset: usize) -> usize {
    line_starts.partition_point(|start| *start <= offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;

    /// A function named `name` spanning exactly `lines` lines, `body` spliced
    /// in as its second line.
    fn function(name: &str, lines: usize, body: &str) -> String {
        let padding = "    let _ = 0;\n".repeat(lines - 3);
        format!("fn {name}() {{\n{body}\n{padding}}}\n")
    }

    /// Write `files` under a fresh scratch root and screen them.
    fn screen(name: &str, files: &[(&str, String)]) -> (Measured, Vec<String>) {
        let root = scratch_dir(name);
        for (rel, text) in files {
            std::fs::write(root.join(rel), text).unwrap();
        }
        let paths: Vec<String> = files.iter().map(|(rel, _)| (*rel).to_owned()).collect();
        let result = screen_files(&root, &paths).expect("the screen runs");
        std::fs::remove_dir_all(&root).unwrap();
        result
    }

    /// Each kind of oversize is reported by path, line and name, and the
    /// honest shapes beside them — a file and a function exactly at their
    /// ceilings, a bodiless trait method — are not.
    #[test]
    fn a_long_file_and_every_kind_of_long_function_are_reported() {
        let nested = format!(
            "fn outer() {{\n{}}}\n",
            function("inner", 85, "    let _ = 1;")
        );
        let method = format!(
            "struct S;\nimpl S {{\n{}}}\n",
            function("method", 81, "    let _ = 1;")
        );
        let honest = format!(
            "trait T {{\n    fn declared(&self);\n}}\n{}",
            function("at_the_line", 80, "    let _ = 1;")
        );
        let (measured, violations) = screen(
            "ceilings-planted",
            &[
                ("long.rs", "// line\n".repeat(1_501)),
                ("at_the_line.rs", "// line\n".repeat(1_500)),
                ("long_fn.rs", function("long_body", 81, "    let _ = 1;")),
                ("nested.rs", nested),
                ("method.rs", method),
                ("honest.rs", honest),
            ],
        );
        assert_eq!(
            violations,
            [
                "long.rs is 1,501 lines (ceiling 1,500)",
                "long_fn.rs:1: fn `long_body` is 81 lines (ceiling 80)",
                "nested.rs:1: fn `outer` is 87 lines (ceiling 80)",
                "nested.rs:2: fn `inner` is 85 lines (ceiling 80)",
                "method.rs:3: fn `method` is 81 lines (ceiling 80)",
            ]
        );
        assert_eq!(
            measured,
            Measured {
                files: 6,
                functions: 5
            }
        );
    }

    /// The length a comment or a string would hide or invent is not read:
    /// a brace opened in a comment does not swallow the functions after it,
    /// a function spelled inside a 200-line block comment is not one, and a
    /// function that spells `}` in a raw string, a string and a character
    /// literal is measured to its real closing brace.
    #[test]
    fn a_comment_or_a_string_can_neither_shrink_nor_grow_a_function() {
        let comment_opens = format!(
            "{}{}",
            function("opens_in_a_comment", 10, "    // {\n    /* { /* { */ { */"),
            function("after", 5, "").repeat(40)
        );
        let ghost = format!(
            "/*\n{}*/\nfn real() {{}}\n",
            function("ghost", 200, "    /* nested */")
        );
        let spelled = function(
            "spells_a_brace",
            100,
            "    let _ = (r#\"}\"#, \"}\\\"}\", '}', b'}', '\\'', br\"}\");",
        );
        let (measured, violations) = screen(
            "ceilings-masked",
            &[
                ("comment.rs", comment_opens),
                ("ghost.rs", ghost),
                ("spelled.rs", spelled),
            ],
        );
        assert_eq!(
            violations,
            ["spelled.rs:1: fn `spells_a_brace` is 100 lines (ceiling 80)"]
        );
        assert_eq!(measured.functions, 43);
    }

    /// A malformed file is read without a panic: an unbalanced brace runs its
    /// function to the end of the file, and an unterminated string blanks the
    /// rest of the file, hiding nothing that was a function before it.
    #[test]
    fn an_unbalanced_brace_and_an_unterminated_string_do_not_panic() {
        let unbalanced = format!("fn open() {{\n    {{\n{}", "// line\n".repeat(90));
        let unterminated = format!("fn fine() {{}}\nfn cut() {{\n    \"{}", "x\n".repeat(90));
        let (measured, violations) = screen(
            "ceilings-malformed",
            &[
                ("unbalanced.rs", unbalanced),
                ("unterminated.rs", unterminated),
                ("tail.rs", "fn tail() { '".to_owned()),
            ],
        );
        assert_eq!(
            violations,
            [
                "unbalanced.rs:1: fn `open` is 92 lines (ceiling 80)",
                "unterminated.rs:2: fn `cut` is 91 lines (ceiling 80)",
            ]
        );
        assert_eq!(measured.functions, 4);
    }

    /// Lifetimes and labels are not character literals, and the literal
    /// forms that are keep their braces out of the count.
    #[test]
    fn lifetimes_stay_code_and_literals_are_blanked() {
        let src = b"fn f<'a>(x: &'a str) { 'outer: loop { let _ = ('{', '\\u{7b}', \"{\"); } }";
        let masked = String::from_utf8(masked(src)).unwrap();
        assert_eq!(masked.len(), src.len());
        assert_eq!(masked.matches('{').count(), 2, "{masked}");
        assert!(masked.contains("<'a>(x: &'a str)"), "{masked}");
        assert!(masked.contains("'outer: loop"), "{masked}");
    }

    /// A raw identifier is code, and a raw string needs its own number of
    /// hashes to close.
    #[test]
    fn raw_identifiers_stay_code_and_raw_strings_close_on_their_hashes() {
        let src = b"let r#type = r##\"a \"# } \"##; }";
        let masked = String::from_utf8(masked(src)).unwrap();
        assert!(masked.starts_with("let r#type = "), "{masked}");
        assert_eq!(masked.matches('}').count(), 1, "{masked}");
    }

    /// Ceilings print with thousands separators, as they are written.
    #[test]
    fn counts_print_with_thousands_separators() {
        assert_eq!(thousands(80), "80");
        assert_eq!(thousands(1_500), "1,500");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
