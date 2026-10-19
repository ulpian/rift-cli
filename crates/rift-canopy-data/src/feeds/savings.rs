//! The Recall card's savings side: the shared savings share, what the window's
//! avoided tokens are worth in dollars, and the presence answer that decides
//! which of the three honest states the card is in.

use std::path::Path;

use rift_canopy::col3::{
    FamilyShareRow, RecallView, RecallWindow, SavingsData, ShareData, ToolOutputClassCounts,
    ToolOutputCounts, UsdRow, WorktreeAxis,
};
use rift_canopy::{FeedPayload, FeedUpdate};
use rift_shadow_cost::{
    avoided_tokens_micros, fmt_usd_estimate, fmt_usd_exact, partition_largest_remainder,
};
use rift_store::{
    effective_prices, projection_db_for_family_under, recall_db_for_family_under, CanonicalStore,
    SqliteStore,
};

use super::calls::{read_call_data, CallDb};
use super::project_family;
use super::window::{mix_window, resolve_card_window, CardWindow};
use crate::store::runtime;

/// Compute the Recall feed: the savings share and what it is worth in dollars
/// (the priced savings estimate), plus per-channel serve-call rates, from the
/// project's own recall.db. `now_ms` is the injected clock (the
/// [`super::compute_activity`] precedent): the *current* prices value the window's
/// avoided total — per-serve historical re-pricing is explicitly out of scope.
///
/// Disabled-vs-quiet, decided deliberately: **recall.db absent** means
/// recall was never enabled here → the enable-prompting placeholder;
/// recall.db present with no served event in the window is enabled-but-quiet
/// → the visibly distinct honest-empty state. Neither is ever a zero line.
///
/// `window` is the viewer's chosen period, and it is resolved **once**
/// (`CardWindow`) into the cut every read below takes: the presence probe,
/// the share, the mix the money is priced through, and the serve count. The
/// headline is the share itself — `avoided ÷ (avoided + read)`, the figure
/// `rift savings` publishes — so the card and the command line cannot show two
/// arithmetics, and the two bars beside it are the two sides of that fraction.
#[must_use]
pub fn compute_recall(root: &Path, path: &str, now_ms: i64, window: RecallWindow) -> FeedUpdate {
    let card = resolve_card_window(path, now_ms, window);
    let cut_ms = card.cut_ms();
    let savings = match read_audit_presence(root, path, cut_ms) {
        RecallDb::Absent => RecallView::Disabled,
        RecallDb::Quiet => RecallView::Quiet,
        RecallDb::Live => {
            // The share is read FIRST because it carries BOTH sides of the
            // fraction, and the money values both of them through one mix at
            // one moment — two bars are comparable in dollars only if they
            // were partitioned across the same models at the same instant.
            let share = read_savings_share(root, path, &card);
            let priced = read_usd_savings(
                root,
                path,
                &PricingRequest {
                    avoided: share.avoided_tokens,
                    read: share.read_tokens,
                    now_ms,
                    cut_ms,
                },
            );
            // Rung 4 (the priced savings estimate): no honest figure → no chip
            // at all; the token headline stands alone.
            let (usd_saved_micros, usd_label, usd_rows) = priced.as_ref().map_or_else(
                || (None, None, Vec::new()),
                |p| {
                    (
                        Some(p.saved_micros),
                        Some(fmt_usd_estimate(p.saved_micros)),
                        p.rows.clone(),
                    )
                },
            );
            let (usd_read_micros, usd_read_label) = priced
                .as_ref()
                .and_then(|p| p.read_micros)
                .map_or((None, None), |micros| {
                    (Some(micros), Some(fmt_usd_estimate(micros)))
                });
            RecallView::Data(SavingsData {
                usd_saved_micros,
                usd_label,
                usd_rows,
                usd_read_micros,
                usd_read_label,
                // the family view: a serve carries no session key, so it
                // cannot be attributed to a member. Not one relation in
                // recall.db holds one — `memd_audit_event`,
                // `serve_call_event` and `serve_session_event` are the whole
                // serve trail, and the last is one row per CHANNEL ever
                // ("refining to per-agent-session granularity is future work
                // ... declared rather than approximated", `schema.rs`). The
                // avoided total is therefore the family's, whole, and the
                // modal says so instead of splitting it on a guess.
                by_worktree: WorktreeAxis {
                    rows: Vec::new(),
                    family_only: share.avoided_tokens,
                },
                // The owner's evaluation figure, computed by the ONE shared
                // function the CLI report, the `savings` verb and the recall
                // status table also call — so the card and the terminal can
                // never show two numbers for one repository.
                share: Box::new(share),
            })
        }
    };
    let calls = match read_call_data(root, path, cut_ms) {
        CallDb::Absent => RecallView::Disabled,
        CallDb::Quiet => RecallView::Quiet,
        CallDb::Data(data) => RecallView::Data(data),
    };
    FeedUpdate {
        project: path.to_string(),
        payload: FeedPayload::Recall { savings, calls },
    }
}

