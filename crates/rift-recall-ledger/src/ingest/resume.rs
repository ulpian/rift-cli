//! Where index warm resumes: the durable watermark per dialect, the stream
//! position its tip held, and the frontier the next ingest reads from them.
//!
//! A module of its own because this is the half of the ingest that talks to
//! the one resume path bundle export also takes
//! ([`rift_store::suffix_after_by`]), and the half that writes what that path
//! needs next time; the rest of the ingest only asks the frontier whether a
//! shard is new.

use std::collections::{BTreeMap, BTreeSet};

use rift_core::Result;
use rift_recall_substrate::CheckpointShard;
use rift_store::{CanonicalStore, StreamCursor};

use super::{record_watermark, IngestReport};
use crate::graph::sha256_id;

/// One dialect's stream: its shards in walk order, keyed by the dialect's
/// checkpoint branch.
type DialectStreams<'a> = BTreeMap<&'static str, Vec<&'a CheckpointShard>>;

/// Which checkpoints of each dialect's stream this index has **not** seen.
///
/// Built once per run and consulted per shard. A dialect absent from the map
/// took a full pass, so every shard of it is admitted.
#[derive(Debug, Default)]
pub(super) struct ResumeFrontier {
    unseen: BTreeMap<&'static str, BTreeSet<String>>,
}

impl ResumeFrontier {
    /// Whether `id` on `branch` is work this run should look at.
    pub(super) fn admits(&self, branch: &str, id: &str) -> bool {
        self.unseen
            .get(branch)
            .is_none_or(|unseen| unseen.contains(id))
    }
}

/// Resume index warm from the durable watermark, **through the same function
/// bundle export resumes through** ([`rift_store::suffix_after_by`]).
///
/// The emphasis is on *shared*: agreement between two implementations is not
/// a property; calling one function is. And both consumers now hand that
/// function a **persisted** cursor: export its manifest's, index warm the tip
/// its watermark names at the position recorded beside it.
///
/// Per dialect, four outcomes, each counted:
///
/// * **no watermark** — the cold case; full pass, `full_pass += 1`;
/// * **the recorded tip at its recorded position** — resume, `resumed += 1`;
/// * **another checkpoint at that position, or a stream too short to have
///   one** — the stream was rewritten under this index (history rewritten, a
///   shard sorted in before the tip, a projection restored from elsewhere).
///   The cursor's id-at-position arm refuses, and the run takes a full pass,
///   `full_pass += 1` and `rewritten += 1`. This is the arm that catches a
///   shard inserted *before* a tip that survived: locating the tip again would
///   resume after its new place and never read the inserted shard;
/// * **a watermark written before positions were kept** — the tip is located
///   in the walk instead ([`rift_store::cursor_of`]), and a tip the walk no
///   longer holds is a full pass and a rewrite as above.
///
/// A rewrite is deliberately *not* an error: a full pass is correct by
/// content-addressed set-union, merely slower, and an index that refused to
/// warm because git changed would be worse than one that re-reads.
pub(super) async fn resume_frontier(
    store: &impl CanonicalStore,
    shards: &[CheckpointShard],
    report: &mut IngestReport,
) -> Result<ResumeFrontier> {
    let mut frontier = ResumeFrontier::default();
    for (branch, stream) in dialect_streams(shards) {
        let watermark =
            rift_store::capture::latest_ingest_watermark_with_position(store, branch).await?;
        let Some((tip, position)) = watermark else {
            report.full_pass += 1;
            continue;
        };
        let resumed = resume_cursor(&stream, &tip, position).and_then(|cursor| {
            rift_store::suffix_after_by(&stream, &cursor, |shard| shard_id(shard)).ok()
        });
        let Some(unseen) = resumed else {
            report.full_pass += 1;
            report.rewritten += 1;
            continue;
        };
        report.resumed += 1;
        frontier.unseen.insert(
            branch,
            unseen.iter().map(|s| s.checkpoint_id.clone()).collect(),
        );
    }
    Ok(frontier)
}

