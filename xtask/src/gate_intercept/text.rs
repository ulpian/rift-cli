//! Reading Rust as text, shared by every leg that asks about code rather than
//! prose: strip what is not code, find the functions, and see which of them
//! call which. One module so the legs share ONE reading -- two readers with
//! two ideas of where a function ends is how a screen goes blind over half a
//! file without anyone noticing.

use std::collections::BTreeSet;

use super::line_of;

/// A function as this screen sees it: a name and the masked text of its body.
#[derive(Debug)]
pub struct FnBody {
    /// The function's own name, with no module or type qualification.
    pub name: String,
    /// `path:line` of the definition, for a finding that names its site.
    pub site: String,
    /// The masked body, braces included.
    pub body: String,
}

/// `text` with every comment, string literal body and character literal
/// replaced by spaces, and every other byte left exactly where it was.
///
/// Offsets are preserved so a finding can still be reported at its real line.
/// The point is to let a leg ask about CODE: a needle named in a doc comment
/// that explains why the needle is absent must not be the thing that reds the
/// gate, and a needle inside a test's expected-output string is not a call.
///
/// Handles the shapes this workspace actually contains: `//` and `/* */`
/// (nested), `"…"` with escapes, `r"…"` and `r#"…"#` (any hash count, with an
/// optional `b` prefix), and `'c'` — with a lifetime (`'a`, `'static`) left
/// alone, since it is code.
#[must_use]
pub fn mask_code(text: &str) -> String {
    mask(text, false)
}

/// `text` with its comments blanked and its string literals LEFT STANDING.
///
/// The legs that ask "does this file write that sentence?" or "does this path
/// get built here?" are asking about string literals, so those legs cannot use
/// [`mask_code`]. What they still must drop is prose: this workspace explains
/// its guards in comments beside the code they guard, and a screen that reds on
/// the paragraph explaining why a thing is absent teaches everyone to stop
/// writing the paragraph.
#[must_use]
pub fn mask_comments(text: &str) -> String {
    mask(text, true)
}

/// Every function defined in `masked`, with its body.
///
/// `masked` must already have comments and literals removed, so every brace it
/// contains is a real one. A declaration with no body (a trait method) is
/// skipped rather than guessed at.
#[must_use]
pub fn fn_bodies(rel: &str, masked: &str) -> Vec<FnBody> {
    let bytes = masked.as_bytes();
    let mut out = Vec::new();
    for at in token_offsets(masked, "fn ") {
        let mut i = at + 3;
        while bytes.get(i) == Some(&b' ') {
            i += 1;
        }
        let name_start = i;
        while bytes.get(i).is_some_and(|b| is_ident_byte(*b)) {
            i += 1;
        }
        if i == name_start {
            continue;
        }
        let name = masked.get(name_start..i).unwrap_or("").to_owned();
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if bytes.get(i) == Some(&b'<') {
            i = skip_balanced(bytes, i, b'<', b'>');
        }
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if bytes.get(i) != Some(&b'(') {
            continue;
        }
        i = skip_balanced(bytes, i, b'(', b')');
        // Return type and where-clause, up to the body or the semicolon that
        // says there is none.
        while bytes.get(i).is_some_and(|b| *b != b'{' && *b != b';') {
            i += 1;
        }
        if bytes.get(i) != Some(&b'{') {
            continue;
        }
        let end = skip_balanced(bytes, i, b'{', b'}');
        out.push(FnBody {
            name,
            site: format!("{rel}:{}", line_of(masked, at).saturating_sub(1)),
            body: masked.get(i..end).unwrap_or("").to_owned(),
        });
    }
    out
}

/// Every function in `defined` that `body` CALLS.
///
/// A call, not a mention: the name is followed by `(` (or a turbofish and then
/// `(`) and is not preceded by `.`. Dropping method position is what keeps the
/// walk usable — this codebase has free functions named `open`, `serve` and
/// `gather`, and counting every `.open(` on every unrelated type as an edge
/// reached nine tenths of the binary from one hook and told a reader nothing.
/// A path-qualified call (`crate::thing::open(`) still counts, so the
/// tightening loses no free call.
#[must_use]
pub fn calls_within(body: &str, defined: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes.get(i).copied().is_some_and(is_ident_byte) {
            i += 1;
            continue;
        }
        let start = i;
        while bytes.get(i).copied().is_some_and(is_ident_byte) {
            i += 1;
        }
        let Some(word) = body.get(start..i) else {
            continue;
        };
        if !defined.contains(word) {
            continue;
        }
        if start > 0 && bytes.get(start - 1) == Some(&b'.') {
            continue;
        }
        // An associated function on a TYPE (`File::open`, `SqliteStore::open`)
        // is not the free function of the same name, and treating it as one is
        // what turned this walk into most of the binary. A module-qualified
        // call (`crate::family::recall_db`) still counts: module segments are
        // lower case, type names are not.
        if is_associated_on_a_type(bytes, start) {
            continue;
        }
        let mut at = i;
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        // A turbofish stands between the name and its argument list.
        if bytes.get(at) == Some(&b':') && bytes.get(at + 1) == Some(&b':') {
            at += 2;
            while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
                at += 1;
            }
            if bytes.get(at) == Some(&b'<') {
                at = skip_balanced(bytes, at, b'<', b'>');
            }
        }
        if bytes.get(at) == Some(&b'(') {
            out.insert(word.to_owned());
        }
    }
    out
}

