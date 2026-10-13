//! The readings of one delivered line: the line itself, and the line with
//! each decoration a known tool adds stripped off.
//!
//! A delivered line is ambiguous by nature — `12:foo` is a `grep -n` hit on
//! `foo`, or a line that says `12:foo` — so every reading is kept and the
//! match decides which one a file carries. A prefix that names a line or a
//! path is an anchor: a reading under it matches only at that place, which is
//! what makes a forged prefix worthless (its claim is checked against the
//! file's own bytes) and a true one decisive.

use super::hash::line_hash;

/// How far into a line a printed path is looked for.
const MAX_PATH_PREFIX_CHARS: usize = 300;

/// The most digits a printed line number has.
const MAX_LINE_DIGITS: usize = 9;

/// One reading of a delivered line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The hash of the reading's bytes.
    pub hash: u64,
    /// Whether the reading is too slight to say anything alone: fewer than
    /// four characters past its indentation, or fewer than three letters or
    /// digits.
    pub trivial: bool,
    /// The path the line's own prefix printed, as printed.
    pub anchor_path: Option<String>,
    /// The line number the line's own prefix printed.
    pub anchor_line: Option<u32>,
}

/// The readings of `line`: itself first, then each decoration stripped.
///
/// `diff` says the output is a unified diff (it carries an `@@ ` line), so a
/// body line's first column is a marker. `is_path` says whether a printed
/// prefix names a file the caller can match against; only then is it read as
/// a path, so a line with a colon in it is not taken apart for nothing.
#[must_use]
pub fn readings(line: &str, diff: bool, is_path: &dyn Fn(&str) -> bool) -> Vec<Reading> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut out = vec![reading(line, None, None)];
    numbered(line, &mut out);
    if let Some(found) = path_prefixed(line, is_path) {
        out.push(found);
    }
    if diff {
        out.extend(diff_body(line));
    }
    out
}

/// The reading of `content` under the anchors given.
fn reading(content: &str, anchor_path: Option<&str>, anchor_line: Option<u32>) -> Reading {
    Reading {
        hash: line_hash(content.as_bytes()),
        trivial: is_trivial(content),
        anchor_path: anchor_path.map(str::to_owned),
        anchor_line,
    }
}

/// Whether a line is too slight to ground anything alone.
fn is_trivial(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.len() < 4 || trimmed.chars().filter(|c| c.is_alphanumeric()).count() < 3
}

/// A line number printed at the front of `text`, and the rest.
fn line_number(text: &str) -> Option<(u32, &str)> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    if end == 0 || end > MAX_LINE_DIGITS {
        return None;
    }
    let (digits, rest) = text.split_at_checked(end)?;
    Some((digits.parse().ok()?, rest))
}

/// The numbered readings: `N→` (a read tool), `N\t` (`cat -n`, `nl`),
/// `N:` and `N-` with no indentation (`grep -n` hits and context), and
/// `  N | ` (a compiler's source snippet).
fn numbered(line: &str, out: &mut Vec<Reading>) {
    let lead = line.trim_start_matches(' ');
    let Some((n, rest)) = line_number(lead) else {
        return;
    };
    let indented = lead.len() != line.len();
    let content = match (indented, rest.chars().next()) {
        (_, Some('→')) => rest.strip_prefix('→'),
        (_, Some('\t')) => rest.strip_prefix('\t'),
        (false, Some(':' | '-')) => rest.get(1..),
        (true, Some(' ')) => rest
            .strip_prefix(" | ")
            .or_else(|| (rest == " |").then_some("")),
        _ => None,
    };
    if let Some(content) = content {
        out.push(reading(content, None, Some(n)));
    }
}

/// The prefixes of a delivered line that could be a printed path: the text
/// before its first `:`, and the text before a `-N-` (a context line of
/// `grep -rn`), each with no blank in it and within the first few hundred
/// characters.
///
/// Two at most, so a caller that resolves them against a filesystem pays for
/// the files a delivery prints rather than for every punctuation mark in it.
#[must_use]
pub fn printed_paths(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let head_end = line
        .char_indices()
        .find(|(at, ch)| *at > MAX_PATH_PREFIX_CHARS || *ch == ' ' || *ch == '\t')
        .map_or(line.len(), |(at, _)| at);
    let head = line.get(..head_end).unwrap_or_default();
    if let Some(at) = head.find(':').filter(|at| *at > 0) {
        out.extend(line.get(..at));
    }
    let context = head.char_indices().skip(1).find(|(at, ch)| {
        *ch == '-'
            && line
                .get(at + 1..)
                .and_then(line_number)
                .is_some_and(|(_, tail)| tail.starts_with('-'))
    });
    if let Some((at, _)) = context {
        out.extend(line.get(..at));
    }
    out
}

/// The reading under a printed path: `path:N:text`, `path-N-text` or
/// `path:text`, where `path` is one of [`printed_paths`] and `is_path`
/// accepts it.
fn path_prefixed(line: &str, is_path: &dyn Fn(&str) -> bool) -> Option<Reading> {
    for path in printed_paths(line) {
        if !is_path(path) {
            continue;
        }
        let rest = line.get(path.len() + 1..)?;
        let separator = line.get(path.len()..)?.chars().next()?;
        if let Some((n, tail)) = line_number(rest) {
            if let Some(content) = tail.strip_prefix(separator) {
                return Some(reading(content, Some(path), Some(n)));
            }
        }
        if separator == ':' {
            return Some(reading(rest, Some(path), None));
        }
    }
    None
}

