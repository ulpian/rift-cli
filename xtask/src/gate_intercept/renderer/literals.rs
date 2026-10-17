//! The half of `one-reason-renderer` that needs no list of phrases: a literal
//! spelling Rift's own line marker anywhere outside the renderer.
//!
//! The opener half guards only the phrases the renderer declares, so a new
//! sentence opened with the marker but with a phrase nobody declared walked
//! straight past it. This half closes that: the marker itself is the thing a
//! model reads as "Rift is speaking", so every literal that spells it is either
//! the renderer, a pinned reader, or a finding.

use std::collections::{BTreeMap, BTreeSet};

use crate::ci_meta::production_half;

use super::super::gate_path::declared_for_tests;
use super::super::text::{fn_spans, is_ident_byte, mask_code, mask_comments, token_offsets};
use super::super::{is_test_surface, line_of};

/// Where the readers are pinned.
pub(super) const RENDERER_ALLOW: &str = "ci/intercept-renderer-allow.txt";

/// The only kind a pinned row may carry: rows exist for readers, never for a
/// site that writes a sentence.
const READER_KIND: &str = "parse-only";

/// What the marker screen concluded.
#[derive(Debug)]
pub(super) struct MarkerScreen {
    /// Every unpinned site, every stale row.
    pub(super) violations: Vec<String>,
    /// Sites a pinned row covered.
    pub(super) pinned: usize,
    /// Files read.
    pub(super) screened: usize,
}

/// Screen `corpus` for literals spelling the marker outside `renderer`.
///
/// A site is a file and the function, const or static the literal sits in —
/// never a line number, so a row survives an edit above it and dies with the
/// item it names.
///
/// # Errors
/// If the allow-list does not parse.
pub(super) fn screen_marker_literals(
    corpus: &[(String, String)],
    renderer: &str,
    allow_text: &str,
) -> Result<MarkerScreen, String> {
    let mut allow = read_allow_list(allow_text)?;
    // A test module split into a file of its own carries no `#[cfg(test)]`
    // marker for the production cut to find; its parent's declaration is what
    // makes it test code to the compiler, so that declaration is read rather
    // than the file's name, which says only what its author meant.
    let for_tests = declared_for_tests(corpus);
    let mut violations = Vec::new();
    let mut pinned = 0_usize;
    let mut screened = 0_usize;
    for (rel, text) in corpus {
        if rel == renderer || is_test_surface(rel) || for_tests.contains(rel.as_str()) {
            continue;
        }
        screened += 1;
        for (item, line) in marker_sites(text) {
            let site = format!("{rel}::{item}");
            if let Some(seen) = allow.get_mut(&site) {
                *seen = true;
                pinned += 1;
                continue;
            }
            violations.push(format!(
                "{rel}:{line}: `{item}` holds a literal spelling Rift's line marker outside \
                 {renderer} — a writer moves its sentence into the renderer; a reader is \
                 named `{site}` in {RENDERER_ALLOW}"
            ));
        }
    }
    for (site, seen) in &allow {
        if !seen {
            violations.push(format!(
                "{RENDERER_ALLOW}: `{site}` is stale — that site holds no literal spelling the \
                 marker, so the row comes out"
            ));
        }
    }
    Ok(MarkerScreen {
        violations,
        pinned,
        screened,
    })
}

/// The two spellings of the marker, assembled so this file never matches
/// itself.
fn markers() -> [String; 2] {
    [["[rift", " "].concat(), ["[rift", "]"].concat()]
}

/// Every item in `text`'s production half with a literal spelling the
/// marker, each with the line of its first such literal.
fn marker_sites(text: &str) -> BTreeMap<String, usize> {
    let production = production_half(text);
    let literals = mask_comments(&production);
    let code = mask_code(&production);
    let mut out = BTreeMap::new();
    for marker in markers() {
        for (at, _) in literals.match_indices(marker.as_str()) {
            // Masking blanks a literal's body and keeps code, so a byte that
            // survives the comment masking and not the code masking sits
            // inside a string.
            if code.as_bytes().get(at) != Some(&b' ') {
                continue;
            }
            let line = line_of(&production, at).saturating_sub(1);
            out.entry(enclosing_item(&code, at))
                .and_modify(|first: &mut usize| *first = (*first).min(line))
                .or_insert(line);
        }
    }
    out
}

