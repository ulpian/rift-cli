//! Four-class token/cost model, versioned bitemporal price tables, and a token
//! estimator.
//!
//! The bill is determined by four token classes with radically different prices,
//! so Rift prices each separately:
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
//! (bitemporal, as the schema requires). Token *estimation* is a fast
//! heuristic ([`estimate_tokens`]); the counts this crate turns into money are
//! the ones a provider reported.
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

/// The four billable token classes for one model call (1:1 with a call, kept a
/// separate relation so the classes evolve independently).
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

/// How a call's token counts were derived (provenance for the `token_usage` row).
/// More authoritative tiers are preferred when available; the heuristic is the
/// always-available offline floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
pub enum CounterMethod {
    /// Counts the provider returned in the API response (most authoritative).
    ProviderReported,
    /// An exact BPE tokenizer (`rift-tokens` / `cl100k_base`).
    Tiktoken,
    /// The built-in deterministic heuristic estimator (the offline floor).
    Heuristic,
}

impl CounterMethod {
    /// The `counter_method` token persisted with a `token_usage` row.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderReported => "provider_reported",
            Self::Tiktoken => "tiktoken",
            Self::Heuristic => "estimator:heuristic",
        }
    }
}

/// Flush an ASCII alphanumeric run to a token estimate: ~4 chars/token (English
/// BPE rule of thumb), at least one token for a non-empty run.
fn alnum_run_tokens(run: u64) -> u64 {
    if run == 0 {
        0
    } else {
        run.div_ceil(4)
    }
}

/// Estimate the token count of a UTF-8 string with a deterministic, panic-free
/// heuristic.
///
/// It is sharper than a flat `chars/4`: ASCII alphanumeric runs cost ~4 chars per
/// token, ASCII punctuation/symbols ~1 token each, and non-ASCII scalars (CJK,
/// emoji) ~1 token each — closer to how real tokenizers split text. The estimate is
/// **monotonic** (appending text never lowers the count), so it never undercounts a
/// suffix.
#[must_use]
pub fn estimate_tokens(text: &str) -> u64 {
    let mut tokens: u64 = 0;
    let mut alnum_run: u64 = 0;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            alnum_run += 1;
            continue;
        }
        tokens += alnum_run_tokens(alnum_run);
        alnum_run = 0;
        if ch.is_whitespace() {
            // Whitespace is absorbed into adjacent tokens — contributes nothing.
        } else {
            // ASCII punctuation/symbols and non-ASCII scalars (CJK/emoji) ≈ 1 token.
            tokens += 1;
        }
    }
    tokens + alnum_run_tokens(alnum_run)
}

// ---------------------------------------------------------------------------
// The savings estimate: pricing tokens that were *avoided*.
//
// Spend prices a call we made; savings price reads we did NOT make. The
// arithmetic is the same integer micro-USD, so it lives here beside
// `call_cost_micros` rather than growing a second money authority elsewhere
// — money has exactly one authority. Everything below is pure: no store, no
// I/O, no clock.
// ---------------------------------------------------------------------------

/// Value `tokens` at `price_per_mtok` (micro-USD per **million** tokens),
/// returning micro-USD.
///
/// `price_per_mtok` is an [`Option`] because that is the honesty channel: a
/// model with no seeded price yields `None`, never `Some(0)` — a real saving
/// must never render as a fabricated zero. A negative price
/// is nonsense input and also yields `None` rather than negative money.
///
/// Arithmetic mirrors [`call_cost_micros`]: an `i128` intermediate, saturating
/// at [`i64::MAX`] so a pathological token count can never panic or wrap.
#[must_use]
pub fn avoided_tokens_micros(tokens: u64, price_per_mtok: Option<i64>) -> Option<i64> {
    let price = price_per_mtok?;
    if price < 0 {
        return None;
    }
    let product = i128::from(tokens).saturating_mul(i128::from(price)) / MILLION;
    Some(i64::try_from(product).unwrap_or(i64::MAX))
}