/// The body of a diff line: its marker column stripped, for `+`, `-` and
/// ` ` but never the `+++`/`---` file headers.
fn diff_body(line: &str) -> Option<Reading> {
    if line.starts_with("+++") || line.starts_with("---") {
        return None;
    }
    let body = line
        .strip_prefix('+')
        .or_else(|| line.strip_prefix('-'))
        .or_else(|| line.strip_prefix(' '))?;
    Some(reading(body, None, None))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn no_path(_: &str) -> bool {
        false
    }

    fn content_of(found: &[Reading], anchor: Option<u32>) -> Option<u64> {
        found
            .iter()
            .find(|r| r.anchor_line == anchor && r.anchor_path.is_none())
            .map(|r| r.hash)
    }

    /// ★ Each tool's prefix is stripped to the line it carries, under the
    /// number it printed.
    #[test]
    fn each_tools_prefix_is_stripped_to_the_line_it_carries() {
        let want = line_hash(b"fn main() {");
        for (line, n) in [
            ("    12\tfn main() {", 12),
            ("12:fn main() {", 12),
            ("12-fn main() {", 12),
            ("    12→fn main() {", 12),
            ("12 | fn main() {", 12),
        ] {
            let found = readings(line, false, &no_path);
            if line.starts_with("12 |") {
                // An unindented `N |` is not a compiler snippet.
                assert_eq!(content_of(&found, Some(n)), None, "{line}");
                continue;
            }
            assert_eq!(content_of(&found, Some(n)), Some(want), "{line}");
        }
        let snippet = readings("  12 | fn main() {", false, &no_path);
        assert_eq!(content_of(&snippet, Some(12)), Some(want));
    }

    /// A printed path is read only where the caller knows it as a file.
    #[test]
    fn a_printed_path_is_read_only_where_it_names_a_file() {
        let known = |p: &str| p == "src/lib.rs";
        let found = readings("src/lib.rs:7:let x = 1;", false, &known);
        assert!(found
            .iter()
            .any(|r| r.anchor_path.as_deref() == Some("src/lib.rs")
                && r.anchor_line == Some(7)
                && r.hash == line_hash(b"let x = 1;")));
        let found = readings("src/lib.rs:let x = 1;", false, &known);
        assert!(found
            .iter()
            .any(|r| r.anchor_line.is_none() && r.hash == line_hash(b"let x = 1;")));
        let found = readings("other.rs:7:let x = 1;", false, &known);
        assert!(found.iter().all(|r| r.anchor_path.is_none()));
    }

    /// A line's printed paths are its head before the first colon, and
    /// before a context line's `-N-`; a line with a blank first has none.
    #[test]
    fn a_line_prints_at_most_two_paths() {
        assert_eq!(printed_paths("src/a.rs:12:let x"), ["src/a.rs"]);
        assert_eq!(printed_paths("src/a-b.rs-12-let x"), ["src/a-b.rs"]);
        assert_eq!(printed_paths("let x: u32 = 1;"), Vec::<&str>::new());
        assert_eq!(printed_paths("no colon here"), Vec::<&str>::new());
    }

    /// A diff body reads through its marker, never through a file header.
    #[test]
    fn a_diff_body_reads_through_its_marker() {
        let found = readings("+let x = 1;", true, &no_path);
        assert!(found.iter().any(|r| r.hash == line_hash(b"let x = 1;")));
        assert_eq!(readings("+++ b/src/lib.rs", true, &no_path).len(), 1);
        assert_eq!(readings("+let x = 1;", false, &no_path).len(), 1);
    }

    /// A short line is trivial; a line of punctuation is trivial.
    #[test]
    fn a_slight_line_is_trivial() {
        assert!(is_trivial("  }"));
        assert!(is_trivial("})};"));
        assert!(!is_trivial("let x = 1;"));
    }

    proptest! {
        /// ★ For any line, the normaliser returns, keeps the line itself as
        /// its first reading, and every reading it strips to is a suffix of
        /// the line: a prefix is removed, never invented.
        #[test]
        fn the_normaliser_only_ever_strips(line in "\\PC{0,400}", diff in any::<bool>()) {
            let found = readings(&line, diff, &|p: &str| p.len().is_multiple_of(2));
            let body = line.strip_suffix('\r').unwrap_or(&line);
            prop_assert_eq!(found.first().map(|r| r.hash), Some(line_hash(body.as_bytes())));
            prop_assert!(found.len() <= 4);
            for r in &found {
                let suffix = (0..=body.len())
                    .filter(|at| body.is_char_boundary(*at))
                    .any(|at| line_hash(&body.as_bytes()[at..]) == r.hash)
                    || r.hash == line_hash(b"");
                prop_assert!(suffix, "a reading that is not a suffix of {:?}", line);
            }
        }

        /// The tools' own spellings, generated: the number and the content
        /// always come back.
        #[test]
        fn a_generated_numbered_line_reads_back(
            n in 1u32..1_000_000,
            content in "[a-z][a-z0-9 _(){};=]{3,60}",
            pad in 0usize..8,
        ) {
            let want = line_hash(content.as_bytes());
            for line in [
                format!("{}{n}\t{content}", " ".repeat(pad)),
                format!("{n}:{content}"),
                format!("{n}-{content}"),
                format!("src/a.rs:{n}:{content}"),
            ] {
                let found = readings(&line, false, &|p: &str| p == "src/a.rs");
                prop_assert!(
                    found.iter().any(|r| r.hash == want && r.anchor_line == Some(n)),
                    "{}", line
                );
            }
        }
    }
}
