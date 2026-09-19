//! Context and model observations: what a session's model context held at a
//! hook boundary, and which model a session was seen using. Observations,
//! never spend — the whole-turn token counts live with the cost relations.

use rift_core::Result;

use crate::{CanonicalStore, Value};

/// A prospective main-chain context snapshot. Unlike [`super::TokenUsageRow`], this
/// is one model context at one observable boundary, never whole-turn spend.
#[derive(Debug, Clone)]
pub struct MainContextObservationRow {
    /// Content-addressed logical observation id.
    pub observation_id: String,
    /// Canonical capture-session row id.
    pub session_id: String,
    /// Provider model identifier.
    pub model: String,
    /// Observation valid-time in epoch milliseconds.
    pub observed_at: i64,
    /// Uncached input tokens currently in the context.
    pub uncached_input: i64,
    /// Cached-read input tokens currently in the context.
    pub cached_input: i64,
    /// Cache-creation tokens currently in the context.
    pub cache_creation: i64,
    /// Output tokens currently in the context.
    pub output: i64,
    /// Counter provenance, for example `provider_reported`.
    pub counter_method: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a main-context observation (idempotent on its logical hook boundary).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_main_context_observation(
    store: &impl CanonicalStore,
    r: &MainContextObservationRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "main_context_observation",
            &[
                "observation_id",
                "session_id",
                "model",
                "observed_at",
                "uncached_input",
                "cached_input",
                "cache_creation",
                "output",
                "counter_method",
                "schema_version",
            ],
            &[
                Value::text(r.observation_id.clone()),
                Value::text(r.session_id.clone()),
                Value::text(r.model.clone()),
                Value::Int(r.observed_at),
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

/// One immutable observation that a capture session used or was configured for
/// a named model.
#[derive(Debug, Clone)]
pub struct ModelObservationRow {
    /// Content-addressed observation id.
    pub observation_id: String,
    /// Canonical capture-session row id.
    pub session_id: String,
    /// Model identifier.
    pub model: String,
    /// `runtime_hook` or `configured_client`.
    pub source: String,
    /// Observation valid-time in epoch milliseconds.
    pub observed_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a provenance-aware model observation.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_model_observation(
    store: &impl CanonicalStore,
    row: &ModelObservationRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "model_observation",
            &[
                "observation_id",
                "session_id",
                "model",
                "source",
                "observed_at",
                "schema_version",
            ],
            &[
                Value::text(row.observation_id.clone()),
                Value::text(row.session_id.clone()),
                Value::text(row.model.clone()),
                Value::text(row.source.clone()),
                Value::Int(row.observed_at),
                Value::Int(row.schema_version),
            ],
        )
        .await
}
