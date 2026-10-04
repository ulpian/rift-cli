//! The models the projection has observed against the price catalog: every
//! model a captured call named that the catalog cannot price now.
//!
//! A module of its own because it is the one leg that reads the catalog. A
//! model with no price produces usage rows and no cost fact, which is the
//! honest absence the catalog promises, but it is also silent: the dollar
//! figure simply reads low. This leg is where that silence gets a name.

use std::path::Path;

use rift_store::{effective_prices, CanonicalStore as _, SqliteStore};

use crate::{hooks, projection::runtime};

use super::capture::doctor_store;

/// One observed model the catalog cannot price.
struct UnpricedModel {
    /// The model as the captured call named it.
    model: String,
    /// How many captured calls named it.
    calls: i64,
}

/// What the catalog says about the observed models.
struct PriceCoverage {
    /// How many distinct models the captured calls named.
    observed: usize,
    /// The ones with no price in force now, in model order.
    unpriced: Vec<UnpricedModel>,
}

impl PriceCoverage {
    /// The leg's line and object. Every unpriced model gets its own line so a
    /// reader sees which figure is missing, not only that one is.
    fn report(&self) -> (String, serde_json::Value) {
        let mut human = match self.unpriced.len() {
            0 => format!(
                "\n  prices:         {} model(s) observed, every one priced",
                self.observed
            ),
            n => format!(
                "\n  prices:         {} model(s) observed, {n} without a price",
                self.observed
            ),
        };
        for it in &self.unpriced {
            human.push_str(&format!(
                "\n    {}: {} call(s) — their usage is recorded, and no cost is derived \
                 until the catalog prices this model",
                it.model, it.calls
            ));
        }
        let unpriced: Vec<_> = self
            .unpriced
            .iter()
            .map(|it| serde_json::json!({ "model": it.model, "calls": it.calls }))
            .collect();
        let json = serde_json::json!({
            "readable": true, "observed": self.observed, "unpriced": unpriced,
        });
        (human, json)
    }
}

/// Every model in `model_call` that `effective_prices` cannot price at now,
/// with its call count. Read-only: the viewer tier opens the projection
/// read-only and nothing is written either way.
pub(super) fn doctor_unpriced_models(repo: &Path) -> (String, serde_json::Value) {
    let (Ok(store), Ok(rt)) = (doctor_store(repo, true), runtime()) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let Ok(coverage) = rt.block_on(price_coverage(&store, hooks::now_ms())) else {
        return (
            "\n  prices:         unreadable".to_owned(),
            serde_json::json!({ "readable": false }),
        );
    };
    coverage.report()
}

/// The observed models priced at `now_ms`, both as the instant priced and as
/// the belief held. Models come back in byte order from SQL, so the lines
/// print in the same order on every run.
async fn price_coverage(store: &SqliteStore, now_ms: i64) -> rift_core::Result<PriceCoverage> {
    let rows = store
        .query(
            "SELECT model, COUNT(*) FROM model_call GROUP BY model ORDER BY model",
            &[],
        )
        .await?;
    let mut unpriced = Vec::new();
    for r in &rows {
        let model = r.get_text(0)?;
        if effective_prices(store, &model, now_ms, now_ms)
            .await?
            .is_some()
        {
            continue;
        }
        unpriced.push(UnpricedModel {
            model,
            calls: r.get_i64(1)?,
        });
    }
    Ok(PriceCoverage {
        observed: rows.len(),
        unpriced,
    })
}

#[cfg(test)]
mod tests {
    use rift_agent::TokenUsage as AdapterTokenUsage;
    use rift_core::{CanonicalEvent, EventKind, Sha256Id, Trust};
    use rift_shadow::project::{project_usage, PayloadFacts, ProjectionCtx};
    use rift_store::{record_model_call, ModelCallRow};

    use super::*;

    /// 2026-10-01T00:00:00Z, after every seeded price was recorded.
    const NOW: i64 = 1_790_812_800_000;