/// Split `total` across `weights` so the parts sum to **exactly** `total`.
///
/// Plain proportional rounding loses or invents tokens, which would let a
/// breakdown contradict the headline it is supposed to explain. Largest
/// remainder fixes that: floor every share, then hand the shortfall to the
/// largest fractional remainders, ties broken by position so the result is
/// deterministic (snapshots depend on it).
///
/// `None` when there is no basis to divide on (no weights, or every weight
/// zero) — the caller must treat that as "no observed mix" and fall back to a
/// single reference model, per the honesty ladder. That keeps the
/// post-condition unconditional: a `Some` always sums to `total`.
#[must_use]
pub fn partition_largest_remainder(total: u64, weights: &[u64]) -> Option<Vec<u64>> {
    let basis: u128 = weights.iter().copied().map(u128::from).sum();
    if basis == 0 {
        return None;
    }
    let total = u128::from(total);

    // Floor each share, remembering the fractional remainder it gave up.
    let mut parts: Vec<u64> = Vec::with_capacity(weights.len());
    let mut remainders: Vec<(u128, usize)> = Vec::with_capacity(weights.len());
    for (idx, &w) in weights.iter().enumerate() {
        let scaled = total.saturating_mul(u128::from(w));
        parts.push(u64::try_from(scaled / basis).unwrap_or(u64::MAX));
        remainders.push((scaled % basis, idx));
    }

    // Hand out the shortfall, largest remainder first (ties: lowest index).
    let allocated: u128 = parts.iter().copied().map(u128::from).sum();
    let shortfall = usize::try_from(total.saturating_sub(allocated)).unwrap_or(0);
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for &(_, idx) in remainders.iter().take(shortfall) {
        if let Some(part) = parts.get_mut(idx) {
            *part = part.saturating_add(1);
        }
    }
    Some(parts)
}

/// Group an integer with thousands separators (`1234567` → `1,234,567`).
fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let len = digits.chars().count();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Render micro-USD as a **tilde-marked estimate**: `~$1,234.56`.
///
/// The tilde is mandatory and never conditional — the figure is
/// an estimate over a chosen model/class basis the savings trail does not
/// record, and the marker is what keeps it from being quoted as a bill.
///
/// A positive amount below a cent renders `~<$0.01`: a real saving is never
/// shown as `$0.00`. An exact zero renders `~$0.00`, which is honest.
#[must_use]
pub fn fmt_usd_estimate(micros: i64) -> String {
    let negative = micros < 0;
    let magnitude = micros.unsigned_abs();
    // Round half-up to cents (1 USD = 1_000_000 micro-USD).
    let cents = (magnitude + 5_000) / 10_000;
    if cents == 0 {
        return if magnitude == 0 {
            "~$0.00".to_owned()
        } else {
            "~<$0.01".to_owned()
        };
    }
    let sign = if negative { "-" } else { "" };
    let dollars = group_thousands(cents / 100);
    let frac = cents % 100;
    format!("~{sign}${dollars}.{frac:02}")
}

