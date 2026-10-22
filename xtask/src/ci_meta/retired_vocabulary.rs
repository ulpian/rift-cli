//! The retired-vocabulary screen and the lexer it runs.
//!
//! The lexer is one machine: a table of lexeme shapes, a lead-byte prefilter
//! and the byte-level tail matchers the table points at. It lives beside the
//! screen because a shape and the screen that reports it are read together,
//! and because nothing else in the workspace has a use for either.
//!
//! There are two tables. The global one runs over every tracked path and
//! byte; its shapes are long or specific enough that no product token spells
//! them. The scoped one holds the short ids — a letter and a digit or two, a
//! bare record number — and runs only over the written guide and the root
//! documents. Anywhere else those bytes are hashes, benchmark arms, fixture
//! headings and escaped code points, and a screen that cries wolf gets
//! suppressed, after which it guards nothing. The guide and the root
//! documents are where program prose would land, so that is where a short
//! id is always a citation.
use std::path::Path;

use super::tracked_paths;

/// One retired lexeme: the literal bytes it starts with, plus the shape that
/// must follow for the match to count.
///
/// Every lead is assembled with `concat!`, so this file spells no retired
/// identifier and needs no exemption from its own screen.
struct Retired {
    /// The literal bytes the lexeme opens with.
    lead: &'static str,
    /// Compare the lead ASCII-case-insensitively.
    fold_case: bool,
    /// The byte before the lead must be non-alphanumeric — the guard that
    /// keeps a short token from firing inside a hex or base64 run.
    boundary_before: bool,
    /// Prefixes the lead must not sit right after, for the notations the
    /// alphanumeric boundary cannot see because they end in punctuation.
    refused_after: &'static [&'static str],
    /// The shape after the lead. Returns the extra bytes it consumes, or
    /// `None` when this is not the lexeme after all.
    tail: fn(&[u8]) -> Option<usize>,
}

/// The lead is the whole lexeme.
fn nothing(_: &[u8]) -> Option<usize> {
    Some(0)
}

/// The lexeme ends here: the next byte, if any, is non-alphanumeric.
fn ends_here(rest: &[u8]) -> Option<usize> {
    if rest.first().is_none_or(|b| !b.is_ascii_alphanumeric()) {
        Some(0)
    } else {
        None
    }
}

/// One decimal digit.
fn digit(rest: &[u8]) -> Option<usize> {
    rest.first().filter(|b| b.is_ascii_digit()).map(|_| 1)
}

/// Four decimal digits — the decision-record numbering.
fn four_digits(rest: &[u8]) -> Option<usize> {
    rest.get(..4)
        .filter(|w| w.iter().all(u8::is_ascii_digit))
        .map(|_| 4)
}