/// Append each dialect's watermark row and the stream position of its tip.
///
/// The position is read from the same per-dialect walk
/// [`resume_frontier`] resumes over, so the next run checks the tip against
/// the very ordering it was recorded in. A tip the walk does not hold records
/// no position, and the next run locates it as it would a row written before
/// positions were kept.
///
/// # Errors
/// Propagates store write errors.
pub(super) async fn record_resume_points(
    store: &impl CanonicalStore,
    shards: &[CheckpointShard],
    tips: &BTreeMap<&'static str, (u64, String)>,
    now_ms: i64,
) -> Result<()> {
    let streams = dialect_streams(shards);
    for (branch, (_, tip)) in tips {
        if tip.is_empty() {
            continue;
        }
        // Content-addressed on (branch, tip), so a re-run at the same tip
        // dedupes.
        let wid = sha256_id("rift.wm.v1", &[branch.as_bytes(), tip.as_bytes()]);
        record_watermark(store, &wid, branch, tip, now_ms).await?;
        let position = streams
            .get(branch)
            .and_then(|stream| rift_store::cursor_of(stream, tip, |shard| shard_id(shard)));
        let Some(cursor) = position else {
            continue;
        };
        rift_store::capture::record_ingest_watermark_position(store, &wid, cursor.chain_seq())
            .await?;
    }
    Ok(())
}

/// The cursor a watermark resumes from: the recorded tip at its recorded
/// position when there is one, or the tip located in `stream` when there is
/// not. `None` means the tip cannot be found at all.
///
/// A recorded position whose tip no cursor can spell falls back to locating
/// it: such a tip was never resumable by position, and the presence check is
/// what resumed it before positions were kept.
fn resume_cursor(
    stream: &[&CheckpointShard],
    tip: &str,
    position: Option<u64>,
) -> Option<StreamCursor> {
    let recorded = position.and_then(|seq| StreamCursor::at(seq, tip).ok());
    match recorded {
        Some(cursor) => Some(cursor),
        None => rift_store::cursor_of(stream, tip, |shard| shard_id(shard)),
    }
}

/// The walk split into one stream per dialect, each in walk order.
fn dialect_streams(shards: &[CheckpointShard]) -> DialectStreams<'_> {
    let mut by_dialect = DialectStreams::new();
    for shard in shards {
        by_dialect
            .entry(shard.dialect.checkpoint_ref())
            .or_default()
            .push(shard);
    }
    by_dialect
}

/// The name a stream item goes by on the resume path.
fn shard_id(shard: &CheckpointShard) -> String {
    shard.checkpoint_id.clone()
}

#[cfg(test)]
mod tests {
    use rift_recall_substrate::{CheckpointShard, MemorySubstrate, SubstrateReader};
    use rift_store::CanonicalStore;

    use super::super::ingest;
    use super::super::tests::{block, recall_store, shard};

    /// **The shared resume path.**
    ///
    /// Index warm and bundle export must not merely *agree* about what a cursor
    /// leaves unseen — they must ask the same function. This asserts the
    /// answers are identical for the same cursor over the same stream, and it
    /// would still pass if they were two implementations that agreed, so the
    /// load-bearing half is the other one: `resume_frontier` calls
    /// `rift_store::suffix_after_by`, the body `suffix_after` (export's path)
    /// delegates to. Break that delegation and the two answers diverge here.
    #[test]
    fn the_index_warm_resumes_through_the_same_suffix_function_as_export() {
        let tc = b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"h1\"}\n";
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_shard(shard("c2", 2))
            .with_shard(shard("c3", 3))
            .with_artifact("c1", "tool_calls.jsonl", tc)
            .with_artifact("c2", "tool_calls.jsonl", tc)
            .with_artifact("c3", "tool_calls.jsonl", tc);
        let store = recall_store();

        // Warm the index over the first two, then let the third arrive.
        let partial = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_shard(shard("c2", 2))
            .with_artifact("c1", "tool_calls.jsonl", tc)
            .with_artifact("c2", "tool_calls.jsonl", tc);
        block(ingest(&store, &partial)).unwrap();

        let full = block(ingest(&store, &sub)).unwrap();
        assert_eq!(full.resumed, 1, "the second run resumed from a cursor");
        assert_eq!(full.full_pass, 0);
        assert_eq!(full.rewritten, 0);
        assert_eq!(full.checkpoints, 3, "every shard is still walked");
        assert_eq!(full.skipped, 2, "c1 and c2 are behind the cursor");

        // …and the suffix the ingest worked on is the one EXPORT's function
        // hands back for the same cursor over the same stream.
        let shards = sub.checkpoints().unwrap();
        let cursor =
            rift_store::cursor_of(&shards, "c2", |s: &CheckpointShard| s.checkpoint_id.clone())
                .expect("the walk holds the tip the watermark recorded");
        let expected: Vec<String> =
            rift_store::suffix_after_by(&shards, &cursor, |s: &CheckpointShard| {
                s.checkpoint_id.clone()
            })
            .unwrap()
            .iter()
            .map(|s| s.checkpoint_id.clone())
            .collect();
        assert_eq!(expected, vec!["c3".to_owned()]);
    }