/// Render micro-USD **exactly**, with no tilde: `$5.00`, `$1,234.56`.
///
/// For catalog facts. A seeded per-MTok rate is a recorded price — displaying
/// it is quoting the catalog, not estimating — so it must *not* carry the
/// estimate marker; only derived savings figures do
/// ([`fmt_usd_estimate`]). Rounds half-up to cents like its sibling, and has
/// no `<$0.01` floor: an exact value renders exactly.
#[must_use]
pub fn fmt_usd_exact(micros: i64) -> String {
    let negative = micros < 0;
    let magnitude = micros.unsigned_abs();
    let cents = (magnitude + 5_000) / 10_000;
    let sign = if negative { "-" } else { "" };
    let dollars = group_thousands(cents / 100);
    let frac = cents % 100;
    format!("{sign}${dollars}.{frac:02}")
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

    #[test]
    fn heuristic_is_sharper_than_flat_chars_over_four() {
        // Punctuation-dense text: a flat chars/4 (=2) under-counts the symbols a
        // real tokenizer splits; the heuristic charges 5 alnum (1 each) + 5 symbols
        // (1 each) = 10, much closer.
        let s = "a=b+c*d-e;";
        assert_eq!(estimate_tokens(s), 10);
        assert!(estimate_tokens(s) > (s.chars().count() as u64) / 4);
        // CJK: each scalar ≈ 1 token (flat chars/4 would under-count by 4×).
        assert_eq!(estimate_tokens("日本語"), 3);
        // A pure word costs ~ceil(len/4).
        assert_eq!(estimate_tokens("tokenization"), 3); // 12 chars → 3
    }

    #[test]
    fn every_counter_method_has_its_persisted_token() {
        assert_eq!(CounterMethod::Heuristic.as_str(), "estimator:heuristic");
        assert_eq!(
            CounterMethod::ProviderReported.as_str(),
            "provider_reported"
        );
        assert_eq!(CounterMethod::Tiktoken.as_str(), "tiktoken");
    }

    proptest::proptest! {
        /// Cost is monotonic in output tokens (more output never costs less).
        #[test]
        fn cost_monotonic_in_output(out_a in 0u64..1_000_000, extra in 0u64..1_000_000) {
            let a = TokenUsage { output: out_a, ..TokenUsage::default() };
            let b = TokenUsage { output: out_a + extra, ..TokenUsage::default() };
            proptest::prop_assert!(call_cost_micros(&b, &prices(), 0) >= call_cost_micros(&a, &prices(), 0));
        }

        /// The estimate is deterministic: same text → same count, every time.
        ///
        /// Determinism over a pure function is nearly free, so the newline the
        /// generator can now draw carries the claim that is not: a LINE BREAK
        /// IS A FREE SEPARATOR. It ends an alphanumeric run (so the two sides
        /// are counted as two runs, never merged into one cheaper one) and it
        /// is charged nothing itself. That is exactly
        /// `estimate(a) + estimate(b)`, asserted below on the same draws —
        /// which pins both halves of the whitespace rule at once, on the
        /// character the capture document is full of.
        #[test]
        fn token_estimate_is_deterministic(s in "(?s).{0,400}") {
            proptest::prop_assert_eq!(estimate_tokens(&s), estimate_tokens(&s));
            // Split at the first newline and put it back: the parts must cost
            // exactly the whole. (A no-op on single-line draws, which is why
            // `(?s)` is what makes this assertion do any work at all.)
            if let Some((head, tail)) = s.split_once('\n') {
                proptest::prop_assert_eq!(
                    estimate_tokens(&s),
                    estimate_tokens(head) + estimate_tokens(tail),
                    "a line break is neither charged nor free of consequence"
                );
            }
        }

        /// The estimate is monotonic: appending text never lowers the count, so a
        /// suffix is never undercounted. (Whitespace-only text legitimately costs
        /// 0, so the floor is "never decreases", not "always ≥ 1".)
        #[test]
        fn token_estimate_is_monotonic_under_append(a in "(?s).{0,200}", b in "(?s).{0,200}") {
            let whole = format!("{a}{b}");
            proptest::prop_assert!(estimate_tokens(&whole) >= estimate_tokens(&a));
            // The docstring's own words are "so a suffix is never
            // undercounted", and only the PREFIX half was asserted. Both are
            // needed and neither implies the other once the text can span
            // lines: an estimator that read only the first line satisfies the
            // prefix half and undercounts every line after it, and one that
            // read only the last line satisfies the suffix half and drops
            // everything before it. Multi-line text is the only input that can
            // tell those apart from a correct one.
            proptest::prop_assert!(estimate_tokens(&whole) >= estimate_tokens(&b));
        }
    }

    // --- the savings estimate ----------------------------------------------

    #[test]
    fn avoided_tokens_price_at_the_given_rate() {
        // 1M tokens at $3/MTok is $3.00; 500k at $10/MTok is $5.00.
        assert_eq!(
            avoided_tokens_micros(1_000_000, Some(3_000_000)),
            Some(3_000_000)
        );
        assert_eq!(
            avoided_tokens_micros(500_000, Some(10_000_000)),
            Some(5_000_000)
        );
    }

    /// The honesty channel: absence stays absent, and never becomes a zero.
    #[test]
    fn an_unpriced_model_yields_none_never_zero() {
        assert_eq!(avoided_tokens_micros(14_594_095, None), None);
        // A nonsense negative price is absence, not negative money.
        assert_eq!(avoided_tokens_micros(1_000, Some(-1)), None);
        // A real saving at a real price is positive, never zero.
        assert!(avoided_tokens_micros(1_000_000, Some(3_000_000)).unwrap() > 0);
    }

    #[test]
    fn pathological_token_counts_saturate_rather_than_wrap() {
        assert_eq!(
            avoided_tokens_micros(u64::MAX, Some(i64::MAX)),
            Some(i64::MAX)
        );
    }

    #[test]
    fn partition_sums_to_the_total_and_is_deterministic() {
        // The M515-03 observed mix shape: a dominant pair plus a sliver.
        let weights = [2_128_005_669_u64, 1_789_470_204, 24_719_472];
        let parts = partition_largest_remainder(14_594_095, &weights).unwrap();
        assert_eq!(parts.iter().sum::<u64>(), 14_594_095);
        assert_eq!(
            parts,
            partition_largest_remainder(14_594_095, &weights).unwrap()
        );
        assert!(parts[0] > parts[1] && parts[1] > parts[2]);
    }

    #[test]
    fn partition_without_a_basis_is_absent_not_zero() {
        assert_eq!(partition_largest_remainder(100, &[]), None);
        assert_eq!(partition_largest_remainder(100, &[0, 0]), None);
    }

    /// The composition the panel actually performs: the rows account for every
    /// avoided token, and a pricier model can out-cost a larger token slice —
    /// the differentiation that a token-only headline hides.
    #[test]
    fn breakdown_rows_account_for_every_token() {
        let avoided = 14_594_095_u64;
        let weights = [2_128_005_669_u64, 1_789_470_204, 24_719_472];
        let rates = [Some(5_000_000_i64), Some(10_000_000), Some(2_000_000)];
        let slices = partition_largest_remainder(avoided, &weights).unwrap();
        assert_eq!(slices.iter().sum::<u64>(), avoided);
        let subtotals: Vec<i64> = slices
            .iter()
            .zip(rates)
            .map(|(&t, r)| avoided_tokens_micros(t, r).unwrap())
            .collect();
        assert!(subtotals.iter().sum::<i64>() > 0);
        assert!(slices[0] > slices[1], "opus-shaped slice is the larger");
        assert!(
            subtotals[1] > subtotals[0],
            "yet the pricier model costs more"
        );
    }

    #[test]
    fn money_always_reads_as_an_estimate() {
        assert_eq!(fmt_usd_estimate(43_800_000), "~$43.80");
        assert_eq!(fmt_usd_estimate(105_900_000), "~$105.90");
        assert_eq!(fmt_usd_estimate(1_234_560_000), "~$1,234.56");
        assert_eq!(fmt_usd_estimate(0), "~$0.00");
        for m in [0, 1, 9_999, 5_000_000, 1_234_560_000, i64::MAX] {
            assert!(fmt_usd_estimate(m).starts_with('~'), "missing tilde: {m}");
        }
    }

    /// Catalog prices render exactly — no tilde (they are recorded facts, not
    /// estimates), no `<$0.01` floor.
    #[test]
    fn exact_rendering_quotes_the_catalog_without_the_tilde() {
        assert_eq!(fmt_usd_exact(5_000_000), "$5.00");
        assert_eq!(fmt_usd_exact(10_000_000), "$10.00");
        assert_eq!(fmt_usd_exact(2_000_000), "$2.00");
        assert_eq!(fmt_usd_exact(500_000), "$0.50");
        assert_eq!(fmt_usd_exact(1_234_560_000), "$1,234.56");
        assert_eq!(fmt_usd_exact(0), "$0.00");
        assert!(!fmt_usd_exact(5_000_000).contains('~'));
    }

    #[test]
    fn a_positive_saving_never_renders_as_zero() {
        assert_eq!(fmt_usd_estimate(1), "~<$0.01");
        assert_eq!(fmt_usd_estimate(4_999), "~<$0.01");
        assert_eq!(fmt_usd_estimate(5_000), "~$0.01");
        for m in 1..10_000 {
            assert_ne!(fmt_usd_estimate(m), "~$0.00", "positive shown as zero: {m}");
        }
    }

    proptest::proptest! {
        /// More avoided tokens never price lower, and money is never negative.
        #[test]
        fn avoided_value_is_monotonic_and_non_negative(
            tokens in 0u64..1_000_000_000_000,
            extra in 0u64..1_000_000_000_000,
            rate in 0i64..100_000_000,
        ) {
            let a = avoided_tokens_micros(tokens, Some(rate)).unwrap();
            let b = avoided_tokens_micros(tokens + extra, Some(rate)).unwrap();
            proptest::prop_assert!(b >= a);
            proptest::prop_assert!(a >= 0);
        }

        /// The breakdown always accounts for exactly the avoided total — a row
        /// set that sums to anything else would contradict its own headline.
        #[test]
        fn partition_always_sums_to_the_total(
            total in 0u64..10_000_000_000,
            weights in proptest::collection::vec(0u64..1_000_000_000, 1..8),
        ) {
            if let Some(parts) = partition_largest_remainder(total, &weights) {
                proptest::prop_assert_eq!(parts.iter().sum::<u64>(), total);
                proptest::prop_assert_eq!(parts.len(), weights.len());
            }
        }

        /// Every rendered amount is marked an estimate, and a real saving is
        /// never rendered as zero.
        #[test]
        fn rendering_is_marked_and_never_fakes_a_zero(micros in 0i64..i64::MAX) {
            let s = fmt_usd_estimate(micros);
            proptest::prop_assert!(s.starts_with('~'));
            if micros > 0 {
                proptest::prop_assert!(s != "~$0.00");
            }
        }
    }
}
