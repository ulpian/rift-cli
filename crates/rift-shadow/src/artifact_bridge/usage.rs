//! The settled usage a shard froze into `usage.jsonl`, read back into the
//! capture projection: the session, the turn, the model call under its frozen
//! key, the token usage and the cost fact priced from the catalog.
//!
//! A module of its own because the artifact has a grammar of its own: every
//! member is declared, required and typed, and a line that strays from it is
//! skipped and named rather than guessed at. The live settle is the writer the
//! rows must agree with, so every row is built the way the settle builds it
//! and every id is minted by the function the settle mints it with — except
//! the call key, which the line carries because the event ordinal it was
//! hashed from is stored nowhere else.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_recall_substrate::parse::{
    IssueKind, Locator, Outcome, ParseIssue, MAX_ARTIFACT_BYTES, MAX_JSONL_LINE_BYTES,
};
use rift_recall_substrate::SubstrateReader;
use rift_shadow_cost::{call_cost_micros, ClassPrices, TokenUsage};
use rift_store::{
    effective_prices, record_capture_session_identity, record_cost_if_changed, record_model_call,
    record_session, record_token_usage, record_turn, CanonicalStore, CaptureSessionIdentityRow,
    ModelCallRow, SessionRow, TokenUsageRow, TurnRow,
};
use serde_json::{Map, Value};

use crate::backend::USAGE_SCHEMA_VERSION;
use crate::ids;

use super::report::{note, BridgeReport};
use super::IngestCtx;

/// The artifact this module reads.
const ARTIFACT: &str = "usage.jsonl";

/// Every member a line declares; a line carrying any other is refused, so a
/// field this reader does not understand can never ride into a row.
const MEMBERS: [&str; 11] = [
    "at",
    "cache_creation",
    "cached_input",
    "call_id",
    "method",
    "model",
    "output",
    "schema_version",
    "session",
    "turn",
    "uncached_input",
];

/// What the writer freezes a member as when its value failed the identifier
/// grammar. Spelled again here because the writer keeps its constant private;
/// `a_withheld_member_the_writer_froze_is_refused` feeds this reader the
/// writer's own bytes, so the two spellings cannot drift apart unnoticed.
const WITHHELD: &str = "withheld";

/// The row schema the live projector stamps on every row it writes from a
/// canonical event; the rebuilt rows carry the same so the two compare equal.
const ROW_SCHEMA_VERSION: i64 = 1;

/// One settled model call, as a `usage.jsonl` line declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UsageLine {
    pub(super) session: String,
    pub(super) turn: u32,
    pub(super) call_id: String,
    pub(super) model: String,
    pub(super) at: i64,
    pub(super) uncached_input: i64,
    pub(super) cached_input: i64,
    pub(super) cache_creation: i64,
    pub(super) output: i64,
    pub(super) method: String,
}

/// Where the rows of one session and one turn start: the earliest line the
/// shard carries for each, so a session or turn the projection has not seen
/// is dated by its first recorded call rather than by the line walked first.
#[derive(Default)]
struct Starts<'a> {
    sessions: BTreeMap<&'a str, &'a UsageLine>,
    turns: BTreeMap<(&'a str, u32), i64>,
}

impl<'a> Starts<'a> {
    fn of(lines: &'a [UsageLine]) -> Self {
        let mut starts = Self::default();
        for line in lines {
            let first = starts.sessions.entry(&line.session).or_insert(line);
            if (line.at, &line.call_id) < (first.at, &first.call_id) {
                *first = line;
            }
            let turn = starts
                .turns
                .entry((&line.session, line.turn))
                .or_insert(line.at);
            *turn = (*turn).min(line.at);
        }
        starts
    }
}

/// Project the shard's `usage.jsonl`, when it has one: a shard written before
/// the artifact existed has none and projects exactly what it did before.
///
/// A line the parser refused is counted and named in the report and the walk
/// goes on; the rows of every other line still land.
pub(super) async fn project_usage_lines<S: CanonicalStore, R: SubstrateReader>(
    ctx: &IngestCtx<'_, S, R>,
    report: &mut BridgeReport,
    id: &str,
) -> Result<()> {
    let Some(bytes) = ctx.sub.read_artifact(id, ARTIFACT)? else {
        return Ok(());
    };
    let parsed = parse_usage(&bytes, id);
    report.parse_issues += parsed.issues.len();
    for issue in &parsed.issues {
        note(report, issue_note(issue));
    }
    let starts = Starts::of(&parsed.value);
    for line in &parsed.value {
        project_line(ctx, &starts, line).await?;
    }
    Ok(())
}