/// The savings share for this project's family: `avoided ÷ (avoided + read)`,
/// overall and per family, over the card's one resolved window.
///
/// The window arrives already composed ([`super::window::resolve_card_window`])
/// and this function neither re-resolves it nor spells a sentence of its own:
/// the cut it hands `savings::compose` is the cut the headline, the bars, the money
/// and the serve count were all taken over, and the sentence it publishes is
/// that boundary's own.
///
/// Read-only on both stores, and empty on ANY failure — a share that cannot be
/// measured draws no chip and no modal rows, exactly as an unpriced project
/// draws no dollar chip. Identity is resolved the way every other reader here
/// resolves it: recall.db under the family's recall id, projection.db under
/// its capture id, never one under the other's key.
fn read_savings_share(root: &Path, path: &str, card: &CardWindow) -> ShareData {
    let Some(family) = project_family(Path::new(path)) else {
        return ShareData::default();
    };
    let (recall_db, projection_db) = (
        recall_db_for_family_under(root, &family),
        projection_db_for_family_under(root, &family),
    );
    if !recall_db.exists() || !projection_db.exists() {
        return ShareData::default();
    }
    let (Ok(recall), Ok(projection)) = (
        SqliteStore::open_read_only(&recall_db),
        SqliteStore::open_read_only(&projection_db),
    ) else {
        return ShareData::default();
    };
    let Ok(rt) = runtime() else {
        return ShareData::default();
    };
    let Ok(savings) = rt.block_on(rift_recall_ledger::savings::compose(
        &recall,
        &projection,
        card.cut_ms(),
    )) else {
        return ShareData::default();
    };
    // Beside the share: a projection that cannot say draws no line, which is
    // what a window with no tool-output row draws too.
    let tool_output = rt
        .block_on(rift_recall_ledger::savings::tool_output_totals(
            &recall,
            card.cut_ms(),
        ))
        .map(|t| tool_output_counts(&t))
        .unwrap_or_default();
    ShareData {
        window: card.label(),
        overall: savings.share(),
        honesty: savings.honesty(),
        avoided_tokens: savings.tokens_avoided,
        read_tokens: savings.read_tokens,
        sessions_with_reads: savings.sessions_with_reads,
        reads_unmeasured: savings.reads_unmeasured,
        by_family: savings.families_share(),
        excluding: savings.share_excluding_undenominated(),
        excluded: savings.excluded().into_iter().map(str::to_owned).collect(),
        claimed_pulls: savings.claimed_by_pulls.decisions,
        claimed_avoided: savings.claimed_by_pulls.tokens_avoided,
        claimed_returned: savings.claimed_by_pulls.tokens_returned,
        tool_output,
        families: savings
            .families
            .iter()
            .map(|f| FamilyShareRow {
                family: f.family.clone(),
                share: f.share(),
                honesty: f.honesty(),
                sessions_with_reads: f.sessions_with_reads,
                decided_without_reads: f.decided_without_reads(),
            })
            .collect(),
    }
}
/// The ledger's tool-output totals in the card's own counts.
fn tool_output_counts(t: &rift_recall_ledger::savings::ToolOutputTotals) -> ToolOutputCounts {
    let class = |c: &rift_recall_ledger::savings::ToolOutputClass| ToolOutputClassCounts {
        would_save: c.observed,
        acted: c.acted,
        avoided: c.avoided,
    };
    ToolOutputCounts {
        held: class(&t.held),
        folded: class(&t.folded),
    }
}

/// One model's share of the observed input-token mix, with its resolved
/// uncached-input rate (micro-USD per MTok; `None` = no seeded price).
struct MixShare {
    model: String,
    weight: u64,
    price_per_mtok: Option<i64>,
}

/// The pure pricing ladder over an already-resolved mix (rungs 1–3):
/// partition `avoided` across the shares (largest remainder — slices sum to
/// exactly `avoided`), price each slice at its model's rate, and sum the
/// priced subtotals into the headline.
///
/// `None` when there is no honest figure: no basis to divide on, or **no
/// share is priced** — a positive saving must never surface as a fabricated
/// `~$0.00`. Partially priced mixes (rung 2) succeed: unpriced rows keep
/// their token slice and contribute no dollars, and the modal discloses them.
fn usd_breakdown(avoided: u64, mix: &[MixShare]) -> Option<(i64, Vec<UsdRow>)> {
    let weights: Vec<u64> = mix.iter().map(|m| m.weight).collect();
    let slices = partition_largest_remainder(avoided, &weights)?;
    let mut rows = Vec::with_capacity(mix.len());
    let mut total: i64 = 0;
    let mut any_priced = false;
    for (share, tokens) in mix.iter().zip(slices) {
        // A rate of exactly zero is **not a price**. A seeded `0` values a
        // real saving at `~$0.00`, which reads as a measured nothing rather
        // than as the absent price it is — the one shape the degradation
        // ladder's rung 4 exists to prevent. Unpriced it is, and the modal
        // says so on its row.
        let rate = share.price_per_mtok.filter(|p| *p > 0);
        let subtotal = avoided_tokens_micros(tokens, rate);
        if let Some(s) = subtotal {
            any_priced = true;
            total = total.saturating_add(s);
        }
        rows.push(UsdRow {
            model: share.model.clone(),
            tokens,
            price_per_mtok_micros: rate,
            subtotal_micros: subtotal,
            rate_label: rate.map(|p| format!("{}/MTok", fmt_usd_exact(p))),
            subtotal_label: subtotal.map(fmt_usd_estimate),
        });
    }
    if !any_priced {
        return None;
    }
    Some((total, rows))
}

