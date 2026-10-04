//! `rift backfill --recall-from`: the serve relations read back out of a
//! recall projection a rebuild set aside.
//!
//! A recall rebuild re-derives every serve a checkpoint shard froze, and sets
//! the old projection aside rather than delete it, because some rows are in no
//! shard: a serve a person made at a terminal, the serves booked before shards
//! froze serves at all, and a grant's typed reason. This module puts those
//! rows back. A module of its own because its source, its target and its
//! writers are all the recall plane's, where the rest of the verb is the
//! capture plane's.
//!
//! **No second writer.** The source holds rows, and the ledger's one appender
//! takes envelopes, so each row is spelled back into the envelope that books
//! exactly that row — the appender reads the key, the figures and the instant
//! off the envelope and never recomputes them — and handed to the appender.
//! The appender's own companion writes follow from it: a decision whose handle
//! is a citation books its `memd_audit_citation` row, the first call on a
//! channel books its `serve_session_event` row, and the attribution a source
//! row carried is written by the attribution writer the live hooks use. A
//! grant goes through its own writer. Every one of them is insert-or-ignore
//! on its key, so a row the target already holds is no new row and a second
//! run inserts nothing.
//!
//! **Source canonical rows are never written.** The selected historical
//! source uses [`SqliteStore::open_historical_snapshot`] with READ_ONLY and
//! one pinned read transaction. WAL pages are included; SQLite may manage
//! WAL/SHM files. Live destination aliases are refused before SQLite opens.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rift_core::{Result, RiftError};
use rift_recall_ledger::attribution::{append_attributed_serve, ServeAttribution};
use rift_recall_ledger::grants::record_serve_grant;
use rift_store::{CanonicalStore, Row, SqliteStore};

use super::TableCount;

/// The relations read back, in the order the report prints them: the two
/// serve relations, then their companions.
const RECALL_TABLES: [&str; 6] = [
    "memd_audit_event",
    "serve_call_event",
    "serve_session_event",
    "memd_audit_citation",
    "serve_grant_event",
    "memd_audit_session",
];

/// The source's attribution rows, by the serve key each names.
type Attributions = BTreeMap<String, ServeAttribution>;

/// What a dry run reports: every relation's rows read, none inserted.
///
/// # Errors
/// [`RiftError::Storage`] when the source cannot be opened read-only or is not
/// a recall projection.
pub(super) async fn count_recall_rows(
    selected: rift_store::HistoricalProjectionSnapshot,
) -> Result<Vec<TableCount>> {
    let (source, present) = open_recall_source(selected).await?;
    let read = source_counts(&source, &present).await?;
    Ok(RECALL_TABLES
        .iter()
        .zip(read)
        .map(|(table, read)| TableCount {
            table,
            read,
            inserted: 0,
        })
        .collect())
}

/// Replay the serve relations of the set-aside projection at `path` into
/// `store`, insert-or-ignore, and count what each relation gained.
///
/// # Errors
/// [`RiftError::Storage`] when the source cannot be opened read-only, is not a
/// recall projection, or a write fails.
pub(super) async fn import_recall_from(
    store: &(impl CanonicalStore + rift_store::Store),
    selected: rift_store::HistoricalProjectionSnapshot,
) -> Result<Vec<TableCount>> {
    selected.refuse_live_destination(Path::new(store.descriptor().path()))?;
    let (source, present) = open_recall_source(selected).await?;
    let read = source_counts(&source, &present).await?;
    let before = target_counts(store).await?;
    let attributions = source_attributions(&source, &present).await?;
    replay_decisions(store, &source, &attributions).await?;
    if present.contains("serve_call_event") {
        replay_calls(store, &source, &attributions).await?;
    }
    if present.contains("serve_grant_event") {
        replay_grants(store, &source).await?;
    }
    let after = target_counts(store).await?;
    Ok(RECALL_TABLES
        .iter()
        .zip(read)
        .zip(before.iter().zip(after))
        .map(|((table, read), (before, after))| TableCount {
            table,
            read,
            inserted: after.saturating_sub(*before),
        })
        .collect())
}

/// Open the source read-only and name the relations it has; refuse a file
/// that is not a recall projection.
///
/// A projection set aside by an older binary may predate the later
/// relations, so each one is read only where the source has it; the two
/// tables every recall projection has had from its first version are what
/// make it one.
async fn open_recall_source(
    selected: rift_store::HistoricalProjectionSnapshot,
) -> Result<(SqliteStore, BTreeSet<String>)> {
    let source = SqliteStore::open_historical_snapshot(selected)?;
    let present: BTreeSet<String> = source
        .query("SELECT name FROM sqlite_master WHERE type = 'table'", &[])
        .await
        .map_err(|_| not_a_recall_projection())?
        .iter()
        .map(|row| row.get_text(0))
        .collect::<Result<_>>()?;
    if !present.contains("graph_node") || !present.contains("memd_audit_event") {
        return Err(not_a_recall_projection());
    }
    Ok((source, present))
}

