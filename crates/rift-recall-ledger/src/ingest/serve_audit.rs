//! The serves a rebuild restores: a checkpoint's `serve_audit.jsonl`, read
//! back through the one appender.
//!
//! A module of its own because it is the second road into the serve
//! relations beside the live one, and the two rules that make the two
//! converge on one row per serve belong together: the key is read off the
//! line and never recomputed, and the clock is the instant the live row was
//! written at and never the rebuild's.
//!
//! There is no backfill for checkpoints a projection already holds. The
//! incremental ingest reads the artifact the first time it projects a
//! checkpoint, and a cold rebuild reads every shard; the only checkpoints it
//! would skip unread are ones a binary projected that wrote the artifact but
//! could not read it, and the artifact and this reader ship together, so no
//! such projection exists.

use rift_core::Result;
use rift_recall_substrate::parse::{self, ParseIssue, ServeAuditEnvelope};
use rift_recall_substrate::SubstrateReader;
use rift_store::CanonicalStore;

/// The artifact the serves are frozen into.
const ARTIFACT: &str = "serve_audit.jsonl";

/// Read and validate one checkpoint's frozen serves; an absent artifact reads
/// as none.
///
/// # Errors
/// Propagates [`SubstrateReader::read_artifact`] read errors.
pub(super) fn read_serves(
    reader: &impl SubstrateReader,
    cid: &str,
    issues: &mut Vec<ParseIssue>,
) -> Result<Vec<ServeAuditEnvelope>> {
    let envelopes = super::read_parsed(reader, cid, ARTIFACT, parse::parse_serve_audit, issues)?;
    Ok(envelopes.unwrap_or_default())
}