/// Resolve the observed model mix and current prices from the **capture**
/// projection and value the avoided total (the priced savings estimate). Read-only, and
/// `None` on *any* failure — a pricing problem degrades to "no chip", never a
/// broken panel.
///
/// Identity is one resolution per projection: this opens projection.db under
/// the family's capture id, while the avoided tokens came from recall.db under
/// the family's recall id — a split confirmed by measurement, now carried by the
/// family record rather than re-derived per path; resolving them the same way
/// is the recorded dashboard defect.
fn read_usd_savings(root: &Path, path: &str, request: &PricingRequest) -> Option<UsdSavings> {
    let db = projection_db_for_family_under(root, &project_family(Path::new(path))?);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open_read_only(&db).ok()?;
    let rt = runtime().ok()?;
    let mix = priced_mix(&store, &rt, request)?;
    let (saved_micros, rows) = usd_breakdown(request.avoided, &mix)?;
    Some(UsdSavings {
        saved_micros,
        rows,
        // The read side, valued through the SAME mix and the SAME rates. It
        // is a second reading of one basis, not a second price table: the two
        // bars are comparable in dollars only because their tokens were
        // partitioned across the same models at the same moment.
        //
        // Uncached input for both, for the reason the avoided side already
        // uses it: a re-read is fresh input, and a bar priced as output would
        // overstate by about five times against a bar that is not.
        read_micros: usd_breakdown(request.read, &mix).map(|(micros, _)| micros),
    })
}

/// The observed model mix with a rate on every share that has one.
fn priced_mix(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    request: &PricingRequest,
) -> Option<Vec<MixShare>> {
    let mut mix = observed_mix(store, rt, request.cut_ms)?;
    // Current prices (a deliberate choice: today's rate on the
    // cumulative total), through the production bitemporal resolver — never
    // a reimplementation of the price SQL, and never the dead in-memory
    // table. `None` unless all four classes are seeded, which is stricter
    // and honest.
    for share in &mut mix {
        share.price_per_mtok = rt
            .block_on(effective_prices(
                store,
                &share.model,
                request.now_ms,
                request.now_ms,
            ))
            .ok()
            .flatten()
            .map(|p| p.uncached_input);
    }
    Some(mix)
}

/// The mix the avoided tokens are partitioned across: the window's own
/// input-token shares, or — when the window recorded no usable mix at all —
/// the single reference row of rung 3.
fn observed_mix(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    cut_ms: Option<i64>,
) -> Option<Vec<MixShare>> {
    let mix = input_token_mix(store, rt, cut_ms)?;
    if !mix.is_empty() {
        return Some(mix);
    }
    let model = most_frequent_model(store, rt, cut_ms)?;
    Some(vec![MixShare {
        model,
        weight: 1,
        price_per_mtok: None,
    }])
}

/// Each model's share of *input* tokens (uncached + cached) **over the card's
/// window**, because the tokens it partitions are that window's.
///
/// A mix taken over all history against a window's avoided total would
/// attribute this week's savings to a model the project stopped running in the
/// spring, and the modal would disclose a basis that was never this window's.
/// Deterministic order.
fn input_token_mix(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    cut_ms: Option<i64>,
) -> Option<Vec<MixShare>> {
    let (clause, params) = mix_window("m.started_at", cut_ms);
    let rows = rt
        .block_on(store.query(
            &format!(
                "SELECT m.model, SUM(t.uncached_input + t.cached_input) AS w \
                 FROM model_call m JOIN token_usage t ON t.call_id = m.call_id{clause} \
                 GROUP BY m.model HAVING w > 0 \
                 ORDER BY w DESC, m.model ASC"
            ),
            &params,
        ))
        .ok()?;
    let mut mix: Vec<MixShare> = Vec::with_capacity(rows.len());
    for r in rows {
        let (Ok(model), Ok(w)) = (r.get_text(0), r.get_i64(1)) else {
            continue;
        };
        mix.push(MixShare {
            model,
            weight: u64::try_from(w.max(0)).unwrap_or(0),
            price_per_mtok: None,
        });
    }
    Some(mix)
}

/// Rung 3: the most-frequent model captured IN THE SAME WINDOW, which then
/// carries the whole total as a single disclosed reference row.
///
/// (`session.model_primary` is NOT a source: it was measured empty.)
fn most_frequent_model(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    cut_ms: Option<i64>,
) -> Option<String> {
    let (clause, params) = mix_window("started_at", cut_ms);
    rt.block_on(store.query(
        &format!(
            "SELECT model, COUNT(*) AS c FROM model_call{clause} \
             GROUP BY model ORDER BY c DESC, model ASC LIMIT 1"
        ),
        &params,
    ))
    .ok()?
    .first()
    .and_then(|r| r.get_text(0).ok())
}

/// What the money is being asked to value: the two token totals of one card,
/// the instant the prices are read at, and the cut both totals were taken
/// over.
///
/// One struct because the four travel together by necessity — a read total
/// priced at a different instant, or over a different cut, from the avoided
/// total beside it is exactly the pair of figures the card must never draw.
struct PricingRequest {
    avoided: u64,
    read: u64,
    now_ms: i64,
    cut_ms: Option<i64>,
}