/// One upper-case letter or digit — the requirement-document numbering.
fn upper_or_digit(rest: &[u8]) -> Option<usize> {
    rest.first()
        .filter(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        .map(|_| 1)
}

/// An optional plural `s`, an optional space or hyphen, then a digit — so the
/// singular, the plural, the spaced and the hyphenated citation all count.
fn numbered_tail(rest: &[u8]) -> Option<usize> {
    let mut at = 0;
    if rest.get(at).is_some_and(|b| b.eq_ignore_ascii_case(&b's')) {
        at += 1;
    }
    if rest.get(at).is_some_and(|b| *b == b' ' || *b == b'-') {
        at += 1;
    }
    if rest.get(at).is_some_and(u8::is_ascii_digit) {
        Some(at + 1)
    } else {
        None
    }
}

/// The second half of the retired master-plan filename, after an optional
/// backslash — Markdown escapes the separator, and an escaped citation is the
/// same citation.
fn master_tail(rest: &[u8]) -> Option<usize> {
    const SECOND_HALF: &str = concat!("_MAS", "TER");
    let escaped = usize::from(rest.first() == Some(&b'\\'));
    rest.get(escaped..escaped + SECOND_HALF.len())
        .filter(|w| *w == SECOND_HALF.as_bytes())
        .map(|_| escaped + SECOND_HALF.len())
}

/// The retired ticket numbering: a milestone digit, an optional point
/// release, a hyphen and a two-digit ticket, bounded on the right.
fn ticket_tail(rest: &[u8]) -> Option<usize> {
    let mut at = 0;
    if !rest.get(at).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    at += 1;
    if rest.get(at) == Some(&b'.') {
        at += 1;
        let first_fraction_digit = at;
        while rest.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == first_fraction_digit {
            return None;
        }
    }
    if rest.get(at) != Some(&b'-') {
        return None;
    }
    at += 1;
    if !rest.get(at).is_some_and(u8::is_ascii_digit)
        || !rest.get(at + 1).is_some_and(u8::is_ascii_digit)
    {
        return None;
    }
    at += 2;
    ends_here(rest.get(at..).unwrap_or_default()).map(|_| at)
}

/// The retired phase-and-step citation: a phase digit, a space, and a step,
/// bounded on the right.
fn step_tail(rest: &[u8]) -> Option<usize> {
    if !rest.first().is_some_and(|b| (b'1'..=b'7').contains(b))
        || rest.get(1) != Some(&b' ')
        || rest.get(2) != Some(&b'S')
        || !rest.get(3).is_some_and(u8::is_ascii_digit)
    {
        return None;
    }
    ends_here(rest.get(4..).unwrap_or_default()).map(|_| 4)
}

/// How many decimal digits open `rest`.
fn digit_run(rest: &[u8]) -> usize {
    rest.iter().take_while(|b| b.is_ascii_digit()).count()
}

/// The lexeme is `len` bytes past the lead, if it ends there.
fn bounded_at(rest: &[u8], len: usize) -> Option<usize> {
    ends_here(rest.get(len..).unwrap_or_default()).map(|_| len)
}

/// One digit, bounded on the right.
fn one_digit_bounded(rest: &[u8]) -> Option<usize> {
    digit(rest).and_then(|len| bounded_at(rest, len))
}

/// A run of digits, bounded on the right.
fn digits_bounded(rest: &[u8]) -> Option<usize> {
    match digit_run(rest) {
        0 => None,
        len => bounded_at(rest, len),
    }
}

/// A run of digits, then at most one lower-case letter, bounded on the
/// right — a numbered id with its lettered sub-part.
fn digits_lettered_bounded(rest: &[u8]) -> Option<usize> {
    let digits = digit_run(rest);
    if digits == 0 {
        return None;
    }
    let lettered = digits + usize::from(rest.get(digits).is_some_and(u8::is_ascii_lowercase));
    bounded_at(rest, lettered).or_else(|| bounded_at(rest, digits))
}

/// A run of digits, then at most one ASCII letter of either case, bounded on
/// the right — a lettered id may spell its sub-part in upper case.
fn digits_any_letter_bounded(rest: &[u8]) -> Option<usize> {
    let digits = digit_run(rest);
    if digits == 0 {
        return None;
    }
    let lettered = digits + usize::from(rest.get(digits).is_some_and(u8::is_ascii_alphabetic));
    bounded_at(rest, lettered).or_else(|| bounded_at(rest, digits))
}

/// Exactly two digits, bounded on the right.
fn two_digits_bounded(rest: &[u8]) -> Option<usize> {
    (digit_run(rest) == 2)
        .then_some(2)
        .and_then(|len| bounded_at(rest, len))
}

/// A run of digits, unbounded: an item number may carry a suffix.
fn digits_any(rest: &[u8]) -> Option<usize> {
    Some(digit_run(rest)).filter(|len| *len > 0)
}

/// The digit 1 or 2, bounded on the right.
fn one_or_two_bounded(rest: &[u8]) -> Option<usize> {
    if !rest.first().is_some_and(|b| *b == b'1' || *b == b'2') {
        return None;
    }
    bounded_at(rest, 1)
}

/// A stream digit, a hyphen and an item number, bounded on the right.
fn stream_item_tail(rest: &[u8]) -> Option<usize> {
    if !rest.first().is_some_and(u8::is_ascii_digit) || rest.get(1) != Some(&b'-') {
        return None;
    }
    digits_bounded(rest.get(2..).unwrap_or_default()).map(|len| len + 2)
}

/// The second half of a record number from 0082 to 0092, bounded.
fn record_tail(rest: &[u8]) -> Option<usize> {
    let in_range = match rest.get(..2) {
        Some([b'8', low]) => (b'2'..=b'9').contains(low),
        Some([b'9', low]) => (b'0'..=b'2').contains(low),
        _ => false,
    };
    if !in_range {
        return None;
    }
    bounded_at(rest, 2)
}

/// The retired program vocabulary, one entry per lexeme shape.
///
/// Deliberately absent: the bare phase, milestone and gate letters. Two or
/// three characters wide, they collide constantly with hex digests, base64
/// payloads and benchmark arm names (a letter-digit arm prefix is an arm, not a gate), so
/// they were swept once by eye rather than screened forever — a screen that
/// cries wolf gets suppressed, and then it guards nothing.
const RETIRED: &[Retired] = &[
    Retired {
        lead: concat!("AD", "R-"),
        fold_case: false,
        boundary_before: false,
        refused_after: &[],
        tail: four_digits,
    },
    Retired {
        lead: concat!("PR", "D-"),
        fold_case: false,
        boundary_before: false,
        refused_after: &[],
        tail: upper_or_digit,
    },
    Retired {
        lead: concat!("rul", "ing"),
        fold_case: true,
        boundary_before: false,
        refused_after: &[],
        tail: numbered_tail,
    },
    Retired {
        lead: concat!("SW", "T-"),
        fold_case: false,
        boundary_before: false,
        refused_after: &[],
        tail: digit,
    },
    Retired {
        lead: concat!("pre-", "SW", "T"),
        fold_case: false,
        boundary_before: false,
        refused_after: &[],
        tail: nothing,
    },
    Retired {
        lead: concat!("RE", "C-"),
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: digit,
    },
    Retired {
        lead: concat!("gate-", "p8"),
        fold_case: false,
        boundary_before: false,
        refused_after: &[],
        tail: nothing,
    },
    Retired {
        lead: concat!("gate-", "swt"),
        fold_case: false,
        boundary_before: false,
        refused_after: &[],
        tail: nothing,
    },
    Retired {
        lead: concat!("gate-", "m"),
        fold_case: false,
        boundary_before: false,
        refused_after: &[],
        tail: digit,
    },
    Retired {
        lead: concat!("GO", "AL"),
        fold_case: false,
        boundary_before: false,
        refused_after: &[],
        tail: master_tail,
    },
    Retired {
        lead: concat!("CHAR", "TER"),
        fold_case: false,
        boundary_before: false,
        refused_after: &[],
        tail: nothing,
    },
    Retired {
        lead: concat!("RESI", "DUE"),
        fold_case: false,
        boundary_before: false,
        refused_after: &[],
        tail: nothing,
    },
    Retired {
        lead: concat!("Sta", "ge "),
        fold_case: false,
        boundary_before: false,
        refused_after: &[],
        tail: digit,
    },
    Retired {
        lead: concat!("G", "7C"),
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: ends_here,
    },
    Retired {
        lead: "M",
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: ticket_tail,
    },
    Retired {
        lead: "P",
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: step_tail,
    },
    Retired {
        lead: concat!("SW", "T"),
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: ends_here,
    },
    Retired {
        lead: concat!("ent", "ire"),
        fold_case: true,
        boundary_before: false,
        refused_after: &[],
        tail: nothing,
    },
    Retired {
        lead: concat!("session", "-imp"),
        fold_case: true,
        boundary_before: true,
        refused_after: &[],
        tail: ends_here,
    },
    Retired {
        lead: concat!("capture", "-facts"),
        fold_case: true,
        boundary_before: true,
        refused_after: &[],
        tail: ends_here,
    },
    Retired {
        lead: concat!("token", "-efficiency"),
        fold_case: true,
        boundary_before: true,
        refused_after: &[],
        tail: ends_here,
    },
    Retired {
        lead: concat!("alpha", "-goal"),
        fold_case: true,
        boundary_before: true,
        refused_after: &[],
        tail: ends_here,
    },
    Retired {
        lead: concat!("wind", "sup"),
        fold_case: true,
        boundary_before: true,
        refused_after: &[],
        tail: ends_here,
    },
    Retired {
        lead: concat!("CF", "X-"),
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: digit,
    },
    Retired {
        lead: concat!("RP", "F-"),
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: digit,
    },
    Retired {
        lead: concat!("00-dec", "isions"),
        fold_case: true,
        boundary_before: false,
        refused_after: &[],
        tail: nothing,
    },
    Retired {
        lead: "S",
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: stream_item_tail,
    },
    Retired {
        lead: "W",
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: digits_lettered_bounded,
    },
];

/// The short program ids, screened only where program prose would land.
///
/// Two or three bytes wide, these collide everywhere else: benchmark arms,
/// fixture headings, escaped code points, percent-encoded paths and test
/// strings all spell them legitimately, and a screen that cries wolf gets
/// suppressed. The guide and the root documents are written for readers, so
/// a bare id there is always a citation and never a value.
const SCOPED: &[Retired] = &[
    Retired {
        lead: "A",
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: one_digit_bounded,
    },
    Retired {
        lead: "B",
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: one_digit_bounded,
    },
    Retired {
        lead: "C",
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: one_digit_bounded,
    },
    Retired {
        lead: "E",
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: one_digit_bounded,
    },
    Retired {
        lead: "D",
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: digits_bounded,
    },
    Retired {
        lead: "O",
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: one_or_two_bounded,
    },
    Retired {
        lead: "Q",
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: digits_lettered_bounded,
    },
    Retired {
        lead: concat!("L", "-"),
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: digits_any_letter_bounded,
    },
    Retired {
        lead: concat!("CF", "-"),
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: two_digits_bounded,
    },
    Retired {
        lead: concat!("FG", "-"),
        fold_case: false,
        boundary_before: true,
        refused_after: &[],
        tail: digits_bounded,
    },
    Retired {
        lead: concat!("it", "em "),
        fold_case: true,
        boundary_before: true,
        refused_after: &[],
        tail: digits_any,
    },
    Retired {
        lead: "00",
        fold_case: false,
        boundary_before: true,
        refused_after: &["U+", "\\u", "0x"],
        tail: record_tail,
    },
];

/// The root documents the short shapes are screened over, beside the guide.
const SCOPED_ROOT_DOCS: [&str; 4] = ["CLAUDE.md", "SECURITY.md", "README.md", "AGENTS.md"];

/// The directory of the written guide, the other half of the scope.
const SCOPED_DIR: &str = "docs/guide/";

/// Whether the short shapes apply to this tracked path.
fn in_short_shape_scope(rel: &str) -> bool {
    rel.starts_with(SCOPED_DIR) || SCOPED_ROOT_DOCS.contains(&rel)
}

/// One table of shapes with its lead-byte prefilter.
struct Lexicon {
    entries: &'static [Retired],
    /// The bytes any entry can open with — so the common case (a byte no
    /// lexeme starts with) costs one lookup instead of a walk over every
    /// entry. Unknown bytes fail OPEN: a lookup that cannot answer runs the
    /// full match rather than skipping it.
    leads: [bool; 256],
}

impl Lexicon {
    fn new(entries: &'static [Retired]) -> Lexicon {
        let mut leads = [false; 256];
        for needle in entries {
            let Some(&first) = needle.lead.as_bytes().first() else {
                continue;
            };
            for byte in [first.to_ascii_lowercase(), first.to_ascii_uppercase()] {
                if !needle.fold_case && byte != first {
                    continue;
                }
                if let Some(slot) = leads.get_mut(usize::from(byte)) {
                    *slot = true;
                }
            }
        }
        Lexicon { entries, leads }
    }

    /// The shapes screened over every tracked path and byte.
    fn global() -> Lexicon {
        Lexicon::new(RETIRED)
    }

    /// The short shapes screened over the guide and the root documents only.
    fn scoped() -> Lexicon {
        Lexicon::new(SCOPED)
    }

    /// The lexeme starting at `at`, if one does. First entry wins, so the
    /// more specific shapes are listed before the bare ones.
    fn lexeme_at(&self, hay: &[u8], at: usize) -> Option<usize> {
        for needle in self.entries {
            let lead = needle.lead.as_bytes();
            let Some(window) = hay.get(at..at + lead.len()) else {
                continue;
            };
            let opens = if needle.fold_case {
                window.eq_ignore_ascii_case(lead)
            } else {
                window == lead
            };
            if !opens || !needle.bounded_before(hay.get(..at).unwrap_or_default()) {
                continue;
            }
            if let Some(extra) = (needle.tail)(hay.get(at + lead.len()..).unwrap_or_default()) {
                return Some(lead.len() + extra);
            }
        }
        None
    }

    /// Every lexeme in `hay`, as it is spelled there.
    ///
    /// A match consumes its own bytes, so one site is reported once even
    /// when a wider shape contains a narrower one.
    fn lexemes(&self, hay: &[u8]) -> Vec<String> {
        let mut found = Vec::new();
        let mut at = 0;
        while at < hay.len() {
            let Some(&byte) = hay.get(at) else { break };
            if self.leads.get(usize::from(byte)).copied().unwrap_or(true) {
                if let Some(len) = self.lexeme_at(hay, at) {
                    found.push(
                        String::from_utf8_lossy(hay.get(at..at + len).unwrap_or_default())
                            .into_owned(),
                    );
                    at += len.max(1);
                    continue;
                }
            }
            at += 1;
        }
        found
    }
}

impl Retired {
    /// Whether the bytes before the lead let it open a lexeme.
    fn bounded_before(&self, before: &[u8]) -> bool {
        if self.boundary_before && before.last().is_some_and(u8::is_ascii_alphanumeric) {
            return false;
        }
        !self
            .refused_after
            .iter()
            .any(|prefix| before.ends_with(prefix.as_bytes()))
    }
}

/// The lexemes of every lexicon in `hay`, in lexicon order.
fn lexemes_in(hay: &[u8], lexicons: &[&Lexicon]) -> Vec<String> {
    lexicons.iter().flat_map(|l| l.lexemes(hay)).collect()
}

/// No tracked path or tracked byte carries the retired program vocabulary.
///
/// The corpus is `tracked_paths` — the git index, not the filesystem — with
/// NO exclusions: lockfiles, snapshots, fixtures, corpora and the written
/// guide are all in it, because the vocabulary this screen retires is exactly
/// the vocabulary a reader of any of those would trip over. Both halves of a
/// tracked file are scanned, its path bytes and its content bytes. The short
/// shapes are the one narrowing: they run over the guide and the root
/// documents only (see `SCOPED`).
///
/// A symlink is scanned as its LINK TEXT and never followed. Following one
/// can land on a directory (the committed skill links do), and reading a
/// target inspects bytes git does not track while skipping the bytes it does.
///
/// Text files are reported by line; a file holding a NUL byte is reported
/// whole, since it has no lines to name.
///
/// Returns `(files examined, violations)`. The count is not decoration: the
/// caller refuses a screen that examined nothing, so a corpus that silently
/// empties out is visible instead of passing.
///
/// # Errors
/// If `git ls-files` fails, a tracked file cannot be read, or the walk
/// returned no files at all.
pub fn retired_vocabulary_screen(root: &Path) -> Result<(usize, Vec<String>), String> {
    screen_tracked(root, &tracked_paths(root)?)
}

/// The screen over an explicit path list — the seam the planted-hit fixture
/// uses, so the shape checks need no repository of their own.
fn screen_tracked(root: &Path, paths: &[String]) -> Result<(usize, Vec<String>), String> {
    const ADVICE: &str = "rewrite to plain-language rationale";
    if paths.is_empty() {
        return Err("retired-vocabulary screen: the tracked-path walk returned nothing".to_owned());
    }
    let global = Lexicon::global();
    let scoped = Lexicon::scoped();
    let mut v = Vec::new();
    for rel in paths {
        let lexicons: &[&Lexicon] = if in_short_shape_scope(rel) {
            &[&global, &scoped]
        } else {
            &[&global]
        };
        for lexeme in lexemes_in(rel.as_bytes(), lexicons) {
            v.push(format!(
                "{rel}: retired program vocabulary '{lexeme}' in the tracked path — {ADVICE}"
            ));
        }
        let bytes = tracked_bytes(root, rel)?;
        if bytes.contains(&0) {
            for lexeme in lexemes_in(&bytes, lexicons) {
                v.push(format!(
                    "{rel}: retired program vocabulary '{lexeme}' in file bytes (binary)"
                ));
            }
            continue;
        }
        for (index, line) in bytes.split(|b| *b == b'\n').enumerate() {
            let number = index + 1;
            for lexeme in lexemes_in(line, lexicons) {
                v.push(format!(
                    "{rel}:{number}: retired program vocabulary '{lexeme}' — {ADVICE}"
                ));
            }
        }
    }
    Ok((paths.len(), v))
}

/// A tracked file's bytes, or a symlink's link text.
fn tracked_bytes(root: &Path, rel: &str) -> Result<Vec<u8>, String> {
    let full = root.join(rel);
    let metadata =
        std::fs::symlink_metadata(&full).map_err(|e| format!("inspect tracked file {rel}: {e}"))?;
    if !metadata.file_type().is_symlink() {
        return std::fs::read(&full).map_err(|e| format!("read tracked file {rel}: {e}"));
    }
    Ok(std::fs::read_link(&full)
        .map_err(|e| format!("read tracked symlink {rel}: {e}"))?
        .as_os_str()
        .to_string_lossy()
        .into_owned()
        .into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;
    use crate::ci_meta::tracked_paths;
    use crate::workspace_root;

    /// The live repository carries none of the retired vocabulary, and the
    /// corpus it was screened over is the whole index — not a handful of
    /// files that happened to survive a filter.
    #[test]
    fn the_repository_carries_no_retired_vocabulary() {
        let (scanned, violations) =
            retired_vocabulary_screen(&workspace_root()).expect("the screen runs");
        assert!(
            violations.is_empty(),
            "retired vocabulary:\n{}",
            violations.join("\n")
        );
        assert!(scanned > 400, "the screen examined only {scanned} files");
    }

    /// A planted citation is caught, named, and located — the proof the
    /// screen's silence on the live tree means absence and not blindness.
    #[test]
    fn a_planted_citation_is_one_located_violation() {
        let root = scratch_dir("retired-vocabulary");
        let planted = format!("a first line\nsee {}0001 for why\n", concat!("AD", "R-"));
        std::fs::write(root.join("notes.md"), planted).unwrap();

        let (scanned, violations) =
            screen_tracked(&root, &["notes.md".to_string()]).expect("the screen runs");
        assert_eq!(scanned, 1);
        assert_eq!(violations.len(), 1, "{violations:?}");
        let only = violations.first().unwrap();
        assert!(
            only.starts_with("notes.md:2: retired program vocabulary "),
            "{only}"
        );
        assert!(
            only.ends_with("rewrite to plain-language rationale"),
            "{only}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// An empty corpus is refused rather than reported as clean.
    #[test]
    fn a_screen_over_nothing_is_an_error() {
        assert!(screen_tracked(&workspace_root(), &[]).is_err());
    }

    /// Each shape, and the near-misses that must stay quiet: the benchmark
    /// arm names (letter-digit prefixes followed by an underscore) live in the tracked corpus
    /// and are not citations, and a bare milestone letter inside a digest is
    /// the reason the short tokens were left out of the table.
    #[test]
    fn every_shape_matches_itself_and_nothing_near_it() {
        let global = Lexicon::global();
        let hit = |s: &str| !global.lexemes(s.as_bytes()).is_empty();

        for spelled in [
            concat!("AD", "R-", "0069"),
            concat!("PR", "D-", "M6"),
            concat!("rul", "ing 52"),
            concat!("Rul", "ings-3"),
            concat!("RUL", "ING7"),
            concat!("SW", "T-", "06"),
            concat!("pre-", "SW", "T"),
            concat!(" ", "RE", "C-", "2"),
            concat!("gate-", "p8"),
            concat!("gate-", "swt"),
            concat!("gate-", "m5"),
            concat!("GO", "AL", "_MAS", "TER"),
            concat!("GO", "AL", "\\", "_MAS", "TER"),
            concat!("CHAR", "TER"),
            concat!("RESI", "DUE"),
            concat!("Sta", "ge 7"),
            concat!(" ", "G", "7C", " "),
            concat!(" M", "5-01 "),
            concat!(" M", "5.15", "-01 "),
            concat!(" P", "6 S", "3 "),
            concat!(" ", "SW", "T", " "),
            concat!("the ", "ENT", "IRE", " thing"),
        ] {
            assert!(hit(spelled), "missed {spelled}");
        }

        for quiet in [
            concat!("G", "1_line_8k"),
            "F1_newline_8k",
            "a rule of thumb",
            "the gateway",
            "record the outcome",
            concat!("no", "RE", "C-", "2"),
            concat!("x", "G", "7C"),
            concat!("G", "7Cx"),
            " M5-0123 ",
            "aM5-01 ",
            concat!(" P", "8 "),
            " S3 ",
            "M5.-01 ",
            concat!("GO", "AL posts"),
            concat!("chart", "er"),
        ] {
            assert!(!hit(quiet), "false positive on {quiet}");
        }
    }

    /// One site is reported once: the wider shape consumes its own bytes, so
    /// the bare token inside it does not fire a second time.
    #[test]
    fn a_wider_shape_swallows_the_bare_one_inside_it() {
        let found = Lexicon::global().lexemes(concat!(" ", "SW", "T-", "06 ").as_bytes());
        assert_eq!(found.len(), 1, "{found:?}");
    }

    /// A NUL byte means the file has no lines worth naming, so the whole
    /// file is the location.
    #[test]
    fn binary_content_is_reported_without_a_line_number() {
        let root = scratch_dir("retired-vocabulary-bin");
        let mut planted = b"\0\0".to_vec();
        planted.extend_from_slice(concat!("gate-", "p8").as_bytes());
        std::fs::write(root.join("blob.bin"), planted).unwrap();

        let (_, violations) =
            screen_tracked(&root, &["blob.bin".to_string()]).expect("the screen runs");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations
                .first()
                .unwrap()
                .ends_with("in file bytes (binary)"),
            "{violations:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The path half of the corpus is screened too — a filename can carry a
    /// citation with no byte of the file mentioning it.
    #[test]
    fn a_citation_in_the_pathname_is_caught() {
        let root = scratch_dir("retired-vocabulary-path");
        let named = format!("{}0007-notes.md", concat!("AD", "R-"));
        std::fs::write(root.join(&named), "clean text\n").unwrap();

        let (_, violations) = screen_tracked(&root, &[named]).expect("the screen runs");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations.first().unwrap().contains("in the tracked path"),
            "{violations:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The short shapes, each planted once, one per line. Spelled in halves
    /// so this file carries none of them whole.
    const SHORT_SHAPES: [&str; 15] = [
        concat!("see ", "A", "1 here"),
        concat!("see ", "E", "9."),
        concat!("(", "D", "12)"),
        concat!("per ", "O", "1 and"),
        concat!("per ", "O", "2"),
        concat!("asked in ", "Q", "41"),
        concat!("asked in ", "Q", "7b,"),
        concat!("closes ", "it", "em 6"),
        concat!("record ", "00", "82 says"),
        concat!("record ", "00", "92"),
        concat!("[", "C", "3]"),
        concat!("per ", "L", "-5 and"),
        concat!("(", "L", "-0A)"),
        concat!("see ", "CF", "-13."),
        concat!("under ", "FG", "-2,"),
    ];

    /// The program words and stream ids are retired everywhere: no tracked
    /// file has a legitimate use for them.
    #[test]
    fn every_global_program_shape_fires_anywhere() {
        let global = Lexicon::global();
        let hit = |s: &str| !global.lexemes(s.as_bytes()).is_empty();
        for spelled in [
            concat!("the ", "session", "-imp", " notes"),
            concat!("Capture", "-Facts"),
            concat!("token", "-efficiency", "/"),
            concat!("docs/", "alpha", "-goal", "/x"),
            concat!("wind", "sup/"),
            concat!(" ", "S", "3-12 "),
            concat!("(", "S", "8-1)"),
            concat!(" ", "W", "1 "),
            concat!(" ", "W", "12,"),
            concat!(" ", "W", "1b."),
            concat!(" ", "W", "13-1b "),
            concat!("(", "CF", "X-4)"),
            concat!(" ", "RP", "F-3a "),
            concat!("the ", "00-dec", "isions", " file"),
        ] {
            assert!(hit(spelled), "missed {spelled}");
        }
        for quiet in [
            concat!("x", "S", "3-12"),
            concat!(" ", "S", "3-x"),
            concat!(" ", "S", "31-2"),
            concat!(" ", "W", "1B "),
            concat!(" ", "W", "1bc "),
            concat!("a", "W", "1 "),
            concat!(" ", "W", "x "),
            concat!("mini", "wind", "sup"),
            concat!("x", "CF", "X-4"),
            concat!(" ", "CF", "X-x"),
            concat!("a", "RP", "F-3"),
            concat!("00-", "decided"),
        ] {
            assert!(!hit(quiet), "false positive on {quiet}");
        }
    }

    /// Plant `text` at each path and screen exactly those paths.
    fn screen_planted(name: &str, paths: &[&str], text: &str) -> Vec<String> {
        let root = scratch_dir(name);
        for rel in paths {
            let full = root.join(rel);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
        let owned: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
        let (_, violations) = screen_tracked(&root, &owned).expect("the screen runs");
        std::fs::remove_dir_all(&root).unwrap();
        violations
    }

    /// Inside the guide and every root document, each short shape is one
    /// located violation.
    #[test]
    fn every_short_shape_fires_inside_the_guide_and_the_root_docs() {
        let text = SHORT_SHAPES.join("\n");
        let mut paths = vec!["docs/guide/99-planted.md"];
        paths.extend(SCOPED_ROOT_DOCS);
        let violations = screen_planted("retired-short-in-scope", &paths, &text);
        assert_eq!(
            violations.len(),
            SHORT_SHAPES.len() * paths.len(),
            "{violations:#?}"
        );
        for rel in paths {
            for number in 1..=SHORT_SHAPES.len() {
                let site = format!("{rel}:{number}: ");
                assert!(
                    violations.iter().any(|v| v.starts_with(&site)),
                    "nothing at {site}"
                );
            }
        }
    }

    /// The same bytes outside the scope are values, not citations, and a
    /// root document's name one directory down is outside it too.
    #[test]
    fn the_short_shapes_stay_silent_outside_their_scope() {
        let text = SHORT_SHAPES.join("\n");
        let paths = [
            "crates/x/src/lib.rs",
            "docs/guide.md",
            "benches/arms/README.md",
            "fixtures/CLAUDE.md",
        ];
        let violations = screen_planted("retired-short-out-of-scope", &paths, &text);
        assert!(violations.is_empty(), "{violations:#?}");
    }

    /// The near-misses a guide may legitimately carry: escaped and prefixed
    /// code points, hex runs, longer numbers and lower-case ids.
    #[test]
    fn the_short_shapes_ignore_code_points_hex_and_longer_runs() {
        let scoped = Lexicon::scoped();
        for quiet in [
            concat!("U+", "00", "85"),
            concat!("\\u", "00", "85"),
            concat!("0x", "00", "85"),
            concat!("ff", "00", "85"),
            concat!(" ", "00", "81 "),
            concat!(" ", "00", "93 "),
            concat!(" ", "00", "820 "),
            concat!("%", "D", "0x"),
            concat!(" ", "a", "1 "),
            concat!(" ", "F", "1 "),
            concat!(" ", "A", "12 "),
            concat!(" ", "O", "3 "),
            concat!(" ", "Q", "4X "),
            concat!(" ", "Q", "4ab "),
            concat!(" C", "L", "-5 "),
            concat!(" ", "L", "-x "),
            concat!(" ", "L", "-5ab "),
            concat!(" ", "CF", "-1 "),
            concat!(" ", "CF", "-123 "),
            concat!(" X", "FG", "-2 "),
            concat!(" ", "FG", "-2x "),
            concat!("6", "D", "1"),
            "an item list",
        ] {
            let found = scoped.lexemes(quiet.as_bytes());
            assert!(found.is_empty(), "false positive on {quiet}: {found:?}");
        }
    }

    /// The program folders are untracked, so the live screen is the proof
    /// that the repository stays green with them deleted: its corpus is the
    /// index, and the index holds nothing under any of them.
    #[test]
    fn the_screen_corpus_is_the_index_and_holds_no_program_folder() {
        let root = workspace_root();
        let tracked = tracked_paths(&root).expect("the index lists");
        let (scanned, _) = retired_vocabulary_screen(&root).expect("the screen runs");
        assert_eq!(scanned, tracked.len());
        for folder in [
            concat!("docs/", "alpha", "-goal/"),
            concat!("docs/", "token", "-efficiency/"),
            concat!("docs/", "session", "-imp/"),
            concat!("docs/", "capture", "-facts/"),
        ] {
            let inside: Vec<_> = tracked.iter().filter(|p| p.starts_with(folder)).collect();
            assert!(inside.is_empty(), "{folder} is tracked: {inside:?}");
        }
    }
}
