//! The one token-counting authority.
//!
//! Extracted verbatim from `rift-recall-serve`'s serve-time accounting so that
//! metrics, budgets, pack compilation, and cost estimation all count with the
//! same tokenizer and carry the same provenance. Every figure this crate
//! produces is honest about its basis: [`TOKENIZER`] when the bundled
//! `cl100k_base` BPE is available, [`FALLBACK`] (a conservative `chars/4 + 1`,
//! rounding up so a budget ceiling is never breached by under-counting) when it
//! is not.
//!
//! Per-model tokenization (provider-specific encodings) is a `rift-shadow-cost`
//! concern layered on top later; it does not live here.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::sync::OnceLock;

use tiktoken_rs::CoreBPE;

/// The tokenizer whose counts back every figure (the provenance tag carried
/// alongside the count).
pub const TOKENIZER: &str = "cl100k_base";

/// The provenance tag when the BPE could not initialize and the conservative
/// estimate was used instead.
pub const FALLBACK: &str = "chars/4+1";

fn bpe() -> Option<&'static CoreBPE> {
    static BPE: OnceLock<Option<CoreBPE>> = OnceLock::new();
    BPE.get_or_init(|| tiktoken_rs::cl100k_base().ok()).as_ref()
}

fn fallback(text: &str) -> u32 {
    u32::try_from(text.chars().count() / 4 + 1).unwrap_or(u32::MAX)
}

/// The token count of `text` (honest; never zero for non-empty input).
#[must_use]
pub fn count(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    match bpe() {
        Some(b) => u32::try_from(b.encode_with_special_tokens(text).len()).unwrap_or(u32::MAX),
        None => fallback(text),
    }
}

/// The provenance of counts produced right now: [`TOKENIZER`] when the BPE is
/// live, [`FALLBACK`] otherwise. Reports carry this tag so a figure is never
/// presented as more exact than its basis.
#[must_use]
pub fn provenance() -> &'static str {
    if bpe().is_some() {
        TOKENIZER
    } else {
        FALLBACK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero_nonempty_is_positive() {
        assert_eq!(count(""), 0);
        assert!(count("hello world, this is a sentence") > 0);
    }

    #[test]
    fn longer_text_costs_more() {
        let short = count("fn main() {}");
        let long = count(&"fn main() {}\n".repeat(50));
        assert!(long > short);
    }

    #[test]
    fn provenance_names_a_known_basis() {
        assert!(matches!(provenance(), TOKENIZER | FALLBACK));
    }
}