/// The refusal, worded as the capture importer words its own.
fn not_a_recall_projection() -> RiftError {
    RiftError::storage(
        "the source is not a recall projection (no `graph_node` and `memd_audit_event` \
         tables) — refusing to import rows out of a file whose shape is unknown",
    )
}

/// Rows per relation in the source, in [`RECALL_TABLES`] order; a relation
/// the source predates reads as none.
async fn source_counts(source: &SqliteStore, present: &BTreeSet<String>) -> Result<Vec<usize>> {
    let mut out = Vec::new();
    for table in RECALL_TABLES {
        let n = if present.contains(table) {
            source.count(table).await?
        } else {
            0
        };
        out.push(usize::try_from(n).unwrap_or(0));
    }
    Ok(out)
}

/// Rows per relation in the target, in [`RECALL_TABLES`] order.
async fn target_counts(store: &impl CanonicalStore) -> Result<Vec<usize>> {
    let mut out = Vec::new();
    for table in RECALL_TABLES {
        out.push(usize::try_from(store.count(table).await?).unwrap_or(0));
    }
    Ok(out)
}

/// The source's attribution rows, keyed by serve; none where it predates them.
async fn source_attributions(
    source: &SqliteStore,
    present: &BTreeSet<String>,
) -> Result<Attributions> {
    if !present.contains("memd_audit_session") {
        return Ok(Attributions::new());
    }
    let rows = source
        .query(
            "SELECT serve_key, session_id, window_key FROM memd_audit_session",
            &[],
        )
        .await?;
    rows.iter()
        .map(|row| {
            let attribution = ServeAttribution {
                session_id: row.get_text(1)?,
                window_key: row.get_opt_text(2)?,
            };
            Ok((row.get_text(0)?, attribution))
        })
        .collect()
}

/// Every decision row, oldest first, through the appender.
async fn replay_decisions(
    store: &impl CanonicalStore,
    source: &SqliteStore,
    attributions: &Attributions,
) -> Result<()> {
    let rows = source
        .query(
            "SELECT audit_id, verb, handle, reason, tokens_returned, tokens_avoided, \
             recorded_at FROM memd_audit_event ORDER BY recorded_at, audit_id",
            &[],
        )
        .await?;
    for row in &rows {
        let audit_id = row.get_text(0)?;
        let recorded_at = row.get_i64(6)?;
        let envelope = decision_envelope(&audit_id, row)?;
        append_attributed_serve(store, &envelope, recorded_at, attributions.get(&audit_id)).await?;
    }
    Ok(())
}

/// The schema-1 envelope that books exactly this decision row: the appender
/// writes `handle`, `reason` and the two estimates from the payload, and the
/// instant from the clock it is handed.
fn decision_envelope(audit_id: &str, row: &Row) -> Result<Vec<u8>> {
    let envelope = serde_json::json!({
        "schema": 1,
        "idempotency_key": audit_id,
        "verb": row.get_text(1)?,
        "payload": {
            "handle": row.get_opt_text(2)?,
            "reason": row.get_opt_text(3)?,
            "estimated_tokens_returned": row.get_i64(4)?,
            "estimated_tokens_avoided": row.get_i64(5)?,
        },
    });
    serde_json::to_vec(&envelope).map_err(|e| RiftError::storage(format!("serve envelope: {e}")))
}

/// Every call row through the appender, the call each channel was first
/// served by ahead of the rest, so the channel's first-served row the
/// appender books names the instant the source's does.
async fn replay_calls(
    store: &impl CanonicalStore,
    source: &SqliteStore,
    attributions: &Attributions,
) -> Result<()> {
    // `serve_session_event` arrived in the same schema version as
    // `serve_call_event`, so a source with one has the other.
    let rows = source
        .query(
            "SELECT c.call_id, c.channel, c.tool_name, c.terminal_verb, c.tokens_returned, \
             c.tokens_avoided, c.recorded_at FROM serve_call_event c \
             LEFT JOIN serve_session_event s ON s.channel = c.channel \
             ORDER BY c.recorded_at <> coalesce(s.first_served_at, c.recorded_at), \
             c.recorded_at, c.call_id",
            &[],
        )
        .await?;
    for row in &rows {
        let call_id = row.get_text(0)?;
        let recorded_at = row.get_i64(6)?;
        let envelope = call_envelope(&call_id, row)?;
        append_attributed_serve(store, &envelope, recorded_at, attributions.get(&call_id)).await?;
    }
    Ok(())
}

