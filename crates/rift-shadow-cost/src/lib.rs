//! Four-class token/cost model, versioned bitemporal price tables, and a token
//! estimator.
//!
//! The bill is determined by four token classes with radically different prices,
//! so Rift prices each separately (`Rift_Product_Spec` §13):
//!
//! ```text
//! call_cost = uncached_input · price_uncached_input
//!           + cached_input   · price_cached_input    (~10× cheaper)
//!           + cache_creation · price_cache_write     (one-time premium)
//!           + output         · price_output
//!           + tool_costs
//! ```
//!
//! Money is integer micro-USD (no floating-point money). Prices are quoted per
//! million tokens and versioned per `(provider, model, effective_from)` so a
//! price change is a new entry and "what did this cost on date X" is answerable
//! (bitemporal, `04-database-design` §4.3 rule 4). Token *estimation* is a fast
//! heuristic; a billing-grade tokenizer (`tiktoken-rs`) plugs in behind
//! [`estimate_tokens`] when exactness is needed.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(clippy::module_name_repetitions, clippy::doc_markdown)]

/// The four billable token classes for one model call (1:1 with a call, kept a
/// separate relation so the classes evolve independently — `04` §4.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
pub struct TokenUsage {
    /// Fresh input tokens billed at the full rate.
    pub uncached_input: u64,
    /// Input tokens served from the prompt cache (~10× cheaper).
    pub cached_input: u64,
    /// Tokens written into the cache (one-time write premium).
    pub cache_creation: u64,
    /// Output tokens (most expensive, least compressible).
    pub output: u64,
}

impl TokenUsage {
    /// Total tokens across all classes.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.uncached_input + self.cached_input + self.cache_creation + self.output
    }
}

/// Prices for the four classes, in micro-USD per **million** tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
pub struct ClassPrices {
    /// Price per 1M uncached input tokens (micro-USD).
    pub uncached_input_per_mtok: i64,
    /// Price per 1M cached input tokens (micro-USD).
    pub cached_input_per_mtok: i64,
    /// Price per 1M cache-creation tokens (micro-USD).
    pub cache_write_per_mtok: i64,
    /// Price per 1M output tokens (micro-USD).
    pub output_per_mtok: i64,
}

const MILLION: i128 = 1_000_000;

/// Compute a call's cost in micro-USD from its token usage, the active prices,
/// and any tool costs. Integer arithmetic throughout (no float money).
#[must_use]
pub fn call_cost_micros(usage: &TokenUsage, prices: &ClassPrices, tool_costs_micros: i64) -> i64 {
    let part = |tokens: u64, per_mtok: i64| -> i128 {
        (i128::from(tokens) * i128::from(per_mtok)) / MILLION
    };
    let total = part(usage.uncached_input, prices.uncached_input_per_mtok)
        + part(usage.cached_input, prices.cached_input_per_mtok)
        + part(usage.cache_creation, prices.cache_write_per_mtok)
        + part(usage.output, prices.output_per_mtok)
        + i128::from(tool_costs_micros);
    i64::try_from(total).unwrap_or(i64::MAX)
}

/// A versioned price-table entry, effective from a transaction time.
#[derive(Clone, Debug, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
pub struct PriceEntry {
    /// Provider, e.g. `anthropic`.
    pub provider: String,
    /// Model id, e.g. `claude-opus-4-8`.
    pub model: String,
    /// Epoch-ms from which this price is effective (valid-time).
    pub effective_from_ms: i64,
    /// The four-class prices.
    pub prices: ClassPrices,
}

/// A versioned, append-only set of price entries.
#[derive(Clone, Debug, Default, ::serde::Serialize, ::serde::Deserialize)]
pub struct PriceTable {
    entries: Vec<PriceEntry>,
}

impl PriceTable {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a price entry (entries are never mutated; a new price is a new row).
    pub fn insert(&mut self, entry: PriceEntry) {
        self.entries.push(entry);
    }

    /// The prices in force for `(provider, model)` as of `at_ms`: the entry with
    /// the greatest `effective_from_ms <= at_ms`. `None` if none applies yet.
    #[must_use]
    pub fn price_for(&self, provider: &str, model: &str, at_ms: i64) -> Option<&ClassPrices> {
        self.entries
            .iter()
            .filter(|e| e.provider == provider && e.model == model && e.effective_from_ms <= at_ms)
            .max_by_key(|e| e.effective_from_ms)
            .map(|e| &e.prices)
    }
}

