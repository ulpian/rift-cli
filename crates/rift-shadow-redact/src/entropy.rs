//! The entropy layer: the detector for the secret nobody wrote a shape for.
//! A token is a candidate by length and Shannon entropy, and is left alone
//! when it is spelled the way an identifier, a path or a file name is spelled
//! ([`word_shaped`]). Its own module because its rules are about spelling and
//! information content, not about any format an issuing authority publishes.

use crate::span::{MatchedAgainst, Span};
use crate::DetectorClass;

/// The entropy layer's detector id. It has no [`ShapeDetector`] to carry it, so
/// the one place a pack rule could collide with it unseen is
/// [`is_builtin_detector_id`] — hence a constant rather than a third literal.
pub(crate) const ENTROPY_DETECTOR_ID: &str = "entropy.high";

/// Minimum token length / entropy for the pass-2 entropy detector.
pub(crate) const ENTROPY_MIN_LEN: usize = 20;
pub(crate) const ENTROPY_MIN_BITS: f64 = 4.0;

/// The longest run of one letter case the word-shape exemption reads as a
/// word. A same-case run longer than this is not an English word or a
/// CamelCase hump, it is a letters-only secret, and the exemption must not
/// take it: a base32 token that happens to draw no digit is exactly that run.
pub(crate) const WORD_PIECE_MAX_LEN: usize = 20;

pub(crate) fn entropy_spans(text: &str) -> Vec<Span> {
    let bytes = text.as_bytes();
    let is_token =
        |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-');
    let mut spans = Vec::new();
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if is_token(b) {
            let start = i;
            while bytes.get(i).copied().is_some_and(is_token) {
                i += 1;
            }
            let tok = &text[start..i];
            if tok.len() >= ENTROPY_MIN_LEN
                && !word_shaped(tok)
                && shannon_bits(tok) >= ENTROPY_MIN_BITS
            {
                spans.push(Span {
                    start,
                    end: i,
                    class: DetectorClass::Entropy,
                    id: ENTROPY_DETECTOR_ID.into(),
                    parse: None,
                    matched: MatchedAgainst::Input,
                });
            }
        } else {
            i += 1;
        }
    }
    spans
}

/// Whether an entropy candidate is **word-shaped**: spelled the way an
/// identifier, a path or a file name is spelled, and therefore left alone by
/// the entropy layer, which exists for the secret nobody wrote a shape for.
///
/// The token class `+ / = _ -` is generous on purpose — it is what lets a
/// base64 blob be one token — and it is also why a snake_case test name, a
/// CamelCase type or an absolute build path is one token of twenty-plus
/// characters with enough distinct letters to clear 4.0 bits. Measured over
/// this repository's own text, every one of the entropy layer's hits that a
/// hand ever classified was one of those (30 of 30, recorded in
/// `benchmarks/redaction-fp-baseline.md`), and that layer carried 468 of the
/// pipeline's 554 masked spans. This is the rule that reads them as words
/// instead, and it is written so that a random generator cannot satisfy it:
///
/// * a token holding `+` or `=` is never word-shaped — those are base64's
///   letters and a `NAME=value` pair is judged as it always was;
/// * the token is split on `_ - /` into segments, and **every** segment must
///   be word-shaped, so a base64url secret — whose `-` and `_` are letters,
///   not separators — fails on its first mixed-case segment;
/// * a segment is a sequence of *pieces*: a digit run, a lowercase run, an
///   uppercase run, or a Capitalized word (one uppercase, then lowercase);
///   each letter piece is at most [`WORD_PIECE_MAX_LEN`] long, so a
///   letters-only secret is refused by its length, and at most **one** piece
///   may be a single letter, and only beside digits (`v7`, `p95`, `3d`,
///   `0x0011`), because a random alphanumeric string spells single letters
///   between its digits everywhere;
/// * letters and digits may meet at most twice in a segment (`sha256`,
///   `full3d`, `m515seed`), because a base32 or alphanumeric secret alternates
///   between them all the way along — `JBSWY3DPEHPK3PXP` is not a word with
///   numbers in it;
/// * a segment that is one character — the article in
///   `a_hook_measure_that_…`, the `d` in a build path — is a word.
///
/// What it deliberately still masks, each pinned by a test: a ULID or any
/// other base32 id, a base64 or base64url token, a hex-bearing path, a
/// `key=value` pair whose value is high-entropy, an escaped vendor key the
/// JSON walker falls back on, and a same-case run of twenty-one letters or
/// more. What it accepts and must be said plainly: a secret spelled as
/// same-case letter groups of twenty or fewer, joined by separators or by at
/// most one digit run, is read as words. That is the residual, and
/// `docs/guide/04` names it.
pub(crate) fn word_shaped(token: &str) -> bool {
    if token.bytes().any(|b| matches!(b, b'+' | b'=')) {
        return false;
    }
    token
        .split(['_', '-', '/'])
        .filter(|segment| !segment.is_empty())
        .all(segment_is_word_shaped)
}

