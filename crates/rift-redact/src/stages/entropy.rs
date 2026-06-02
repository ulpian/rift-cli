//! Shannon-entropy detection of high-randomness tokens (keys, hashes, blobs).

use crate::pipeline::{Finding, RedactStage, Span};

/// Flags whitespace/punctuation-delimited tokens that are both long enough and
/// random enough to look like a credential.
pub struct EntropyStage {
    /// Minimum token length (in characters) to consider.
    min_len: usize,
    /// Minimum Shannon entropy, in bits per character.
    min_bits_per_char: f64,
}

impl Default for EntropyStage {
    fn default() -> Self {
        Self {
            min_len: 20,
            min_bits_per_char: 4.0,
        }
    }
}

impl EntropyStage {
    #[must_use]
    pub fn new(min_len: usize, min_bits_per_char: f64) -> Self {
        Self {
            min_len,
            min_bits_per_char,
        }
    }
}

impl RedactStage for EntropyStage {
    fn name(&self) -> &'static str {
        "entropy"
    }

    fn scan(&self, text: &str) -> Vec<Finding> {
        let mut findings = Vec::new();
        for (start, token) in tokens(text) {
            let len = token.chars().count();
            if len >= self.min_len && shannon_bits_per_char(token) >= self.min_bits_per_char {
                findings.push(Finding {
                    stage: "entropy",
                    kind: "high_entropy".to_string(),
                    span: Span {
                        start,
                        end: start + token.len(),
                    },
                    confidence: 0.5,
                });
            }
        }
        findings
    }
}

/// Split `text` into tokens, yielding each token's byte offset and slice.
fn tokens(text: &str) -> Vec<(usize, &str)> {
    fn is_sep(c: char) -> bool {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\''
                    | '`'
                    | ','
                    | ';'
                    | ':'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '='
                    | '<'
                    | '>'
                    | '\\'
            )
    }

    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if is_sep(c) {
            if let Some(s) = start.take() {
                out.push((s, &text[s..i]));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push((s, &text[s..]));
    }
    out
}

/// Shannon entropy of a string in bits per character.
fn shannon_bits_per_char(s: &str) -> f64 {
    use std::collections::HashMap;
    let mut counts: HashMap<char, u32> = HashMap::new();
    let mut total = 0u32;
    for c in s.chars() {
        *counts.entry(c).or_insert(0) += 1;
        total += 1;
    }
    if total == 0 {
        return 0.0;
    }
    let total = f64::from(total);
    -counts
        .values()
        .map(|&n| {
            let p = f64::from(n) / total;
            p * p.log2()
        })
        .sum::<f64>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_a_random_blob() {
        let stage = EntropyStage::default();
        let secret = "Zx9Qv2Lp7Rt4Wn1Kc8Yb3Hd6Mf0Ja5";
        let findings = stage.scan(&format!("token={secret}"));
        assert!(findings.iter().any(|f| f.kind == "high_entropy"));
    }

    #[test]
    fn leaves_prose_alone() {
        let stage = EntropyStage::default();
        let findings = stage.scan("the quick brown fox jumps over the lazy dog");
        assert!(findings.is_empty());
    }
}
