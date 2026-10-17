//! Actual migrated SQLite query plans and duplicate-sensitive witness fixtures.
use super::default_migrations;
use crate::migrate::{apply_migrations, checksum_hex};
use crate::{CanonicalStore, CapturedEventRow, Row, SqliteStore, TurnBoundarySnapshotRow, Value};
use std::fmt::Write as _;

const EVENT_INDEX: &str = "idx_captured_event_session_turn_kind";
const NATIVE: &str = "native-fixture-session";
const HASHED: &str = "abababababababababababababababababababababababababababababababab";
const EVENT_FIELDS: &str = "CASE WHEN length(CAST(idempotency_key AS BLOB)) <= 64 THEN idempotency_key ELSE NULL END, event_ordinal, CASE WHEN length(CAST(kind AS BLOB)) <= 32 THEN kind ELSE NULL END, CASE WHEN length(CAST(trust AS BLOB)) <= 32 THEN trust ELSE NULL END, CASE WHEN length(CAST(payload_sha256 AS BLOB)) <= 64 THEN payload_sha256 ELSE NULL END, occurred_at_ms, schema_version";
const EXACT: &str = "SELECT CASE WHEN length(CAST(snapshot_id AS BLOB)) <= 64 THEN snapshot_id ELSE NULL END,captured_at,schema_version FROM turn_boundary_snapshot WHERE session_id=?1 AND turn_ordinal=?2 AND boundary=?3 LIMIT 2";
const LATER: &str =
    "SELECT 1 FROM turn_boundary_snapshot WHERE session_id=?1 AND turn_ordinal>?2 LIMIT 1";
const END: &str = "SELECT 1 FROM turn_boundary_snapshot WHERE session_id=?1 AND turn_ordinal=?2 AND boundary='end' LIMIT 1";
const SAME_TURN: &str = "SELECT CASE WHEN length(CAST(boundary AS BLOB)) <= 5 THEN boundary ELSE NULL END,CASE WHEN length(CAST(snapshot_id AS BLOB)) <= 64 THEN snapshot_id ELSE NULL END,schema_version FROM turn_boundary_snapshot WHERE session_id=?1 AND turn_ordinal=?2 LIMIT 3";

struct Fixture {
    store: SqliteStore,
    rt: tokio::runtime::Runtime,
}

impl Fixture {
    fn new() -> Self {
        Self::at_version(default_migrations().len())
    }

    fn at_version(version: usize) -> Self {
        let store = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&store, &default_migrations()[..version]).unwrap();
        Self {
            store,
            rt: tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap(),
        }
    }

    fn query(&self, sql: &str, params: &[Value]) -> Vec<Row> {
        self.rt.block_on(self.store.query(sql, params)).unwrap()
    }

    fn record_event(&self, row: &CapturedEventRow) -> bool {
        self.rt
            .block_on(crate::record_captured_event(&self.store, row))
            .unwrap()
    }

    fn seed_unrelated_events(&self) {
        for n in 0..128 {
            assert!(self.record_event(&event(
                10000 + n,
                &format!("other-{n}"),
                i64::from(n),
                "turn_end"
            )));
        }
    }

    fn snapshot(&self, id: u32, ordinal: i64, boundary: &str) {
        self.snapshot_for(HASHED, id, ordinal, boundary);
    }

    fn snapshot_for(&self, session: &str, id: u32, ordinal: i64, boundary: &str) {
        let row = TurnBoundarySnapshotRow {
            snapshot_id: format!("{id:064x}"),
            session_id: session.into(),
            turn_ordinal: ordinal,
            boundary: boundary.into(),
            actor: "agent".into(),
            worktree_hash: "11".repeat(32),
            captured_at: 1000,
            schema_version: 1,
        };
        assert!(self
            .rt
            .block_on(crate::record_turn_boundary_snapshot(&self.store, &row))
            .unwrap());
    }

    fn plan(&self, sql: &str, params: &[Value]) -> Vec<String> {
        self.query(&format!("EXPLAIN QUERY PLAN {sql}"), params)
            .into_iter()
            .map(|row| row.get_text(3).unwrap())
            .collect()
    }
}

