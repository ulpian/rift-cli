//! The column-3 feeds (ADR-0009's Tokens/Nic/Recall trio): scoped, read-only
//! computations from a project's own projections into the six section
//! view-models.
//!
//! Every feed emits **full section state** (never a delta): a dropped or
//! duplicated delivery therefore converges on the fresh-read truth by
//! construction — the delivery-robustness property is a property of the
//! payload shape, not of channel heroics. Feeds follow the **active**
//! project (the reducer's `SetActive` is the subscribe/unsubscribe request:
//! selecting a project subscribes it to all three kinds; switching away
//! unsubscribes — recorded as the M4-11 reading of ADR-0009's
//! subscribe/unsubscribe language), and the worker recomputes them on every
//! observed change of that project — a `Rebuilt` epoch bump re-bootstraps by
//! recomputation (user story 12).
//!
//! Scope: every query runs against the project's **own per-repo** databases
//! (resolved through `repo_id_for`), so cross-project spend cannot leak into
//! a panel (user story 17) — the scope property is tested over multi-repo
//! fixtures.

use std::path::Path;

use rift_canopy::col3::{BreakdownData, BreakdownView, TokensData, TokensView};
use rift_canopy::{FeedPayload, FeedUpdate};
use rift_git::repo_id_for;
#[cfg(test)]
use rift_store::Value;
use rift_store::{projection_db_under, CanonicalStore, SqliteStore};

use crate::store::runtime;

/// The bucket-size ladder (ms) for the time-bucketed rate series, coarsest
/// last. Adaptive policy (M4-11 decision, deterministic in the data): the
/// smallest step where the captured span fits in [`TARGET_BUCKETS`] buckets —
/// so a short burst gets second-level detail and a month of history still
/// fits one plot.
const BUCKET_LADDER_MS: [i64; 9] = [
    1_000,      // 1s
    5_000,      // 5s
    15_000,     // 15s
    60_000,     // 1m
    300_000,    // 5m
    900_000,    // 15m
    3_600_000,  // 1h
    21_600_000, // 6h
    86_400_000, // 1d
];

/// How many rate buckets the sections display (≈ one per plot column).
const TARGET_BUCKETS: i64 = 48;

/// One token-usage fact: the model call's start time + the four classes.
struct TokenRow {
    t: i64,
    uncached_input: i64,
    cached_input: i64,
    cache_creation: i64,
    output: i64,
}

/// Compute the Tokens feed for the project at `path`: the token-usage section
/// (bucketed rate + cumulative total) and the input | read | output breakdown
/// ("read" = cache reads; cache writes are the secondary band). Total — any
/// unreadable projection yields the honest placeholders, never an error or a
/// fake zero.
#[must_use]
pub fn compute_tokens(root: &Path, path: &str) -> FeedUpdate {
    let payload = match read_token_rows(root, path) {
        Some(rows) if !rows.is_empty() => FeedPayload::Tokens {
            tokens: TokensView::Data(tokens_data(&rows)),
            breakdown: BreakdownView::Data(breakdown_data(&rows)),
        },
        // No facts in scope (or no readable projection): the placeholder.
        _ => FeedPayload::Tokens {
            tokens: TokensView::NoActivity,
            breakdown: BreakdownView::NoActivity,
        },
    };
    FeedUpdate {
        project: path.to_string(),
        payload,
    }
}

/// Read the project's token facts, scoped to its own projection. `None` when
/// the projection is absent/unreadable.
fn read_token_rows(root: &Path, path: &str) -> Option<Vec<TokenRow>> {
    let repo = repo_id_for(Path::new(path)).ok()?;
    let db = projection_db_under(root, &repo);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open(&db).ok()?;
    let rt = runtime().ok()?;
    let rows = rt
        .block_on(store.query(
            "SELECT mc.started_at, tu.uncached_input, tu.cached_input, \
                    tu.cache_creation, tu.output \
             FROM token_usage tu JOIN model_call mc ON mc.call_id = tu.call_id \
             ORDER BY mc.started_at, tu.call_id",
            &[],
        ))
        .ok()?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(TokenRow {
            t: r.get_i64(0).ok()?,
            uncached_input: r.get_i64(1).ok()?,
            cached_input: r.get_i64(2).ok()?,
            cache_creation: r.get_i64(3).ok()?,
            output: r.get_i64(4).ok()?,
        });
    }
    Some(out)
}

