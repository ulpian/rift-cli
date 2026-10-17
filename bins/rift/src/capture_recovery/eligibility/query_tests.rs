//! The point queries' plans on real migrated stores, the per-key cost formula
//! over a full pass, and the cell guards against actual SQLite typing.
use rift_store::{CanonicalStore, Row, SqliteStore, Value};

use super::fixtures::{payload, Rig};
use super::rows::{self, cell_bytes};
use super::{
    Budget, Reason, Verdict, CALLS_PER_CANDIDATE, MAX_CURRENT_CANDIDATES, ROWS_PER_CANDIDATE,
};
use crate::agent_queue::BoundaryEdge::End;

const EVENT_INDEX: &str = "idx_captured_event_session_turn_kind";
const SNAPSHOT_UNIQUE: &str = "sqlite_autoindex_turn_boundary_snapshot";

struct Planner {
    store: SqliteStore,
    rt: tokio::runtime::Runtime,
}

impl Planner {
    fn fresh() -> Self {
        let store = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&store, &rift_store::default_migrations()).unwrap();
        Self::over(store)
    }

    fn upgraded() -> Self {
        let store = SqliteStore::open_in_memory().unwrap();
        let all = rift_store::default_migrations();
        rift_store::apply_migrations(&store, &all[..all.len() - 1]).unwrap();
        rift_store::apply_migrations(&store, &all).unwrap();
        Self::over(store)
    }

    fn over(store: SqliteStore) -> Self {
        Self {
            store,
            rt: tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap(),
        }
    }

    fn plan(&self, sql: &str, params: &[Value]) -> Vec<String> {
        self.rt
            .block_on(
                self.store
                    .query(&format!("EXPLAIN QUERY PLAN {sql}"), params),
            )
            .unwrap()
            .into_iter()
            .map(|row| row.get_text(3).unwrap())
            .collect()
    }

    fn query(&self, sql: &str, params: &[Value]) -> Vec<Row> {
        self.rt.block_on(self.store.query(sql, params)).unwrap()
    }
}

fn assert_searches(plan: &[String], table: &str, index: &str) {
    assert!(
        plan.iter()
            .any(|line| line.contains("SEARCH") && line.contains(table) && line.contains(index)),
        "no SEARCH of {table} via {index}: {plan:?}"
    );
    assert!(
        plan.iter()
            .all(|line| !line.contains("SCAN") && !line.contains("TEMP B-TREE")),
        "a scan or sort: {plan:?}"
    );
}

/// Every point query with representative bound parameters, the table it
/// reads and the index its plan must name.
fn probes() -> Vec<(&'static str, Vec<Value>, &'static str, &'static str)> {
    let text = Value::text("x".repeat(64));
    let native = Value::text("native");
    let one = Value::Int(1);
    vec![
        (
            rows::IDENTITY_BY_PK,
            vec![text.clone()],
            "capture_session_identity",
            "sqlite_autoindex_capture_session_identity",
        ),
        (
            rows::SESSION_BY_PK,
            vec![text.clone()],
            "session",
            "sqlite_autoindex_session",
        ),
        (
            rows::TURN_BY_PK,
            vec![text.clone()],
            "turn",
            "sqlite_autoindex_turn",
        ),
        (
            rows::START_WITNESS,
            vec![native.clone(), one.clone()],
            "captured_event",
            EVENT_INDEX,
        ),
        (
            rows::END_WITNESS,
            vec![native.clone(), one.clone()],
            "captured_event",
            EVENT_INDEX,
        ),
        (
            rows::RECEIPT_BY_PK,
            vec![text.clone()],
            "agent_hook_event",
            "sqlite_autoindex_agent_hook_event",
        ),
        (
            rows::TARGET_BY_PK,
            vec![text.clone()],
            "turn_boundary_snapshot",
            SNAPSHOT_UNIQUE,
        ),
        (
            rows::LATER_EXISTS,
            vec![text.clone(), one.clone()],
            "turn_boundary_snapshot",
            SNAPSHOT_UNIQUE,
        ),
        (
            rows::SAME_TURN,
            vec![text.clone(), one.clone()],
            "turn_boundary_snapshot",
            SNAPSHOT_UNIQUE,
        ),
        (
            rows::OBSERVATION_EXISTS,
            vec![text.clone()],
            "turn_boundary_observation",
            "sqlite_autoindex_turn_boundary_observation",
        ),
        (
            rows::FILE_EXISTS,
            vec![text.clone()],
            "turn_boundary_file",
            "sqlite_autoindex_turn_boundary_file",
        ),
        (
            rows::FILE_KIND_EXISTS,
            vec![text.clone()],
            "turn_boundary_file_kind",
            "sqlite_autoindex_turn_boundary_file_kind",
        ),
    ]
}