fn event(id: u32, session: &str, ordinal: i64, kind: &str) -> CapturedEventRow {
    CapturedEventRow {
        idempotency_key: format!("{id:064x}"),
        session_id: session.into(),
        turn_ordinal: ordinal,
        event_ordinal: i64::from(id),
        kind: kind.into(),
        trust: rift_core::event::Trust::TranscriptUntrusted.as_str().into(),
        payload_sha256: "22".repeat(32),
        occurred_at_ms: 1000 + i64::from(id),
        schema_version: 1,
    }
}

fn witness_sql(start: bool) -> String {
    let kind = if start {
        "kind IN ('turn_start','prompt_submit')"
    } else {
        "kind='turn_end'"
    };
    format!("SELECT {EVENT_FIELDS} FROM captured_event WHERE session_id=?1 AND turn_ordinal=?2 AND {kind} LIMIT 2")
}

fn witness(f: &Fixture, session: &str, ordinal: i64, start: bool) -> Vec<Row> {
    f.query(
        &witness_sql(start),
        &[Value::text(session), Value::Int(ordinal)],
    )
}

fn assert_targeted_search(plan: &[String], table: &str, index: &str) {
    assert!(
        plan.iter()
            .any(|line| line.contains(&format!("SEARCH {table}")) && line.contains(index)),
        "{plan:?}"
    );
    assert!(
        !plan
            .iter()
            .any(|line| line.contains("SCAN ") || line.contains("TEMP B-TREE")),
        "{plan:?}"
    );
}

#[test]
fn compiled_capture_migrations_preserve_the_pre_index_twenty_one_body_receipt() {
    let migrations = default_migrations();
    assert!(migrations.len() >= 21);
    let saved = std::env::var_os("RIFT_RECOVERY_EVENT_BASELINE").map(std::path::PathBuf::from);
    for (index, migration) in migrations.iter().take(21).enumerate() {
        assert_eq!(migration.version, u32::try_from(index + 1).unwrap());
        let checksum = checksum_hex(&migration.sql);
        let mut hex = String::new();
        for byte in migration.sql.as_bytes() {
            write!(hex, "{byte:02x}").unwrap();
        }
        println!(
            "RECOVERY_INDEX_BASELINE\t{}\t{}\t{}\t{}",
            migration.version, migration.name, checksum, hex
        );
        if let Some(dir) = &saved {
            assert_eq!(
                migration.sql.as_bytes(),
                std::fs::read(dir.join(format!("migration-{:02}.sql", migration.version))).unwrap()
            );
        }
    }
}

#[test]
fn explicit_end_witness_lookup_uses_the_targeted_event_index() {
    let f = Fixture::new();
    f.seed_unrelated_events();
    assert!(f.record_event(&event(1, NATIVE, 7, "turn_end")));
    let rows = witness(&f, NATIVE, 7, false);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_text(0).unwrap(), format!("{:064x}", 1));
    let plan = f.plan(&witness_sql(false), &[Value::text(NATIVE), Value::Int(7)]);
    println!("RECOVERY_INDEX_Q1_PLAN {plan:?}");
    assert_targeted_search(&plan, "captured_event", EVENT_INDEX);
}

#[test]
fn both_explicit_start_kinds_use_the_targeted_event_index() {
    let f = Fixture::new();
    f.seed_unrelated_events();
    assert!(f.record_event(&event(1, NATIVE, 7, "turn_start")));
    assert!(f.record_event(&event(2, NATIVE, 7, "prompt_submit")));
    assert_eq!(witness(&f, NATIVE, 7, true).len(), 2);
    let plan = f.plan(&witness_sql(true), &[Value::text(NATIVE), Value::Int(7)]);
    println!("RECOVERY_INDEX_Q2_PLAN {plan:?}");
    assert_targeted_search(&plan, "captured_event", EVENT_INDEX);
}

#[test]
fn the_event_lookup_index_is_nonunique_and_has_only_the_selected_three_columns() {
    let f = Fixture::new();
    let indexes = f.query("PRAGMA index_list('captured_event')", &[]);
    let found: Vec<_> = indexes
        .iter()
        .filter(|row| row.get_text(1).unwrap() == EVENT_INDEX)
        .collect();
    println!("RECOVERY_INDEX_PRESENT {}", found.len());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].get_i64(2).unwrap(), 0);
    let columns: Vec<_> = f
        .query(
            "SELECT name FROM pragma_index_info(?1) ORDER BY seqno",
            &[Value::text(EVENT_INDEX)],
        )
        .iter()
        .map(|row| row.get_text(0).unwrap())
        .collect();
    assert_eq!(columns, ["session_id", "turn_ordinal", "kind"]);
    assert!(f.record_event(&event(1, NATIVE, 7, "turn_end")));
    assert!(f.record_event(&event(2, NATIVE, 7, "turn_end")));
    assert_eq!(witness(&f, NATIVE, 7, false).len(), 2);
}

