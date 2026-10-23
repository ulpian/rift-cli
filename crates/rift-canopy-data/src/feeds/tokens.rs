//! The Tokens card: the four-class token facts a project's projection holds,
//! their trailing rate series, the input | read | output breakdown, and the
//! by-worktree decomposition of the same rows.
//!
//! One read feeds every figure on the card, which is why the axis sums to the
//! headline instead of merely agreeing with it.

use std::collections::BTreeMap;
use std::path::Path;

use rift_canopy::col3::{
    BreakdownData, BreakdownView, TokensData, TokensView, WorktreeAxis, WorktreeSlice,
};
use rift_canopy::{FeedPayload, FeedUpdate};
use rift_store::{projection_db_for_family_under, CanonicalStore, SqliteStore};

use super::project_family;
use crate::store::runtime;

/// The bucket-size ladder (ms) for the time-bucketed rate series, coarsest
/// last. Adaptive policy, deterministic in the data: the
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

/// One token-usage fact: the model call's start time + the four classes, and
/// the worktree its session was last observed in (worktree provenance).
struct TokenRow {
    t: i64,
    uncached_input: i64,
    cached_input: i64,
    cache_creation: i64,
    output: i64,
    /// The member's name, or `None` for **unknown** — no session behind the
    /// call, or a session with no observation. Never `main` by default
    /// (worktree provenance).
    worktree: Option<String>,
}

/// Each session's CURRENT worktree: the observation with the greatest
/// `first_observed_at` (worktree provenance), keyed by session id.
///
/// Conflicting observations at the same latest timestamp remain unknown. Absence is
/// the answer for every session with no row, which is every session captured
/// before worktree provenance; sealed shards are never rewritten, so the
/// history that would have named them can never be back-filled.
///
/// Total: an unreadable table is an empty map, never an error. A projection
/// still on a pre-v14 schema has no `session_worktree` at all, and the token
/// panel must keep working there — it simply reads every fact as `unknown`.
/// Both observation relations are read, so a session whose latest read a
/// leading `cd` filed under another member is that member's.
fn latest_worktree_by_session(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
) -> BTreeMap<String, String> {
    let relation = rt.block_on(rift_store::SessionWorktreeRow::relation(store));
    let Ok(rows) = rt.block_on(store.query(
        &format!(
            "SELECT w.session_id, MIN(w.worktree_name) \
             FROM {relation} w \
             JOIN (SELECT session_id, MAX(first_observed_at) AS latest \
                   FROM {relation} GROUP BY session_id) l \
               ON l.session_id = w.session_id AND l.latest = w.first_observed_at \
             GROUP BY w.session_id HAVING MIN(w.worktree_name)=MAX(w.worktree_name)"
        ),
        &[],
    )) else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for r in rows {
        if let (Ok(session), Ok(name)) = (r.get_text(0), r.get_text(1)) {
            out.insert(session, name);
        }
    }
    out
}

/// Decompose the token rows by worktree (the family view).
///
/// Computed from the **same** rows [`tokens_data`] totals, so
/// [`WorktreeAxis::total`] is that total by construction and no surface can
/// pair one read's figure with another read's rows. `family_only` stays 0:
/// every token fact reaches a session (or fails to, and is `unknown`) — a
/// fact with no session key at all is the serve trail's case, not this one.
fn worktree_axis(rows: &[TokenRow]) -> WorktreeAxis {
    let mut by_name: BTreeMap<Option<&str>, u64> = BTreeMap::new();
    for row in rows {
        let tokens = u64::try_from(row_total(row).max(0.0) as i64).unwrap_or(0);
        let slot = by_name.entry(row.worktree.as_deref()).or_insert(0);
        *slot = slot.saturating_add(tokens);
    }
    let mut slices: Vec<WorktreeSlice> = by_name
        .into_iter()
        .map(|(name, tokens)| WorktreeSlice {
            name: name.map(ToOwned::to_owned),
            tokens,
        })
        .collect();
    // Largest first, ties by name, `unknown` always last: a stable order, so
    // the disclosure is a golden and not a hash walk.
    slices.sort_by(|a, b| {
        b.tokens
            .cmp(&a.tokens)
            .then_with(|| a.name.is_none().cmp(&b.name.is_none()))
            .then_with(|| a.name.cmp(&b.name))
    });
    WorktreeAxis {
        rows: slices,
        family_only: 0,
    }
}