/// Parse `usage.jsonl`: one [`UsageLine`] per line that declares every member,
/// no other, each with its type. A refused line is one [`ParseIssue`] naming
/// the shard and the line number; the detail names the member or the rule,
/// never a byte of the line.
pub(super) fn parse_usage(bytes: &[u8], checkpoint_id: &str) -> Outcome<Vec<UsageLine>> {
    let issue = |locator: Locator, kind: IssueKind, detail: String| ParseIssue {
        checkpoint_id: checkpoint_id.to_owned(),
        artifact: ARTIFACT,
        locator,
        kind,
        detail,
    };
    if bytes.len() > MAX_ARTIFACT_BYTES {
        let detail = format!("{} bytes over cap", bytes.len());
        return Outcome {
            value: Vec::new(),
            issues: vec![issue(
                Locator::WholeFile,
                IssueKind::OversizeSkipped,
                detail,
            )],
        };
    }
    let mut value = Vec::new();
    let mut issues = Vec::new();
    for (n, line) in bytes.split(|&b| b == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let locator = Locator::Line(n + 1);
        if line.len() > MAX_JSONL_LINE_BYTES {
            let detail = format!("{} bytes over cap", line.len());
            issues.push(issue(locator, IssueKind::OversizeSkipped, detail));
            continue;
        }
        match usage_line(line) {
            Ok(parsed) => value.push(parsed),
            Err(detail) => issues.push(issue(locator, IssueKind::Corrupt, detail)),
        }
    }
    Outcome { value, issues }
}

/// One line under the declared grammar, or the reason it is refused.
fn usage_line(line: &[u8]) -> std::result::Result<UsageLine, String> {
    let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(line) else {
        return Err("not a JSON object".to_owned());
    };
    if obj.keys().any(|key| !MEMBERS.contains(&key.as_str())) {
        return Err("carries an undeclared member".to_owned());
    }
    if int_member(&obj, "schema_version")? != i64::from(USAGE_SCHEMA_VERSION) {
        return Err("declares a schema_version this reader does not know".to_owned());
    }
    let turn = int_member(&obj, "turn")?;
    let Ok(turn) = u32::try_from(turn) else {
        return Err("member `turn` is not a turn ordinal".to_owned());
    };
    Ok(UsageLine {
        session: string_member(&obj, "session")?,
        turn,
        call_id: identifier_member(&obj, "call_id")?,
        model: identifier_member(&obj, "model")?,
        at: int_member(&obj, "at")?,
        uncached_input: count_member(&obj, "uncached_input")?,
        cached_input: count_member(&obj, "cached_input")?,
        cache_creation: count_member(&obj, "cache_creation")?,
        output: count_member(&obj, "output")?,
        method: identifier_member(&obj, "method")?,
    })
}

fn int_member(obj: &Map<String, Value>, name: &str) -> std::result::Result<i64, String> {
    match obj.get(name) {
        None => Err(format!("misses member `{name}`")),
        Some(value) => value
            .as_i64()
            .ok_or_else(|| format!("member `{name}` is not an integer")),
    }
}

/// A token count: the live projector clamps provider counts into `0..=i64::MAX`,
/// so a negative one was never written by it.
fn count_member(obj: &Map<String, Value>, name: &str) -> std::result::Result<i64, String> {
    let count = int_member(obj, name)?;
    if count < 0 {
        return Err(format!("member `{name}` is a negative count"));
    }
    Ok(count)
}

fn string_member(obj: &Map<String, Value>, name: &str) -> std::result::Result<String, String> {
    match obj.get(name) {
        None => Err(format!("misses member `{name}`")),
        Some(value) => value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("member `{name}` is not a string")),
    }
}

/// A member the writer screened: [`WITHHELD`] means the live value did not
/// survive the screen, and a row keyed or priced by a placeholder would be a
/// fact the live projection never held.
fn identifier_member(obj: &Map<String, Value>, name: &str) -> std::result::Result<String, String> {
    let value = string_member(obj, name)?;
    if value == WITHHELD {
        return Err(format!("member `{name}` was withheld"));
    }
    Ok(value)
}

/// The report line for one refused line: the shard, the artifact, where, why.
fn issue_note(issue: &ParseIssue) -> String {
    let at = match issue.locator {
        Locator::WholeFile => "the whole file".to_owned(),
        Locator::Line(n) => format!("line {n}"),
    };
    format!(
        "{}: {} {at} skipped: {}",
        issue.checkpoint_id, issue.artifact, issue.detail
    )
}