/// Hand every frozen envelope to the appender, at the instant its live row
/// was written.
///
/// The appender is insert-or-ignore on the key the envelope carries, so a
/// serve the live worker already booked, or a line an earlier checkpoint of
/// the same session froze too, is no new row.
///
/// # Errors
/// Propagates store write errors.
pub(super) async fn append_serves(
    store: &impl CanonicalStore,
    envelopes: &[ServeAuditEnvelope],
) -> Result<()> {
    for frozen in envelopes {
        crate::audit::append_memd_audit(store, &frozen.envelope, frozen.recorded_at).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rift_recall_events::{MemdEvent, ServeChannel};
    use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
    use rift_store::{recall_migrations, SqliteStore};

    use super::*;
    use crate::audit::append_memd_audit;
    use crate::ingest::ingest;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    fn shard(id: &str, ord: u64) -> CheckpointShard {
        CheckpointShard {
            checkpoint_id: id.into(),
            dialect: Dialect::Rift,
            prev: None,
            ordinal: ord,
        }
    }

    /// A read the gate declined, with figures the generator picks.
    fn declined(avoided: u32) -> MemdEvent {
        MemdEvent::ReadIntercepted {
            handle: format!("file:src/a.rs@sha256:{}:L1-40", "a".repeat(64)),
            reason: "already_held".to_owned(),
            estimated_tokens_returned: 40,
            estimated_tokens_avoided: avoided,
        }
    }

    /// One hook decision's live pair: the audit twin and the call, both
    /// written at `at` as the worker writes them.
    fn live_pair(event: &MemdEvent, call_id: &str, at: i64) -> [String; 2] {
        [
            event.to_hook_json_keyed(call_id),
            event.to_serve_call_json(
                ServeChannel::HookInject,
                call_id,
                "claude-code:read-gate",
                rift_core::Timestamp::from_millis(at),
            ),
        ]
    }

    /// A live envelope's line as the canonical writer freezes it: keys sorted
    /// and, on schema 1, the instant its row was written at. The events these
    /// tests use carry no typed text, so nothing is withheld.
    fn frozen_line(envelope: &str, at: i64) -> String {
        let mut value: serde_json::Value = serde_json::from_str(envelope).unwrap();
        if value["schema"] == 1 {
            value["recorded_at"] = at.into();
        }
        value.to_string()
    }

    fn artifact(lines: &[String]) -> Vec<u8> {
        lines
            .iter()
            .flat_map(|l| format!("{l}\n").into_bytes())
            .collect()
    }

    fn append(store: &SqliteStore, envelope: &str, at: i64) {
        block(append_memd_audit(store, envelope.as_bytes(), at)).unwrap();
    }

    /// `(key, recorded_at)` of every row of both serve relations, sorted.
    fn serve_rows(store: &SqliteStore) -> Vec<(String, i64)> {
        let mut rows = Vec::new();
        for sql in [
            "SELECT audit_id, recorded_at FROM memd_audit_event",
            "SELECT call_id, recorded_at FROM serve_call_event",
        ] {
            for row in block(store.query(sql, &[])).unwrap() {
                rows.push((row.get_text(0).unwrap(), row.get_i64(1).unwrap()));
            }
        }
        rows.sort();
        rows
    }

    fn count(store: &SqliteStore, table: &str) -> i64 {
        block(store.count(table)).unwrap()
    }

    proptest! {
        // Each case migrates a fresh projection; a few dozen cover the orders
        // and the key shapes without making the suite wait on migrations.
        #![proptest_config(ProptestConfig::with_cases(24))]

        /// One decision, one key, one row: the live envelopes and the frozen
        /// lines of the same serves land on the same keys whichever arrives
        /// first, and every rebuilt row carries the instant the live row was
        /// written at — including a spend that has no schema-4 sibling.
        #[test]
        fn a_live_serve_and_its_frozen_line_leave_one_row_at_the_live_instant(
            serves in proptest::collection::vec((0u32..5_000, 1_700_000_000_000i64..1_800_000_000_000), 1..4),
            spend in (1u32..900, 1_700_000_000_000i64..1_800_000_000_000),
            live_first in any::<bool>(),
        ) {
            let mut live = Vec::new();
            for (i, (avoided, at)) in serves.iter().enumerate() {
                for envelope in live_pair(&declined(*avoided), &format!("call-{i}"), *at) {
                    live.push((envelope, *at));
                }
            }
            let injected = MemdEvent::ManifestInjected { estimated_tokens_returned: spend.0 };
            live.push((injected.to_hook_json_keyed("turn-1"), spend.1));
            let lines: Vec<String> = live.iter().map(|(e, at)| frozen_line(e, *at)).collect();
            let sub = MemorySubstrate::local()
                .with_shard(shard("c1", 1))
                .with_artifact("c1", ARTIFACT, &artifact(&lines));

            let store = recall_store();
            if !live_first {
                block(ingest(&store, &sub)).unwrap();
            }
            for (envelope, at) in &live {
                append(&store, envelope, *at);
            }
            if live_first {
                block(ingest(&store, &sub)).unwrap();
            }

            let mut expected: Vec<(String, i64)> = live
                .iter()
                .map(|(e, at)| {
                    let value: serde_json::Value = serde_json::from_str(e).unwrap();
                    (value["idempotency_key"].as_str().unwrap().to_owned(), *at)
                })
                .collect();
            expected.sort();
            expected.dedup();
            prop_assert_eq!(serve_rows(&store), expected);
        }
    }

    /// A session's second checkpoint repeats the first one's serves and adds
    /// one: a cold ingest books each distinct key once, a second pass books
    /// nothing, and a checkpoint without the artifact books no serve.
    #[test]
    fn a_cold_ingest_books_each_frozen_serve_once_across_a_sessions_checkpoints() {
        let [a_audit, a_call] = live_pair(&declined(100), "call-a", 1_750_000_000_000);
        let [b_audit, b_call] = live_pair(&declined(200), "call-b", 1_750_000_060_000);
        let first = [
            frozen_line(&a_call, 1_750_000_000_000),
            frozen_line(&a_audit, 1_750_000_000_000),
        ];
        let mut second = first.to_vec();
        second.push(frozen_line(&b_call, 1_750_000_060_000));
        second.push(frozen_line(&b_audit, 1_750_000_060_000));
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_shard(shard("c2", 2))
            .with_shard(shard("c3", 3))
            .with_artifact("c1", ARTIFACT, &artifact(&first))
            .with_artifact("c2", ARTIFACT, &artifact(&second));
        let store = recall_store();

        let report = block(ingest(&store, &sub)).unwrap();
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        assert_eq!(count(&store, "memd_audit_event"), 2);
        assert_eq!(count(&store, "serve_call_event"), 2);
        let rows = serve_rows(&store);

        block(ingest(&store, &sub)).unwrap();
        assert_eq!(serve_rows(&store), rows, "a second pass books nothing");
    }

    /// A checkpoint without the artifact projects the rows it always did and
    /// no serve.
    #[test]
    fn a_checkpoint_without_the_artifact_ingests_as_before_and_books_no_serve() {
        let tool_calls =
            b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"h1\",\"input_tokens\":7}\n";
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", "tool_calls.jsonl", tool_calls);
        let store = recall_store();
        let report = block(ingest(&store, &sub)).unwrap();
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        assert_eq!(count(&store, "n_file_read"), 1);
        assert_eq!(count(&store, "memd_audit_event"), 0);
        assert_eq!(count(&store, "serve_call_event"), 0);
        assert_eq!(count(&store, "serve_session_event"), 0);
        assert_eq!(
            count(&store, "retention_lineage"),
            1,
            "the read's own lineage only"
        );
    }
}