/// A row's total spend across the four classes (saturating; negatives from a
/// corrupt projection clamp to zero — validation, not trust).
fn row_total(r: &TokenRow) -> f64 {
    let clamp = |v: i64| -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let f = v.max(0) as f64;
        f
    };
    clamp(r.uncached_input) + clamp(r.cached_input) + clamp(r.cache_creation) + clamp(r.output)
}

/// The adaptive bucket size for a span: the smallest ladder step where the
/// span fits [`TARGET_BUCKETS`] buckets (the coarsest step otherwise).
fn bucket_ms(span_ms: i64) -> i64 {
    for step in BUCKET_LADDER_MS {
        if span_ms <= step.saturating_mul(TARGET_BUCKETS) {
            return step;
        }
    }
    *BUCKET_LADDER_MS.last().unwrap_or(&86_400_000)
}

/// Bucket rows into the trailing rate series: per-bucket total-token sums over
/// the last [`TARGET_BUCKETS`] buckets ending at the newest fact, zero-filled
/// where a bucket saw nothing (an honest quiet bucket inside an active span —
/// distinct from the placeholder, which means no facts at all). Deterministic
/// in the rows; replaying the same rows yields the same series (bucketing
/// idempotence).
fn rate_series(rows: &[TokenRow]) -> Vec<f64> {
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
        return Vec::new();
    };
    let step = bucket_ms(last.t.saturating_sub(first.t));
    let end_bucket = last.t.div_euclid(step);
    let start_bucket = end_bucket - (TARGET_BUCKETS - 1);
    let mut series = vec![0.0f64; usize::try_from(TARGET_BUCKETS).unwrap_or(48)];
    for row in rows {
        let b = row.t.div_euclid(step);
        if b < start_bucket {
            continue; // older than the visible window
        }
        let idx = usize::try_from(b - start_bucket).unwrap_or(0);
        if let Some(slot) = series.get_mut(idx) {
            *slot += row_total(row);
        }
    }
    // Trim the leading zero buckets before the first activity so a young
    // projection is not mostly empty margin.
    let first_active = series.iter().position(|v| *v > 0.0).unwrap_or(0);
    series.split_off(first_active)
}

/// The token-usage section's numbers.
fn tokens_data(rows: &[TokenRow]) -> TokensData {
    TokensData {
        rate: rate_series(rows),
        total: crate::series::saturating_sum(rows.iter().map(row_total)),
    }
}

/// The breakdown section's numbers: input = uncached input, read = cached
/// reads, output = output, cache writes as the secondary band; the read
/// fraction is read / (input + read), `0/0 ≡ 0` (user story 3).
fn breakdown_data(rows: &[TokenRow]) -> BreakdownData {
    let sum = |f: fn(&TokenRow) -> i64| -> f64 {
        crate::series::saturating_sum(rows.iter().map(|r| {
            #[allow(clippy::cast_precision_loss)]
            let v = f(r).max(0) as f64;
            v
        }))
    };
    let input = sum(|r| r.uncached_input);
    let read = sum(|r| r.cached_input);
    let output = sum(|r| r.output);
    let cache_write = sum(|r| r.cache_creation);
    let denominator = input + read;
    let read_fraction = if denominator > 0.0 {
        read / denominator
    } else {
        0.0
    };
    BreakdownData {
        input,
        read,
        output,
        cache_write,
        read_fraction,
    }
}

