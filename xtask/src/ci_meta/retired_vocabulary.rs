//! The retired-vocabulary screen and the lexer it runs.
//!
//! The lexer is one machine: a table of lexeme shapes, a lead-byte prefilter
//! and the byte-level tail matchers the table points at. It lives beside the
//! screen because a shape and the screen that reports it are read together,
//! and because nothing else in the workspace has a use for either.
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
        tail: four_digits,
    },
    Retired {
        lead: concat!("PR", "D-"),
        fold_case: false,
        boundary_before: false,
        tail: upper_or_digit,
    },
    Retired {
        lead: concat!("rul", "ing"),
        fold_case: true,
        boundary_before: false,
        tail: numbered_tail,
    },
    Retired {
        lead: concat!("SW", "T-"),
        fold_case: false,
        boundary_before: false,
        tail: digit,
    },
    Retired {
        lead: concat!("pre-", "SW", "T"),
        fold_case: false,
        boundary_before: false,
        tail: nothing,
    },
    Retired {
        lead: concat!("RE", "C-"),
        fold_case: false,
        boundary_before: true,
        tail: digit,
    },
    Retired {
        lead: concat!("gate-", "p8"),
        fold_case: false,
        boundary_before: false,
        tail: nothing,
    },
    Retired {
        lead: concat!("gate-", "swt"),
        fold_case: false,
        boundary_before: false,
        tail: nothing,
    },
    Retired {
        lead: concat!("gate-", "m"),
        fold_case: false,
        boundary_before: false,
        tail: digit,
    },
    Retired {
        lead: concat!("GO", "AL"),
        fold_case: false,
        boundary_before: false,
        tail: master_tail,
    },
    Retired {
        lead: concat!("CHAR", "TER"),
        fold_case: false,
        boundary_before: false,
        tail: nothing,
    },
    Retired {
        lead: concat!("RESI", "DUE"),
        fold_case: false,
        boundary_before: false,
        tail: nothing,
    },
    Retired {
        lead: concat!("Sta", "ge "),
        fold_case: false,
        boundary_before: false,
        tail: digit,
    },
    Retired {
        lead: concat!("G", "7C"),
        fold_case: false,
        boundary_before: true,
        tail: ends_here,
    },
    Retired {
        lead: "M",
        fold_case: false,
        boundary_before: true,
        tail: ticket_tail,
    },
    Retired {
        lead: "P",
        fold_case: false,
        boundary_before: true,
        tail: step_tail,
    },
    Retired {
        lead: concat!("SW", "T"),
        fold_case: false,
        boundary_before: true,
        tail: ends_here,
    },
    Retired {
        lead: concat!("ent", "ire"),
        fold_case: true,
        boundary_before: false,
        tail: nothing,
    },
];

/// The bytes any retired lexeme can open with — a prefilter, so the common
/// case (a byte no lexeme starts with) costs one table lookup instead of a
/// walk over every entry. Unknown bytes fail OPEN: a lookup that cannot
/// answer runs the full match rather than skipping it.
fn lead_bytes() -> [bool; 256] {
    let mut table = [false; 256];
    for needle in RETIRED {
        let Some(&first) = needle.lead.as_bytes().first() else {
            continue;
        };
        for byte in [first.to_ascii_lowercase(), first.to_ascii_uppercase()] {
            if !needle.fold_case && byte != first {
                continue;
            }
            if let Some(slot) = table.get_mut(usize::from(byte)) {
                *slot = true;
            }
        }
    }
    table
}

/// The lexeme starting at `at`, if one does. First entry wins, so the more
/// specific shapes are listed before the bare ones.
fn lexeme_at(hay: &[u8], at: usize) -> Option<usize> {
    for needle in RETIRED {
        let lead = needle.lead.as_bytes();
        let Some(window) = hay.get(at..at + lead.len()) else {
            continue;
        };
        let opens = if needle.fold_case {
            window.eq_ignore_ascii_case(lead)
        } else {
            window == lead
        };
        if !opens {
            continue;
        }
        if needle.boundary_before
            && at > 0
            && hay.get(at - 1).is_some_and(u8::is_ascii_alphanumeric)
        {
            continue;
        }
        if let Some(extra) = (needle.tail)(hay.get(at + lead.len()..).unwrap_or_default()) {
            return Some(lead.len() + extra);
        }
    }
    None
}

/// Every retired lexeme in `hay`, as it is spelled there.
///
/// A match consumes its own bytes, so one site is reported once even when a
/// wider shape contains a narrower one.
fn retired_lexemes(hay: &[u8], table: &[bool; 256]) -> Vec<String> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < hay.len() {
        let Some(&byte) = hay.get(at) else { break };
        if table.get(usize::from(byte)).copied().unwrap_or(true) {
            if let Some(len) = lexeme_at(hay, at) {
                found.push(
                    String::from_utf8_lossy(hay.get(at..at + len).unwrap_or_default()).into_owned(),
                );
                at += len.max(1);
                continue;
            }
        }
        at += 1;
    }
    found
}

/// No tracked path or tracked byte carries the retired program vocabulary.
///
/// The corpus is `tracked_paths` — the git index, not the filesystem — with
/// NO exclusions: lockfiles, snapshots, fixtures, corpora and the written
/// guide are all in it, because the vocabulary this screen retires is exactly
/// the vocabulary a reader of any of those would trip over. Both halves of a
/// tracked file are scanned, its path bytes and its content bytes.
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
    let table = lead_bytes();
    let mut v = Vec::new();
    for rel in paths {
        for lexeme in retired_lexemes(rel.as_bytes(), &table) {
            v.push(format!(
                "{rel}: retired program vocabulary '{lexeme}' in the tracked path — {ADVICE}"
            ));
        }
        let full = root.join(rel);
        let metadata = std::fs::symlink_metadata(&full)
            .map_err(|e| format!("inspect tracked file {rel}: {e}"))?;
        let bytes = if metadata.file_type().is_symlink() {
            std::fs::read_link(&full)
                .map_err(|e| format!("read tracked symlink {rel}: {e}"))?
                .as_os_str()
                .to_string_lossy()
                .into_owned()
                .into_bytes()
        } else {
            std::fs::read(&full).map_err(|e| format!("read tracked file {rel}: {e}"))?
        };
        if bytes.contains(&0) {
            for lexeme in retired_lexemes(&bytes, &table) {
                v.push(format!(
                    "{rel}: retired program vocabulary '{lexeme}' in file bytes (binary)"
                ));
            }
        } else {
            for (index, line) in bytes.split(|b| *b == b'\n').enumerate() {
                let number = index + 1;
                for lexeme in retired_lexemes(line, &table) {
                    v.push(format!(
                        "{rel}:{number}: retired program vocabulary '{lexeme}' — {ADVICE}"
                    ));
                }
            }
        }
    }
    Ok((paths.len(), v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;
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
        let table = lead_bytes();
        let hit = |s: &str| !retired_lexemes(s.as_bytes(), &table).is_empty();

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
        let table = lead_bytes();
        let found = retired_lexemes(concat!(" ", "SW", "T-", "06 ").as_bytes(), &table);
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
}