    /// A stream rewritten under the index falls back to a full pass, loudly
    /// enough to count — never to a silently wrong suffix.
    ///
    /// The recorded tip is simply gone from the walk (history rewritten, a
    /// projection restored from elsewhere). Resuming "after" it is
    /// unanswerable, so the run re-reads; correctness is the content-addressed
    /// seen-set, and the cost is visible in `full_pass`.
    #[test]
    fn a_tip_the_walk_no_longer_holds_falls_back_to_a_full_pass() {
        let tc = b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"h1\"}\n";
        let store = recall_store();
        let first = MemorySubstrate::local()
            .with_shard(shard("gone", 1))
            .with_artifact("gone", "tool_calls.jsonl", tc);
        block(ingest(&store, &first)).unwrap();

        // The rewrite: the recorded tip is not in this walk at all.
        let rewritten = MemorySubstrate::local()
            .with_shard(shard("x1", 1))
            .with_shard(shard("x2", 2))
            .with_artifact("x1", "tool_calls.jsonl", tc)
            .with_artifact("x2", "tool_calls.jsonl", tc);
        let r = block(ingest(&store, &rewritten)).unwrap();
        assert_eq!(r.full_pass, 1, "an unlocatable tip is a full pass");
        assert_eq!(r.rewritten, 1, "and it is counted as a rewrite");
        assert_eq!(r.resumed, 0);
        assert_eq!(r.skipped, 0, "nothing was behind a cursor, so nothing was");
        assert_eq!(r.checkpoints, 2);
    }

    /// The watermark's tip and its place in the walk are one fact, so an
    /// ingest writes both: c2 sits second in the walk that produced it.
    #[test]
    fn an_ingest_records_its_tips_position_beside_the_watermark() {
        let tc = b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"h1\"}\n";
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_shard(shard("c2", 2))
            .with_artifact("c1", "tool_calls.jsonl", tc)
            .with_artifact("c2", "tool_calls.jsonl", tc);
        let store = recall_store();
        block(ingest(&store, &sub)).unwrap();
        let rows = block(store.query(
            "SELECT w.branch_tip, p.chain_seq FROM ingest_watermark w \
             JOIN ingest_watermark_position p ON p.watermark_id = w.watermark_id",
            &[],
        ))
        .unwrap();
        assert_eq!(rows.len(), 1, "one position per watermark row");
        assert_eq!(rows[0].get_text(0).unwrap(), "c2");
        assert_eq!(rows[0].get_i64(1).unwrap(), 2);
    }

    /// A shard inserted before the recorded tip moves the tip without removing
    /// it, so presence alone would resume after the tip's new place and never
    /// read the inserted shard. The recorded position names another checkpoint
    /// now, which is the rewrite the position arm exists to catch.
    #[test]
    fn a_shard_inserted_before_the_recorded_tip_is_caught_and_ingested() {
        let tc = b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"h1\"}\n";
        let store = recall_store();
        let first = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_shard(shard("c2", 2))
            .with_artifact("c1", "tool_calls.jsonl", tc)
            .with_artifact("c2", "tool_calls.jsonl", tc);
        block(ingest(&store, &first)).unwrap();

        let rewritten = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_shard(shard("cx", 2))
            .with_shard(shard("c2", 3))
            .with_artifact("c1", "tool_calls.jsonl", tc)
            .with_artifact("cx", "tool_calls.jsonl", tc)
            .with_artifact("c2", "tool_calls.jsonl", tc);
        let r = block(ingest(&store, &rewritten)).unwrap();
        let held = block(store.query(
            "SELECT count(*) FROM n_checkpoint WHERE node_id = 'cx'",
            &[],
        ))
        .unwrap();
        assert_eq!(
            held[0].get_i64(0).unwrap(),
            1,
            "the inserted shard is ingested"
        );
        assert_eq!(r.full_pass, 1, "a moved tip is a full pass");
        assert_eq!(r.rewritten, 1, "caught by the position, not by presence");
        assert_eq!(r.resumed, 0);
    }
}