/// The priced side of the Recall card: the avoided total, its per-model rows,
/// and the read total valued on the same basis.
struct UsdSavings {
    saved_micros: i64,
    rows: Vec<UsdRow>,
    read_micros: Option<i64>,
}
/// The recall.db read outcome — a **presence** answer, not a figure.
///
/// Every number the card draws is composed by `savings::compose` over the same
/// window; what this read decides is which of the three honest states the card
/// is in. It used to pull every audit row into memory to sum them here, which
/// was both a second arithmetic beside the shared one and a full-table read
/// filtered in Rust.
enum RecallDb {
    /// No recall.db: recall was never enabled for this repo.
    Absent,
    /// recall.db exists but holds no served event in this window.
    Quiet,
    /// The window holds at least one served audit fact.
    Live,
}
/// Does this project have a recall plane, and did it decide anything inside
/// the card's window?
///
/// The window is pushed into SQL rather than filtered in Rust after selecting
/// every row: a projection with a long serve history made the dashboard read
/// all of it thirty times a minute to answer a question `COUNT(*)` answers in
/// the store, and the rows it materialised were then used for a series nothing
/// drew.
fn read_audit_presence(root: &Path, path: &str, cut_ms: Option<i64>) -> RecallDb {
    // recall.db is keyed by the family's RECALL id, not its capture id (which
    // keys projection.db). The serve/audit writer resolves the same family, so
    // reader and writer cannot land on different files — the defect that
    // stranded this panel on "no serves yet" while real serves were being made.
    let Some(family) = project_family(Path::new(path)) else {
        return RecallDb::Absent;
    };
    let db = recall_db_for_family_under(root, &family);
    if !db.exists() {
        return RecallDb::Absent;
    }
    // Read-only: a measurement never migrates the projection it measures, and
    // a dashboard that bootstrapped a store by looking at it would change the
    // Disabled/Quiet distinction as a side effect of being drawn.
    let Ok(store) = SqliteStore::open_read_only(&db) else {
        return RecallDb::Quiet; // present but unreadable: enabled, no data now
    };
    let Ok(rt) = runtime() else {
        return RecallDb::Quiet;
    };
    // The cut, on the same column and with the same `>=` the shared
    // computation's own clause uses (`savings::audit_totals`), so this probe
    // and the share beside it are answering over one set of rows.
    let (clause, params) = mix_window("recorded_at", cut_ms);
    let count = rt.block_on(store.query_opt_i64(
        &format!("SELECT COUNT(*) FROM memd_audit_event{clause}"),
        &params,
    ));
    let Ok(Some(count)) = count else {
        return RecallDb::Quiet; // e.g. a pre-v4 recall.db without the table
    };
    // A window that excludes every row is **quiet, not absent**: the plane is
    // enabled here and has served, just not lately. Absent means "recall was
    // never enabled", and answering a narrow window with the enable-prompt
    // would name a door the operator has already walked through.
    if count <= 0 {
        return RecallDb::Quiet;
    }
    RecallDb::Live
}

#[cfg(test)]
mod tests {
    use rift_git::repo_id_for;
    use rift_store::{recall_db_under, recall_repo_id, Value};

    use super::*;
    use crate::feeds::fixtures::{
        make_project, open_proj, seed_model_fact, seed_one_read, seed_recall_db, USD_NOW,
    };