#[test]
fn real_event_witnesses_preserve_zero_one_two_dedup_and_native_session_namespaces() {
    for kind in ["turn_end", "turn_start", "prompt_submit"] {
        let f = Fixture::new();
        f.seed_unrelated_events();
        let start = kind != "turn_end";
        assert!(witness(&f, NATIVE, 7, start).is_empty());
        let first = event(1, NATIVE, 7, kind);
        assert!(f.record_event(&first));
        assert_eq!(witness(&f, NATIVE, 7, start).len(), 1);
        assert!(!f.record_event(&first));
        assert_eq!(witness(&f, NATIVE, 7, start).len(), 1);
        assert!(witness(&f, HASHED, 7, start).is_empty());
        assert!(witness(&f, NATIVE, 8, start).is_empty());
        assert!(f.record_event(&event(2, NATIVE, 7, kind)));
        assert_eq!(witness(&f, NATIVE, 7, start).len(), 2);
        assert!(f.record_event(&event(3, NATIVE, 7, kind)));
        assert_eq!(witness(&f, NATIVE, 7, start).len(), 2);
    }
    let f = Fixture::new();
    assert!(f.record_event(&event(1, NATIVE, 7, "turn_start")));
    assert!(f.record_event(&event(2, NATIVE, 7, "prompt_submit")));
    let rows = witness(&f, NATIVE, 7, true);
    let mut kinds: Vec<_> = rows.iter().map(|row| row.get_text(2).unwrap()).collect();
    kinds.sort();
    assert_eq!(kinds, ["prompt_submit", "turn_start"]);
}

#[test]
fn malformed_return_cells_remain_counted_evidence_instead_of_being_filtered_away() {
    let f = Fixture::new();
    let mut invalid = event(1, NATIVE, 7, "turn_end");
    invalid.idempotency_key = "x".repeat(65);
    invalid.payload_sha256 = "y".repeat(65);
    invalid.trust = "future-trust".into();
    invalid.schema_version = 2;
    invalid.event_ordinal = -1;
    assert!(f.record_event(&invalid));
    let rows = witness(&f, NATIVE, 7, false);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].get_opt_text(0).unwrap().is_none());
    assert_eq!(rows[0].get_i64(1).unwrap(), -1);
    assert_eq!(rows[0].get_text(3).unwrap(), "future-trust");
    assert!(rows[0].get_opt_text(4).unwrap().is_none());
    assert_eq!(rows[0].get_i64(6).unwrap(), 2);
    assert!(f.record_event(&event(2, NATIVE, 7, "turn_end")));
    assert_eq!(witness(&f, NATIVE, 7, false).len(), 2);
    f.snapshot(1, 7, "future-edge");
    let snapshots = f.query(SAME_TURN, &[Value::text(HASHED), Value::Int(7)]);
    assert_eq!(snapshots.len(), 1);
    assert!(snapshots[0].get_opt_text(0).unwrap().is_none());
    f.snapshot(2, 7, "start");
    f.snapshot(3, 7, "end");
    f.snapshot(4, 7, "other");
    assert_eq!(
        f.query(SAME_TURN, &[Value::text(HASHED), Value::Int(7)])
            .len(),
        3
    );
    let invalid_snapshot = TurnBoundarySnapshotRow {
        snapshot_id: "z".repeat(65),
        session_id: HASHED.into(),
        turn_ordinal: 9,
        boundary: "start".into(),
        actor: "agent".into(),
        worktree_hash: "33".repeat(32),
        captured_at: 2000,
        schema_version: 2,
    };
    assert!(f
        .rt
        .block_on(crate::record_turn_boundary_snapshot(
            &f.store,
            &invalid_snapshot
        ))
        .unwrap());
    let rows = f.query(
        EXACT,
        &[Value::text(HASHED), Value::Int(9), Value::text("start")],
    );
    assert_eq!(rows.len(), 1);
    assert!(rows[0].get_opt_text(0).unwrap().is_none());
    assert_eq!(rows[0].get_i64(2).unwrap(), 2);
}

