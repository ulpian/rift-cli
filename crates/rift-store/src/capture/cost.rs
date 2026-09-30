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
}
