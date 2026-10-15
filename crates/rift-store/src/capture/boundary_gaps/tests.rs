use super::*;
use crate::SqliteStore;

fn ticket(source: u32) -> GapTicket {
    GapTicket::new(GapTicketFields {
        source_key: SourceKey::parse_hex(&format!("{source:064x}")).unwrap(),
        context_digest: ContextDigest::parse_hex(&"44".repeat(32)).unwrap(),
        ticket_digest: TicketDigest::parse_hex(&"55".repeat(32)).unwrap(),
        session_key: SessionKey::parse_hex(&"66".repeat(32)).unwrap(),
        turn_ordinal: 7,
        boundary: BoundaryEdge::End,
        head_oid: Some("77".repeat(20)),
        captured_at: 1000,
        queued_at: 1200,
        attempts: 2,
    })
    .unwrap()
}

fn batch(at: i64, tickets: Vec<GapTicket>) -> GapBatch {
    GapBatch::new(
        RepoId::new("repo-1").unwrap(),
        MemberKey::parse_hex(&"11".repeat(32)).unwrap(),
        EvidenceDigest::parse_hex(&"22".repeat(32)).unwrap(),
        at,
        tickets,
    )
    .unwrap()
}

fn store() -> SqliteStore {
    let store = SqliteStore::open_in_memory().unwrap();
    store.create_canonical_schema().unwrap();
    store
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

#[test]
fn a_complete_identical_batch_is_a_noop_but_different_recorded_time_refuses() {
    let store = store();
    let rt = runtime();
    let original = batch(1500, vec![ticket(1), ticket(2)]);
    assert!(rt
        .block_on(record_boundary_gap_batch(&store, &original))
        .unwrap());
    assert!(!rt
        .block_on(record_boundary_gap_batch(&store, &original))
        .unwrap());
    let changed = batch(1501, original.tickets().to_vec());
    assert_eq!(original.gap_id(), changed.gap_id());
    assert!(rt
        .block_on(record_boundary_gap_batch(&store, &changed))
        .is_err());
    assert_eq!(
        rt.block_on(boundary_gap_batch(&store, original.gap_id()))
            .unwrap(),
        Some(original)
    );
    assert_eq!(
        rt.block_on(store.count("turn_boundary_gap_ticket"))
            .unwrap(),
        2
    );
}

#[test]
fn exact_partial_members_resume_but_incomplete_parent_never_certifies_a_batch() {
    let store = store();
    let rt = runtime();
    let original = batch(1500, vec![ticket(1), ticket(2)]);
    rt.block_on(append_ticket(&store, original.gap_id(), &ticket(1)))
        .unwrap();
    assert!(rt
        .block_on(boundary_gap_batch(&store, original.gap_id()))
        .is_err());
    assert!(rt
        .block_on(record_boundary_gap_batch(&store, &original))
        .unwrap());
    let other = batch(1600, vec![ticket(3), ticket(4)]);
    rt.block_on(append_batch(&store, &other)).unwrap();
    let before = rt
        .block_on(store.count("turn_boundary_gap_ticket"))
        .unwrap();
    assert!(rt
        .block_on(record_boundary_gap_batch(&store, &other))
        .is_err());
    assert_eq!(
        rt.block_on(store.count("turn_boundary_gap_ticket"))
            .unwrap(),
        before
    );
}

#[test]
fn orphan_extra_and_conflicting_members_are_preserved_without_parent_completion() {
    for conflict in [ticket(3), {
        let mut changed = ticket(1);
        changed = GapTicket::new(GapTicketFields {
            source_key: changed.source_key(),
            context_digest: changed.context_digest(),
            ticket_digest: changed.ticket_digest(),
            session_key: changed.session_key(),
            turn_ordinal: changed.turn_ordinal(),
            boundary: changed.boundary(),
            head_oid: changed.head_oid().map(str::to_owned),
            captured_at: changed.captured_at(),
            queued_at: changed.queued_at(),
            attempts: 9,
        })
        .unwrap();
        changed
    }] {
        let store = store();
        let rt = runtime();
        let original = batch(1500, vec![ticket(1), ticket(2)]);
        rt.block_on(append_ticket(&store, original.gap_id(), &conflict))
            .unwrap();
        let before = rt
            .block_on(store.query(
                "SELECT * FROM turn_boundary_gap_ticket ORDER BY source_key",
                &[],
            ))
            .unwrap();
        assert!(rt
            .block_on(record_boundary_gap_batch(&store, &original))
            .is_err());
        assert_eq!(
            rt.block_on(store.count("turn_boundary_gap_batch")).unwrap(),
            0
        );
        assert_eq!(
            rt.block_on(store.query(
                "SELECT * FROM turn_boundary_gap_ticket ORDER BY source_key",
                &[]
            ))
            .unwrap(),
            before
        );
    }
}

#[test]
fn audit_subjects_do_not_create_native_or_capture_session_facts() {
    let store = store();
    let rt = runtime();
    let original = batch(1500, vec![ticket(1), ticket(2)]);
    rt.block_on(record_boundary_gap_batch(&store, &original))
        .unwrap();
    for table in [
        "session",
        "session_in_checkpoint",
        "session_event",
        "token_usage",
        "turn",
        "turn_diff_hunk",
    ] {
        assert_eq!(rt.block_on(store.count(table)).unwrap(), 0, "{table}");
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Interleave {
    None,
    ExtraMember,
    ConflictingMember,
    CompetingParent,
}

struct Interleaved {
    inner: SqliteStore,
    batch: GapBatch,
    action: Interleave,
    fired: std::sync::atomic::AtomicBool,
    largest_member_fetch: std::sync::atomic::AtomicUsize,
}

impl Interleaved {
    fn new(action: Interleave, batch: GapBatch) -> Self {
        Self {
            inner: store(),
            batch,
            action,
            fired: std::sync::atomic::AtomicBool::new(false),
            largest_member_fetch: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl CanonicalStore for Interleaved {
    async fn register_canonical_table(&self, table: &crate::schema::CanonicalTable) -> Result<()> {
        self.inner.register_canonical_table(table).await
    }
    async fn insert(&self, table: &str, columns: &[&str], values: &[Value]) -> Result<()> {
        self.inner.insert(table, columns, values).await
    }
    async fn insert_or_ignore(
        &self,
        table: &str,
        columns: &[&str],
        values: &[Value],
    ) -> Result<bool> {
        use std::sync::atomic::Ordering::SeqCst;
        let target = match self.action {
            Interleave::ExtraMember | Interleave::ConflictingMember => "turn_boundary_gap_ticket",
            Interleave::CompetingParent => "turn_boundary_gap_batch",
            Interleave::None => "",
        };
        let act = table == target && !self.fired.swap(true, SeqCst);
        if act && self.action == Interleave::ConflictingMember {
            let mut competing = values.to_vec();
            *competing.get_mut(10).unwrap() = Value::Int(99);
            self.inner.insert(table, columns, &competing).await?;
        }
        if act && self.action == Interleave::CompetingParent {
            let winner = batch(self.batch.recorded_at() + 1, self.batch.tickets().to_vec());
            assert!(record_boundary_gap_batch(&self.inner, &winner).await?);
        }
        let inserted = self.inner.insert_or_ignore(table, columns, values).await?;
        if act && self.action == Interleave::ExtraMember {
            append_ticket(&self.inner, self.batch.gap_id(), &ticket(99)).await?;
        }
        Ok(inserted)
    }
    async fn count(&self, table: &str) -> Result<i64> {
        self.inner.count(table).await
    }
    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        let rows = self.inner.query(sql, params).await?;
        if sql.starts_with(TICKET_SELECT) && sql.contains("ORDER BY source_key LIMIT") {
            self.largest_member_fetch
                .fetch_max(rows.len(), std::sync::atomic::Ordering::SeqCst);
        }
        Ok(rows)
    }
    async fn query_opt_i64(&self, sql: &str, params: &[Value]) -> Result<Option<i64>> {
        self.inner.query_opt_i64(sql, params).await
    }
}

fn facts(store: &impl CanonicalStore, rt: &tokio::runtime::Runtime) -> (Vec<Row>, Vec<Row>) {
    (
        rt.block_on(store.query("SELECT * FROM turn_boundary_gap_batch ORDER BY gap_id", &[]))
            .unwrap(),
        rt.block_on(store.query(
            "SELECT * FROM turn_boundary_gap_ticket ORDER BY gap_id, source_key",
            &[],
        ))
        .unwrap(),
    )
}

/// Controlled actual-store decision interleavings, not scheduler stress.
#[test]
fn competing_writers_cannot_bless_extra_members_collisions_or_a_different_parent() {
    use std::sync::atomic::Ordering::SeqCst;
    for action in [
        Interleave::ExtraMember,
        Interleave::ConflictingMember,
        Interleave::CompetingParent,
    ] {
        let original = batch(1500, vec![ticket(1), ticket(2)]);
        let store = Interleaved::new(action, original.clone());
        let rt = runtime();
        assert!(rt
            .block_on(record_boundary_gap_batch(&store, &original))
            .is_err());
        assert!(store.fired.load(SeqCst));
        assert_eq!(
            rt.block_on(store.count("turn_boundary_gap_batch")).unwrap(),
            i64::from(action == Interleave::CompetingParent)
        );
        if action == Interleave::CompetingParent {
            let winner = rt
                .block_on(boundary_gap_batch(&store, original.gap_id()))
                .unwrap()
                .unwrap();
            assert_eq!(winner.recorded_at(), 1501);
            assert_eq!(winner.tickets(), original.tickets());
        }
        let retained = facts(&store, &rt);
        assert!(rt
            .block_on(record_boundary_gap_batch(&store, &original))
            .is_err());
        assert_eq!(facts(&store, &rt), retained);
    }
}

#[test]
fn a_late_extra_member_invalidates_checked_reads_without_rewriting_facts() {
    let store = store();
    let rt = runtime();
    let original = batch(1500, vec![ticket(1)]);
    rt.block_on(record_boundary_gap_batch(&store, &original))
        .unwrap();
    rt.block_on(append_ticket(&store, original.gap_id(), &ticket(2)))
        .unwrap();
    let before = facts(&store, &rt);
    assert!(rt
        .block_on(boundary_gap_batch(&store, original.gap_id()))
        .is_err());
    assert!(rt
        .block_on(record_boundary_gap_batch(&store, &original))
        .is_err());
    assert_eq!(facts(&store, &rt), before);
}

#[test]
fn actual_member_fetch_is_capped_even_when_corrupt_extra_rows_exist() {
    use std::sync::atomic::Ordering::SeqCst;
    let original = batch(1500, vec![ticket(1)]);
    let store = Interleaved::new(Interleave::None, original.clone());
    let rt = runtime();
    for source in 0..u32::try_from(MAX_TICKETS + 17).unwrap() {
        rt.block_on(append_ticket(
            &store.inner,
            original.gap_id(),
            &ticket(source),
        ))
        .unwrap();
    }
    assert!(rt
        .block_on(boundary_gap_batch(&store, original.gap_id()))
        .is_err());
    assert_eq!(store.largest_member_fetch.load(SeqCst), MAX_TICKETS + 1);
    assert_eq!(
        rt.block_on(store.count("turn_boundary_gap_ticket"))
            .unwrap(),
        i64::try_from(MAX_TICKETS + 17).unwrap()
    );
}

#[test]
fn both_relations_reject_raw_update_and_delete() {
    let store = store();
    let rt = runtime();
    let original = batch(1500, vec![ticket(1)]);
    rt.block_on(record_boundary_gap_batch(&store, &original))
        .unwrap();
    let before = facts(&store, &rt);
    for table in ["turn_boundary_gap_batch", "turn_boundary_gap_ticket"] {
        for sql in [
            format!("UPDATE {table} SET schema_version = schema_version"),
            format!("DELETE FROM {table}"),
        ] {
            let error = store.with_conn(|conn| conn.execute(&sql, [])).unwrap_err();
            assert!(error.to_string().contains("append-only"));
        }
    }
    assert_eq!(facts(&store, &rt), before);
}

#[test]
fn context_lookup_uses_the_covering_index_among_unrelated_contexts() {
    let store = SqliteStore::open_in_memory().unwrap();
    crate::migrate::apply_migrations(&store, &crate::default_migrations()).unwrap();
    let rt = runtime();
    let original = batch(1500, vec![ticket(1)]);
    for source in 0..128 {
        let template = ticket(source);
        let member = GapTicket::new(GapTicketFields {
            source_key: template.source_key(),
            context_digest: ContextDigest::parse_hex(&format!("{source:064x}")).unwrap(),
            ticket_digest: template.ticket_digest(),
            session_key: template.session_key(),
            turn_ordinal: template.turn_ordinal(),
            boundary: template.boundary(),
            head_oid: None,
            captured_at: 1000,
            queued_at: 1200,
            attempts: 2,
        })
        .unwrap();
        rt.block_on(append_ticket(&store, original.gap_id(), &member))
            .unwrap();
    }
    let sql = "SELECT gap_id FROM turn_boundary_gap_ticket WHERE context_sha256 = ?1 ORDER BY gap_id LIMIT 2";
    let params = [Value::text(format!("{:064x}", 73))];
    let rows = rt.block_on(store.query(sql, &params)).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_text(0).unwrap(), original.gap_id().to_hex());
    let plan = rt
        .block_on(store.query(&format!("EXPLAIN QUERY PLAN {sql}"), &params))
        .unwrap();
    let descriptions: Vec<_> = plan.iter().map(|row| row.get_text(3).unwrap()).collect();
    assert!(
        descriptions
            .iter()
            .any(|text| text.contains("USING COVERING INDEX idx_turn_boundary_gap_context")),
        "{descriptions:?}"
    );
    assert!(!descriptions
        .iter()
        .any(|text| text.contains("SCAN turn_boundary_gap_ticket")));
}

async fn raw_member(
    store: &SqliteStore,
    batch: &GapBatch,
    version: i64,
    attempts: i64,
    head: Option<&str>,
) {
    let member = batch.tickets().first().unwrap();
    store
        .insert(
            "turn_boundary_gap_ticket",
            &[
                "gap_id",
                "source_key",
                "context_sha256",
                "ticket_sha256",
                "session_key",
                "turn_ordinal",
                "boundary",
                "head_oid",
                "captured_at",
                "queued_at",
                "attempts",
                "schema_version",
            ],
            &[
                Value::text(batch.gap_id().to_hex()),
                Value::text(member.source_key().to_hex()),
                Value::text(member.context_digest().to_hex()),
                Value::text(member.ticket_digest().to_hex()),
                Value::text(member.session_key().to_hex()),
                Value::Int(i64::from(member.turn_ordinal())),
                Value::text(member.boundary().as_str()),
                head.map_or(Value::Null, Value::text),
                Value::Int(member.captured_at()),
                Value::Int(member.queued_at()),
                Value::Int(attempts),
                Value::Int(version),
            ],
        )
        .await
        .unwrap();
}

async fn raw_parent(store: &SqliteStore, batch: &GapBatch, version: i64, count: i64) {
    store
        .insert(
            "turn_boundary_gap_batch",
            &[
                "gap_id",
                "repo_id",
                "member_key",
                "selection_sha256",
                "evidence_sha256",
                "ticket_count",
                "recorded_at",
                "reason",
                "schema_version",
            ],
            &[
                Value::text(batch.gap_id().to_hex()),
                Value::text(batch.repo_id().as_str()),
                Value::text(batch.member_key().to_hex()),
                Value::text(batch.selection_digest().to_hex()),
                Value::text(batch.evidence_digest().to_hex()),
                Value::Int(count),
                Value::Int(batch.recorded_at()),
                Value::text(batch.reason().as_str()),
                Value::Int(version),
            ],
        )
        .await
        .unwrap();
}

#[test]
fn unsupported_or_corrupt_stored_facts_refuse_reads_and_replay_without_repair() {
    for (parent_version, member_version, count, attempts, bad_head) in [
        (2, 1, 1, 2, false),
        (1, 2, 1, 2, false),
        (1, 1, 2, 2, false),
        (1, 1, 1, -1, false),
        (1, 1, 1, 2, true),
    ] {
        let store = store();
        let rt = runtime();
        let original = batch(1500, vec![ticket(1)]);
        let member = original.tickets().first().unwrap();
        rt.block_on(raw_member(
            &store,
            &original,
            member_version,
            attempts,
            if bad_head {
                Some("bad-head")
            } else {
                member.head_oid()
            },
        ));
        rt.block_on(raw_parent(&store, &original, parent_version, count));
        let before = facts(&store, &rt);
        assert!(rt
            .block_on(boundary_gap_batch(&store, original.gap_id()))
            .is_err());
        assert!(rt
            .block_on(record_boundary_gap_batch(&store, &original))
            .is_err());
        assert_eq!(facts(&store, &rt), before);
    }
}

#[test]
fn whole_batch_preflight_is_read_only_for_absent_partial_complete_and_conflicting_sets() {
    for case in 0..5 {
        let store = store();
        let rt = runtime();
        let expected = batch(1500, vec![ticket(1), ticket(2)]);
        match case {
            1 => {
                rt.block_on(append_ticket(&store, expected.gap_id(), &ticket(1)))
                    .unwrap();
            }
            2 => {
                rt.block_on(record_boundary_gap_batch(&store, &expected))
                    .unwrap();
            }
            3 => {
                rt.block_on(append_ticket(&store, expected.gap_id(), &ticket(99)))
                    .unwrap();
            }
            4 => {
                rt.block_on(append_batch(&store, &expected)).unwrap();
            }
            _ => {}
        }
        let before = facts(&store, &rt);
        assert_eq!(
            rt.block_on(check_boundary_gap_batch(&store, &expected))
                .is_ok(),
            case < 3
        );
        assert_eq!(
            facts(&store, &rt),
            before,
            "preflight never appends or completes facts"
        );
    }
}