fn snapshot_unique_index(f: &Fixture) -> String {
    for row in f.query("PRAGMA index_list('turn_boundary_snapshot')", &[]) {
        if row.get_i64(2).unwrap() != 1 {
            continue;
        }
        let name = row.get_text(1).unwrap();
        let columns: Vec<_> = f
            .query(
                "SELECT name FROM pragma_index_info(?1) ORDER BY seqno",
                &[Value::text(&name)],
            )
            .iter()
            .map(|row| row.get_text(0).unwrap())
            .collect();
        if columns == ["session_id", "turn_ordinal", "boundary"] {
            return name;
        }
    }
    panic!("existing snapshot unique index is missing")
}

#[test]
fn exact_later_and_same_turn_boundary_queries_use_the_existing_unique_index() {
    let f = Fixture::new();
    f.snapshot(1, 7, "start");
    f.snapshot(2, 7, "end");
    f.snapshot(3, 8, "start");
    for n in 0..128 {
        f.snapshot_for(
            &format!("{:064x}", 10000 + n),
            100 + n,
            100 + i64::from(n),
            "start",
        );
    }
    let index = snapshot_unique_index(&f);
    for (sql, params) in [
        (
            EXACT,
            vec![Value::text(HASHED), Value::Int(7), Value::text("start")],
        ),
        (LATER, vec![Value::text(HASHED), Value::Int(7)]),
        (END, vec![Value::text(HASHED), Value::Int(7)]),
        (SAME_TURN, vec![Value::text(HASHED), Value::Int(7)]),
    ] {
        let plan = f.plan(sql, &params);
        println!("RECOVERY_SNAPSHOT_POINT_PLAN {plan:?}");
        assert_targeted_search(&plan, "turn_boundary_snapshot", &index);
    }
}

#[test]
fn boundary_existence_queries_distinguish_exact_later_and_same_turn_edges() {
    let f = Fixture::new();
    let params = [Value::text(HASHED), Value::Int(7)];
    assert!(f
        .query(
            EXACT,
            &[Value::text(HASHED), Value::Int(7), Value::text("start")]
        )
        .is_empty());
    assert!(f.query(LATER, &params).is_empty());
    assert!(f.query(END, &params).is_empty());
    f.snapshot(1, 6, "end");
    f.snapshot(2, 7, "start");
    assert_eq!(
        f.query(
            EXACT,
            &[Value::text(HASHED), Value::Int(7), Value::text("start")]
        )
        .len(),
        1
    );
    assert!(f.query(LATER, &params).is_empty());
    assert!(f.query(END, &params).is_empty());
    f.snapshot(3, 7, "end");
    assert_eq!(f.query(END, &params).len(), 1);
    assert!(f.query(LATER, &params).is_empty());
    f.snapshot(4, 8, "start");
    assert_eq!(f.query(LATER, &params).len(), 1);
    assert!(f
        .query(LATER, &[Value::text(NATIVE), Value::Int(7)])
        .is_empty());
}

fn migration_ledger(f: &Fixture, version: i64) -> Vec<Row> {
    f.query(
        "SELECT version,name,checksum FROM _migrations WHERE version<=?1 ORDER BY version",
        &[Value::Int(version)],
    )
}

fn assert_canonical_event_guards(f: &Fixture) {
    for sql in [
        "UPDATE captured_event SET occurred_at_ms=999 WHERE session_id='native-fixture-session'",
        "DELETE FROM captured_event WHERE session_id='native-fixture-session'",
        "UPDATE turn_boundary_snapshot SET captured_at=999",
        "DELETE FROM turn_boundary_snapshot",
    ] {
        assert!(f.store.execute_raw(sql).is_err(), "{sql}");
    }
}

fn assert_upgrade_query_plans(f: &Fixture) {
    for start in [false, true] {
        let plan = f.plan(&witness_sql(start), &[Value::text(NATIVE), Value::Int(7)]);
        println!("RECOVERY_UPGRADED_EVENT_PLAN {plan:?}");
        assert_targeted_search(&plan, "captured_event", EVENT_INDEX);
    }
    let index = snapshot_unique_index(f);
    for (sql, params) in [
        (
            EXACT,
            vec![Value::text(HASHED), Value::Int(7), Value::text("start")],
        ),
        (LATER, vec![Value::text(HASHED), Value::Int(7)]),
        (END, vec![Value::text(HASHED), Value::Int(7)]),
        (SAME_TURN, vec![Value::text(HASHED), Value::Int(7)]),
    ] {
        let plan = f.plan(sql, &params);
        println!("RECOVERY_UPGRADED_SNAPSHOT_PLAN {plan:?}");
        assert_targeted_search(&plan, "turn_boundary_snapshot", &index);
    }
}

