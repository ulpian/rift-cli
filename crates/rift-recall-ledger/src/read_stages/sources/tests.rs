use rift_store::{default_migrations, recall_migrations, SqliteStore};

use super::*;
use crate::read_stages::{report, ArchiveStage};

const WINDOW: &str = "w1";
const PATH: &str = "src/a.rs";
const HASH: &str = "h1";
const RECEIVED: i64 = 1_000;

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(f)
}

/// A recall store and a capture projection, both migrated and empty.
struct Fixture {
    recall: SqliteStore,
    capture: SqliteStore,
}

impl Fixture {
    fn new() -> Self {
        let recall = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&recall, &recall_migrations()).unwrap();
        let capture = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&capture, &default_migrations()).unwrap();
        Self { recall, capture }
    }

    fn stores(&self) -> StageStores<'_> {
        StageStores {
            recall: &self.recall,
            capture: &self.capture,
        }
    }

    fn insert(store: &SqliteStore, table: &str, columns: &[&str], values: &[Value]) {
        block(store.insert(table, columns, values)).unwrap();
    }

    /// A window row of `kind` on the fixture key, frozen by checkpoint `cid`.
    fn window_row(&self, row_id: &str, kind: &str, at: i64, cid: &str) {
        let columns = [
            "row_id",
            "window_key",
            "kind",
            "turn",
            "at",
            "path",
            "content_sha256",
            "checkpoint_id",
            "schema_version",
        ];
        let values = [
            Value::text(row_id),
            Value::text(WINDOW),
            Value::text(kind),
            Value::Int(1),
            Value::Int(at),
            Value::text(PATH),
            Value::text(HASH),
            Value::text(cid),
            Value::Int(1),
        ];
        Self::insert(&self.recall, "n_window_row", &columns, &values);
    }

    /// Checkpoint `cid` at walk position `ordinal`, sealed at `sealed_at`.
    fn checkpoint(&self, cid: &str, ordinal: i64, sealed_at: i64) {
        let columns = ["node_id", "dialect", "ordinal"];
        let values = [Value::text(cid), Value::text("rift"), Value::Int(ordinal)];
        Self::insert(&self.recall, "n_checkpoint", &columns, &values);
        let columns = [
            "checkpoint_id",
            "strategy",
            "created_at",
            "merkle_root",
            "schema_version",
        ];
        let values = [
            Value::text(cid),
            Value::text("turn-end"),
            Value::Int(sealed_at),
            Value::text("m"),
            Value::Int(1),
        ];
        Self::insert(&self.capture, "checkpoint", &columns, &values);
    }

    /// An ingest that walked to position `chain_seq` at `ingested_at`.
    fn ingest(&self, id: &str, chain_seq: i64, ingested_at: i64) {
        let columns = [
            "watermark_id",
            "branch",
            "branch_tip",
            "ingested_at",
            "schema_version",
        ];
        let values = [
            Value::text(id),
            Value::text("rift/checkpoints/v1"),
            Value::text("tip"),
            Value::Int(ingested_at),
            Value::Int(1),
        ];
        Self::insert(&self.recall, "ingest_watermark", &columns, &values);
        let columns = ["watermark_id", "chain_seq", "schema_version"];
        let values = [Value::text(id), Value::Int(chain_seq), Value::Int(1)];
        Self::insert(&self.recall, "ingest_watermark_position", &columns, &values);
    }

    fn facts(
        &self,
        receipts: &[ReadReceipt<'_>],
        calls: &[CallEmission<'_>],
    ) -> Vec<ReadStageFacts> {
        block(read_stage_facts(self.stores(), receipts, calls)).unwrap()
    }
}

fn receipt(invocation_id: Option<&str>) -> ReadReceipt<'_> {
    ReadReceipt {
        window_key: WINDOW,
        path: PATH,
        content_sha256: HASH,
        received_at: RECEIVED,
        invocation_id,
    }
}

const fn at(at_ms: i64, join: StageJoin) -> StageFact {
    StageFact::At { at_ms, join }
}

/// A read frozen by c1 (sealed 1500), ingested by a walk that stopped short
/// at 1700 and one that reached it at 2000.
fn archived() -> Fixture {
    let fx = Fixture::new();
    fx.window_row("r1", "read", RECEIVED, "c1");
    fx.checkpoint("c1", 2, 1_500);
    fx.ingest("wm-short", 1, 1_700);
    fx.ingest("wm-reach", 2, 2_000);
    fx
}

#[test]
fn read_stages_come_from_each_store_with_delivery_joined_by_invocation_id() {
    let fx = archived();
    fx.window_row("r2", "deny", 1_100, "c1");
    let calls = [
        CallEmission {
            invocation_id: "call_1",
            wrote: false,
            at_ms: 1_050,
        },
        CallEmission {
            invocation_id: "call_1",
            wrote: true,
            at_ms: 1_200,
        },
        CallEmission {
            invocation_id: "call_2",
            wrote: true,
            at_ms: 1_010,
        },
    ];
    let facts = fx.facts(&[receipt(Some("call_1"))], &calls);
    let expected = ReadStageFacts {
        received_at: RECEIVED,
        captured: at(1_500, StageJoin::PathHash),
        queryable: at(2_000, StageJoin::PathHash),
        delivered: at(1_200, StageJoin::InvocationId),
    };
    assert_eq!(facts, vec![expected]);
    assert_eq!(
        report(expected.times()).archived_to,
        ArchiveStage::Queryable
    );
}

#[test]
fn read_stages_fall_back_to_the_window_answer_row_on_path_and_hash() {
    let fx = archived();
    fx.window_row("r0", "deny", 900, "c1");
    fx.window_row("r2", "hold", 1_300, "c1");
    fx.window_row("r3", "prevented", 1_100, "c1");
    let facts = fx.facts(&[receipt(Some("call_9"))], &[]);
    assert_eq!(facts[0].delivered, at(1_100, StageJoin::PathHash));
}