/// The schema-4 envelope that books exactly this call row.
fn call_envelope(call_id: &str, row: &Row) -> Result<Vec<u8>> {
    let envelope = serde_json::json!({
        "schema": 4,
        "idempotency_key": call_id,
        "verb": row.get_text(3)?,
        "serve_call": {
            "call_id": call_id,
            "channel": row.get_text(1)?,
            "tool_name": row.get_text(2)?,
            "tokens_returned": row.get_i64(4)?,
            "tokens_avoided": row.get_i64(5)?,
            "recorded_at": row.get_i64(6)?,
        },
    });
    serde_json::to_vec(&envelope).map_err(|e| RiftError::storage(format!("serve envelope: {e}")))
}

/// Every grant's reason through the grant writer, which derives the same id
/// from the same three values.
async fn replay_grants(store: &impl CanonicalStore, source: &SqliteStore) -> Result<()> {
    let rows = source
        .query(
            "SELECT audit_id, kind, reason_text, recorded_at FROM serve_grant_event \
             ORDER BY recorded_at, grant_id",
            &[],
        )
        .await?;
    for row in &rows {
        record_serve_grant(
            store,
            &row.get_text(0)?,
            &row.get_text(1)?,
            &row.get_text(2)?,
            row.get_i64(3)?,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    async fn import_recall_from(store: &SqliteStore, path: &Path) -> Result<Vec<TableCount>> {
        let selected = rift_store::HistoricalProjectionSnapshot::user_selected(path)?;
        super::import_recall_from(store, selected).await
    }

    async fn count_recall_rows(path: &Path) -> Result<Vec<TableCount>> {
        let selected = rift_store::HistoricalProjectionSnapshot::user_selected(path)?;
        super::count_recall_rows(selected).await
    }

    #[tokio::test]
    async fn a_markerless_preserved_wal_triple_imports_its_wal_only_serves() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.db");
        let writer = open_migrated(&live, &recall_migrations()).unwrap();
        let conn = rift_store::Store::conn(&writer).await.unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0")
            .await
            .unwrap();
        let event = MemdEvent::GrepCapped {
            handle: "grep:matches=7".into(),
        };
        book(&writer, &event, "wal-call", AT, None).await;
        let preserved = dir.path().join("selected-history.db");
        for suffix in ["", "-wal", "-shm"] {
            std::fs::copy(
                format!("{}{suffix}", live.display()),
                format!("{}{suffix}", preserved.display()),
            )
            .unwrap();
        }
        assert!(!dir.path().join("selected-history.db.lock").exists());
        let target = recall_target(dir.path());
        let imported = import_recall_from(&target, &preserved).await.unwrap();
        assert_eq!(
            imported
                .iter()
                .find(|count| count.table == "memd_audit_event")
                .unwrap()
                .inserted,
            1
        );
        assert_eq!(
            imported
                .iter()
                .find(|count| count.table == "serve_call_event")
                .unwrap()
                .inserted,
            1
        );
        assert!(!dir.path().join("selected-history.db.lock").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::remove_file(format!("{}-shm", preserved.display())).unwrap();
            for suffix in ["", "-wal"] {
                std::fs::set_permissions(
                    format!("{}{suffix}", preserved.display()),
                    std::fs::Permissions::from_mode(0o444),
                )
                .unwrap();
            }
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
            let result = count_recall_rows(&preserved).await;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
            match result {
                Ok(counts) => assert_eq!(
                    counts
                        .iter()
                        .find(|count| count.table == "memd_audit_event")
                        .unwrap()
                        .read,
                    1
                ),
                Err(error) => assert!(!error.to_string().is_empty()),
            }
        }
    }
    use rift_recall_events::{MemdEvent, ServeChannel};
    use rift_recall_ledger::grants::CROSS_BRANCH_GRANT_TAG;
    use rift_store::{open_migrated, recall_migrations};

    use super::*;

    const AT: i64 = 1_790_856_000_000;

    /// Every row of the six relations, as text, sorted: two projections hold
    /// the same rows exactly when these are equal.
    async fn every_row(store: &impl CanonicalStore) -> Vec<String> {
        let mut out = Vec::new();
        for table in RECALL_TABLES {
            for row in store
                .query(&format!("SELECT * FROM {table}"), &[])
                .await
                .unwrap()
            {
                out.push(format!("{table} {row:?}"));
            }
        }
        out.sort();
        out
    }

    /// A source booked the way the live writers book it: a person-made
    /// serve, a session's serve with its window, a citation decision, and a
    /// widened serve with its typed reason.
    async fn seed_source(path: &Path) {
        let store = open_migrated(path, &recall_migrations()).unwrap();
        let grep = |n: u32| MemdEvent::GrepCapped {
            handle: format!("grep:matches={n}"),
        };
        book(&store, &grep(3), "call-person", AT, None).await;
        let attribution = ServeAttribution {
            session_id: "s-1".to_owned(),
            window_key: Some("a".repeat(64)),
        };
        book(&store, &grep(4), "call-session", AT + 1, Some(&attribution)).await;
        let citation = MemdEvent::Refused {
            target: format!("pack:{}/evidence:2", "b".repeat(64)),
            reason: "not_found".to_owned(),
            estimated_tokens_returned: 9,
        };
        book(&store, &citation, "call-cite", AT + 2, None).await;
        let widened = format!(
            r#"{{"schema":1,"idempotency_key":"grant-1","verb":"memd.exact_retrieved",
                "payload":{{"handle":"log:x","reason":"{CROSS_BRANCH_GRANT_TAG}"}}}}"#
        );
        append_attributed_serve(&store, widened.as_bytes(), AT + 3, None)
            .await
            .unwrap();
        record_serve_grant(&store, "grant-1", "branches", "reviewing a hotfix", AT + 3)
            .await
            .unwrap();
    }

    /// One decision's pair, as the skill CLI books it, attributed when a
    /// session made it.
    async fn book(
        store: &SqliteStore,
        event: &MemdEvent,
        call: &str,
        at: i64,
        attribution: Option<&ServeAttribution>,
    ) {
        let audited = event.to_hook_json_keyed(call);
        let called = event.to_serve_call_json(
            ServeChannel::SkillCli,
            call,
            "recall:grep",
            rift_core::Timestamp::from_millis(at),
        );
        for envelope in [audited, called] {
            append_attributed_serve(store, envelope.as_bytes(), at, attribution)
                .await
                .unwrap();
        }
    }

    fn recall_target(dir: &Path) -> SqliteStore {
        open_migrated(dir.join("recall.db"), &recall_migrations()).unwrap()
    }

    fn sha256_of(path: &Path) -> String {
        rift_hashchain::leaf_hash(&std::fs::read(path).unwrap()).to_hex()
    }

    #[tokio::test]
    async fn every_row_of_the_six_relations_comes_back_and_a_second_run_inserts_none() {
        let tmp = tempfile::tempdir().unwrap();
        let source_path = tmp.path().join("recall.db.set-aside-20261001T120000Z");
        seed_source(&source_path).await;
        let record = sha256_of(&source_path);
        let target = recall_target(tmp.path());

        let first = import_recall_from(&target, &source_path).await.unwrap();

        let source = SqliteStore::open_read_only(&source_path).unwrap();
        assert_eq!(every_row(&target).await, every_row(&source).await);
        assert!(
            first.iter().all(|t| t.read == t.inserted && t.read > 0),
            "{first:?}"
        );
        let again = import_recall_from(&target, &source_path).await.unwrap();
        assert!(again.iter().all(|t| t.inserted == 0), "{again:?}");
        assert_eq!(every_row(&target).await, every_row(&source).await);
        assert_eq!(
            sha256_of(&source_path),
            record,
            "the source is never written"
        );
    }

    #[tokio::test]
    async fn a_dry_run_reads_every_relation_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let source_path = tmp.path().join("recall.db.set-aside-20261001T120000Z");
        seed_source(&source_path).await;
        let record = sha256_of(&source_path);

        let counts = count_recall_rows(&source_path).await.unwrap();

        let read: Vec<(&str, usize, usize)> = counts
            .iter()
            .map(|t| (t.table, t.read, t.inserted))
            .collect();
        assert_eq!(
            read,
            [
                ("memd_audit_event", 4, 0),
                ("serve_call_event", 3, 0),
                ("serve_session_event", 1, 0),
                ("memd_audit_citation", 1, 0),
                ("serve_grant_event", 1, 0),
                ("memd_audit_session", 2, 0),
            ]
        );
        assert_eq!(sha256_of(&source_path), record);
    }

    #[tokio::test]
    async fn a_capture_projection_or_a_stranger_is_refused_and_nothing_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let capture = tmp.path().join("projection.db");
        drop(rift_store::open_capture_projection(&capture).unwrap());
        let stranger = tmp.path().join("notes.db");
        drop(SqliteStore::open(&stranger).unwrap());
        let target = recall_target(tmp.path());

        for source in [&capture, &stranger] {
            let err = import_recall_from(&target, source).await.unwrap_err();
            assert!(err.to_string().contains("not a recall projection"), "{err}");
            assert!(count_recall_rows(source).await.is_err());
        }
        assert!(every_row(&target).await.is_empty());
    }

    #[tokio::test]
    async fn a_missing_source_is_an_error_and_is_not_created() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("recall.db.set-aside-gone");
        let target = recall_target(tmp.path());
        assert!(import_recall_from(&target, &missing).await.is_err());
        assert!(!missing.exists());
    }
}