#[test]
fn a_fresh_projection_applies_twenty_two_then_reapplies_as_a_noop() {
    let f = Fixture::at_version(0);
    let all = default_migrations();
    assert_eq!(all.len(), 22);
    assert_eq!(
        (all[21].version, all[21].name),
        (22, "capture_event_session_turn_kind_index")
    );
    assert_eq!(all[21].sql, "CREATE INDEX IF NOT EXISTS idx_captured_event_session_turn_kind ON captured_event(session_id, turn_ordinal, kind);\n");
    let applied = apply_migrations(&f.store, &all).unwrap();
    assert_eq!(
        (applied.applied, applied.skipped, applied.high_water),
        (22, 0, 22)
    );
    assert_eq!(migration_ledger(&f, 22).len(), 22);
    assert!(f.record_event(&event(1, NATIVE, 7, "turn_end")));
    f.snapshot(1, 7, "start");
    assert_canonical_event_guards(&f);
    let before = migration_ledger(&f, 22);
    let again = apply_migrations(&f.store, &all).unwrap();
    assert_eq!(
        (again.applied, again.skipped, again.high_water),
        (0, 22, 22)
    );
    assert_eq!(migration_ledger(&f, 22), before);
    assert_eq!(witness(&f, NATIVE, 7, false).len(), 1);
}

#[test]
fn a_seeded_twenty_one_projection_gains_only_the_event_index_and_keeps_old_facts() {
    let f = Fixture::at_version(21);
    f.seed_unrelated_events();
    for (id, kind) in [(1, "turn_end"), (2, "turn_start"), (3, "prompt_submit")] {
        assert!(f.record_event(&event(id, NATIVE, 7, kind)));
    }
    f.snapshot(1, 7, "start");
    f.snapshot(2, 7, "end");
    f.snapshot(3, 8, "start");
    let before_events = f.query("SELECT * FROM captured_event ORDER BY idempotency_key", &[]);
    let before_snapshots = f.query(
        "SELECT * FROM turn_boundary_snapshot ORDER BY snapshot_id",
        &[],
    );
    let before_ledger = migration_ledger(&f, 21);
    let before_relations = f.query("SELECT type,name,sql FROM sqlite_master WHERE type IN ('table','trigger') ORDER BY type,name", &[]);
    for start in [false, true] {
        assert!(f
            .plan(&witness_sql(start), &[Value::text(NATIVE), Value::Int(7)])
            .iter()
            .any(|line| line == "SCAN captured_event"));
    }
    assert_canonical_event_guards(&f);
    let all = default_migrations();
    let applied = apply_migrations(&f.store, &all).unwrap();
    assert_eq!(
        (applied.applied, applied.skipped, applied.high_water),
        (1, 21, 22)
    );
    assert_eq!(migration_ledger(&f, 21), before_ledger);
    assert_eq!(
        f.query("SELECT * FROM captured_event ORDER BY idempotency_key", &[]),
        before_events
    );
    assert_eq!(
        f.query(
            "SELECT * FROM turn_boundary_snapshot ORDER BY snapshot_id",
            &[]
        ),
        before_snapshots
    );
    assert_eq!(f.query("SELECT type,name,sql FROM sqlite_master WHERE type IN ('table','trigger') ORDER BY type,name", &[]), before_relations);
    assert_canonical_event_guards(&f);
    assert_upgrade_query_plans(&f);
    assert_eq!(witness(&f, NATIVE, 7, false).len(), 1);
    assert_eq!(witness(&f, NATIVE, 7, true).len(), 2);
    let before_all = migration_ledger(&f, 22);
    let again = apply_migrations(&f.store, &all).unwrap();
    assert_eq!(
        (again.applied, again.skipped, again.high_water),
        (0, 22, 22)
    );
    assert_eq!(migration_ledger(&f, 22), before_all);
    assert_eq!(
        f.query("SELECT * FROM captured_event ORDER BY idempotency_key", &[]),
        before_events
    );
    assert_eq!(
        f.query(
            "SELECT * FROM turn_boundary_snapshot ORDER BY snapshot_id",
            &[]
        ),
        before_snapshots
    );
}