#[test]
fn read_stages_report_a_delivery_that_came_before_the_capture() {
    let fx = archived();
    fx.window_row("r2", "deny", 1_050, "c1");
    let facts = fx.facts(&[receipt(None)], &[]);
    let rep = report(facts[0].times());
    assert_eq!(rep.archival_lag_ms, Some(500));
    assert_eq!(rep.delivery_latency_ms, Some(50));
    assert!(rep.delivered && rep.anomalies.is_empty(), "{rep:?}");
}

#[test]
fn read_stages_leave_capture_absent_when_no_store_holds_the_read() {
    let fx = Fixture::new();
    let calls = [CallEmission {
        invocation_id: "call_1",
        wrote: true,
        at_ms: 1_200,
    }];
    let facts = fx.facts(&[receipt(Some("call_1"))], &calls);
    assert_eq!(facts[0].captured, StageFact::Absent);
    assert_eq!(facts[0].queryable, StageFact::Absent);
    assert_eq!(facts[0].delivered, at(1_200, StageJoin::InvocationId));
    assert_eq!(report(facts[0].times()).archived_to, ArchiveStage::Received);
}

#[test]
fn read_stages_name_two_reads_on_one_path_and_hash_ambiguous_never_merged() {
    let fx = archived();
    fx.window_row("r2", "deny", 1_100, "c1");
    let calls = [CallEmission {
        invocation_id: "call_2",
        wrote: true,
        at_ms: 1_400,
    }];
    let mut second = receipt(Some("call_2"));
    second.received_at = 1_050;
    let facts = fx.facts(&[receipt(None), second], &calls);
    let ambiguous = StageFact::Ambiguous {
        join: StageJoin::PathHash,
    };
    for fact in &facts {
        assert_eq!((fact.captured, fact.queryable), (ambiguous, ambiguous));
    }
    assert_eq!(facts[0].delivered, ambiguous);
    assert_eq!(facts[1].delivered, at(1_400, StageJoin::InvocationId));
    assert_eq!(facts[0].times().captured_at, None);
}

#[test]
fn read_stages_serialize_each_stage_with_its_join() {
    let fx = archived();
    let facts = fx.facts(&[receipt(None)], &[]);
    let json = serde_json::to_value(facts[0]).unwrap();
    assert_eq!(json["captured"]["join"], "path_hash");
    assert_eq!(json["queryable"]["state"], "at");
    assert_eq!(json["delivered"]["state"], "absent");
}

/// The projection's read facts of one session reading the fixture file in
/// turn `turn`, and the turn-end checkpoint `cid` sealed over that session.
fn projected_read(fx: &Fixture, session: &str, turn: &str, cid: &str, sealed_at: i64) {
    let rows: [(&str, &[&str], Vec<Value>); 5] = [
        (
            "file_revision",
            &[
                "file_revision_id",
                "path",
                "blob_oid",
                "content_sha256",
                "schema_version",
            ],
            vec![
                Value::text("rev"),
                Value::text(PATH),
                Value::text("oid"),
                Value::text(HASH),
                Value::Int(1),
            ],
        ),
        (
            "turn",
            &[
                "turn_id",
                "session_id",
                "ordinal",
                "started_at",
                "schema_version",
            ],
            vec![
                Value::text(turn),
                Value::text(session),
                Value::Int(1),
                Value::Int(RECEIVED),
                Value::Int(1),
            ],
        ),
        (
            "tool_call",
            &[
                "tool_call_id",
                "turn_id",
                "tool_name",
                "started_at",
                "schema_version",
            ],
            vec![
                Value::text(turn),
                Value::text(turn),
                Value::text("Read"),
                Value::Int(RECEIVED),
                Value::Int(1),
            ],
        ),
        (
            "tool_reads_file",
            &["tool_call_id", "file_revision_id"],
            vec![Value::text(turn), Value::text("rev")],
        ),
        (
            "session_in_checkpoint",
            &["checkpoint_id", "session_id", "ordinal", "schema_version"],
            vec![
                Value::text(cid),
                Value::text(session),
                Value::Int(1),
                Value::Int(1),
            ],
        ),
    ];
    for (table, columns, values) in rows {
        if table == "file_revision" && session != "s1" {
            continue;
        }
        Fixture::insert(&fx.capture, table, columns, &values);
    }
    fx.checkpoint(cid, 2, sealed_at);
}

#[test]
fn read_stages_find_capture_in_the_projection_before_any_ingest_reached_it() {
    let fx = Fixture::new();
    projected_read(&fx, "s1", "t1", "c1", 1_500);
    let facts = fx.facts(&[receipt(None)], &[]);
    assert_eq!(facts[0].captured, at(1_500, StageJoin::PathHash));
    assert_eq!(facts[0].queryable, StageFact::Absent);
    fx.ingest("wm", 2, 2_000);
    let facts = fx.facts(&[receipt(None)], &[]);
    assert_eq!(facts[0].queryable, at(2_000, StageJoin::PathHash));
}

#[test]
fn read_stages_name_projection_capture_ambiguous_when_two_sessions_read_the_same_bytes() {
    let fx = Fixture::new();
    projected_read(&fx, "s1", "t1", "c1", 1_500);
    projected_read(&fx, "s2", "t2", "c2", 1_600);
    let facts = fx.facts(&[receipt(None)], &[]);
    let ambiguous = StageFact::Ambiguous {
        join: StageJoin::PathHash,
    };
    assert_eq!(
        (facts[0].captured, facts[0].queryable),
        (ambiguous, ambiguous)
    );
}