pub(crate) fn shannon_bits(token: &str) -> f64 {
    let mut counts = [0u32; 256];
    let len = token.len();
    if len == 0 {
        return 0.0;
    }
    for b in token.bytes() {
        // a u8 index into a 256-slot histogram cannot be out of range
        if let Some(c) = counts.get_mut(usize::from(b)) {
            *c += 1;
        }
    }
    let n = len as f64;
    let mut bits = 0.0;
    for &c in &counts {
        if c > 0 {
            let p = f64::from(c) / n;
            bits -= p * p.log2();
        }
    }
    bits
}

/// One `_ - /`-delimited segment of a token, judged by [`word_shaped`]'s
/// piece rule.
fn segment_is_word_shaped(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if let [only] = bytes {
        return only.is_ascii_alphanumeric();
    }
    let mut i = 0;
    let mut single_letters = 0u8;
    let mut transitions = 0u8;
    let mut previous_was_digit: Option<bool> = None;
    while let Some(&lead) = bytes.get(i) {
        let start = i;
        i += 1;
        // Letters and digits may meet at most twice in a segment (`full3d`,
        // `0x0011`, `m515seed`): a base32 or alphanumeric secret alternates
        // between the two all the way along, and that alternation is what
        // tells `JBSWY3DPEHPK3PXP` apart from `SHA256`.
        let is_digit = lead.is_ascii_digit();
        if previous_was_digit.is_some_and(|prev| prev != is_digit) {
            transitions += 1;
            if transitions > 2 {
                return false;
            }
        }
        previous_was_digit = Some(is_digit);
        let Some(same_class) = piece_class(lead, bytes.get(i).copied()) else {
            return false;
        };
        while bytes.get(i).is_some_and(|&b| same_class(b)) {
            i += 1;
        }
        if lead.is_ascii_digit() {
            continue;
        }
        let len = i - start;
        if len > WORD_PIECE_MAX_LEN {
            return false;
        }
        if len == 1 {
            let beside_digits = start
                .checked_sub(1)
                .and_then(|p| bytes.get(p))
                .is_some_and(u8::is_ascii_digit)
                || bytes.get(i).is_some_and(u8::is_ascii_digit);
            single_letters += 1;
            if !beside_digits || single_letters > 1 {
                return false;
            }
        }
    }
    true
}

/// The run the byte `lead` opens, read one byte ahead: a digit run, a
/// lowercase run, an uppercase run, or — an uppercase letter followed by a
/// lowercase one — a Capitalized word, which is one piece. `Word` is one
/// piece; `WORD` is one piece; `W` alone is a single letter and judged by the
/// caller. `None` for a byte outside the token alphabet, which is refused
/// rather than assumed.
fn piece_class(lead: u8, next: Option<u8>) -> Option<fn(u8) -> bool> {
    let class: fn(u8) -> bool = if lead.is_ascii_digit() {
        |b| b.is_ascii_digit()
    } else if lead.is_ascii_lowercase() {
        |b| b.is_ascii_lowercase()
    } else if lead.is_ascii_uppercase() {
        if next.is_some_and(|b| b.is_ascii_lowercase()) {
            |b| b.is_ascii_lowercase()
        } else {
            |b| b.is_ascii_uppercase()
        }
    } else {
        return None;
    };
    Some(class)
}
