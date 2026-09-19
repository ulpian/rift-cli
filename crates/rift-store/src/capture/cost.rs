//! The cost relations: per-call token usage, the bitemporal price table and
//! the resolution of one model's four effective prices. Prices and usage are
//! separate relations so the two evolve independently.

use rift_core::Result;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::fixtures::store;

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
}