/// The innermost function whose body holds `at`; failing that, the nearest
/// const or static declared before it.
fn enclosing_item(code: &str, at: usize) -> String {
    let innermost = fn_spans(code)
        .into_iter()
        .filter(|span| span.body.contains(&at))
        .max_by_key(|span| span.body.start);
    if let Some(span) = innermost {
        return span.name;
    }
    ["const ", "static "]
        .into_iter()
        .flat_map(|keyword| {
            token_offsets(code, keyword)
                .into_iter()
                .filter(move |offset| *offset < at)
                .filter_map(move |offset| declared_name(code, offset + keyword.len()))
        })
        .max_by_key(|(offset, _)| *offset)
        .map_or_else(|| "<module>".to_owned(), |(_, name)| name)
}

/// The name a `const`/`static` declares when its keyword ends at `from`,
/// with its offset; `None` for `const fn` and anything else that is not a
/// `NAME:` declaration.
fn declared_name(code: &str, from: usize) -> Option<(usize, String)> {
    let bytes = code.as_bytes();
    let mut i = from;
    if code.get(i..)?.starts_with("mut ") {
        i += 4;
    }
    let start = i;
    while bytes.get(i).copied().is_some_and(is_ident_byte) {
        i += 1;
    }
    let name = code.get(start..i)?;
    let rest = code.get(i..)?.trim_start();
    if name.is_empty() || !rest.starts_with(':') {
        return None;
    }
    Some((start, name.to_owned()))
}