    /// A migrated projection holding `calls` captured calls per model.
    fn projection_with_calls(dir: &Path, calls: &[(&str, usize)]) -> SqliteStore {
        let store = rift_store::open_capture_projection(dir.join("p.db")).unwrap();
        let rt = runtime().unwrap();
        for (model, n) in calls {
            for idx in 0..*n {
                let row = ModelCallRow {
                    call_id: format!("{model}-{idx}"),
                    turn_id: "turn-1".to_owned(),
                    model: (*model).to_owned(),
                    started_at: NOW,
                    schema_version: 1,
                };
                rt.block_on(record_model_call(&store, &row)).unwrap();
            }
        }
        store
    }

    #[test]
    fn only_the_models_the_catalog_cannot_price_are_named_with_their_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let store = projection_with_calls(
            tmp.path(),
            &[
                ("claude-fable-5-1", 4),
                ("claude-opus-5-5", 8),
                ("Gemini 3.7 Flash (Low)", 12),
            ],
        );
        drop(store);
        let store = SqliteStore::open_read_only(tmp.path().join("p.db")).unwrap();
        let coverage = runtime()
            .unwrap()
            .block_on(price_coverage(&store, NOW))
            .unwrap();
        let (human, json) = coverage.report();
        println!("{human}");
        assert_eq!(
            human,
            "\n  prices:         3 model(s) observed, 2 without a price\
             \n    Gemini 3.7 Flash (Low): 12 call(s) — their usage is recorded, and no \
             cost is derived until the catalog prices this model\
             \n    claude-opus-5-5: 8 call(s) — their usage is recorded, and no cost is \
             derived until the catalog prices this model"
        );
        assert_eq!(json["unpriced"][0]["model"], "Gemini 3.7 Flash (Low)");
        assert_eq!(json["unpriced"][1]["calls"], 8);
    }

    #[test]
    fn no_model_is_named_when_every_observed_model_prices() {
        let tmp = tempfile::tempdir().unwrap();
        let store =
            projection_with_calls(tmp.path(), &[("claude-fable-5-1", 2), ("claude-opus-5", 1)]);
        let coverage = runtime()
            .unwrap()
            .block_on(price_coverage(&store, NOW))
            .unwrap();
        let (human, json) = coverage.report();
        assert_eq!(
            human,
            "\n  prices:         2 model(s) observed, every one priced"
        );
        assert_eq!(json["unpriced"], serde_json::json!([]));
    }

    /// The live projector resolves the seed and persists the hand-computed
    /// cost: 1,000 × $10 + 200,000 × $0.25 + 10,000 × $12.50 +
    /// 2,000 × $50, per million, is 285,000 micro-USD.
    #[test]
    fn a_fable_5_1_call_costs_what_the_published_rates_give() {
        let tmp = tempfile::tempdir().unwrap();
        let store = projection_with_calls(tmp.path(), &[]);
        let event = CanonicalEvent {
            schema_version: 1,
            session: "session-1".to_owned(),
            turn_ordinal: 1,
            event_ordinal: 1,
            kind: EventKind::PromptResponse,
            trust: Trust::TranscriptUntrusted,
            occurred_at_ms: NOW,
            payload_sha256: Sha256Id::from_bytes([7; 32]),
        };
        let facts = PayloadFacts {
            model: Some("claude-fable-5-1".to_owned()),
            ..PayloadFacts::default()
        };
        let context = ProjectionCtx {
            agent: "claude-code",
            repo_id: "repo-1",
        };
        let usage = AdapterTokenUsage {
            input: Some(1_000),
            cache_read: Some(200_000),
            cache_write: Some(10_000),
            output: Some(2_000),
        };
        let rt = runtime().unwrap();
        rt.block_on(project_usage(&store, &event, &facts, context, &usage, NOW))
            .unwrap();
        let session_row = rift_shadow::ids::session_row_id(context.agent, &event.session);
        let call_id = rift_shadow::ids::model_call_id(&session_row, 1, 1);
        assert_eq!(
            rt.block_on(rift_store::current_cost_micros(&store, &call_id))
                .unwrap(),
            Some(285_000)
        );
    }
}