#[test]
fn every_point_query_searches_its_index_on_fresh_and_upgraded_stores() {
    for planner in [Planner::fresh(), Planner::upgraded()] {
        for (sql, params, table, index) in probes() {
            assert_searches(&planner.plan(sql, &params), table, index);
            assert!(planner.query(sql, &params).is_empty(), "{sql}");
        }
    }
}

/// 128 healthy sessions, one End each: exactly the formula's eleven calls per
/// key, under the fourteen-row ceiling, with the pass's own tally equal to the
/// store's count.
#[test]
fn one_hundred_twenty_eight_healthy_candidates_cost_exactly_the_per_key_formula() {
    let rig = Rig::new();
    let hold = rig.admit();
    let jobs: Vec<_> = (0..MAX_CURRENT_CANDIDATES)
        .map(|n| {
            let native = format!("bulk-{n:03}");
            let body = payload("session_id", &native);
            rig.ingest(
                "claude-code",
                "UserPromptSubmit",
                &body,
                10 + i64::try_from(n).unwrap(),
            );
            rig.ingest("claude-code", "Stop", &body, 20 + i64::try_from(n).unwrap());
            Rig::snapshot("claude-code", &native, 1, End)
        })
        .collect();
    rig.store.reset();
    let pass = rig.pass(&hold, &jobs).unwrap();
    assert_eq!(pass.classified.len(), MAX_CURRENT_CANDIDATES);
    assert!(pass
        .classified
        .iter()
        .all(|c| matches!(c.verdict, Verdict::Eligible(_))));
    let keys = u32::try_from(MAX_CURRENT_CANDIDATES).unwrap();
    assert_eq!(rig.store.calls(), keys * CALLS_PER_CANDIDATE);
    assert_eq!(pass.calls, rig.store.calls());
    assert_eq!(pass.rows, rig.store.rows());
    assert!(pass.rows <= keys * ROWS_PER_CANDIDATE, "{}", pass.rows);
    assert_eq!(keys * CALLS_PER_CANDIDATE, 1408);
    assert_eq!(keys * ROWS_PER_CANDIDATE, 1792);
}

/// The guards against actual SQLite typing: a text, blob or real where an
/// integer is bound, an integer where text is, and text past its bound all
/// come back as the NULL sentinel, which the decoders refuse rather than
/// drop. The canonical tables are `STRICT`, so such cells cannot be seeded
/// into them; the guard expressions are proven over bound parameters.
#[test]
fn wrong_typed_or_oversized_cells_are_bounded_invalid_rows_not_hidden() {
    let planner = Planner::fresh();
    let guarded = |guard: &str, params: &[Value]| planner.query(&format!("SELECT {guard}"), params);
    let is_null = |rows: Vec<Row>| {
        matches!(rows[0].get_opt_i64(0), Ok(None)) || matches!(rows[0].get_opt_text(0), Ok(None))
    };
    for wrong in [
        Value::text("12"),
        Value::Blob(vec![1, 2]),
        Value::Null,
        Value::Int(-1),
        Value::Int(1 << 40),
    ] {
        assert!(
            is_null(guarded(rows::U32_PARAM_GUARD, std::slice::from_ref(&wrong))),
            "{wrong:?}"
        );
    }
    assert_eq!(
        guarded(rows::U32_PARAM_GUARD, &[Value::Int(7)])[0]
            .get_i64(0)
            .unwrap(),
        7
    );
    assert!(is_null(guarded(
        "CASE WHEN typeof(1.5)='integer' THEN 1 ELSE NULL END",
        &[]
    )));
    for wrong in [
        Value::Int(1),
        Value::Blob(b"abc".to_vec()),
        Value::text("x".repeat(65)),
    ] {
        assert!(
            is_null(guarded(
                rows::TEXT64_PARAM_GUARD,
                std::slice::from_ref(&wrong)
            )),
            "{wrong:?}"
        );
    }
    let bounded = guarded(rows::TEXT64_PARAM_GUARD, &[Value::text("y".repeat(64))]);
    assert_eq!(bounded[0].get_text(0).unwrap().len(), 64);
    for wrong in [Value::Int(2), Value::text("1"), Value::Null] {
        assert!(
            is_null(guarded(
                rows::SCHEMA_PARAM_GUARD,
                std::slice::from_ref(&wrong)
            )),
            "{wrong:?}"
        );
    }
}