/// The shared scanner behind both maskings.
fn mask(text: &str, keep_strings: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = vec![b' '; bytes.len()];
    let mut i = 0;
    // Keep a byte as it stands; anything not kept stays a space.
    let keep = |out: &mut Vec<u8>, at: usize, byte: u8| {
        if let Some(slot) = out.get_mut(at) {
            *slot = if byte == b'\n' { b'\n' } else { byte };
        }
    };
    while let Some(&b) = bytes.get(i) {
        let next = bytes.get(i + 1).copied();
        // Line comment: to end of line, newlines preserved by the fill.
        if b == b'/' && next == Some(b'/') {
            while bytes.get(i).is_some_and(|b| *b != b'\n') {
                i += 1;
            }
            continue;
        }
        // Block comment, nested as Rust allows.
        if b == b'/' && next == Some(b'*') {
            let mut depth = 1_usize;
            i += 2;
            while depth > 0 {
                let Some(&here) = bytes.get(i) else { break };
                if here == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if here == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    if here == b'\n' {
                        keep(&mut out, i, b'\n');
                    }
                    i += 1;
                }
            }
            continue;
        }
        // A string literal, raw or ordinary: copied whole when the caller
        // wants literals kept, and otherwise blanked with its newlines left
        // where they were so line numbers still hold.
        let literal = raw_string_len(bytes, i).or_else(|| ordinary_string_len(bytes, i));
        if let Some(len) = literal {
            for j in i..i + len {
                match bytes.get(j) {
                    Some(&byte) if keep_strings || byte == b'\n' => keep(&mut out, j, byte),
                    _ => {}
                }
            }
            i += len;
            continue;
        }
        // A character literal is masked; a lifetime is code and is kept.
        if b == b'\'' {
            if let Some(len) = char_literal_len(bytes, i) {
                i += len;
                continue;
            }
        }
        keep(&mut out, i, b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Byte length of the raw string literal starting at `at`, if one does.
fn raw_string_len(bytes: &[u8], at: usize) -> Option<usize> {
    let mut i = at;
    if bytes.get(i) == Some(&b'b') {
        i += 1;
    }
    if bytes.get(i) != Some(&b'r') {
        return None;
    }
    // `r` must open a token, not end an identifier such as `_r`.
    if at > 0 && bytes.get(at - 1).is_some_and(|p| is_ident_byte(*p)) {
        return None;
    }
    i += 1;
    let hash_start = i;
    while bytes.get(i) == Some(&b'#') {
        i += 1;
    }
    let hashes = i - hash_start;
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    let closer: Vec<u8> = std::iter::once(b'"')
        .chain(std::iter::repeat_n(b'#', hashes))
        .collect();
    while i < bytes.len() {
        if bytes.get(i..i + closer.len()) == Some(closer.as_slice()) {
            return Some(i + closer.len() - at);
        }
        i += 1;
    }
    Some(bytes.len() - at)
}

/// Byte length of the ordinary string literal starting at `at`, if one does.
fn ordinary_string_len(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at) != Some(&b'"') {
        return None;
    }
    let mut i = at + 1;
    while let Some(&here) = bytes.get(i) {
        if here == b'\\' {
            i += 2;
            continue;
        }
        i += 1;
        if here == b'"' {
            break;
        }
    }
    Some(i - at)
}

/// Byte length of the character literal starting at `at`, or `None` for a
/// lifetime.
fn char_literal_len(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at + 1) == Some(&b'\\') {
        let mut i = at + 2;
        while bytes.get(i).is_some_and(|b| *b != b'\'') {
            i += 1;
        }
        return Some(i + 1 - at);
    }
    // `'x'` — one character then a closing quote. Anything else is a lifetime.
    let mut i = at + 1;
    while bytes.get(i).is_some_and(|b| !b.is_ascii()) {
        i += 1;
    }
    if i < bytes.len() {
        i += 1;
    }
    if bytes.get(i) == Some(&b'\'') {
        return Some(i + 1 - at);
    }
    None
}