/// The rows one line stands for, in the order the live path writes them: the
/// session and its identity, the turn, the call, its usage, then its cost.
/// Every write is insert-or-ignore on the live key, so a row the live path
/// already wrote wins and a replay writes nothing.
async fn project_line<S: CanonicalStore, R>(
    ctx: &IngestCtx<'_, S, R>,
    starts: &Starts<'_>,
    line: &UsageLine,
) -> Result<()> {
    let session_row = ids::session_row_id(ctx.default_agent, &line.session);
    record_session_rows(ctx, starts, line, &session_row).await?;
    let turn_id = ids::turn_row_id(&session_row, line.turn);
    let turn_start = starts
        .turns
        .get(&(line.session.as_str(), line.turn))
        .copied()
        .unwrap_or(line.at);
    record_turn(
        ctx.store,
        &TurnRow {
            turn_id: turn_id.clone(),
            session_id: session_row,
            ordinal: i64::from(line.turn),
            started_at: turn_start,
            schema_version: ROW_SCHEMA_VERSION,
        },
    )
    .await?;
    record_call_rows(ctx, line, turn_id).await?;
    record_line_cost(ctx, line).await
}

/// The session row and its native identity, as the live projector writes the
/// pair. The row is dated by the session's earliest call and names that
/// call's model; a session the live path already recorded keeps its own.
async fn record_session_rows<S: CanonicalStore, R>(
    ctx: &IngestCtx<'_, S, R>,
    starts: &Starts<'_>,
    line: &UsageLine,
    session_row: &str,
) -> Result<()> {
    let first = starts
        .sessions
        .get(line.session.as_str())
        .copied()
        .unwrap_or(line);
    record_session(
        ctx.store,
        &SessionRow {
            session_id: session_row.to_owned(),
            agent: ctx.default_agent.to_owned(),
            model_primary: first.model.clone(),
            repo_id: ctx.repo_id.to_owned(),
            started_at: first.at,
            schema_version: ROW_SCHEMA_VERSION,
        },
    )
    .await?;
    record_capture_session_identity(
        ctx.store,
        &CaptureSessionIdentityRow {
            session_id: session_row.to_owned(),
            agent: ctx.default_agent.to_owned(),
            native_session_id: line.session.clone(),
            schema_version: ROW_SCHEMA_VERSION,
        },
    )
    .await?;
    Ok(())
}

/// The model call under its frozen key, and its token usage with the frozen
/// classes and counting method.
async fn record_call_rows<S: CanonicalStore, R>(
    ctx: &IngestCtx<'_, S, R>,
    line: &UsageLine,
    turn_id: String,
) -> Result<()> {
    record_model_call(
        ctx.store,
        &ModelCallRow {
            call_id: line.call_id.clone(),
            turn_id,
            model: line.model.clone(),
            started_at: line.at,
            schema_version: ROW_SCHEMA_VERSION,
        },
    )
    .await?;
    record_token_usage(
        ctx.store,
        &TokenUsageRow {
            call_id: line.call_id.clone(),
            uncached_input: line.uncached_input,
            cached_input: line.cached_input,
            cache_creation: line.cache_creation,
            output: line.output,
            counter_method: line.method.clone(),
            schema_version: ROW_SCHEMA_VERSION,
        },
    )
    .await?;
    Ok(())
}