    /// Seed a model's full four-class price row set (the priced savings estimate: a partial
    /// catalog prices nothing) — cached at 0.1×, write at 1.25×, output at
    /// 5×, the shipped seeds' derivation.
    fn seed_price(store: &SqliteStore, rt: &tokio::runtime::Runtime, model: &str, uncached: i64) {
        for (class, price) in [
            ("uncached_input", uncached),
            ("cached_input", uncached / 10),
            ("cache_creation", uncached + uncached / 4),
            ("output", uncached * 5),
        ] {
            rt.block_on(rift_store::record_price(
                store,
                &rift_store::PriceTableRow {
                    price_id: format!("test-{model}-{class}"),
                    model: model.to_owned(),
                    token_class: class.to_owned(),
                    unit_price_micros: price,
                    currency: "USD".to_owned(),
                    valid_from: 0,
                    valid_to: None,
                    recorded_at: 1,
                    supersedes_id: None,
                    schema_version: 1,
                },
            ))
            .unwrap();
        }
    }
    /// Append one **injected**-manifest audit row: the push tier's spend
    /// (hook-and-skill serving), which has its own verb and — unlike every pull-side
    /// serve — no `serve_call_event` sibling, because nobody called anything.
    fn seed_injection_row(root: &Path, path: &str, id: &str, t: i64, returned: i64) {
        let repo = recall_repo_id(Path::new(path)).unwrap();
        let db = recall_db_under(root, &repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        let rt = runtime().unwrap();
        rt.block_on(store.insert(
            "memd_audit_event",
            &[
                "audit_id",
                "verb",
                "handle",
                "reason",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("memd.manifest_injected"),
                Value::Null,
                Value::Null,
                Value::Int(returned),
                Value::Int(0),
                Value::Int(t),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }
    #[test]
    fn recall_absent_db_is_disabled_present_empty_is_quiet() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        // No recall.db → Disabled (the enable prompt), never a zero line.
        let update = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime);
        let FeedPayload::Recall { savings, calls } = update.payload else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Disabled);
        assert_eq!(calls, RecallView::Disabled);

        // recall.db exists but has no served events → Quiet (visibly distinct).
        seed_recall_db(root.path(), &proj, &[]);
        let update = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime);
        let FeedPayload::Recall { savings, calls, .. } = update.payload else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Quiet);
        assert_eq!(calls, RecallView::Quiet);
    }
    /// The card's savings share is the shared computation's own figure, over
    /// the two stores this feed resolves — not a second arithmetic that could
    /// drift from the terminal's.
    #[test]
    fn the_card_publishes_the_shared_savings_share() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        seed_recall_db(root.path(), &proj, &[(1_000, 600), (2_000, 400)]);
        seed_one_read(root.path(), &proj, "claude-code", 9_000);

        let update = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime);
        let FeedPayload::Recall { savings, .. } = update.payload else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        // The fixture's second event is a pull: its 400 avoided tokens are
        // claimed beside the share, and only the read-channel 600 are in it.
        let expected = rift_recall_ledger::savings::share(600, 9_000);
        assert_eq!(sd.share.overall, expected);
        assert_eq!(sd.share.avoided_tokens, 600);
        assert_eq!((sd.share.claimed_pulls, sd.share.claimed_avoided), (1, 400));
        assert_eq!(sd.share.read_tokens, 9_000);
        assert_eq!(sd.share.sessions_with_reads, 1);
        assert!(
            sd.share.families.iter().any(|f| f.family == "claude-code"),
            "the family that read has a row: {:?}",
            sd.share.families
        );
    }

    #[test]
    fn recall_sums_saved_tokens_and_counts_calls() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        seed_recall_db(
            root.path(),
            &proj,
            &[(1_000, 500), (2_000, 1_500), (3_000, 0)],
        );
        seed_one_read(root.path(), &proj, "claude-code", 9_000);
        let update = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime);
        let FeedPayload::Recall { savings, calls } = update.payload else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        // The 1,500 of the second event were a pull's, beside the share and
        // out of every figure the card draws from it.
        assert_eq!(sd.share.avoided_tokens, 500);
        assert_eq!(
            sd.by_worktree.family_only, 500,
            "the axis decomposes the same total"
        );
        let RecallView::Data(cd) = calls else {
            panic!("expected calls data");
        };
        assert_eq!(cd.total, 3, "one attributed call per served event");
        assert_eq!(cd.series.len(), 2);
        assert!(cd.series.iter().all(|s| s.rate.len() == 30));
    }

    /// Rung 1 (the priced savings estimate): a fully priced mix composes the headline — the
    /// rows' tokens sum to exactly the avoided total, the priced subtotals
    /// sum to exactly the headline, and every label carries its marker.
    #[test]
    fn usd_full_mix_composes_the_headline() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        // The observed mix: 3M input tokens on opus, 1M on fable (input =
        // uncached + cached; output never weighs the split).
        seed_model_fact(
            &store,
            &rt,
            "c1",
            "claude-opus-4-8",
            1_000,
            (2_000_000, 1_000_000, 0, 999),
        );
        seed_model_fact(
            &store,
            &rt,
            "c2",
            "claude-fable-5",
            2_000,
            (600_000, 400_000, 0, 999),
        );
        seed_price(&store, &rt, "claude-opus-4-8", 5_000_000);
        seed_price(&store, &rt, "claude-fable-5", 10_000_000);
        // One read-channel event: the fixture's odd events are pulls, which
        // no dollar figure prices.
        seed_recall_db(root.path(), &proj, &[(1_000, 1_000_000)]);

        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        // 1M avoided split 3:1 → 750k @ $5/MTok + 250k @ $10/MTok = $6.25.
        assert_eq!(sd.usd_saved_micros, Some(6_250_000));
        assert_eq!(sd.usd_label.as_deref(), Some("~$6.25"));
        assert_eq!(sd.usd_rows.len(), 2);
        assert_eq!(sd.usd_rows.iter().map(|r| r.tokens).sum::<u64>(), 1_000_000);
        let priced: i64 = sd.usd_rows.iter().filter_map(|r| r.subtotal_micros).sum();
        assert_eq!(priced, 6_250_000, "subtotals compose the headline exactly");
        // Deterministic order: heaviest share first.
        assert_eq!(sd.usd_rows[0].model, "claude-opus-4-8");
        assert_eq!(sd.usd_rows[0].tokens, 750_000);
        assert_eq!(sd.usd_rows[0].rate_label.as_deref(), Some("$5.00/MTok"));
        assert_eq!(sd.usd_rows[0].subtotal_label.as_deref(), Some("~$3.75"));
        assert_eq!(sd.usd_rows[1].model, "claude-fable-5");
        assert_eq!(sd.usd_rows[1].subtotal_label.as_deref(), Some("~$2.50"));
    }

    /// The read side is valued through the **same mix and the same rates** as
    /// the avoided side — one basis read twice, never a second price table.
    ///
    /// The check is a ratio: the two bars are comparable in dollars only if
    /// their token counts went through one partition at one moment, so the
    /// dollar ratio has to equal the token ratio for a fully-priced mix.
    #[test]
    fn the_read_side_is_priced_through_the_same_mix_as_the_avoided_side() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "priced");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_model_fact(
            &store,
            &rt,
            "c1",
            "claude-opus-4-8",
            1_000,
            (1_000_000, 0, 0, 0),
        );
        seed_price(&store, &rt, "claude-opus-4-8", 5_000_000);
        seed_recall_db(root.path(), &proj, &[(1_000, 1_000_000)]);

        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        // 1M avoided at $5/MTok.
        assert_eq!(sd.usd_saved_micros, Some(5_000_000));
        // This fixture captures no reads, so the read side has no tokens to
        // value and honestly carries `0` rather than a figure.
        assert_eq!(sd.share.read_tokens, 0);
        assert_eq!(sd.usd_read_micros, Some(0));
        assert_eq!(sd.usd_read_label.as_deref(), Some("~$0.00"));
        // The label is the money authority's, never this crate's: no surface
        // here formats a currency.
        assert_eq!(
            sd.usd_read_label,
            sd.usd_read_micros.map(rift_shadow_cost::fmt_usd_estimate)
        );
    }

    /// Rung 2: an unpriced model keeps its token slice, contributes no
    /// dollars, and the headline honestly covers only the priced share.
    #[test]
    fn usd_partial_coverage_prices_what_it_can() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_model_fact(
            &store,
            &rt,
            "c1",
            "claude-opus-4-8",
            1_000,
            (1_000_000, 0, 0, 0),
        );
        seed_model_fact(
            &store,
            &rt,
            "c2",
            "mystery-model",
            2_000,
            (1_000_000, 0, 0, 0),
        );
        seed_price(&store, &rt, "claude-opus-4-8", 5_000_000);
        // mystery-model: no seeded price.
        seed_recall_db(root.path(), &proj, &[(1_000, 1_000_000)]);

        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        // Even split (1M:1M): only opus's 500k slice is priced → $2.50.
        assert_eq!(sd.usd_saved_micros, Some(2_500_000));
        let unpriced: Vec<_> = sd
            .usd_rows
            .iter()
            .filter(|r| r.subtotal_micros.is_none())
            .collect();
        assert_eq!(unpriced.len(), 1);
        assert_eq!(unpriced[0].model, "mystery-model");
        assert_eq!(unpriced[0].tokens, 500_000, "the slice is kept, undollared");
        assert_eq!(unpriced[0].rate_label, None);
        assert_eq!(unpriced[0].subtotal_label, None);
    }

    /// Rung 3: no input-token mix at all — the most-frequent captured model
    /// carries the whole total as a single disclosed reference row.
    #[test]
    fn usd_no_mix_falls_back_to_most_frequent_model() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        // model_call rows without token_usage: calls observed, no mix.
        for (id, model) in [
            ("c1", "claude-opus-4-8"),
            ("c2", "claude-opus-4-8"),
            ("c3", "claude-fable-5"),
        ] {
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
                    Value::text(id),
                    Value::text("t1"),
                    Value::text(model),
                    Value::Int(1_000),
                    Value::Int(1),
                ],
            ))
            .unwrap();
        }
        seed_price(&store, &rt, "claude-opus-4-8", 5_000_000);
        seed_recall_db(root.path(), &proj, &[(1_000, 2_000_000)]);

        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        assert_eq!(sd.usd_rows.len(), 1, "one reference row");
        assert_eq!(sd.usd_rows[0].model, "claude-opus-4-8", "most frequent");
        assert_eq!(sd.usd_rows[0].tokens, 2_000_000, "carries the whole total");
        assert_eq!(sd.usd_saved_micros, Some(10_000_000)); // 2M @ $5/MTok
    }

    /// Rung 4: honest absence — no capture projection, or nothing priced,
    /// yields no figure at all while the token headline stands untouched.
    /// Never `Some(0)` for a real saving.
    #[test]
    fn usd_absent_when_nothing_is_priced_or_no_capture() {
        // (a) recall data exists but there is no capture projection at all.
        let root = tempfile::tempdir().unwrap();
        let proj_dir = root.path().join("bare");
        std::fs::create_dir_all(&proj_dir).unwrap();
        // A repository with no projection — the state under test. Without the
        // gitdir there would be no family either, and the panel would be
        // reporting the absence of an identity rather than of a projection.
        rift_git::test_support::fake_repo(&proj_dir).unwrap();
        let proj = proj_dir.to_string_lossy().into_owned();
        seed_recall_db(root.path(), &proj, &[(1_000, 500)]);
        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        assert_eq!(
            sd.share,
            Box::default(),
            "no capture projection: the share is unanswerable, never a 100 % \
             over a denominator nobody observed"
        );
        assert_eq!(sd.usd_saved_micros, None);
        assert_eq!(sd.usd_label, None);
        assert!(sd.usd_rows.is_empty());

        // (b) a mix exists but no model has a seeded price → still None.
        let proj2 = make_project(root.path(), "unpriced");
        let store = open_proj(root.path(), &proj2);
        let rt = runtime().unwrap();
        seed_model_fact(
            &store,
            &rt,
            "c1",
            "mystery-model",
            1_000,
            (1_000_000, 0, 0, 0),
        );
        seed_recall_db(root.path(), &proj2, &[(1_000, 500)]);
        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj2, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        assert_eq!(sd.usd_saved_micros, None, "never a fabricated ~$0.00");
        assert!(sd.usd_rows.is_empty());
    }

    /// The money multiplies the NET avoided figure (the serve accounting seam) — the same
    /// value the percentage uses — never gross (net + returned).
    #[test]
    fn usd_values_net_avoided_not_gross() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_model_fact(
            &store,
            &rt,
            "c1",
            "claude-opus-4-8",
            1_000,
            (1_000_000, 0, 0, 0),
        );
        seed_price(&store, &rt, "claude-opus-4-8", 5_000_000);
        // Two read-channel serves, each returned=10 (the fixture's fixed
        // cost), either side of a pull that avoided nothing: net avoided is
        // 1_000_000; gross would be 1_000_020.
        seed_recall_db(
            root.path(),
            &proj,
            &[(1_000, 600_000), (2_000, 0), (3_000, 400_000)],
        );

        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        // net 1M @ $5/MTok = $5.00 exactly; gross would price 20 more tokens.
        assert_eq!(sd.usd_saved_micros, Some(5_000_000));
    }

    /// The model mix is the **window's own**.
    ///
    /// The avoided tokens a period holds are partitioned across the models
    /// that period ran, not across every model the project ever ran: a mix
    /// taken over all history against a window's tokens attributes this
    /// week's savings to a model the project stopped using in the spring, and
    /// the modal then discloses a basis that was never this window's.
    #[test]
    fn the_model_mix_is_cut_where_the_tokens_it_prices_are() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "mixed");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        let day = 24 * 60 * 60 * 1000;
        // An old model, long out of use, and a current one.
        seed_model_fact(
            &store,
            &rt,
            "old",
            "claude-opus-4-8",
            USD_NOW - 10 * day,
            (3_000_000, 0, 0, 0),
        );
        seed_model_fact(
            &store,
            &rt,
            "new",
            "claude-fable-5",
            USD_NOW - 60_000,
            (1_000_000, 0, 0, 0),
        );
        seed_price(&store, &rt, "claude-opus-4-8", 5_000_000);
        seed_price(&store, &rt, "claude-fable-5", 10_000_000);
        seed_recall_db(root.path(), &proj, &[(USD_NOW - 60_000, 1_000_000)]);

        let models = |window: RecallWindow| -> Vec<String> {
            let FeedPayload::Recall { savings, .. } =
                compute_recall(root.path(), &proj, USD_NOW, window).payload
            else {
                panic!("expected Recall");
            };
            let RecallView::Data(sd) = savings else {
                panic!("expected savings data for {window:?}");
            };
            sd.usd_rows.iter().map(|r| r.model.clone()).collect()
        };
        assert_eq!(
            models(RecallWindow::Today),
            vec!["claude-fable-5".to_owned()],
            "only the model this window actually ran"
        );
        assert_eq!(
            models(RecallWindow::AllTime).len(),
            2,
            "the whole record is still the whole mix"
        );
    }

    /// The pure ladder: no basis and all-unpriced are both absence, and a
    /// partially priced mix reports exactly its priced sum.
    #[test]
    fn usd_breakdown_ladder_edges() {
        // No shares at all → no basis.
        assert!(usd_breakdown(1_000, &[]).is_none());
        // Shares exist but nothing is priced → absence, never Some(0).
        let unpriced = [
            MixShare {
                model: "a".into(),
                weight: 1,
                price_per_mtok: None,
            },
            MixShare {
                model: "b".into(),
                weight: 1,
                price_per_mtok: None,
            },
        ];
        assert!(usd_breakdown(1_000, &unpriced).is_none());
        // A negative price is nonsense input, treated as unpriced — and so is
        // a price of exactly ZERO. A seeded `0` used to value a real saving at
        // `~$0.00`, which reads as a measured nothing rather than as the
        // missing price it is; the ladder's bottom rung is "no chip at all".
        for nonsense in [-5, 0] {
            let mix = [MixShare {
                model: "a".into(),
                weight: 1,
                price_per_mtok: Some(nonsense),
            }];
            assert!(
                usd_breakdown(1_000, &mix).is_none(),
                "a rate of {nonsense} is not a price"
            );
        }
        // Zero avoided with a priced share is a real zero: "~$0.00" is honest.
        let priced = [MixShare {
            model: "a".into(),
            weight: 1,
            price_per_mtok: Some(5_000_000),
        }];
        let (total, rows) = usd_breakdown(0, &priced).unwrap();
        assert_eq!(total, 0);
        assert_eq!(rows[0].tokens, 0);
    }
    #[test]
    fn recall_reads_the_writers_recall_repo_id_not_the_capture_id() {
        // Regression: the serve/audit writer (`rift hooks memd` / `rift-memd`)
        // keys recall.db by `recall_repo_id`; the panel used the capture
        // `repo_id_for` and opened a different, empty recall.db — stranding it
        // on "no serves yet" even after real serves landed.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");

        // The two id spaces genuinely differ for the same path.
        let cap_id = repo_id_for(Path::new(&proj)).unwrap();
        let recall_id = recall_repo_id(Path::new(&proj)).unwrap();
        assert_ne!(
            cap_id.as_str(),
            recall_id.as_str(),
            "capture and recall repo-ids must differ, else the test proves nothing"
        );

        // A decoy EMPTY recall.db under the CAPTURE id: reading it would yield
        // Quiet. The reader must ignore it.
        let decoy = recall_db_under(root.path(), &cap_id);
        std::fs::create_dir_all(decoy.parent().unwrap()).unwrap();
        rift_store::open_migrated(&decoy, &rift_store::recall_migrations()).unwrap();

        // Real serves land under the recall id.
        seed_recall_db(
            root.path(),
            &proj,
            &[(1_000, 500), (2_000, 0), (3_000, 700)],
        );
        seed_one_read(root.path(), &proj, "claude-code", 9_000);

        let FeedPayload::Recall { savings, calls, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        // If the reader used the capture id it would find the empty decoy and
        // report Quiet; matching the writer's id, it reports the served data.
        let RecallView::Data(sd) = savings else {
            panic!("must read the writer's recall.db, not the capture-id decoy");
        };
        assert_eq!(sd.share.avoided_tokens, 1_200);
        assert!(matches!(calls, RecallView::Data(_)));
    }
    #[test]
    fn recall_is_scope_filtered_across_projects() {
        let root = tempfile::tempdir().unwrap();
        let pa = make_project(root.path(), "a");
        let pb = make_project(root.path(), "b");
        seed_recall_db(root.path(), &pa, &[(1_000, 700)]);
        seed_one_read(root.path(), &pa, "claude-code", 9_000);
        // b has NO recall.db: a's events must not leak into b's sections.
        let ub = compute_recall(root.path(), &pb, USD_NOW, RecallWindow::AllTime);
        let FeedPayload::Recall { savings, .. } = ub.payload else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Disabled);
        let ua = compute_recall(root.path(), &pa, USD_NOW, RecallWindow::AllTime);
        let FeedPayload::Recall {
            savings: RecallView::Data(sd),
            ..
        } = ua.payload
        else {
            panic!("expected a's data");
        };
        assert_eq!(sd.share.avoided_tokens, 700);
    }
    /// The card's headline is the **shared share** and nothing of its own.
    ///
    /// It used to be a second ratio — `avoided ÷ (avoided + returned)` over
    /// the audit trail — computed here, with a different denominator from the
    /// one `rift savings` publishes and the same shape on screen. There is one
    /// figure now, and this test pins it to the ledger's own function over the
    /// same two sides.
    #[test]
    fn the_headline_is_the_shared_share_and_never_a_second_ratio() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        // seed_recall_db fixes tokens_returned = 10 per event, and its odd
        // events are pulls, here booking nothing: their 20 returned tokens
        // are the claimed line's, not the honesty figure's.
        let events = [
            (1_000, 90),
            (2_000, 0),
            (3_000, 40),
            (4_000, 0),
            (5_000, 20),
        ];
        seed_recall_db(root.path(), &proj, &events);
        seed_one_read(root.path(), &proj, "claude-code", 850);
        let FeedPayload::Recall {
            savings: RecallView::Data(sd),
            ..
        } = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected savings data");
        };
        let avoided = 90 + 40 + 20; // 150
        assert_eq!(sd.share.avoided_tokens, avoided);
        assert_eq!(sd.share.read_tokens, 850);
        assert_eq!(
            sd.share.overall,
            rift_recall_ledger::savings::share(avoided, 850),
            "the headline is the ledger's share, denominator and all"
        );
        // …and the honesty figure carries what the serves cost: 150 − 30.
        assert_eq!(sd.share.honesty, 120);
    }

    /// The verb split must not make the panel lie in the other direction.
    ///
    /// The savings feed reads `memd_audit_event` with no verb predicate, so a
    /// newly minted verb is neither dropped nor bucketed as credit — it lands
    /// in `Σreturned`, which is what the **honesty** figure charges it
    /// against, exactly as a push-tier cost should. This test is the pin for
    /// that: without it, adding a verb predicate here (or filtering unknown
    /// verbs out "defensively") would silently delete injected spend from the
    /// card and leave the avoided total looking free.
    #[test]
    fn an_injected_manifest_is_read_as_spend_and_never_as_credit() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        // One pull-side serve: 90 avoided, 10 returned (seed_recall_db fixes 10).
        seed_recall_db(root.path(), &proj, &[(1_000, 90)]);
        seed_one_read(root.path(), &proj, "claude-code", 910);
        let FeedPayload::Recall {
            savings: RecallView::Data(before),
            ..
        } = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected savings data");
        };

        // Then one injected manifest: 40 tokens of spend, no credit, no call row.
        seed_injection_row(root.path(), &proj, "inj-1", 2_000, 40);
        let FeedPayload::Recall {
            savings: RecallView::Data(after),
            ..
        } = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected savings data");
        };

        // Credit is unchanged: an injection avoided nothing, so neither the
        // headline share nor the token figure under it moves.
        assert_eq!(
            after.share.avoided_tokens, before.share.avoided_tokens,
            "an injection must add no credit"
        );
        assert_eq!(after.share.overall, before.share.overall);
        // Spend is charged where spend belongs: 90 avoided − (10 + 40)
        // returned, down by exactly the injection.
        assert_eq!(before.share.honesty, 80);
        assert_eq!(
            after.share.honesty, 40,
            "unrequested spend must be charged, not vanish"
        );
    }

    /// Nothing avoided over a window that read is **0 %**, not absence and
    /// never a NaN: the question was asked and the answer is zero.
    #[test]
    fn a_window_that_avoided_nothing_reads_zero_rather_than_absent() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        seed_recall_db(root.path(), &proj, &[(1_000, 0), (2_000, 0)]);
        seed_one_read(root.path(), &proj, "claude-code", 4_000);
        let FeedPayload::Recall {
            savings: RecallView::Data(sd),
            ..
        } = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected savings data");
        };
        assert_eq!(sd.share.overall, Some(0.0));
        assert_eq!(sd.share.avoided_tokens, 0);
    }
}