/// The decoders: a sentinel, a wrong type or an unexpected width is one
/// invalid row, counted and classified, never an absent fact.
#[test]
fn decoders_refuse_sentinels_wrong_types_and_unexpected_widths() {
    let row = |cells: Vec<Value>| Row::new(cells);
    assert_eq!(
        rows::text(&row(vec![Value::Null]), 0, 64),
        Err(Reason::CellInvalid)
    );
    assert_eq!(
        rows::text(&row(vec![Value::Int(1)]), 0, 64),
        Err(Reason::CellInvalid)
    );
    assert_eq!(
        rows::text(&row(vec![Value::text("x".repeat(65))]), 0, 64),
        Err(Reason::CellInvalid)
    );
    assert_eq!(
        rows::text(&row(vec![Value::text("ok")]), 0, 64).as_deref(),
        Ok("ok")
    );
    assert_eq!(
        rows::ordinal(&row(vec![Value::text("1")]), 0),
        Err(Reason::CellInvalid)
    );
    assert_eq!(
        rows::ordinal(&row(vec![Value::Int(-1)]), 0),
        Err(Reason::CellInvalid)
    );
    assert_eq!(
        rows::ordinal(&row(vec![Value::Int(1 << 40)]), 0),
        Err(Reason::CellInvalid)
    );
    assert_eq!(rows::ordinal(&row(vec![Value::Int(3)]), 0), Ok(3));
    assert_eq!(
        rows::hex64(&row(vec![Value::text("G".repeat(64))]), 0),
        Err(Reason::CellInvalid)
    );
    assert_eq!(
        rows::hex64(&row(vec![Value::text("A".repeat(64))]), 0),
        Err(Reason::CellInvalid)
    );
    assert!(rows::hex64(&row(vec![Value::text("a".repeat(64))]), 0).is_ok());
    assert!(matches!(
        rows::identity(&row(vec![Value::text("a".repeat(64))])),
        Err(Reason::CellInvalid)
    ));
    assert_eq!(rows::exists(&[row(vec![Value::Int(1)])]), Ok(true));
    assert_eq!(rows::exists(&[]), Ok(false));
    assert_eq!(
        rows::exists(&[row(vec![Value::Int(1)]), row(vec![Value::Int(1)])]),
        Err(Reason::CellInvalid)
    );
    assert_eq!(
        cell_bytes(&[row(vec![
            Value::text("abc"),
            Value::Int(1),
            Value::Blob(vec![0; 5]),
            Value::Null
        ])]),
        16
    );
}

#[test]
fn the_budget_charges_actual_calls_rows_and_bytes_and_refuses_the_whole_pass_on_overflow() {
    let mut budget = Budget::default();
    let rows: Vec<Row> = (0..3)
        .map(|_| Row::new(vec![Value::text("abcd")]))
        .collect();
    budget.charge(&rows).unwrap();
    assert_eq!((budget.calls, budget.rows, budget.cells), (1, 3, 12));
    budget.charge_cache(10).unwrap();
    assert_eq!(budget.cache_bytes, 10);
    let mut calls = Budget {
        calls: super::MAX_CALLS,
        ..Budget::default()
    };
    assert_eq!(calls.charge(&[]), Err(Reason::BudgetExceeded));
    let mut many = Budget {
        rows: super::MAX_ROWS - 2,
        ..Budget::default()
    };
    assert_eq!(many.charge(&rows), Err(Reason::BudgetExceeded));
    let mut bytes = Budget {
        cells: super::MAX_CELL_BYTES - 11,
        ..Budget::default()
    };
    assert_eq!(bytes.charge(&rows), Err(Reason::BudgetExceeded));
    let mut cache = Budget {
        cache_bytes: super::MAX_CACHE_BYTES,
        ..Budget::default()
    };
    assert_eq!(cache.charge_cache(1), Err(Reason::BudgetExceeded));
    assert_eq!(Reason::BudgetExceeded.as_str(), "budget-exceeded");
    assert_eq!(Budget::preflight(MAX_CURRENT_CANDIDATES), Ok(()));
    assert_eq!(
        Budget::preflight(MAX_CURRENT_CANDIDATES + 1),
        Err(Reason::CandidatesOverflow)
    );
}