/// The call's cost, priced from the catalog as of the rebuild's clock — the
/// shard carries no dollar figure, because the catalog moves independently of
/// usage. Mirrors the live settle's rule in `project.rs`, which keeps it
/// private: nothing without all four class prices, nothing when the current
/// cost already equals this one, and the fact's id and transaction time taken
/// from the clock the live path takes them from (there the settle's, here the
/// rebuild's).
async fn record_line_cost<S: CanonicalStore, R>(
    ctx: &IngestCtx<'_, S, R>,
    line: &UsageLine,
) -> Result<()> {
    let Some(prices) = effective_prices(ctx.store, &line.model, line.at, ctx.now_ms).await? else {
        return Ok(());
    };
    let count = |n: i64| u64::try_from(n).unwrap_or(0);
    let usage = TokenUsage {
        uncached_input: count(line.uncached_input),
        cached_input: count(line.cached_input),
        cache_creation: count(line.cache_creation),
        output: count(line.output),
    };
    let class_prices = ClassPrices {
        uncached_input_per_mtok: prices.uncached_input,
        cached_input_per_mtok: prices.cached_input,
        cache_write_per_mtok: prices.cache_creation,
        output_per_mtok: prices.output,
    };
    let amount = call_cost_micros(&usage, &class_prices, 0);
    record_cost_if_changed(ctx.store, &line.call_id, amount, line.at, ctx.now_ms).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{usage_jsonl_bytes, ArchivedUsageFact};

    const SHARD: &str = "shard-under-test";

    /// A token no refusal detail may echo: it stands in for the line's bytes.
    const MARKER: &str = "zzmarkerzz";

    fn fixture_bytes() -> &'static [u8] {
        include_bytes!("../../shard-goldens/every_artifact/artifacts/usage.jsonl")
    }

    fn fact(model: &str, call_id: &str) -> ArchivedUsageFact {
        ArchivedUsageFact {
            session: "native-1".to_owned(),
            turn: 1,
            call_id: call_id.to_owned(),
            model: model.to_owned(),
            at: 1_000,
            uncached_input: 20,
            cached_input: 30,
            cache_creation: 0,
            output: 2,
            method: "provider_reported".to_owned(),
        }
    }

    /// The fixture's line 1 with `edit` applied to its object.
    fn edited_line(edit: impl FnOnce(&mut Map<String, Value>)) -> Vec<u8> {
        let first = fixture_bytes().split(|&b| b == b'\n').next().unwrap();
        let mut obj: Map<String, Value> = serde_json::from_slice(first).unwrap();
        edit(&mut obj);
        serde_json::to_vec(&obj).unwrap()
    }

    fn refused(bytes: &[u8]) -> ParseIssue {
        let out = parse_usage(bytes, SHARD);
        assert!(out.value.is_empty(), "the line is skipped");
        assert_eq!(out.issues.len(), 1, "{:?}", out.issues);
        let issue = out.issues.into_iter().next().unwrap();
        assert_eq!(issue.checkpoint_id, SHARD, "the issue names the shard");
        assert_eq!(issue.artifact, ARTIFACT);
        assert!(
            !issue.detail.contains(MARKER) && !issue_note(&issue).contains(MARKER),
            "the refusal never echoes the line: {}",
            issue.detail
        );
        issue
    }

    async fn shard_reversion_prices(store: &rift_store::SqliteStore, model: &str, now: i64) {
        for (class, rate) in [
            ("uncached_input", 3_000_000),
            ("cached_input", 300_000),
            ("cache_creation", 3_750_000),
            ("output", 15_000_000),
        ] {
            assert!(
                rift_store::record_price(
                    store,
                    &rift_store::PriceTableRow {
                        price_id: format!("zz-revert-{class}"),
                        model: model.into(),
                        token_class: class.into(),
                        unit_price_micros: rate,
                        currency: "USD".into(),
                        valid_from: now,
                        valid_to: None,
                        recorded_at: now,
                        supersedes_id: None,
                        schema_version: 1,
                    },
                )
                .await
                .unwrap(),
                "reversion catalog row must be inserted"
            );
        }
    }

    #[tokio::test]
    async fn same_clock_shard_correction_retry_and_revert_preserve_booking_history() {
        const NOW: i64 = 1_789_603_200_000;
        let store = rift_store::SqliteStore::open_in_memory().unwrap();
        let migrations = rift_store::default_migrations();
        rift_store::apply_migrations(&store, &migrations[..15]).unwrap();
        let substrate = rift_recall_substrate::MemorySubstrate::local();
        let registry = rift_shadow_hooks::AdapterRegistry::with_builtin();
        let ctx = IngestCtx {
            store: &store,
            sub: &substrate,
            repo_id: "repo",
            default_agent: "claude-code",
            now_ms: NOW,
            registry: &registry,
        };
        let line = UsageLine {
            session: "session".into(),
            turn: 1,
            call_id: "same-instant-call".into(),
            model: "claude-sonnet-5".into(),
            at: NOW,
            uncached_input: 1000,
            cached_input: 0,
            cache_creation: 0,
            output: 0,
            method: "provider_reported".into(),
        };
        record_line_cost(&ctx, &line).await.unwrap();
        let old = rift_store::latest_booked_cost_at(&store, &line.call_id, NOW)
            .await
            .unwrap()
            .unwrap();
        rift_store::apply_migrations(&store, &migrations).unwrap();
        for _ in 0..2 {
            record_line_cost(&ctx, &line).await.unwrap();
        }
        let corrected = rift_store::latest_booked_cost_at(&store, &line.call_id, NOW)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(corrected.amount_micros, 2000);
        assert_eq!(store.count("cost_fact").await.unwrap(), 2);
        shard_reversion_prices(&store, &line.model, NOW).await;
        for _ in 0..2 {
            record_line_cost(&ctx, &line).await.unwrap();
        }
        let reverted = rift_store::latest_booked_cost_at(&store, &line.call_id, NOW)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reverted.amount_micros, 3000);
        assert_ne!(reverted.cost_fact_id, old.cost_fact_id);
        let chain = store
            .query(
                "SELECT supersedes_id FROM cost_fact WHERE cost_fact_id = ?1",
                &[rift_store::Value::text(reverted.cost_fact_id)],
            )
            .await
            .unwrap();
        assert_eq!(chain[0].get_text(0).unwrap(), corrected.cost_fact_id);
        assert_eq!(store.count("cost_fact").await.unwrap(), 3);
    }

    #[test]
    fn the_fixtures_two_lines_parse_to_their_two_calls() {
        let out = parse_usage(fixture_bytes(), SHARD);
        assert!(out.is_clean(), "{:?}", out.issues);
        let calls: Vec<_> = out
            .value
            .iter()
            .map(|l| (l.turn, l.model.as_str(), l.uncached_input, l.output))
            .collect();
        assert_eq!(
            calls,
            vec![
                (1, "claude-sonnet-4-5", 1_200, 90),
                (2, "claude-opus-4-1", 2_400, 180)
            ]
        );
        let first = &out.value[0];
        assert_eq!(first.session, "01JSHARDGOLDENA");
        assert_eq!(first.call_id, "5".repeat(64));
        assert_eq!(first.at, 1_755_999_992_500);
        assert_eq!((first.cached_input, first.cache_creation), (30_000, 512));
        assert_eq!(first.method, "provider_reported");
    }

    #[test]
    fn a_line_with_an_undeclared_member_is_refused_without_naming_it() {
        let line = edited_line(|o| {
            o.insert(MARKER.to_owned(), Value::from(1));
        });
        assert_eq!(refused(&line).locator, Locator::Line(1));
    }

    #[test]
    fn a_withheld_member_the_writer_froze_is_refused() {
        let typed_text = format!("{MARKER} said something");
        for fact in [
            fact(&typed_text, "call-1"),
            fact("claude-fable-5", &typed_text),
        ] {
            let bytes = usage_jsonl_bytes(&[fact]).unwrap();
            assert!(refused(&bytes).detail.contains("withheld"));
        }
    }

    /// A synthetic assistant message's call, frozen by the writer under
    /// Claude Code's own `<synthetic>` model name, reads back as that call with
    /// its four token classes rather than as a withheld line.
    #[test]
    fn a_synthetic_models_call_reads_back_with_its_tokens() {
        let bytes = usage_jsonl_bytes(&[fact("<synthetic>", "call-1")]).unwrap();
        let out = parse_usage(&bytes, SHARD);
        assert!(out.is_clean(), "{:?}", out.issues);
        let calls: Vec<_> = out
            .value
            .iter()
            .map(|l| {
                let tokens = (l.uncached_input, l.cached_input, l.cache_creation, l.output);
                (l.call_id.as_str(), l.model.as_str(), tokens)
            })
            .collect();
        assert_eq!(calls, vec![("call-1", "<synthetic>", (20, 30, 0, 2))]);
    }

    #[test]
    fn a_line_missing_a_member_or_mistyping_one_is_refused() {
        let missing = edited_line(|o| {
            o.remove("model");
        });
        assert!(refused(&missing).detail.contains("`model`"));
        let mistyped = edited_line(|o| {
            o.insert("output".to_owned(), Value::from(MARKER));
        });
        assert!(refused(&mistyped).detail.contains("`output`"));
        let negative = edited_line(|o| {
            o.insert("cached_input".to_owned(), Value::from(-1));
        });
        assert!(refused(&negative).detail.contains("negative"));
        let future = edited_line(|o| {
            o.insert("schema_version".to_owned(), Value::from(2));
        });
        assert!(refused(&future).detail.contains("schema_version"));
    }

    #[test]
    fn a_malformed_line_is_refused_and_the_next_line_still_parses() {
        let mut bytes = format!("{{\"{MARKER}\": \n").into_bytes();
        bytes.extend_from_slice(fixture_bytes());
        let out = parse_usage(&bytes, SHARD);
        assert_eq!(out.value.len(), 2, "both good lines survive the bad one");
        assert_eq!(out.issues.len(), 1);
        assert_eq!(out.issues[0].locator, Locator::Line(1));
        assert!(!out.issues[0].detail.contains(MARKER));
        assert_eq!(
            issue_note(&out.issues[0]),
            "shard-under-test: usage.jsonl line 1 skipped: not a JSON object"
        );
    }
}