/// Parse the allow-list: `path::item  parse-only | <reason>`, one site a
/// row.
fn read_allow_list(text: &str) -> Result<BTreeMap<String, bool>, String> {
    let mut out = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let row = index + 1;
        let shape =
            || format!("{RENDERER_ALLOW}:{row}: expected `path::item  {READER_KIND} | reason`");
        let Some((site, rest)) = line.split_once(char::is_whitespace) else {
            return Err(shape());
        };
        let Some(reason) = rest
            .trim_start()
            .strip_prefix(READER_KIND)
            .and_then(|after| after.trim_start().strip_prefix('|'))
        else {
            return Err(shape());
        };
        if !site.contains(".rs::") {
            return Err(shape());
        }
        if reason.trim().len() < 20 {
            return Err(format!(
                "{RENDERER_ALLOW}:{row}: the reason must say what the site reads, in a sentence"
            ));
        }
        if !seen.insert(site.to_owned()) {
            return Err(format!("{RENDERER_ALLOW}:{row}: `{site}` is pinned twice"));
        }
        out.insert(site.to_owned(), false);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RENDERER: &str = "crates/rift-recall-validity/src/trust/frames/mod.rs";

    /// A corpus of one file.
    fn corpus(rel: &str, text: &str) -> Vec<(String, String)> {
        vec![(rel.to_owned(), text.to_owned())]
    }

    /// A function writing a marked line, outside the renderer.
    const WRITER: &str = "fn say() -> &'static str {\n    \"[rift] anything at all\"\n}\n";

    /// ★ A marked line the renderer never declared is still caught, at its
    /// line and under its function's name.
    #[test]
    fn a_marked_literal_outside_the_renderer_is_caught_whatever_its_phrase() {
        let screen =
            screen_marker_literals(&corpus("bins/rift/src/elsewhere.rs", WRITER), RENDERER, "")
                .expect("the screen runs");
        assert_eq!(screen.violations.len(), 1, "{:#?}", screen.violations);
        assert!(
            screen.violations[0].contains("bins/rift/src/elsewhere.rs:2: `say`"),
            "{:#?}",
            screen.violations
        );
    }

    /// ★ A pinned reader passes, keyed by file and item.
    #[test]
    fn a_site_named_in_the_allow_list_passes() {
        let allow = "bins/rift/src/elsewhere.rs::say  parse-only | recognises a marked line it \
                     was handed\n";
        let screen = screen_marker_literals(
            &corpus("bins/rift/src/elsewhere.rs", WRITER),
            RENDERER,
            allow,
        )
        .expect("the screen runs");
        assert!(screen.violations.is_empty(), "{:#?}", screen.violations);
        assert_eq!(screen.pinned, 1);
    }

    /// ★ A row whose site no longer holds the literal is a finding.
    #[test]
    fn a_row_naming_a_site_with_no_marked_literal_is_stale() {
        let allow = "bins/rift/src/elsewhere.rs::say  parse-only | recognises a marked line it \
                     was handed\n";
        let quiet = corpus("bins/rift/src/elsewhere.rs", "fn say() -> u8 {\n    1\n}\n");
        let screen = screen_marker_literals(&quiet, RENDERER, allow).expect("the screen runs");
        assert_eq!(screen.violations.len(), 1, "{:#?}", screen.violations);
        assert!(
            screen.violations[0].contains("is stale"),
            "{:#?}",
            screen.violations
        );
    }

    /// A comment quoting the marker, an inline test module, a test module
    /// its parent declares under `#[cfg(test)]` (whatever the file is called)
    /// and the renderer itself are all outside the screen.
    #[test]
    fn comments_tests_and_the_renderer_are_not_screened() {
        let commented = "/// Writes `[rift] x` nowhere.\nfn f() {}\n";
        let tested =
            "fn f() {}\n\n#[cfg(test)]\nmod tests {\n    const X: &str = \"[rift] x\";\n}\n";
        let parent = "fn c() {}\n\n#[cfg(test)]\nmod emission_tests;\n#[cfg(test)]\nmod tests;\n";
        let files = vec![
            ("bins/rift/src/a.rs".to_owned(), commented.to_owned()),
            ("bins/rift/src/b.rs".to_owned(), tested.to_owned()),
            ("bins/rift/src/c.rs".to_owned(), parent.to_owned()),
            ("bins/rift/src/c/tests.rs".to_owned(), WRITER.to_owned()),
            (
                "bins/rift/src/c/emission_tests.rs".to_owned(),
                WRITER.to_owned(),
            ),
            (RENDERER.to_owned(), WRITER.to_owned()),
        ];
        let screen = screen_marker_literals(&files, RENDERER, "").expect("the screen runs");
        assert!(screen.violations.is_empty(), "{:#?}", screen.violations);
    }

    /// ★ The declaration decides, not the name: a file called `x_tests.rs`
    /// that its parent declares as a plain `mod x_tests;` ships, so its marked
    /// literal is caught.
    #[test]
    fn a_shipped_module_named_like_a_test_file_is_screened() {
        let files = vec![
            ("bins/rift/src/c.rs".to_owned(), "mod x_tests;\n".to_owned()),
            ("bins/rift/src/c/x_tests.rs".to_owned(), WRITER.to_owned()),
        ];
        let screen = screen_marker_literals(&files, RENDERER, "").expect("the screen runs");
        assert_eq!(screen.violations.len(), 1, "{:#?}", screen.violations);
        assert!(
            screen.violations[0].contains("bins/rift/src/c/x_tests.rs:2: `say`"),
            "{:#?}",
            screen.violations
        );
    }

    /// A const or a byte string is keyed by its own name, and a const fn is
    /// not mistaken for one.
    #[test]
    fn a_const_and_a_byte_string_are_keyed_by_their_item() {
        let text = "const fn n() -> u8 { 1 }\npub const OPEN: &str = \"[rift] held\";\n\
                    fn scan(b: &[u8]) -> bool {\n    b.starts_with(b\"[rift \")\n}\n";
        let screen = screen_marker_literals(&corpus("crates/x/src/lib.rs", text), RENDERER, "")
            .expect("the screen runs");
        let found = screen.violations.join("\n");
        assert!(found.contains("lib.rs:2: `OPEN`"), "{found}");
        assert!(found.contains("lib.rs:4: `scan`"), "{found}");
        assert_eq!(screen.violations.len(), 2, "{found}");
    }

    /// A row that cannot say what its site reads is no row.
    #[test]
    fn a_row_without_a_reader_reason_is_refused() {
        for bad in [
            "a.rs::f  parse-only | short\n",
            "a.rs::f  writes | a sentence for the model to read\n",
            "a.rs  parse-only | recognises a marked line it was handed\n",
        ] {
            assert!(read_allow_list(bad).is_err(), "{bad}");
        }
        let twice = "a.rs::f  parse-only | recognises a marked line it was handed\n\
                     a.rs::f  parse-only | recognises a marked line it was handed\n";
        assert!(read_allow_list(twice).is_err());
    }
}