/// Whether `b` can appear inside a Rust identifier.
pub(super) fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether the occurrence of `needle` at `at` in `hay` stands as its own token
/// rather than inside a longer identifier.
fn is_whole_token(hay: &str, at: usize, needle: &str) -> bool {
    let bytes = hay.as_bytes();
    let before = at.checked_sub(1).and_then(|i| bytes.get(i));
    let after = bytes.get(at + needle.len());
    let opens = needle.as_bytes().first().is_some_and(|b| is_ident_byte(*b));
    let closes = needle.as_bytes().last().is_some_and(|b| is_ident_byte(*b));
    (!opens || !before.is_some_and(|b| is_ident_byte(*b)))
        && (!closes || !after.is_some_and(|b| is_ident_byte(*b)))
}

/// Byte offsets of every whole-token occurrence of `needle` in `hay`.
pub(super) fn token_offsets(hay: &str, needle: &str) -> Vec<usize> {
    hay.match_indices(needle)
        .filter(|(at, _)| is_whole_token(hay, *at, needle))
        .map(|(at, _)| at)
        .collect()
}

/// Skip a balanced bracket run starting at `at` (which must be the opener),
/// returning the offset just past its closer.
pub(super) fn skip_balanced(bytes: &[u8], at: usize, open: u8, close: u8) -> usize {
    let mut depth = 0_usize;
    let mut i = at;
    while let Some(&b) = bytes.get(i) {
        // `->` inside a generic bound is an arrow, not a closing angle.
        if open == b'<' && b == b'-' && bytes.get(i + 1) == Some(&b'>') {
            i += 2;
            continue;
        }
        if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    bytes.len()
}

/// Whether the identifier at `start` is qualified by a preceding path segment
/// that names a type rather than a module.
fn is_associated_on_a_type(bytes: &[u8], start: usize) -> bool {
    if start < 2 || bytes.get(start - 1) != Some(&b':') || bytes.get(start - 2) != Some(&b':') {
        return false;
    }
    let mut at = start - 2;
    while at > 0 && bytes.get(at - 1).is_some_and(|b| is_ident_byte(*b)) {
        at -= 1;
    }
    bytes.get(at).is_some_and(u8::is_ascii_uppercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Masking removes prose and literal text and moves nothing.
    #[test]
    fn masking_keeps_code_and_drops_everything_else() {
        let src = "let a = \"open_migrated\"; // open_migrated\n/* open_migrated */ f();\n";
        let masked = mask_code(src);
        assert_eq!(masked.len(), src.len(), "offsets survive masking");
        assert_eq!(masked.matches("open_migrated").count(), 0, "{masked}");
        assert!(masked.contains("let a ="), "{masked}");
        assert!(masked.contains("f();"), "{masked}");
        assert_eq!(masked.lines().count(), src.lines().count());
    }

    /// A raw string, a nested block comment and a lifetime are each read as
    /// what they are.
    #[test]
    fn masking_reads_raw_strings_comments_and_lifetimes() {
        let masked = mask_code("let r = r#\"open_migrated\"#; /* a /* b */ c */ let x: &'a u8;\n");
        assert_eq!(masked.matches("open_migrated").count(), 0, "{masked}");
        assert!(
            masked.contains("let x: &'a u8;"),
            "a lifetime is code: {masked}"
        );
        assert!(
            !masked.contains('c'),
            "the nested comment closes once: {masked}"
        );
    }

    /// A body is the braces that belong to it, generics and all.
    #[test]
    fn a_function_body_is_found_through_generics_and_where_clauses() {
        let src =
            "fn outer<F: Fn(u8) -> u8>(f: F) -> u8\nwhere\n    F: Copy,\n{\n    inner(1)\n}\n\
                   fn inner(x: u8) -> u8 { x }\n";
        let found = fn_bodies("a.rs", &mask_code(src));
        let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["outer", "inner"], "{names:?}");
        assert!(found[0].body.contains("inner(1)"), "{}", found[0].body);
    }

    /// Strings survive the comment-only masking; prose does not.
    #[test]
    fn comment_masking_keeps_the_strings_a_path_is_built_from() {
        let masked = mask_comments("let p = \".rift/state/doc\"; // .rift/state/doc\n");
        assert_eq!(masked.matches(".rift/state/doc").count(), 1, "{masked}");
        assert!(masked.contains("let p ="), "{masked}");
    }
}