/// Seed one `(model_call, token_usage)` pair — the fixture helper the feed
/// tests (and the recall/nic feeds' tests) share.
#[cfg(test)]
pub(crate) fn seed_token_fact(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    call_id: &str,
    started_at: i64,
    classes: (i64, i64, i64, i64),
) {
    rt.block_on(store.insert(
        "model_call",
        &[
            "call_id",
            "turn_id",
            "model",
            "started_at",
            "schema_version",
        ],
        &[
            Value::text(call_id),
            Value::text("t1"),
            Value::text("claude-fable-5"),
            Value::Int(started_at),
            Value::Int(1),
        ],
    ))
    .unwrap();
    rt.block_on(store.insert(
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
            Value::text(call_id),
            Value::Int(classes.0),
            Value::Int(classes.1),
            Value::Int(classes.2),
            Value::Int(classes.3),
            Value::text("provider_reported"),
            Value::Int(1),
        ],
    ))
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rift_canopy::col3::{BreakdownView, TokensView};

    fn make_project(root: &Path, name: &str) -> String {
        let proj = root.join(name);
        std::fs::create_dir_all(&proj).unwrap();
        let repo = repo_id_for(&proj).unwrap();
        let db = projection_db_under(root, &repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = SqliteStore::open(&db).unwrap();
        store.create_canonical_schema().unwrap();
        proj.to_string_lossy().into_owned()
    }

    fn open_proj(root: &Path, path: &str) -> SqliteStore {
        let repo = repo_id_for(Path::new(path)).unwrap();
        SqliteStore::open(projection_db_under(root, &repo)).unwrap()
    }

    #[test]
    fn no_facts_yields_the_placeholder_never_zero() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "empty");
        let update = compute_tokens(root.path(), &proj);
        assert_eq!(
            update.payload,
            FeedPayload::Tokens {
                tokens: TokensView::NoActivity,
                breakdown: BreakdownView::NoActivity,
            }
        );
    }

    #[test]
    fn totals_and_breakdown_sum_the_four_classes() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_token_fact(&store, &rt, "c1", 1_000, (100, 400, 50, 200));
        seed_token_fact(&store, &rt, "c2", 2_000, (50, 600, 0, 100));

        let update = compute_tokens(root.path(), &proj);
        let FeedPayload::Tokens { tokens, breakdown } = update.payload else {
            panic!("expected a Tokens payload");
        };
        let TokensView::Data(t) = tokens else {
            panic!("expected token data");
        };
        assert!((t.total - 1_500.0).abs() < f64::EPSILON);
        assert!(!t.rate.is_empty());
        let BreakdownView::Data(b) = breakdown else {
            panic!("expected breakdown data");
        };
        assert!((b.input - 150.0).abs() < f64::EPSILON);
        assert!((b.read - 1_000.0).abs() < f64::EPSILON);
        assert!((b.output - 300.0).abs() < f64::EPSILON);
        assert!((b.cache_write - 50.0).abs() < f64::EPSILON);
        // read/(input+read) = 1000/1150
        assert!((b.read_fraction - 1_000.0 / 1_150.0).abs() < 1e-9);
    }

    #[test]
    fn read_fraction_zero_over_zero_is_zero() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        // Output-only usage: input + read == 0.
        seed_token_fact(&store, &rt, "c1", 1_000, (0, 0, 0, 500));
        let update = compute_tokens(root.path(), &proj);
        let FeedPayload::Tokens { breakdown, .. } = update.payload else {
            panic!("expected a Tokens payload");
        };
        let BreakdownView::Data(b) = breakdown else {
            panic!("expected breakdown data");
        };
        assert!((b.read_fraction - 0.0).abs() < f64::EPSILON, "0/0 ≡ 0");
    }

    #[test]
    fn bucketing_is_deterministic_and_idempotent_under_recompute() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        for i in 0..20i64 {
            seed_token_fact(&store, &rt, &format!("c{i}"), i * 7_000, (10, 20, 0, 5));
        }
        let a = compute_tokens(root.path(), &proj);
        let b = compute_tokens(root.path(), &proj);
        assert_eq!(a, b, "recomputing over the same rows is identical");
    }

    #[test]
    fn adaptive_bucket_ladder_scales_with_span() {
        assert_eq!(bucket_ms(0), 1_000, "tiny spans take the finest step");
        assert_eq!(bucket_ms(48_000), 1_000);
        assert_eq!(bucket_ms(49_000), 5_000, "one over → the next step");
        assert_eq!(bucket_ms(3_600_000), 300_000);
        assert!(
            bucket_ms(i64::MAX) == 86_400_000,
            "absurd spans clamp to the coarsest step"
        );
    }

    #[test]
    fn corrupt_negative_counts_clamp_to_zero() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_token_fact(&store, &rt, "c1", 1_000, (-50, 100, -3, 20));
        let update = compute_tokens(root.path(), &proj);
        let FeedPayload::Tokens { tokens, breakdown } = update.payload else {
            panic!("expected a Tokens payload");
        };
        let TokensView::Data(t) = tokens else {
            panic!("expected data");
        };
        assert!(
            (t.total - 120.0).abs() < f64::EPSILON,
            "negatives contribute 0"
        );
        let BreakdownView::Data(b) = breakdown else {
            panic!("expected data");
        };
        assert!((b.input - 0.0).abs() < f64::EPSILON);
    }

    proptest! {
        // Real stores per case: keep the case count farm-friendly.
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(8))]

        /// Scope: two projects' feeds never mix, for arbitrary fixtures
        /// (user story 17).
        #[test]
        fn feeds_are_scope_filtered_across_projects(
            a_facts in proptest::collection::vec((0i64..100_000, 0i64..5_000), 1..12),
            b_facts in proptest::collection::vec((0i64..100_000, 0i64..5_000), 0..12),
        ) {
            let root = tempfile::tempdir().unwrap();
            let pa = make_project(root.path(), "a");
            let pb = make_project(root.path(), "b");
            let (sa, sb) = (open_proj(root.path(), &pa), open_proj(root.path(), &pb));
            let rt = runtime().unwrap();
            let mut a_total = 0.0f64;
            for (i, (t, v)) in a_facts.iter().enumerate() {
                seed_token_fact(&sa, &rt, &format!("a{i}"), *t, (*v, 0, 0, 0));
                #[allow(clippy::cast_precision_loss)]
                { a_total += *v as f64; }
            }
            for (i, (t, v)) in b_facts.iter().enumerate() {
                seed_token_fact(&sb, &rt, &format!("b{i}"), *t, (0, *v, 0, 0));
            }
            let ua = compute_tokens(root.path(), &pa);
            let FeedPayload::Tokens { tokens, breakdown } = ua.payload else {
                panic!("expected Tokens");
            };
            let TokensView::Data(t) = tokens else { panic!("a has facts") };
            prop_assert!((t.total - a_total).abs() < 1e-6, "a's total is exactly a's rows");
            // a seeded only uncached input: any cached-read leakage came from b.
            let BreakdownView::Data(bd) = breakdown else { panic!("a has facts") };
            prop_assert!(bd.read.abs() < f64::EPSILON, "project b's reads leaked into a");
        }

        /// The rate series is total and non-negative for arbitrary fact sets.
        #[test]
        fn rate_series_is_total(
            facts in proptest::collection::vec((0i64..10_000_000, -100i64..100_000), 0..40),
        ) {
            let mut rows: Vec<TokenRow> = facts
                .into_iter()
                .map(|(t, v)| TokenRow {
                    t,
                    uncached_input: v,
                    cached_input: 0,
                    cache_creation: 0,
                    output: 0,
                })
                .collect();
            rows.sort_by_key(|r| r.t);
            let series = rate_series(&rows);
            prop_assert!(series.len() <= usize::try_from(TARGET_BUCKETS).unwrap());
            for v in series {
                prop_assert!(v.is_finite() && v >= 0.0);
            }
        }
    }
}
