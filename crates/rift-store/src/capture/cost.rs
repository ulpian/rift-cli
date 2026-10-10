//! The cost relations: per-call token usage, the bitemporal price table and
//! the resolution of one model's four effective prices. Prices and usage are
//! separate relations so the two evolve independently.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_shadow_cost::{call_cost_micros, ClassPrices, TokenUsage};

use super::usage_windows::{window_binds, WindowScope, CALL_IN_WINDOW, USAGE_WINDOWS_CTE};
use crate::{CanonicalStore, Value};

/// A `token_usage` row: the four token classes, 1:1 with a `model_call` but a
/// separate relation, so the two evolve independently.
#[derive(Debug, Clone)]
pub struct TokenUsageRow {
    /// The model call these counts are for.
    pub call_id: String,
    /// Uncached input tokens.
    pub uncached_input: i64,
    /// Cached input tokens.
    pub cached_input: i64,
    /// Cache-creation tokens.
    pub cache_creation: i64,
    /// Output tokens.
    pub output: i64,
    /// How the counts were derived (`provider_reported` | `tiktoken` | `estimator:<model>`).
    pub counter_method: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record `token_usage` (idempotent on `call_id`).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_token_usage(store: &impl CanonicalStore, r: &TokenUsageRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "token_usage",
            &[
                "call_id",
                "uncached_input",
                "cached_input",
                "cache_creation",
                "output",
                "counter_method",
                "schema_version",
            ],
            &[
                Value::text(r.call_id.clone()),
                Value::Int(r.uncached_input),
                Value::Int(r.cached_input),
                Value::Int(r.cache_creation),
                Value::Int(r.output),
                Value::text(r.counter_method.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// One settled model call of a session with its provider-reported usage:
/// the row set the checkpoint shard's `usage.jsonl` freezes, one line each.
///
/// The call's own key travels rather than the coordinates it was minted
/// from: the event ordinal is hashed into `call_id` and stored nowhere else,
/// so a rebuild re-inserts under this key instead of re-deriving it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionUsageFact {
    /// The ordinal of the turn the call belongs to.
    pub turn_ordinal: i64,
    /// The `model_call` key.
    pub call_id: String,
    /// The model the provider reported for the call.
    pub model: String,
    /// When the call's anchoring event occurred (epoch ms).
    pub started_at: i64,
    /// Uncached input tokens.
    pub uncached_input: i64,
    /// Cached input tokens.
    pub cached_input: i64,
    /// Cache-creation tokens.
    pub cache_creation: i64,
    /// Output tokens.
    pub output: i64,
    /// How the counts were derived.
    pub counter_method: String,
}

/// A `price_table` row: a versioned (bitemporal) per-`model×class` price.
#[derive(Debug, Clone)]
pub struct PriceTableRow {
    /// Surrogate id (ULID).
    pub price_id: String,
    /// Model the price applies to.
    pub model: String,
    /// Token class (`uncached_input` | `cached_input` | `cache_creation` | `output`).
    pub token_class: String,
    /// Price in micro-units of currency per **million** tokens.
    pub unit_price_micros: i64,
    /// Currency code.
    pub currency: String,
    /// Valid-time start (provider effective date, epoch ms).
    pub valid_from: i64,
    /// Valid-time end (`None` = current).
    pub valid_to: Option<i64>,
    /// Transaction-time (epoch ms).
    pub recorded_at: i64,
    /// The price row this supersedes, if any.
    pub supersedes_id: Option<String>,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `price_table` row (idempotent on `price_id`; price changes supersede).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_price(store: &impl CanonicalStore, r: &PriceTableRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "price_table",
            &[
                "price_id",
                "model",
                "token_class",
                "unit_price_micros",
                "currency",
                "valid_from",
                "valid_to",
                "recorded_at",
                "supersedes_id",
                "schema_version",
            ],
            &[
                Value::text(r.price_id.clone()),
                Value::text(r.model.clone()),
                Value::text(r.token_class.clone()),
                Value::Int(r.unit_price_micros),
                Value::text(r.currency.clone()),
                Value::Int(r.valid_from),
                r.valid_to.map_or(Value::Null, Value::Int),
                Value::Int(r.recorded_at),
                r.supersedes_id.clone().map_or(Value::Null, Value::text),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// The four effective per-MTok prices for one model, resolved bitemporally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectivePrices {
    /// micro-currency per 1M uncached input tokens.
    pub uncached_input: i64,
    /// micro-currency per 1M cached input tokens.
    pub cached_input: i64,
    /// micro-currency per 1M cache-creation tokens.
    pub cache_creation: i64,
    /// micro-currency per 1M output tokens.
    pub output: i64,
}

/// Resolve one class's effective price: in force at `valid_at` per the belief
/// held at `as_of` (bitemporal): latest `valid_from ≤ valid_at`, then
/// latest `recorded_at ≤ as_of`, deterministic tie-break on `price_id`.
async fn effective_class_price(
    store: &impl CanonicalStore,
    model: &str,
    token_class: &str,
    valid_at: i64,
    as_of: i64,
) -> Result<Option<i64>> {
    store
        .query_opt_i64(
            "SELECT unit_price_micros FROM price_table \
             WHERE model = ?1 AND token_class = ?2 \
               AND valid_from <= ?3 AND (valid_to IS NULL OR valid_to > ?3) \
               AND recorded_at <= ?4 \
             ORDER BY valid_from DESC, recorded_at DESC, price_id DESC LIMIT 1",
            &[
                Value::text(model.to_owned()),
                Value::text(token_class.to_owned()),
                Value::Int(valid_at),
                Value::Int(as_of),
            ],
        )
        .await
}

/// Resolve all four classes for `model`, or `None` if any class has no price on
/// file — a partial catalog never produces a partially-priced cost fact.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn effective_prices(
    store: &impl CanonicalStore,
    model: &str,
    valid_at: i64,
    as_of: i64,
) -> Result<Option<EffectivePrices>> {
    let (Some(uncached_input), Some(cached_input), Some(cache_creation), Some(output)) = (
        effective_class_price(store, model, "uncached_input", valid_at, as_of).await?,
        effective_class_price(store, model, "cached_input", valid_at, as_of).await?,
        effective_class_price(store, model, "cache_creation", valid_at, as_of).await?,
        effective_class_price(store, model, "output", valid_at, as_of).await?,
    ) else {
        return Ok(None);
    };
    Ok(Some(EffectivePrices {
        uncached_input,
        cached_input,
        cache_creation,
        output,
    }))
}

/// Every settled model call of `session_row_id` (the projected session id)
/// with its usage, ordered by start time then call id — the condense-time
/// source of the shard's `usage.jsonl`. A call with no `token_usage` row
/// settled nothing and is left out.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn session_usage_facts(
    store: &impl CanonicalStore,
    session_row_id: &str,
) -> Result<Vec<SessionUsageFact>> {
    let rows = store
        .query(
            "SELECT t.ordinal, mc.call_id, mc.model, mc.started_at, tu.uncached_input, \
             tu.cached_input, tu.cache_creation, tu.output, tu.counter_method \
             FROM model_call mc \
             JOIN token_usage tu ON tu.call_id = mc.call_id \
             JOIN turn t ON t.turn_id = mc.turn_id \
             WHERE t.session_id = ?1 \
             ORDER BY mc.started_at ASC, mc.call_id ASC",
            &[Value::text(session_row_id.to_owned())],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(SessionUsageFact {
            turn_ordinal: r.get_i64(0)?,
            call_id: r.get_text(1)?,
            model: r.get_text(2)?,
            started_at: r.get_i64(3)?,
            uncached_input: r.get_i64(4)?,
            cached_input: r.get_i64(5)?,
            cache_creation: r.get_i64(6)?,
            output: r.get_i64(7)?,
            counter_method: r.get_text(8)?,
        });
    }
    Ok(out)
}

/// Which settled calls a current catalog quote includes, and what the quote
/// is keyed by.
#[derive(Debug, Clone, Copy)]
pub enum CostQuoteScope<'a> {
    /// Every settled call, keyed by session.
    All,
    /// The settled calls belonging to one projected session, keyed by it.
    Session(&'a str),
    /// Calls starting in the inclusive window, independent of session start,
    /// keyed by session.
    Window(i64, i64),
    /// The calls each checkpoint in scope froze — its usage window — keyed by
    /// checkpoint id. An undated checkpoint has no window and no key.
    CheckpointWindows(WindowScope<'a>),
}

/// The four token classes summed over the calls of one checkpoint's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowTokens {
    /// Uncached input tokens.
    pub uncached_input: i64,
    /// Cached input tokens.
    pub cached_input: i64,
    /// Cache-creation tokens.
    pub cache_creation: i64,
    /// Output tokens.
    pub output: i64,
}

/// The token sum of every checkpoint in `scope` over the calls its usage
/// window frames, keyed by checkpoint id, in one grouped query.
///
/// A checkpoint whose window holds no settled call is absent, as is an undated
/// one: absence is "no calls known", never a zero. The calls are the ones
/// [`quote_usage_costs`] prices under [`CostQuoteScope::CheckpointWindows`],
/// through the same join, so tokens and money never count different calls.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn checkpoint_window_tokens(
    store: &impl CanonicalStore,
    scope: WindowScope<'_>,
) -> Result<BTreeMap<String, WindowTokens>> {
    let rows = store
        .query(
            &format!(
                "{USAGE_WINDOWS_CTE}SELECT w.checkpoint_id, SUM(tu.uncached_input), \
                 SUM(tu.cached_input), SUM(tu.cache_creation), SUM(tu.output) \
                 FROM model_call mc JOIN token_usage tu ON tu.call_id = mc.call_id \
                 {CALL_IN_WINDOW} GROUP BY w.checkpoint_id"
            ),
            &window_binds(scope),
        )
        .await?;
    let mut out = BTreeMap::new();
    for r in rows {
        let tokens = WindowTokens {
            uncached_input: r.get_i64(1)?,
            cached_input: r.get_i64(2)?,
            cache_creation: r.get_i64(3)?,
            output: r.get_i64(4)?,
        };
        out.insert(r.get_text(0)?, tokens);
    }
    Ok(out)
}

/// Quote captured usage against the catalog belief supplied by the reader.
/// Each call uses its own start as valid time and contributes exactly once to
/// each key it falls under. Keys without any completely priced call are
/// absent; priced zero is present. Two batched queries read usage and prices
/// without creating durable quote state.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on query or malformed row failure.
pub async fn quote_usage_costs(
    store: &impl CanonicalStore,
    scope: CostQuoteScope<'_>,
    belief_at: i64,
) -> Result<BTreeMap<String, i64>> {
    let (sql, binds) = quoted_calls(scope);
    let calls = store.query(&sql, &binds).await?;
    let prices = quote_catalog(store, belief_at).await?;
    let mut resolved = BTreeMap::new();
    let mut totals = BTreeMap::<String, i64>::new();
    for call in calls {
        let key = (call.get_text(1)?, call.get_i64(2)?);
        let effective = *resolved
            .entry(key.clone())
            .or_insert_with(|| quote_prices(&prices, &key.0, key.1));
        let Some(effective) = effective else { continue };
        let count = |index| call.get_i64(index).map(|n| u64::try_from(n).unwrap_or(0));
        let usage = TokenUsage {
            uncached_input: count(3)?,
            cached_input: count(4)?,
            cache_creation: count(5)?,
            output: count(6)?,
        };
        let amount = call_cost_micros(
            &usage,
            &ClassPrices {
                uncached_input_per_mtok: effective.uncached_input,
                cached_input_per_mtok: effective.cached_input,
                cache_write_per_mtok: effective.cache_creation,
                output_per_mtok: effective.output,
            },
            0,
        );
        let total = totals.entry(call.get_text(0)?).or_default();
        *total = total.saturating_add(amount);
    }
    Ok(totals)
}

/// The calls a quote prices, keyed in column 0, with their binds: the scope's
/// predicate over the call's own turn, or a checkpoint's usage window.
fn quoted_calls(scope: CostQuoteScope<'_>) -> (String, Vec<Value>) {
    const COLUMNS: &str = "mc.model, mc.started_at, \
        tu.uncached_input, tu.cached_input, tu.cache_creation, tu.output \
        FROM model_call mc JOIN token_usage tu ON tu.call_id = mc.call_id";
    let (predicate, binds) = match scope {
        CostQuoteScope::All => ("1 = 1", Vec::new()),
        CostQuoteScope::Session(id) => ("t.session_id = ?1", vec![Value::text(id)]),
        CostQuoteScope::Window(start, end) => (
            "mc.started_at >= ?1 AND mc.started_at <= ?2",
            vec![Value::Int(start), Value::Int(end)],
        ),
        CostQuoteScope::CheckpointWindows(windows) => {
            let sql = format!(
                "{USAGE_WINDOWS_CTE}SELECT w.checkpoint_id, {COLUMNS} {CALL_IN_WINDOW} \
                 ORDER BY w.checkpoint_id, mc.call_id"
            );
            return (sql, window_binds(windows));
        }
    };
    let sql = format!(
        "SELECT t.session_id, {COLUMNS} JOIN turn t ON t.turn_id = mc.turn_id \
         WHERE {predicate} ORDER BY mc.call_id"
    );
    (sql, binds)
}

struct QuotePrice {
    model: String,
    class: String,
    from: i64,
    until: Option<i64>,
    rate: i64,
}

async fn quote_catalog(store: &impl CanonicalStore, belief_at: i64) -> Result<Vec<QuotePrice>> {
    let rows = store
        .query(
            "SELECT model, token_class, valid_from, valid_to, unit_price_micros \
        FROM price_table WHERE recorded_at <= ?1 \
        ORDER BY valid_from DESC, recorded_at DESC, price_id DESC",
            &[Value::Int(belief_at)],
        )
        .await?;
    rows.into_iter()
        .map(|r| {
            Ok(QuotePrice {
                model: r.get_text(0)?,
                class: r.get_text(1)?,
                from: r.get_i64(2)?,
                until: r.get_opt_i64(3)?,
                rate: r.get_i64(4)?,
            })
        })
        .collect()
}

fn quote_prices(prices: &[QuotePrice], model: &str, valid_at: i64) -> Option<EffectivePrices> {
    let class = |name: &str| {
        prices
            .iter()
            .find(|p| {
                p.model == model
                    && p.class == name
                    && p.from <= valid_at
                    && p.until.is_none_or(|end| end > valid_at)
            })
            .map(|p| p.rate)
    };
    Some(EffectivePrices {
        uncached_input: class("uncached_input")?,
        cached_input: class("cached_input")?,
        cache_creation: class("cache_creation")?,
        output: class("output")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::fixtures::store;

    async fn quote_call(
        s: &crate::SqliteStore,
        session: &str,
        call: &str,
        model: &str,
        at: i64,
        input: i64,
    ) {
        crate::record_turn(
            s,
            &crate::TurnRow {
                turn_id: call.into(),
                session_id: session.into(),
                ordinal: 1,
                started_at: 999_999,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        crate::record_model_call(
            s,
            &crate::ModelCallRow {
                call_id: call.into(),
                turn_id: call.into(),
                model: model.into(),
                started_at: at,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        record_token_usage(
            s,
            &TokenUsageRow {
                call_id: call.into(),
                uncached_input: input,
                cached_input: 0,
                cache_creation: 0,
                output: 0,
                counter_method: "provider_reported".into(),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
    }

    async fn quote_rates(
        s: &crate::SqliteStore,
        id: &str,
        rate: i64,
        from: i64,
        until: Option<i64>,
        belief: i64,
    ) -> bool {
        let mut inserted = true;
        for class in ["uncached_input", "cached_input", "cache_creation", "output"] {
            inserted &= record_price(
                s,
                &PriceTableRow {
                    price_id: format!("{id}-{class}"),
                    model: "quoted".into(),
                    token_class: class.into(),
                    unit_price_micros: rate,
                    currency: "USD".into(),
                    valid_from: from,
                    valid_to: until,
                    recorded_at: belief,
                    supersedes_id: None,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
        }
        inserted
    }

    #[tokio::test]
    async fn batched_quotes_match_effective_prices_at_every_belief_and_valid_boundary() {
        let s = store();
        quote_rates(&s, "base", 1_000_000, 0, None, 1).await;
        quote_rates(&s, "epoch", 2_000_000, 10, Some(20), 1).await;
        quote_rates(&s, "a", 3_000_000, 10, Some(20), 2).await;
        assert!(
            !quote_rates(&s, "z", 4_000_000, 10, Some(20), 2).await,
            "canonical same-coordinate duplicates are ignored by UNIQUE"
        );
        quote_rates(&s, "late-low-epoch", 9_000_000, 0, None, 9).await;
        for at in [0, 9, 10, 19, 20] {
            quote_call(
                &s,
                &format!("s-{at}"),
                &format!("c-{at}"),
                "quoted",
                at,
                1000,
            )
            .await;
        }
        for belief in [0, 1, 2, 9] {
            let quotes = quote_usage_costs(&s, CostQuoteScope::All, belief)
                .await
                .unwrap();
            for at in [0, 9, 10, 19, 20] {
                let expected = effective_prices(&s, "quoted", at, belief)
                    .await
                    .unwrap()
                    .map(|p| p.uncached_input / 1000);
                assert_eq!(quotes.get(&format!("s-{at}")).copied(), expected);
            }
        }
        let bound = quote_usage_costs(&s, CostQuoteScope::Window(10, 19), 9)
            .await
            .unwrap();
        assert_eq!(bound.keys().cloned().collect::<Vec<_>>(), ["s-10", "s-19"]);
        assert_eq!(bound.values().copied().collect::<Vec<_>>(), [3000, 3000]);
    }

    #[tokio::test]
    async fn quotes_distinguish_unpriced_absence_from_priced_zero_and_ignore_orphan_bookings() {
        let s = store();
        quote_rates(&s, "base", 1_000_000, 0, None, 1).await;
        quote_call(&s, "zero", "zero-call", "quoted", 10, 0).await;
        quote_call(&s, "unknown", "unknown-call", "unpriced", 10, 1000).await;
        record_price(
            &s,
            &PriceTableRow {
                price_id: "partial-input".into(),
                model: "unpriced".into(),
                token_class: "uncached_input".into(),
                unit_price_micros: 1_000_000,
                currency: "USD".into(),
                valid_from: 0,
                valid_to: None,
                recorded_at: 1,
                supersedes_id: None,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        quote_call(&s, "mixed", "priced-call", "quoted", 10, 1000).await;
        quote_call(&s, "mixed", "other-call", "unpriced", 10, 1000).await;
        for id in ["orphan-a", "orphan-z"] {
            crate::record_cost_fact(
                &s,
                &crate::CostFact {
                    cost_fact_id: id.into(),
                    call_id: "priced-call".into(),
                    amount_micros: 999_999,
                    valid_from: 0,
                    valid_to: None,
                    recorded_at: 1,
                    supersedes_id: None,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
        }
        let quotes = quote_usage_costs(&s, CostQuoteScope::All, 1).await.unwrap();
        assert_eq!(quotes.get("zero"), Some(&0));
        assert_eq!(quotes.get("unknown"), None);
        assert_eq!(quotes.get("mixed"), Some(&1000));
        assert_eq!(
            quote_usage_costs(&s, CostQuoteScope::Session("mixed"), 1)
                .await
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            [("mixed".to_owned(), 1000)]
        );
        assert_eq!(s.count("cost_fact").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn current_quotes_preserve_integer_rounding_count_clamping_and_sum_saturation() {
        let s = store();
        quote_rates(&s, "small", 1, 0, None, 1).await;
        quote_call(&s, "round-down", "below", "quoted", 0, 999_999).await;
        quote_call(&s, "round-one", "above", "quoted", 0, 1_000_001).await;
        quote_call(&s, "negative", "negative", "quoted", 0, -1).await;
        let quotes = quote_usage_costs(&s, CostQuoteScope::All, 1).await.unwrap();
        assert_eq!(quotes.get("round-down"), Some(&0));
        assert_eq!(quotes.get("round-one"), Some(&1));
        assert_eq!(quotes.get("negative"), Some(&0));
        quote_rates(&s, "large", i64::MAX, 0, None, 2).await;
        quote_call(&s, "saturated", "large-a", "quoted", 0, i64::MAX).await;
        quote_call(&s, "saturated", "large-b", "quoted", 0, i64::MAX).await;
        let amount = quote_usage_costs(&s, CostQuoteScope::Session("saturated"), 2)
            .await
            .unwrap();
        assert_eq!(amount.get("saturated"), Some(&i64::MAX));
        let per_call = quote_usage_costs(&s, CostQuoteScope::Window(0, 0), 2)
            .await
            .unwrap();
        assert_eq!(per_call.get("saturated"), Some(&i64::MAX));
    }

    #[tokio::test]
    async fn alternate_relations_with_duplicate_price_coordinates_obey_the_full_sql_order() {
        let s = crate::SqliteStore::open_in_memory().unwrap();
        // Isolated alternate relations, not a modified or migrated canonical schema.
        s.with_conn(|conn| conn.execute_batch("CREATE TABLE turn (turn_id TEXT PRIMARY KEY, session_id TEXT, ordinal INTEGER, started_at INTEGER, schema_version INTEGER);
            CREATE TABLE model_call (call_id TEXT PRIMARY KEY, turn_id TEXT, model TEXT, started_at INTEGER, schema_version INTEGER);
            CREATE TABLE token_usage (call_id TEXT PRIMARY KEY, uncached_input INTEGER, cached_input INTEGER, cache_creation INTEGER, output INTEGER, counter_method TEXT, schema_version INTEGER);
            CREATE TABLE price_table (price_id TEXT PRIMARY KEY, model TEXT, token_class TEXT, unit_price_micros INTEGER, currency TEXT, valid_from INTEGER, valid_to INTEGER, recorded_at INTEGER, supersedes_id TEXT, schema_version INTEGER);"))
            .unwrap();
        assert!(quote_rates(&s, "base", 1_000_000, 0, None, 1).await);
        assert!(quote_rates(&s, "epoch", 2_000_000, 10, Some(20), 1).await);
        assert!(quote_rates(&s, "a", 3_000_000, 10, Some(20), 2).await);
        assert!(quote_rates(&s, "z", 4_000_000, 10, Some(20), 2).await);
        assert!(quote_rates(&s, "late-low-epoch", 9_000_000, 0, None, 9).await);
        quote_call(&s, "session", "call", "quoted", 10, 1000).await;
        let original = effective_prices(&s, "quoted", 10, 9)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(original.uncached_input, 4_000_000);
        let quoted = quote_usage_costs(&s, CostQuoteScope::Session("session"), 9)
            .await
            .unwrap();
        assert_eq!(quoted.get("session"), Some(&4000));
        assert_eq!(s.count("price_table").await.unwrap(), 20);
    }

    #[tokio::test]
    async fn token_usage_is_idempotent_per_call() {
        let s = store();
        let row = TokenUsageRow {
            call_id: "call1".into(),
            uncached_input: 100,
            cached_input: 20,
            cache_creation: 5,
            output: 50,
            counter_method: "estimator:heuristic".into(),
            schema_version: 1,
        };
        assert!(record_token_usage(&s, &row).await.unwrap());
        assert!(
            !record_token_usage(&s, &row).await.unwrap(),
            "1:1 per call_id"
        );
        assert_eq!(s.count("token_usage").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_price_row_round_trips() {
        let s = store();
        record_price(
            &s,
            &PriceTableRow {
                price_id: "p1".into(),
                model: "claude-opus".into(),
                token_class: "output".into(),
                unit_price_micros: 15_000_000,
                currency: "USD".into(),
                valid_from: 0,
                valid_to: None,
                recorded_at: 1,
                supersedes_id: None,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        assert_eq!(s.count("price_table").await.unwrap(), 1);
    }

    /// One settled call: its turn in `session`, the call row and its usage.
    async fn settle(s: &crate::SqliteStore, session: &str, turn: i64, call: (&str, &str, i64)) {
        use crate::capture::{record_model_call, record_turn, ModelCallRow, TurnRow};
        let (call_id, model, started_at) = call;
        let turn_id = format!("{session}-turn-{turn}");
        record_turn(
            s,
            &TurnRow {
                turn_id: turn_id.clone(),
                session_id: session.to_owned(),
                ordinal: turn,
                started_at: turn,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        record_model_call(
            s,
            &ModelCallRow {
                call_id: call_id.to_owned(),
                turn_id,
                model: model.to_owned(),
                started_at,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        let row = TokenUsageRow {
            call_id: call_id.to_owned(),
            uncached_input: started_at,
            cached_input: 2,
            cache_creation: 3,
            output: 4,
            counter_method: "provider_reported".into(),
            schema_version: 1,
        };
        record_token_usage(s, &row).await.unwrap();
    }

    #[tokio::test]
    async fn a_sessions_usage_facts_are_exactly_its_settled_calls_in_start_order() {
        use crate::capture::{record_model_call, ModelCallRow};
        let s = store();
        settle(&s, "sess-a", 2, ("call-late", "claude-fable-5", 300)).await;
        settle(&s, "sess-a", 1, ("call-b", "claude-opus-5", 100)).await;
        settle(&s, "sess-a", 1, ("call-a", "claude-fable-5", 100)).await;
        settle(&s, "sess-b", 1, ("call-other", "claude-opus-5", 50)).await;
        // A call whose usage never settled carries nothing to freeze.
        record_model_call(
            &s,
            &ModelCallRow {
                call_id: "call-unsettled".into(),
                turn_id: "sess-a-turn-2".into(),
                model: "claude-opus-5".into(),
                started_at: 200,
                schema_version: 1,
            },
        )
        .await
        .unwrap();

        let facts = session_usage_facts(&s, "sess-a").await.unwrap();
        let seen: Vec<(i64, &str, &str, i64)> = facts
            .iter()
            .map(|f| {
                (
                    f.turn_ordinal,
                    f.call_id.as_str(),
                    f.model.as_str(),
                    f.started_at,
                )
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                (1, "call-a", "claude-fable-5", 100),
                (1, "call-b", "claude-opus-5", 100),
                (2, "call-late", "claude-fable-5", 300),
            ],
            "the session's settled calls only, by start time then call id"
        );
        let first = &facts[0];
        assert_eq!(
            (
                first.uncached_input,
                first.cached_input,
                first.cache_creation,
                first.output
            ),
            (100, 2, 3, 4)
        );
        assert_eq!(first.counter_method, "provider_reported");
        assert!(session_usage_facts(&s, "sess-none")
            .await
            .unwrap()
            .is_empty());
    }

    /// A dated checkpoint archiving `session` first.
    async fn window_checkpoint(s: &crate::SqliteStore, id: &str, at: i64, session: &str) {
        s.insert(
            "checkpoint",
            &[
                "checkpoint_id",
                "strategy",
                "created_at",
                "merkle_root",
                "schema_version",
            ],
            &[
                Value::text(id.to_owned()),
                Value::text("manual-commit".to_owned()),
                Value::Int(at),
                Value::text("00".to_owned()),
                Value::Int(1),
            ],
        )
        .await
        .unwrap();
        let row = crate::capture::SessionInCheckpointRow {
            checkpoint_id: id.into(),
            session_id: session.into(),
            ordinal: 0,
            schema_version: 1,
        };
        crate::capture::record_session_in_checkpoint(s, &row)
            .await
            .unwrap();
    }

    /// Calls on both bounds of `ck-2`'s window `(100, 200]`, one past it and
    /// one of another session inside it; each call's input count is its start.
    async fn window_fixture() -> crate::SqliteStore {
        let s = store();
        quote_rates(&s, "base", 1_000_000, 0, None, 1).await;
        for (session, turn, call, at) in [
            ("s", 1, "at-first", 100),
            ("s", 2, "inside", 150),
            ("s", 3, "at-second", 200),
            ("s", 4, "after", 201),
            ("other", 1, "other-inside", 150),
        ] {
            settle(&s, session, turn, (call, "quoted", at)).await;
        }
        window_checkpoint(&s, "ck-1", 100, "s").await;
        window_checkpoint(&s, "ck-2", 200, "s").await;
        s
    }

    #[tokio::test]
    async fn a_window_excludes_its_lower_bound_and_includes_its_upper_bound() {
        let s = window_fixture().await;
        let scope = WindowScope::Checkpoint("ck-2");
        let tokens = checkpoint_window_tokens(&s, scope).await.unwrap();
        let second = tokens.get("ck-2").map(|t| t.uncached_input);
        assert_eq!(second, Some(150 + 200), "inside and at-second only");
        assert_eq!(tokens.len(), 1, "the scope names one checkpoint");
    }

    #[tokio::test]
    async fn a_sessions_first_window_takes_every_call_up_to_its_checkpoint() {
        let s = window_fixture().await;
        let tokens = checkpoint_window_tokens(&s, WindowScope::Every)
            .await
            .unwrap();
        assert_eq!(tokens.get("ck-1").map(|t| t.uncached_input), Some(100));
    }

    #[tokio::test]
    async fn the_window_token_sum_and_the_window_quote_count_the_same_calls() {
        let s = window_fixture().await;
        let tokens = checkpoint_window_tokens(&s, WindowScope::Every)
            .await
            .unwrap();
        let scope = CostQuoteScope::CheckpointWindows(WindowScope::Every);
        let quotes = quote_usage_costs(&s, scope, 1).await.unwrap();
        assert_eq!(quotes.keys().collect::<Vec<_>>(), ["ck-1", "ck-2"]);
        for (ck, amount) in &quotes {
            // One micro-USD per token of every class at the fixture's one rate.
            let summed = tokens
                .get(ck)
                .map(|t| t.uncached_input + t.cached_input + t.cache_creation + t.output);
            assert_eq!(Some(*amount), summed, "{ck}");
        }
        let one = quote_usage_costs(
            &s,
            CostQuoteScope::CheckpointWindows(WindowScope::Checkpoint("ck-2")),
            1,
        )
        .await
        .unwrap();
        assert_eq!(
            one.into_iter().collect::<Vec<_>>(),
            [("ck-2".to_owned(), 350 + 2 * 9)]
        );
    }
}