/// Compute the Tokens feed for the project at `path`: the token-usage section
/// (bucketed rate + cumulative total) and the input | read | output breakdown
/// ("read" = cache reads; cache writes are the secondary band). Total — any
/// unreadable projection yields the honest placeholders, never an error or a
/// fake zero.
#[must_use]
pub fn compute_tokens(root: &Path, path: &str) -> FeedUpdate {
    let member = rift_git::discover(Path::new(path))
        .ok()
        .map(|paths| paths.member_name());
    let rows = read_token_rows(root, path).map(|mut rows| {
        rows.retain(|row| {
            row.worktree
                .as_ref()
                .is_some_and(|name| Some(name) == member.as_ref())
        });
        rows
    });
    let payload = match rows {
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
///
/// The `turn` hop is a LEFT JOIN and so is the worktree lookup that follows
/// it: raw family totals retain calls with or without an attributed session.
/// The public feed then selects only this checkout's attributed rows.
fn read_token_rows(root: &Path, path: &str) -> Option<Vec<TokenRow>> {
    let db = projection_db_for_family_under(root, &project_family(Path::new(path))?);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open_read_only(&db).ok()?;
    let rt = runtime().ok()?;
    let rows = rt
        .block_on(store.query(
            "SELECT mc.started_at, tu.uncached_input, tu.cached_input, \
                    tu.cache_creation, tu.output, tr.session_id \
             FROM token_usage tu JOIN model_call mc ON mc.call_id = tu.call_id \
             LEFT JOIN turn tr ON tr.turn_id = mc.turn_id \
             ORDER BY mc.started_at, tu.call_id",
            &[],
        ))
        .ok()?;
    // Separate query, deliberately: a pre-v14 projection has no
    // `session_worktree` table. Keep its raw facts as unknown; the worktree
    // feed excludes them rather than assigning them to main.
    let worktrees = latest_worktree_by_session(&store, &rt);
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let session = r.get_opt_text(5).ok()?;
        out.push(TokenRow {
            t: r.get_i64(0).ok()?,
            uncached_input: r.get_i64(1).ok()?,
            cached_input: r.get_i64(2).ok()?,
            cache_creation: r.get_i64(3).ok()?,
            output: r.get_i64(4).ok()?,
            worktree: session.and_then(|id| worktrees.get(&id).cloned()),
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
        by_worktree: worktree_axis(rows),
    }
}

/// The breakdown section's numbers: input = uncached input, read = cached
/// reads, output = output, cache writes as the secondary band; the read
/// fraction is read / (input + read), `0/0 ≡ 0`.
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

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rift_store::Value;

    use super::*;
    use crate::feeds::fixtures::{
        make_project, open_proj, seed_model_fact_in_turn, seed_token_fact,
    };

    /// Seed a session and one turn belonging to it.
    fn seed_session_turn(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        session: &str,
        turn: &str,
    ) {
        rt.block_on(store.insert(
            "session",
            &[
                "session_id",
                "agent",
                "model_primary",
                "repo_id",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(session),
                Value::text("claude-code"),
                Value::text("claude-fable-5"),
                Value::text("repo"),
                Value::Int(1),
                Value::Int(1),
            ],
        ))
        .unwrap();
        rt.block_on(store.insert(
            "turn",
            &[
                "turn_id",
                "session_id",
                "ordinal",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(turn),
                Value::text(session),
                Value::Int(1),
                Value::Int(1),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// Seed one `session_worktree` OBSERVATION (worktree provenance). The relation is
    /// append-only and keyed by observed state, so a session that moved has
    /// two rows and the greatest `first_observed_at` is its current worktree.
    fn seed_observation(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        observation: &str,
        session: &str,
        name: &str,
        first_observed_at: i64,
    ) {
        rt.block_on(store.insert(
            "session_worktree",
            &[
                "observation_id",
                "session_id",
                "worktree_token",
                "worktree_name",
                "is_linked",
                "work_tree",
                "basis",
                "first_observed_at",
                "schema_version",
            ],
            &[
                Value::text(observation),
                Value::text(session),
                Value::text("0123456789ab"),
                Value::text(name),
                Value::Int(i64::from(name != "main")),
                Value::text(format!("/w/{name}")),
                Value::text("payload_cwd"),
                Value::Int(first_observed_at),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }
    fn token_row(t: i64, uncached: i64, cached: i64, creation: i64, output: i64) -> TokenRow {
        TokenRow {
            t,
            uncached_input: uncached,
            cached_input: cached,
            cache_creation: creation,
            output,
            worktree: None,
        }
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
        seed_session_turn(&store, &rt, "s1", "t1");
        seed_observation(&store, &rt, "o1", "s1", "main", 1);
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
    /// ★ the family view: the rows are per `worktree_name` (plus `unknown`)
    /// from each session's LATEST observation, and they **sum to the family
    /// total** — the panel's Σ and the axis come out of one pass over one set
    /// of rows, so they cannot disagree.
    ///
    /// The fixture carries every shape the join can meet: a session observed
    /// in the main work tree, a session observed twice (moved into a linked
    /// member — the later observation wins), a session with no observation at
    /// all, and a call whose turn row does not exist. The last two are
    /// `unknown`, never `main` (worktree provenance).
    #[test]
    fn the_breakdown_sums_to_the_family_total() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();

        seed_session_turn(&store, &rt, "s-main", "t-main");
        seed_session_turn(&store, &rt, "s-moved", "t-moved");
        seed_session_turn(&store, &rt, "s-unseen", "t-unseen");
        seed_observation(&store, &rt, "o1", "s-main", "main", 1_000);
        // The moved session: observed in main first, then in the member. The
        // greatest `first_observed_at` is its current worktree.
        seed_observation(&store, &rt, "o2", "s-moved", "main", 1_000);
        seed_observation(&store, &rt, "o3", "s-moved", "swt-d", 2_000);
        seed_session_turn(&store, &rt, "s-conflict", "t-conflict");
        seed_observation(&store, &rt, "o4", "s-conflict", "main", 2_000);
        seed_observation(&store, &rt, "o5", "s-conflict", "swt-d", 2_000);
        seed_model_fact_in_turn(&store, &rt, "c5", "t-conflict", "m", 5_000, (7, 0, 0, 0));
        // 100 + 400 + 50 + 200 = 750 under `main`
        seed_model_fact_in_turn(&store, &rt, "c1", "t-main", "m", 1_000, (100, 400, 50, 200));
        // 300 under `swt-d`
        seed_model_fact_in_turn(&store, &rt, "c2", "t-moved", "m", 2_000, (100, 100, 0, 100));
        // 60 under `unknown` (a session that was never observed)
        seed_model_fact_in_turn(&store, &rt, "c3", "t-unseen", "m", 3_000, (10, 20, 0, 30));
        // 5 under `unknown` (a call whose turn row is missing altogether —
        // the LEFT JOIN keeps it, which is why the sum holds)
        seed_model_fact_in_turn(&store, &rt, "c4", "t-missing", "m", 4_000, (5, 0, 0, 0));

        let rows = read_token_rows(root.path(), &proj).expect("raw family facts");
        let t = tokens_data(&rows);
        assert!((t.total - 1_122.0).abs() < f64::EPSILON, "{}", t.total);
        assert_eq!(
            t.by_worktree.total(),
            1_122,
            "the breakdown sums to the family total"
        );
        assert_eq!(
            t.by_worktree.rows,
            vec![
                WorktreeSlice {
                    name: Some("main".to_owned()),
                    tokens: 750,
                },
                WorktreeSlice {
                    name: Some("swt-d".to_owned()),
                    tokens: 300,
                },
                WorktreeSlice {
                    name: None,
                    tokens: 72,
                },
            ],
            "largest first; the moved session counts under its LATEST \
             observation; unattributable and conflicting calls land in ONE unknown row"
        );
        assert_eq!(
            t.by_worktree.family_only, 0,
            "every token fact reaches a session or fails to — none is \
             family-only, which is the serve trail's case"
        );
        let FeedPayload::Tokens {
            tokens: TokensView::Data(main),
            ..
        } = compute_tokens(root.path(), &proj).payload
        else {
            panic!("main has attributed facts");
        };
        assert_eq!(
            main.total, 750.0,
            "the main feed excludes moved and unknown facts"
        );
    }

    /// worktree provenance, the reader's half: a projection that records no
    /// observation reads `unknown` — never `main` — and the total still
    /// decomposes. This is also the pre-v14 shape: the table is empty (or, on
    /// a projection that has not migrated, absent), and the panel keeps
    /// working.
    #[test]
    fn a_projection_with_no_observations_reads_unknown_never_main() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_session_turn(&store, &rt, "s1", "t1");
        seed_token_fact(&store, &rt, "c1", 1_000, (10, 0, 0, 0));

        let rows = read_token_rows(root.path(), &proj).expect("raw family facts");
        let t = tokens_data(&rows);
        assert_eq!(
            t.by_worktree.rows,
            vec![WorktreeSlice {
                name: None,
                tokens: 10,
            }],
            "no observation ⇒ unknown, and unknown is None — never `main`"
        );
        assert_eq!(t.by_worktree.total(), 10);
        assert!(
            matches!(
                compute_tokens(root.path(), &proj).payload,
                FeedPayload::Tokens {
                    tokens: TokensView::NoActivity,
                    ..
                }
            ),
            "unknown or other-member facts never appear in the main feed"
        );
    }

    /// A pre-v14 projection retains its raw token facts as unknown.
    /// The worktree feed excludes them instead of assigning them to main.
    #[test]
    fn a_projection_without_the_worktree_table_still_reads_its_tokens() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_token_fact(&store, &rt, "c1", 1_000, (7, 0, 0, 0));
        assert!(
            latest_worktree_by_session(&store, &rt).is_empty(),
            "no observations yet"
        );
        rt.block_on(async {
            let conn = rift_store::Store::conn(&store).await.unwrap();
            conn.execute("DROP TABLE session_worktree", &[]).await
        })
        .unwrap();

        assert!(
            latest_worktree_by_session(&store, &rt).is_empty(),
            "an unreadable relation is an empty map, not an error"
        );
        let rows = read_token_rows(root.path(), &proj).expect("raw family facts");
        let t = tokens_data(&rows);
        assert!((t.total - 7.0).abs() < f64::EPSILON);
        assert_eq!(t.by_worktree.total(), 7, "and still decomposes, as unknown");
        assert!(
            matches!(
                compute_tokens(root.path(), &proj).payload,
                FeedPayload::Tokens {
                    tokens: TokensView::NoActivity,
                    ..
                }
            ),
            "unknown or other-member facts never appear in the main feed"
        );
    }

    /// A session whose latest read a leading `cd` filed under a member is
    /// that member's on the worktree axis — read from the second relation —
    /// and the axis still reads the first relation alone without it.
    #[test]
    fn a_cd_filed_session_is_its_members_on_the_worktree_axis() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_observation(&store, &rt, "o1", "s1", "main", 10);
        rt.block_on(rift_store::record_session_worktree(
            &store,
            &rift_store::SessionWorktreeRow {
                observation_id: "o2".into(),
                session_id: "s1".into(),
                worktree_token: "112233445566".into(),
                worktree_name: "member".into(),
                is_linked: 1,
                work_tree: "/w/member".into(),
                branch: None,
                head_oid: None,
                subagent_id: None,
                basis: "shell_cd".into(),
                first_observed_at: 20,
                schema_version: 1,
            },
        ))
        .unwrap();
        assert_eq!(
            latest_worktree_by_session(&store, &rt)
                .get("s1")
                .map(String::as_str),
            Some("member")
        );
        seed_session_turn(&store, &rt, "s1", "t1");
        seed_model_fact_in_turn(&store, &rt, "c1", "t1", "m", 1_000, (10, 0, 0, 0));
        let rows = read_token_rows(root.path(), &proj).expect("raw family facts");
        let t = tokens_data(&rows);
        assert_eq!(
            t.by_worktree.rows,
            vec![WorktreeSlice {
                name: Some("member".to_owned()),
                tokens: 10,
            }],
            "the worktree column names the member the read was filed under"
        );

        assert!(
            matches!(
                compute_tokens(root.path(), &proj).payload,
                FeedPayload::Tokens {
                    tokens: TokensView::NoActivity,
                    ..
                }
            ),
            "unknown or other-member facts never appear in the main feed"
        );
        rt.block_on(async {
            let conn = rift_store::Store::conn(&store).await.unwrap();
            conn.execute("DROP TABLE session_worktree_cd", &[]).await
        })
        .unwrap();
        assert_eq!(
            latest_worktree_by_session(&store, &rt)
                .get("s1")
                .map(String::as_str),
            Some("main"),
            "a projection without the second relation reads the first"
        );
    }

    #[test]
    fn read_fraction_zero_over_zero_is_zero() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_session_turn(&store, &rt, "s1", "t1");
        seed_observation(&store, &rt, "o1", "s1", "main", 1);
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
        seed_session_turn(&store, &rt, "s1", "t1");
        seed_observation(&store, &rt, "o1", "s1", "main", 1);
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
        seed_session_turn(&store, &rt, "s1", "t1");
        seed_observation(&store, &rt, "o1", "s1", "main", 1);
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
    /// A row's total is the sum of all four classes, and a negative class
    /// contributes zero rather than subtracting — a bad row must not be able
    /// to pull a displayed total downwards.
    #[test]
    fn a_negative_token_class_contributes_zero_not_a_subtraction() {
        assert_eq!(row_total(&token_row(0, 1, 2, 3, 4)), 10.0);
        assert_eq!(row_total(&token_row(0, 0, 0, 0, 0)), 0.0);
        assert_eq!(
            row_total(&token_row(0, 10, -5, 3, -1)),
            13.0,
            "negatives floor at zero"
        );
    }

    /// The bucket ladder: the smallest step where the span fits the display
    /// budget, and the coarsest step for anything past the ladder's end. A
    /// span at the exact boundary still fits the smaller step.
    #[test]
    fn the_bucket_ladder_picks_the_smallest_step_that_fits() {
        assert_eq!(bucket_ms(0), 1_000);
        assert_eq!(bucket_ms(1_000 * TARGET_BUCKETS), 1_000, "exactly fits");
        assert_eq!(bucket_ms(1_000 * TARGET_BUCKETS + 1), 5_000);
        assert_eq!(bucket_ms(60_000 * TARGET_BUCKETS), 60_000);
        assert_eq!(bucket_ms(60_000 * TARGET_BUCKETS + 1), 300_000);
        assert_eq!(
            bucket_ms(86_400_000 * TARGET_BUCKETS + 1),
            86_400_000,
            "past the ladder, the coarsest step"
        );
        assert_eq!(bucket_ms(i64::MAX), 86_400_000, "no overflow off the end");
    }

    /// The rate series is a bucketed sum: no facts is an empty series (the
    /// placeholder's job, not a row of zeros), a quiet bucket inside an active
    /// span is an honest zero, leading empty margin is trimmed, and replaying
    /// the same rows yields the same series.
    #[test]
    fn the_rate_series_is_bucketed_trimmed_and_replayable() {
        assert!(rate_series(&[]).is_empty());

        // Two facts one second apart: step 1s, so they land in adjacent
        // buckets at the END of the window, with the empty margin trimmed.
        let rows = vec![token_row(0, 1, 0, 0, 0), token_row(1_000, 0, 0, 0, 2)];
        let series = rate_series(&rows);
        assert_eq!(series, vec![1.0, 2.0]);
        assert_eq!(
            rate_series(&rows),
            series,
            "bucketing is idempotent in the rows"
        );

        // A quiet bucket between two active ones is a zero, not a gap.
        let quiet = rate_series(&[token_row(0, 3, 0, 0, 0), token_row(2_000, 0, 0, 0, 5)]);
        assert_eq!(quiet, vec![3.0, 0.0, 5.0]);

        // Facts older than the visible window are dropped rather than folded
        // into the first bucket, which would invent a spike.
        let old = rate_series(&[
            token_row(0, 100, 0, 0, 0),
            token_row(86_400_000 * TARGET_BUCKETS * 2, 0, 0, 0, 7),
        ]);
        assert_eq!(
            crate::series::saturating_sum(old.iter().copied()),
            7.0,
            "the out-of-window fact is dropped, not relocated"
        );
    }

    /// The breakdown's read fraction is `read / (input + read)` with `0/0 ≡ 0`
    /// — an unpriced-style division that must never produce NaN in a drawn
    /// bar — and every class floors at zero.
    #[test]
    fn the_read_fraction_is_zero_when_there_is_nothing_to_divide() {
        let empty = breakdown_data(&[]);
        assert_eq!(empty.read_fraction, 0.0);
        assert!(empty.read_fraction.is_finite(), "never NaN");

        let output_only = breakdown_data(&[token_row(0, 0, 0, 0, 9)]);
        assert_eq!(output_only.output, 9.0);
        assert_eq!(
            output_only.read_fraction, 0.0,
            "output alone does not make a read fraction"
        );

        let mixed = breakdown_data(&[token_row(0, 3, 1, 5, 2), token_row(1, -4, 0, 0, 0)]);
        assert_eq!((mixed.input, mixed.read), (3.0, 1.0));
        assert_eq!((mixed.output, mixed.cache_write), (2.0, 5.0));
        assert!((mixed.read_fraction - 0.25).abs() < 1e-12);
    }

    /// The tokens section's total is the sum of the same row totals its rate
    /// series buckets, so the headline number and the plot cannot disagree.
    #[test]
    fn the_tokens_total_is_the_same_arithmetic_as_the_plot() {
        let rows = vec![
            token_row(0, 1, 2, 3, 4),
            token_row(1_000, 5, 0, 0, 0),
            token_row(2_000, 0, 0, 0, 6),
        ];
        let data = tokens_data(&rows);
        assert_eq!(data.total, 21.0);
        assert_eq!(
            crate::series::saturating_sum(data.rate.iter().copied()),
            data.total,
            "the plot sums to the headline"
        );
        let empty = tokens_data(&[]);
        assert_eq!(empty.total, 0.0);
        assert!(empty.rate.is_empty());
    }
    proptest! {
        // Real stores per case: keep the case count farm-friendly.
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(8))]

        /// Scope: two projects' feeds never mix, for arbitrary fixtures.
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
            for store in [&sa, &sb] {
                seed_session_turn(store, &rt, "s1", "t1");
                seed_observation(store, &rt, "o1", "s1", "main", 1);
            }
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
                    worktree: None,
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