/// Estimate the token count of a UTF-8 string. A fast heuristic
/// (`max(words, chars/4)`) — replace with `tiktoken-rs` for billing-grade
/// exactness. Deterministic and never panics.
#[must_use]
pub fn estimate_tokens(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    let words = text.split_whitespace().count() as u64;
    (chars / 4).max(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prices() -> ClassPrices {
        // illustrative: $3 / 1M uncached, $0.30 / 1M cached (10×), $3.75 write, $15 output
        ClassPrices {
            uncached_input_per_mtok: 3_000_000,
            cached_input_per_mtok: 300_000,
            cache_write_per_mtok: 3_750_000,
            output_per_mtok: 15_000_000,
        }
    }

    #[test]
    fn computes_four_class_cost() {
        let usage = TokenUsage {
            uncached_input: 1_000_000,
            cached_input: 1_000_000,
            cache_creation: 0,
            output: 1_000_000,
        };
        let c = call_cost_micros(&usage, &prices(), 0);
        // 3_000_000 + 300_000 + 15_000_000 = 18_300_000 micro-USD
        assert_eq!(c, 18_300_000);
    }

    #[test]
    fn cached_is_cheaper_than_uncached() {
        let uncached = TokenUsage {
            uncached_input: 1_000_000,
            ..TokenUsage::default()
        };
        let cached = TokenUsage {
            cached_input: 1_000_000,
            ..TokenUsage::default()
        };
        assert!(
            call_cost_micros(&cached, &prices(), 0) < call_cost_micros(&uncached, &prices(), 0)
        );
    }

    #[test]
    fn tool_costs_are_added() {
        let usage = TokenUsage::default();
        assert_eq!(call_cost_micros(&usage, &prices(), 4_242), 4_242);
    }

    #[test]
    fn price_table_picks_latest_effective_entry() {
        let mut t = PriceTable::new();
        t.insert(PriceEntry {
            provider: "anthropic".into(),
            model: "claude-opus-4-8".into(),
            effective_from_ms: 1_000,
            prices: ClassPrices {
                uncached_input_per_mtok: 100,
                cached_input_per_mtok: 10,
                cache_write_per_mtok: 120,
                output_per_mtok: 500,
            },
        });
        t.insert(PriceEntry {
            provider: "anthropic".into(),
            model: "claude-opus-4-8".into(),
            effective_from_ms: 5_000,
            prices: ClassPrices {
                uncached_input_per_mtok: 90,
                cached_input_per_mtok: 9,
                cache_write_per_mtok: 110,
                output_per_mtok: 450,
            },
        });
        // before any price → None
        assert!(t.price_for("anthropic", "claude-opus-4-8", 500).is_none());
        // between → first entry
        assert_eq!(
            t.price_for("anthropic", "claude-opus-4-8", 2_000)
                .unwrap()
                .uncached_input_per_mtok,
            100
        );
        // after the reprice → latest
        assert_eq!(
            t.price_for("anthropic", "claude-opus-4-8", 9_000)
                .unwrap()
                .uncached_input_per_mtok,
            90
        );
        // unknown model → None
        assert!(t.price_for("anthropic", "other", 9_000).is_none());
    }

    #[test]
    fn token_estimate_is_reasonable_and_total() {
        assert_eq!(estimate_tokens(""), 0);
        assert!(estimate_tokens("the quick brown fox") >= 4);
        let u = TokenUsage {
            uncached_input: 1,
            cached_input: 2,
            cache_creation: 3,
            output: 4,
        };
        assert_eq!(u.total(), 10);
    }

    proptest::proptest! {
        /// Cost is monotonic in output tokens (more output never costs less).
        #[test]
        fn cost_monotonic_in_output(out_a in 0u64..1_000_000, extra in 0u64..1_000_000) {
            let a = TokenUsage { output: out_a, ..TokenUsage::default() };
            let b = TokenUsage { output: out_a + extra, ..TokenUsage::default() };
            proptest::prop_assert!(call_cost_micros(&b, &prices(), 0) >= call_cost_micros(&a, &prices(), 0));
        }
    }
}
