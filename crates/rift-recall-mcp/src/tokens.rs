//! Honest token accounting (Stage 3 Phase 5 Step 3, W6).
//!
//! Every content-bearing response reports `estimated_tokens_returned` (the real
//! token count of the bytes actually inlined) and `estimated_tokens_avoided` (the
//! counterfactual a full re-read would have spent). `returned` is computed with
//! the bundled `cl100k_base` tokenizer so the figure is honest rather than a
//! `bytes/4` guess; if the tokenizer fails to initialize we fall back to a
//! conservative `chars/4 + 1` estimate (rounding up so a budget ceiling is never
//! breached by under-counting).

use std::sync::OnceLock;

use tiktoken_rs::CoreBPE;

/// The tokenizer whose counts back the `estimated_tokens_returned` figure (the
/// provenance tag carried alongside the count).
pub const TOKENIZER: &str = "cl100k_base";

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
}
